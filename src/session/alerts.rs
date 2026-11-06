//! Desktop notifications, called alerts here to keep them apart from the
//! core's database notifications. Port of upstream applogic's
//! `notifications/` (the non-Android path) and `NotificationPolicy`:
//! - which processed queue events alert, and what they say;
//! - what is shown given what the user is looking at and the
//!   `notifications` setting;
//! - the suppression reported to the user's other devices.
//!
//! An alert says what the chat's notification state in the core holds: its
//! newest unread, not yet notified entry from others. So an edit or a
//! remote delete rewrites or takes down an alert already shown.
//!
//! Alerts and dismissals reach the notifier (`crate::notifier`) in order,
//! through a [`NoticeQueue`] that keeps only the latest notices of each
//! chat, so it is bounded by the chats and never drops one. There is one alert per chat; a
//! newer one replaces it, so a busy chat can't flood the desktop.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aircommon::messages::client_state::NotificationSuppression;
use aircoreclient::clients::process::process_qs::{NewChat, ProcessedQsMessages};
use aircoreclient::clients::{CoreUser, SiblingClientStates};
use aircoreclient::{Chat, ChatId, ChatMessage, ChatNotificationEntry, ChatType, StoredRequest};
use tokio::sync::{Notify, watch};
use tracing::error;

use super::preview;
use crate::model::{Prefix, name_line, preview_line};
use crate::settings::Notifications;

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
    /// Rewrites the chat's alert if one is on screen, without showing it
    /// again: what it said was edited, deleted or read elsewhere.
    Refresh(Alert),
    Dismiss(Dismiss),
}

impl Notice {
    fn chat(&self) -> Option<ChatId> {
        match self {
            Self::Show(alert) | Self::Refresh(alert) => Some(alert.chat),
            Self::Dismiss(Dismiss::Chat(chat)) => Some(*chat),
            Self::Dismiss(Dismiss::All) => None,
        }
    }
}

/// Alerts to take off the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dismiss {
    /// The chat's alert is stale: opened, answered elsewhere, or moved.
    Chat(ChatId),
    /// The user asked alerts to reveal less than the ones shown do.
    All,
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
    pub fn watched(&self) -> Option<ChatId> {
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
    pub fn shown(&self, alert: Alert) -> Option<Alert> {
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

/// Moves to `next`, returning what that makes stale: the chat the user
/// starts looking at, or everything shown when the setting got stricter.
pub fn attend(attention: &mut Attention, next: Attention) -> Option<Dismiss> {
    let previous = std::mem::replace(attention, next);
    if next.content > previous.content {
        Some(Dismiss::All)
    } else if next.watched() != previous.watched() {
        next.watched().map(Dismiss::Chat)
    } else {
        None
    }
}

/// Notices on their way to the notifier, in order, at most two per chat:
/// a dismissal and the alert after it. A newer alert replaces a waiting one
/// of its chat (a show stays a show), a dismissal drops the chat's waiting
/// notices, and dismissing all drops everything waiting. Peers can't grow
/// it beyond the user's chats, so nothing needs to be dropped: dropping a
/// refresh or dismissal would leave stale text on screen.
#[derive(Default)]
pub struct NoticeQueue {
    queue: Mutex<VecDeque<Notice>>,
    ready: Notify,
}

impl NoticeQueue {
    pub fn push(&self, notice: Notice) {
        {
            let mut queue = self.queue.lock().unwrap_or_else(|err| err.into_inner());
            let notice = match notice {
                Notice::Refresh(alert) => {
                    let was_shown = take_alert(&mut queue, alert.chat);
                    if was_shown {
                        Notice::Show(alert)
                    } else {
                        Notice::Refresh(alert)
                    }
                }
                Notice::Show(alert) => {
                    take_alert(&mut queue, alert.chat);
                    Notice::Show(alert)
                }
                Notice::Dismiss(Dismiss::Chat(chat)) => {
                    queue.retain(|queued| queued.chat() != Some(chat));
                    Notice::Dismiss(Dismiss::Chat(chat))
                }
                Notice::Dismiss(Dismiss::All) => {
                    queue.clear();
                    Notice::Dismiss(Dismiss::All)
                }
            };
            queue.push_back(notice);
        }
        self.ready.notify_one();
    }

    /// The next notice, waiting for one.
    pub async fn next(&self) -> Notice {
        loop {
            let next = self
                .queue
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .pop_front();
            if let Some(notice) = next {
                return notice;
            }
            // A push since the check left a permit, so none is missed.
            self.ready.notified().await;
        }
    }
}

/// Takes the chat's waiting alert out of the queue, returning whether it
/// was a show.
fn take_alert(queue: &mut VecDeque<Notice>, chat: ChatId) -> bool {
    let waiting = queue.iter().position(|queued| {
        matches!(queued, Notice::Show(alert) | Notice::Refresh(alert) if alert.chat == chat)
    });
    matches!(
        waiting.and_then(|ix| queue.remove(ix)),
        Some(Notice::Show(_))
    )
}

/// The session's end of alerts: what the user is looking at, and the way to
/// the notifier. Cloned into the queue and username listeners.
#[derive(Clone)]
pub struct Alerts {
    attention: watch::Receiver<Attention>,
    notices: Arc<NoticeQueue>,
}

impl Alerts {
    pub fn new(attention: watch::Receiver<Attention>, notices: Arc<NoticeQueue>) -> Self {
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

    fn show(&self, alerts: Vec<Alert>) {
        // Checked here to skip work, and again by the notifier at show time.
        let attention = *self.attention.borrow();
        for alert in alerts
            .into_iter()
            .filter_map(|alert| attention.shown(alert))
        {
            self.notices.push(Notice::Show(alert));
        }
    }

    pub fn dismiss(&self, dismiss: Dismiss) {
        self.notices.push(Notice::Dismiss(dismiss));
    }

    /// Alerts for what processing a queue event stored. Port of upstream's
    /// `show_notifications_for_processed_qs_messages` and
    /// `message_and_reaction_notifications`, one alert per chat.
    pub async fn processed(
        &self,
        user: &CoreUser,
        processed: &ProcessedQsMessages,
        siblings: &SiblingClientStates,
    ) {
        let mut alerts = Vec::new();
        new_chats(user, &processed.new_chats, &mut alerts).await;

        // Chats others wrote or reacted in alert, unless another device of
        // the user shows them. As upstream, those are marked as notified,
        // which leaves this device's alert for them nothing to say.
        let mut quiet = Vec::new();
        let active = processed
            .new_messages
            .iter()
            .filter(|message| !is_own(user, message))
            .map(|message| message.chat_id())
            .chain(processed.reaction_notifications.iter().map(|r| r.chat_id));
        for chat_id in unique(active) {
            if siblings.suppresses(chat_id) {
                if let Err(err) = user.mark_chat_notified(chat_id).await {
                    error!("failed to mark a chat as notified: {err:#}");
                }
                quiet.push(chat_id);
                continue;
            }
            let Some(chat) = user.chat(&chat_id).await.filter(|chat| !chat.is_muted()) else {
                continue;
            };
            match current_alert(user, &chat).await {
                Some(alert) => alerts.push(alert),
                // Gone again within the same event (say, deleted).
                None => quiet.push(chat_id),
            }
        }
        for chat in &processed.new_connections {
            alerts.extend(connection_alert(user, *chat).await);
        }
        let alerts = latest_per_chat(alerts);

        // Chats that changed without alerting (an edit, a remote delete, a
        // retracted reaction, a message the user sent from another device)
        // get their alert rewritten from what is left, or taken down.
        quiet.extend(processed.chats_with_changed_notifications.iter().copied());
        quiet.extend(
            processed
                .new_messages
                .iter()
                .filter(|message| is_own(user, message))
                .map(|message| message.chat_id()),
        );
        quiet.retain(|chat| !alerts.iter().any(|alert| alert.chat == *chat));
        self.show(alerts);
        for chat_id in unique(quiet) {
            let alert = match user.chat(&chat_id).await {
                Some(chat) => current_alert(user, &chat).await,
                None => None,
            };
            self.notices.push(match alert {
                Some(alert) => Notice::Refresh(alert),
                None => Notice::Dismiss(Dismiss::Chat(chat_id)),
            });
        }
        for chat in &processed.removed_chats {
            self.dismiss(Dismiss::Chat(*chat));
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
            self.dismiss(Dismiss::Chat(moved_from));
        }
    }
}

/// Each chat once, in order of first appearance.
fn unique(chats: impl IntoIterator<Item = ChatId>) -> Vec<ChatId> {
    let mut seen = Vec::new();
    for chat in chats {
        if !seen.contains(&chat) {
            seen.push(chat);
        }
    }
    seen
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

/// The chat's alert as its notification state stands: the newest unread,
/// not yet notified entry from others that can be put into words (error
/// notes the core adds can't, as the chat list doesn't preview them).
/// Deleted messages aren't in that state, and edited ones are current.
async fn current_alert(user: &CoreUser, chat: &Chat) -> Option<Alert> {
    let rebuild = user
        .chat_notification_rebuild_set(chat.id)
        .await
        .inspect_err(|err| error!("failed to load a chat's notification state: {err:#}"))
        .ok()?;
    for entry in rebuild.rebuild_set.entries.iter().rev() {
        let body = match entry {
            ChatNotificationEntry::Message(message) if is_own(user, message) => continue,
            ChatNotificationEntry::Message(message) => message_text(user, message, chat).await,
            ChatNotificationEntry::Reaction(reaction) if &reaction.reactor == user.user_id() => {
                continue;
            }
            ChatNotificationEntry::Reaction(reaction) => {
                let reactor = super::display_name(user, &reaction.reactor).await;
                let target = match &reaction.target {
                    Some(target) => message_text(user, target, chat).await,
                    None => None,
                };
                let target = target.unwrap_or_else(|| "your message".into());
                // Shown as in the app: one grapheme of the peer's text.
                let emoji = crate::model::reaction_label(&reaction.emoji);
                let line = format!("{reactor} reacted {emoji} to {target}");
                Some(preview_line(&line).to_string())
            }
        };
        if let Some(body) = body {
            return Some(Alert {
                chat: chat.id,
                title: chat_title(user, chat).await,
                body,
            });
        }
    }
    None
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
    fn looking_at_a_chat_or_revealing_less_dismisses() {
        let mut attention = Attention::default();
        let looking = |window_active, open: Option<u128>| Attention {
            window_active,
            open_chat: open.map(chat),
            content: Notifications::Full,
        };
        // Opening a chat in the background dismisses nothing; activating
        // the window then does, once.
        assert_eq!(attend(&mut attention, looking(false, Some(1))), None);
        assert_eq!(
            attend(&mut attention, looking(true, Some(1))),
            Some(Dismiss::Chat(chat(1)))
        );
        assert_eq!(attend(&mut attention, looking(true, Some(1))), None);
        assert_eq!(attend(&mut attention, looking(true, None)), None);

        let reveal = |content| Attention {
            content,
            ..Attention::default()
        };
        let mut attention = reveal(Notifications::Full);
        assert_eq!(
            attend(&mut attention, reveal(Notifications::Private)),
            Some(Dismiss::All)
        );
        assert_eq!(attend(&mut attention, reveal(Notifications::Full)), None);
    }

    /// Takes what is queued, without waiting for more.
    async fn drain(queue: &NoticeQueue) -> Vec<Notice> {
        let mut drained = Vec::new();
        let wait = std::time::Duration::from_millis(10);
        while let Ok(notice) = tokio::time::timeout(wait, queue.next()).await {
            drained.push(notice);
        }
        drained
    }

    #[tokio::test]
    async fn the_queue_keeps_each_chats_latest_notices_in_order() {
        let queue = NoticeQueue::default();
        // A busy chat holds one place, its newest alert; a rewrite of an
        // alert not shown yet is still shown.
        for n in 0..100 {
            queue.push(Notice::Show(alert(1, &n.to_string())));
        }
        queue.push(Notice::Show(alert(2, "b")));
        queue.push(Notice::Refresh(alert(1, "edited")));
        queue.push(Notice::Refresh(alert(3, "c")));
        assert_eq!(
            drain(&queue).await,
            [
                Notice::Show(alert(2, "b")),
                Notice::Show(alert(1, "edited")),
                Notice::Refresh(alert(3, "c")),
            ]
        );

        // A dismissal drops what it makes stale; an alert after it stays
        // after it.
        queue.push(Notice::Show(alert(1, "a")));
        queue.push(Notice::Dismiss(Dismiss::Chat(chat(1))));
        queue.push(Notice::Show(alert(1, "after")));
        assert_eq!(
            drain(&queue).await,
            [
                Notice::Dismiss(Dismiss::Chat(chat(1))),
                Notice::Show(alert(1, "after"))
            ]
        );

        queue.push(Notice::Show(alert(1, "a")));
        queue.push(Notice::Dismiss(Dismiss::All));
        assert_eq!(drain(&queue).await, [Notice::Dismiss(Dismiss::All)]);
    }

    #[test]
    fn a_burst_leaves_one_alert_per_chat() {
        let alerts = vec![alert(1, "a"), alert(2, "b"), alert(1, "c")];
        assert_eq!(latest_per_chat(alerts), vec![alert(2, "b"), alert(1, "c")]);
    }
}
