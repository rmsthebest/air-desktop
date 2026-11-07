//! Deleting a message (for the user or for everyone) and loading its edit
//! history for the message details. Port of upstream `ChatDetailsCubit`'s
//! `delete_message` and `message_versions`.
//!
//! Who may delete what is decided by [`DeleteFor::check`] from the
//! message's and chat's facts alone (pure, tested; the view asks it too);
//! the core's delete for everyone doesn't check the sender itself.

use aircoreclient::clients::CoreUser;
use aircoreclient::{ChatId as CoreChatId, ChatMessage, EventMessage};
use aircoreclient::{Message as CoreMessage, MessageId as CoreMessageId};
use chrono::{DateTime, Utc};
use gpui_kit::App;
use mimi_content::MessageStatus as CoreStatus;

use super::chats::{group_ended, one_to_one};
use super::contacts::{Failure, on_tokio};
use super::messages::{Messages, core_message_id};
use super::preview::{Body, body_of};
use crate::model::{
    ChatFacts, DeleteFor, EditHistory, MAX_VERSIONS, MessageFacts, MessageId, takes_messages,
};

/// What the view shows as deletable for the user: as [`erasable`], but
/// a request's system messages count as erasable whatever the chat's
/// state, as they become once it is answered; the window isn't reloaded
/// when that happens, and the session checks the real state.
pub(super) fn may_erase(message: &ChatMessage) -> bool {
    match message.message() {
        CoreMessage::Event(EventMessage::System(system)) => system.is_deletable(false),
        _ => true,
    }
}

/// The core lets the user delete `message` on their devices: everything
/// but some system messages (as `ChatMessage::is_deletable`, which isn't
/// public).
pub(super) async fn erasable(user: &CoreUser, message: &ChatMessage) -> bool {
    match message.message() {
        CoreMessage::Event(EventMessage::System(system)) => user
            .chat(&message.chat_id())
            .await
            .is_some_and(|chat| system.is_deletable(chat.is_open_request())),
        _ => true,
    }
}

/// `id`, if it is a message of `chat`: the core finds messages by id
/// alone.
async fn in_chat(
    user: &CoreUser,
    chat: CoreChatId,
    id: Option<CoreMessageId>,
) -> Result<ChatMessage, Failure> {
    let gone = || Failure::Shown("That message is gone".into());
    let id = id.ok_or_else(gone)?;
    user.message(id)
        .await?
        .filter(|found| found.chat_id() == chat)
        .ok_or_else(gone)
}

/// What [`DeleteFor::check`] needs, from the core's state.
async fn facts(
    user: &CoreUser,
    chat: CoreChatId,
    message: &ChatMessage,
) -> Result<(MessageFacts, ChatFacts), Failure> {
    let room = user
        .chat(&chat)
        .await
        .ok_or_else(|| Failure::Shown("The chat is gone".into()))?;
    let self_chat = user.chat_is_self_group(&room.group_id).await?;
    let live = takes_messages(
        one_to_one(&room.chat_type, room.status()),
        group_ended(&room),
    );
    let message_facts = MessageFacts {
        content: matches!(message.message(), CoreMessage::Content(_)),
        own: message.message().sender() == Some(user.user_id()),
        sent: message.is_sent() && message.status() != CoreStatus::Error,
        deleted: message.message().is_deleted(),
        erasable: erasable(user, message).await,
    };
    Ok((message_facts, ChatFacts { self_chat, live }))
}

/// Deletes `id` in `chat` for whom `mode` says, if [`DeleteFor::check`]
/// allows it on the core's current state (the view checked an older
/// copy).
pub(super) async fn delete(
    user: CoreUser,
    chat: CoreChatId,
    id: Option<CoreMessageId>,
    mode: DeleteFor,
) -> Result<(), Failure> {
    let message = in_chat(&user, chat, id).await?;
    let (message_facts, chat_facts) = facts(&user, chat, &message).await?;
    mode.check(message_facts, chat_facts)
        .map_err(|refusal| Failure::Shown(refusal.into()))?;
    match mode {
        DeleteFor::Me => user.delete_message(message.id()).await?,
        DeleteFor::Everyone => {
            Box::pin(user.delete_message_for_everyone(chat, message.id())).await?;
        }
    }
    Ok(())
}

/// The edit history of `id`: none for a system message, a deleted one
/// (nothing of what it said is kept) or one never edited.
pub(super) async fn history(
    user: CoreUser,
    chat: CoreChatId,
    id: Option<CoreMessageId>,
) -> Result<EditHistory, Failure> {
    let message = in_chat(&user, chat, id).await?;
    let CoreMessage::Content(content) = message.message() else {
        return Ok(EditHistory::default());
    };
    if message.message().is_deleted() || content.edited_at().is_none() {
        return Ok(EditHistory::default());
    }
    let versions = user.message_versions(message.id()).await?;
    // A sender can edit often: only the shown versions are converted.
    let skipped = versions.len().saturating_sub(MAX_VERSIONS);
    let sources: Vec<(Option<String>, DateTime<Utc>)> = versions[skipped..]
        .iter()
        .map(|version| {
            let source = match body_of(&version.content) {
                Body::Markdown(source) => Some(source),
                Body::Attachment | Body::Unsupported => None,
            };
            (source, DateTime::<Utc>::from(version.created_at))
        })
        .collect();
    Ok(EditHistory::new(
        skipped,
        sources
            .iter()
            .map(|(source, at)| (source.as_deref(), *at))
            .collect(),
    ))
}

impl Messages {
    /// Deletes the message for whom `mode` says. `None` for fixed
    /// messages (no account). Runs detached, as sends do.
    pub fn delete(
        &self,
        id: &MessageId,
        mode: DeleteFor,
        cx: &App,
    ) -> Option<impl Future<Output = Result<(), String>> + use<>> {
        let (user, chat) = self.live_chat()?;
        let id = core_message_id(id);
        // Local: a deletion for everyone is queued, sent when it can be.
        let what = "delete the message";
        Some(on_tokio(cx, what, async move {
            delete(user, chat, id, mode)
                .await
                .map_err(|failure| failure.local(what))
        }))
    }

    /// The message's edit history. `None` for fixed messages.
    pub fn edit_history(
        &self,
        id: &MessageId,
        cx: &App,
    ) -> Option<impl Future<Output = Result<EditHistory, String>> + use<>> {
        let (user, chat) = self.live_chat()?;
        let id = core_message_id(id);
        let what = "load the edit history";
        Some(on_tokio(cx, what, async move {
            history(user, chat, id)
                .await
                .map_err(|failure| failure.local(what))
        }))
    }
}
