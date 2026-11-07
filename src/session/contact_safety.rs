//! Blocking, reporting spam, retracting a contact request and deleting a
//! one-to-one chat. Port of upstream's `UserCubit` `block_contact`,
//! `unblock_contact`, `report_spam`, `retract_contact_request` and
//! `delete_chat` as the contact screens use them.
//!
//! What each operation may do in a chat is decided by
//! [`ContactOp::check`] from the chat's facts alone (pure, tested; the
//! view asks it too); the core then runs it, detached like the other
//! contact operations.
//!
//! Blocking is local and synced to the user's other devices: the blocked
//! user isn't told, their requests are declined and their messages
//! dropped. Reporting spam tells Air's server who, never what was said.

use aircommon::identifiers::UserId as CoreUserId;
use aircoreclient::ChatId as CoreChatId;
use aircoreclient::ChatType;
use aircoreclient::clients::CoreUser;
use gpui::SharedString;
use gpui_kit::App;

use super::Session;
use super::chats::one_to_one;
use super::contacts::{Failure, on_tokio};
use super::groups::Members;
use super::messages::core_chat_id;
use crate::model::{ChatId, ContactOp};

/// For "Couldn't …" when the core fails.
fn what(op: ContactOp) -> &'static str {
    match op {
        ContactOp::Block => "block the contact",
        ContactOp::Unblock => "unblock the contact",
        ContactOp::Report => "report spam",
        ContactOp::Retract => "retract the contact request",
        ContactOp::Delete => "delete the chat",
    }
}

/// The peer of a one-to-one chat, once known.
fn peer(chat_type: &ChatType) -> Option<&CoreUserId> {
    match chat_type {
        ChatType::Connection(peer)
        | ChatType::PendingConnection(peer)
        | ChatType::TargetedMessageConnection(peer) => Some(peer),
        ChatType::HandleConnection(_) | ChatType::Group(_) => None,
    }
}

/// Runs `op` in `chat` if [`ContactOp::check`] allows it on the core's
/// current state (the view checked an older copy).
pub(super) async fn run(user: CoreUser, op: ContactOp, chat: CoreChatId) -> Result<(), Failure> {
    let Some(found) = user.chat(&chat).await else {
        return Err(Failure::Shown("The chat is gone".into()));
    };
    // The self chat is a group in the core: refused here too.
    let Some(facts) = one_to_one(&found.chat_type, found.status()) else {
        return Err(Failure::Shown(
            match op {
                ContactOp::Report => "In a group, name the member: :report <name or #tag>",
                _ => "That's for a contact's chat",
            }
            .into(),
        ));
    };
    op.check(facts)
        .map_err(|refusal| Failure::Shown(refusal.into()))?;
    let peer = || {
        // `check` allows peer operations only with a known peer.
        peer(&found.chat_type)
            .cloned()
            .ok_or_else(|| Failure::Internal(anyhow::anyhow!("no peer for {op:?}")))
    };
    match op {
        ContactOp::Block => user.block_contact(peer()?).await?,
        ContactOp::Unblock => user.unblock_contact(peer()?).await?,
        ContactOp::Report => user.report_spam(peer()?).await?,
        ContactOp::Retract => user.retract_contact_request(chat).await?,
        ContactOp::Delete => user.delete_and_erase_chat(chat).await?,
    }
    Ok(())
}

impl Session {
    /// Runs `op` in the one-to-one chat (or request) `chat`.
    pub fn contact_op(
        &self,
        op: ContactOp,
        chat: &ChatId,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat) = (self.user.clone(), core_chat_id(chat));
        on_tokio(cx, what(op), run(user, op, chat))
    }
}

/// A group member to report, as `Members::suspect` found them.
pub struct Suspect {
    /// Their (peer-chosen) name and `#tag`, to confirm with.
    pub label: SharedString,
    id: CoreUserId,
}

impl Members {
    /// The member `query` names (by `#tag` or name), to report for spam.
    pub fn suspect(&self, query: &str) -> Result<Suspect, String> {
        let (name, tag, id) = self.find(query)?;
        Ok(Suspect {
            label: format!("{name} (#{tag})").into(),
            id,
        })
    }

    /// Reports the member `suspect` found for spam.
    pub fn report_spam(
        &self,
        suspect: Suspect,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let user = self.user().clone();
        on_tokio(cx, "report spam", async move {
            Ok(user.report_spam(suspect.id).await?)
        })
    }
}
