//! Desktop notifications, called alerts here to keep them apart from the
//! core's database notifications. Port of upstream applogic's
//! `notifications/` (the non-Android path) and `NotificationPolicy`:
//! - which processed queue events alert, and what they say;
//! - what is shown given what the user is looking at and the
//!   `notifications` setting;
//! - the suppression reported to the user's other devices.
//!
//! Alerts go to the notifier (`crate::notifier`) through a bounded channel.
//! There is one alert per chat. A newer alert replaces it, so a busy chat
//! can't flood the desktop.

use std::collections::HashSet;

use aircommon::messages::client_state::NotificationSuppression;
use aircoreclient::clients::process::process_qs::{
    NewChat, ProcessedQsMessages, ReactionNotification,
};
use aircoreclient::clients::{CoreUser, SiblingClientStates};
use aircoreclient::{Chat, ChatId, ChatMessage, ChatType, StoredRequest};
use tokio::sync::{mpsc, watch};
use tracing::{error, warn};

use super::preview;
use crate::model::{Prefix, name_line, preview_line};
use crate::settings::Notifications;

/// Alerts waiting for the notifier. Peers decide how many arrive; beyond
/// this they are dropped.
pub(crate) const NOTICES_BUFFER: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alert {
    pub chat: ChatId,
    /// One bounded line each; peer-chosen text is never passed on raw.
    pub title: String,
    pub body: String,
}

/// What the notifier is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Show(Alert),
    /// The chat's alert is stale: opened, answered elsewhere, or moved.
    Dismiss(ChatId),
}

/// What the user is looking at, and what they want to be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attention {
    pub window_active: bool,
    pub open_chat: Option<ChatId>,
    pub content: Notifications,
}

impl Attention {
    /// The chat the user is looking at.
    fn watched(&self) -> Option<ChatId> {
        self.open_chat.filter(|_| self.window_active)
    }

    /// What this client suppresses, for the user's other devices. As
    /// upstream on desktop: only the chat on screen, and only while the
    /// window is active. The chat list sits beside it and suppresses
    /// nothing. Turning alerts off here doesn't silence the other devices.
    pub fn suppression(&self) -> NotificationSuppression {
        match self.watched() {
            Some(chat) => NotificationSuppression::Chat(chat.uuid()),
            None => NotificationSuppression::None,
        }
    }

    /// The alert as it may be shown, if at all.
    fn shown(&self, alert: Alert) -> Option<Alert> {
        if self.watched() == Some(alert.chat) {
            return None;
        }
        match self.content {
            Notifications::Full => Some(alert),
            Notifications::Private => Some(Alert {
                chat: alert.chat,
                title: "Air".into(),
                body: "New activity".into(),
            }),
            Notifications::Off => None,
        }
    }
}

/// The session's end of alerts: what the user is looking at, and the way to
/// the notifier. Cloned into the queue and username listeners.
#[derive(Clone)]
pub struct Alerts {
    attention: watch::Receiver<Attention>,
    notices: mpsc::Sender<Notice>,
}

impl Alerts {
    pub fn new(attention: watch::Receiver<Attention>, notices: mpsc::Sender<Notice>) -> Self {
        Self { attention, notices }
    }

    pub fn suppression(&self) -> NotificationSuppression {
        self.attention.borrow().suppression()
    }

    /// Waits until what the user is looking at changes. Pending forever once
    /// the session is gone (the listeners stop with it anyway).
    pub async fn attention_changed(&mut self) {
        if self.attention.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    fn send(&self, notice: Notice) {
        match self.notices.try_send(notice) {
            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
            Err(mpsc::error::TrySendError::Full(_)) => warn!("too many alerts; dropping one"),
        }
    }

    fn show(&self, alerts: Vec<Alert>) {
        let attention = *self.attention.borrow();
        for alert in alerts
            .into_iter()
            .filter_map(|alert| attention.shown(alert))
        {
            self.send(Notice::Show(alert));
        }
    }

    /// Alerts for what processing a queue event stored. Port of upstream's
    /// `show_notifications_for_processed_qs_messages`, collapsed to the
    /// latest alert per chat.
    pub async fn processed(
        &self,
        user: &CoreUser,
        processed: &ProcessedQsMessages,
        siblings: &SiblingClientStates,
    ) {
        let mut alerts = Vec::new();
        new_chats(user, &processed.new_chats, &mut alerts).await;
        let (chats, quiet) = message_alerts(
            user,
            &processed.new_messages,
            &processed.reaction_notifications,
            siblings,
        )
        .await;
        alerts.extend(chats);
        for chat in &processed.new_connections {
            alerts.extend(connection_alert(user, *chat).await);
        }
        self.show(latest_per_chat(alerts));

        // As upstream: a chat that changed without alerting (an edit, a
        // message the user sent from another device) keeps its alert only
        // while something in it is still unread and not notified.
        for chat in quiet {
            match user.chat_notification_rebuild_set(chat).await {
                Ok(rebuild) if rebuild.rebuild_set.entries.is_empty() => {
                    self.send(Notice::Dismiss(chat))
                }
                Ok(_) => {}
                Err(err) => error!("failed to load a chat's notification state: {err:#}"),
            }
        }
        for chat in &processed.removed_chats {
            self.send(Notice::Dismiss(*chat));
        }
    }

    /// Alert for a contact request a username listener stored.
    pub async fn contact_request(&self, user: &CoreUser, stored: StoredRequest) {
        self.show(
            connection_alert(user, stored.chat_id)
                .await
                .into_iter()
                .collect(),
        );
        if let Some(moved_from) = stored.moved_from {
            self.send(Notice::Dismiss(moved_from));
        }
    }

    /// The user opened a chat: its alert has served its purpose.
    pub fn dismiss(&self, chat: ChatId) {
        self.send(Notice::Dismiss(chat));
    }
}

/// Keeps the last alert of each chat, in order.
fn latest_per_chat(alerts: Vec<Alert>) -> Vec<Alert> {
    let mut latest: Vec<Alert> = Vec::new();
    for alert in alerts {
        latest.retain(|kept| kept.chat != alert.chat);
        latest.push(alert);
    }
    latest
}

/// Whether the local user caused it, from any of their devices.
fn is_own(user: &CoreUser, message: &ChatMessage) -> bool {
    message.message().actor() == Some(user.user_id())
}

/// Alerts for new messages and reactions from others, plus the chats that
/// changed without alerting. Port of upstream's
/// `message_and_reaction_notifications`.
async fn message_alerts(
    user: &CoreUser,
    messages: &[ChatMessage],
    reactions: &[ReactionNotification],
    siblings: &SiblingClientStates,
) -> (Vec<Alert>, Vec<ChatId>) {
    // Only the newest message of a chat is put into words: a burst of
    // messages yields one alert.
    let mut newest: Vec<&ChatMessage> = Vec::new();
    for message in messages.iter().filter(|message| !is_own(user, message)) {
        newest.retain(|kept| kept.chat_id() != message.chat_id());
        newest.push(message);
    }
    let mut alerting = HashSet::new();
    let mut alerts = Vec::new();
    for message in newest {
        let Some(chat) = alerting_chat(user, message.chat_id(), siblings).await else {
            continue;
        };
        alerting.insert(chat.id);
        // Error notes the core adds aren't from anyone: no alert, as the
        // chat list doesn't preview them either.
        if let Some(body) = message_text(user, message, &chat).await {
            alerts.push(Alert {
                chat: chat.id,
                title: chat_title(user, &chat).await,
                body,
            });
        }
    }
    for reaction in reactions {
        // A message in the same chat says more than a reaction.
        if alerting.contains(&reaction.chat_id) {
            continue;
        }
        let Some(chat) = alerting_chat(user, reaction.chat_id, siblings).await else {
            continue;
        };
        let reactor = super::display_name(user, &reaction.reactor).await;
        let target = message_text(user, &reaction.original_chat_message, &chat)
            .await
            .unwrap_or_else(|| "your message".into());
        alerts.push(Alert {
            chat: chat.id,
            title: chat_title(user, &chat).await,
            body: preview_line(&format!("{reactor} reacted {} to {target}", reaction.emoji))
                .to_string(),
        });
    }
    alerting.extend(alerts.iter().map(|alert| alert.chat));

    let quiet = messages
        .iter()
        .filter(|message| is_own(user, message))
        .map(|message| message.chat_id())
        .filter(|chat| !alerting.contains(chat))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    (alerts, quiet)
}

/// The chat, if activity in it may alert: it exists, isn't muted, and no
/// other device of the user is showing it. A chat another device shows is
/// marked as notified there, as upstream.
async fn alerting_chat(
    user: &CoreUser,
    chat: ChatId,
    siblings: &SiblingClientStates,
) -> Option<Chat> {
    if siblings.suppresses(chat) {
        if let Err(err) = user.mark_chat_notified(chat).await {
            error!("failed to mark a chat as notified: {err:#}");
        }
        return None;
    }
    user.chat(&chat).await.filter(|chat| !chat.is_muted())
}

/// A message as one line, sender first in groups (as the chat list).
async fn message_text(user: &CoreUser, message: &ChatMessage, chat: &Chat) -> Option<String> {
    let is_group = matches!(chat.chat_type, ChatType::Group(_));
    let described = preview::describe(user, message).await?;
    let preview = preview::preview(None, Some(described), is_group)?;
    Some(match preview.prefix {
        Some(Prefix::Sender(name)) => format!("{name}: {}", preview.text),
        _ => preview.text.to_string(),
    })
}

async fn chat_title(user: &CoreUser, chat: &Chat) -> String {
    let title = match &chat.chat_type {
        ChatType::Group(attributes) => attributes.title.clone(),
        ChatType::HandleConnection(username) => username.plaintext().to_string(),
        ChatType::Connection(peer)
        | ChatType::PendingConnection(peer)
        | ChatType::TargetedMessageConnection(peer) => {
            user.user_profile(peer).await.display_name.to_string()
        }
    };
    name_line(&title).to_string()
}

async fn new_chats(user: &CoreUser, new_chats: &[NewChat], alerts: &mut Vec<Alert>) {
    for NewChat { chat_id, added_by } in new_chats {
        // A chat the user's own device made, like the self chat.
        if added_by == user.user_id() {
            continue;
        }
        let Some(chat) = user.chat(chat_id).await.filter(|chat| !chat.is_muted()) else {
            continue;
        };
        let title = match &chat.chat_type {
            ChatType::Group(attributes) => name_line(&attributes.title).to_string(),
            _ => "a group".into(),
        };
        alerts.push(Alert {
            chat: *chat_id,
            title: format!("You were added to {title}"),
            body: "Say hi to everyone".into(),
        });
    }
}

async fn connection_alert(user: &CoreUser, chat: ChatId) -> Option<Alert> {
    let chat = user.chat(&chat).await?;
    let (title, body) = match &chat.chat_type {
        ChatType::Connection(peer) => {
            let name = super::display_name(user, peer).await;
            (format!("New connection with {name}"), "Say hi")
        }
        ChatType::PendingConnection(peer) => {
            let name = super::display_name(user, peer).await;
            (
                format!("New contact request from {name}"),
                "Open Air to respond",
            )
        }
        _ => return None,
    };
    Some(Alert {
        chat: chat.id,
        title,
        body: body.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(n: u128) -> ChatId {
        ChatId::new(uuid::Uuid::from_u128(n))
    }

    fn alert(chat_n: u128, body: &str) -> Alert {
        Alert {
            chat: chat(chat_n),
            title: "Grace".into(),
            body: body.into(),
        }
    }

    #[test]
    fn only_the_chat_on_screen_is_quiet_and_reported_to_siblings() {
        let mut attention = Attention {
            window_active: true,
            open_chat: Some(chat(1)),
            content: Notifications::Full,
        };
        assert_eq!(attention.shown(alert(1, "hi")), None);
        assert_eq!(attention.shown(alert(2, "hi")), Some(alert(2, "hi")));
        assert_eq!(
            attention.suppression(),
            NotificationSuppression::Chat(chat(1).uuid())
        );

        // In the background, the open chat alerts too, and the other
        // devices are free to alert.
        attention.window_active = false;
        assert_eq!(attention.shown(alert(1, "hi")), Some(alert(1, "hi")));
        assert_eq!(attention.suppression(), NotificationSuppression::None);
    }

    #[test]
    fn the_setting_decides_what_an_alert_reveals() {
        let attention = |content| Attention {
            window_active: false,
            open_chat: None,
            content,
        };
        let shown = attention(Notifications::Private)
            .shown(alert(1, "the secret"))
            .unwrap();
        assert!(!format!("{shown:?}").contains("Grace"));
        assert!(!format!("{shown:?}").contains("secret"));
        assert_eq!(attention(Notifications::Off).shown(alert(1, "hi")), None);
        // Off here still lets the user's other devices alert.
        assert_eq!(
            Attention {
                window_active: true,
                open_chat: None,
                content: Notifications::Off,
            }
            .suppression(),
            NotificationSuppression::None
        );
    }

    #[test]
    fn a_burst_leaves_one_alert_per_chat() {
        let alerts = vec![alert(1, "a"), alert(2, "b"), alert(1, "c")];
        assert_eq!(latest_per_chat(alerts), vec![alert(2, "b"), alert(1, "c")]);
    }
}
