//! Blocking, reporting spam, retracting a contact request and deleting a
//! one-to-one chat. Port of upstream's `UserCubit` `block_contact`,
//! `unblock_contact`, `report_spam`, `retract_contact_request` and
//! `delete_chat` as the contact screens use them.
//!
//! What each operation may do in a chat is decided by [`check`] from the
//! chat's type and status alone (pure, tested); the core then runs it,
//! detached like the other contact operations.
//!
//! Blocking is local and synced to the user's other devices: the blocked
//! user isn't told, their requests are declined and their messages
//! dropped. Reporting spam tells Air's server who, never what was said.

use aircommon::identifiers::UserId as CoreUserId;
use aircoreclient::clients::CoreUser;
use aircoreclient::{ChatId as CoreChatId, ChatStatus, ChatType};
use gpui::SharedString;
use gpui_kit::App;

use super::Session;
use super::contacts::{Failure, on_tokio};
use super::groups::Members;
use super::messages::core_chat_id;
use crate::model::ChatId;

/// What the user asked to do in a one-to-one chat (or request).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactOp {
    Block,
    Unblock,
    Report,
    Retract,
    Delete,
}

impl ContactOp {
    /// For "Couldn't …" when the core fails.
    fn what(self) -> &'static str {
        match self {
            ContactOp::Block => "block the contact",
            ContactOp::Unblock => "unblock the contact",
            ContactOp::Report => "report spam",
            ContactOp::Retract => "retract the contact request",
            ContactOp::Delete => "delete the chat",
        }
    }
}

/// What the core is asked to do, and to whom.
#[derive(Debug, PartialEq)]
enum Action {
    Block(CoreUserId),
    Unblock(CoreUserId),
    Report(CoreUserId),
    Retract,
    Delete,
}

/// What `op` does in a chat of this type and status, or why it can't.
///
/// The peer of a request that is still open is not a contact: blocking an
/// incoming one declines it (as upstream), an outgoing one is retracted
/// rather than deleted, and an incoming one declined rather than deleted.
/// A request sent to a username has no known peer to block or report.
fn check(op: ContactOp, chat_type: &ChatType, status: &ChatStatus) -> Result<Action, &'static str> {
    let peer = match chat_type {
        ChatType::Group(_) => {
            return Err(match op {
                ContactOp::Report => "In a group, name the member: :report <name or #tag>",
                _ => "That's for a contact's chat",
            });
        }
        ChatType::Connection(peer)
        | ChatType::PendingConnection(peer)
        | ChatType::TargetedMessageConnection(peer) => Some(peer),
        ChatType::HandleConnection(_) => None,
    };
    let blocked = matches!(status, ChatStatus::Blocked);
    let open = matches!(status, ChatStatus::Active | ChatStatus::Pending);
    let outgoing = matches!(
        chat_type,
        ChatType::HandleConnection(_) | ChatType::TargetedMessageConnection(_)
    );
    let incoming = matches!(chat_type, ChatType::PendingConnection(_));
    let no_peer = "Only their username is known until they answer";
    match op {
        ContactOp::Block if blocked => Err("They're blocked already"),
        ContactOp::Block if outgoing => Err("Retract your request instead: :retract"),
        ContactOp::Block => peer.cloned().map(Action::Block).ok_or(no_peer),
        ContactOp::Unblock if !blocked => Err("They aren't blocked"),
        ContactOp::Unblock => peer.cloned().map(Action::Unblock).ok_or(no_peer),
        ContactOp::Report => peer.cloned().map(Action::Report).ok_or(no_peer),
        ContactOp::Retract if !outgoing => Err("Only a contact request you sent can be retracted"),
        ContactOp::Retract if matches!(status, ChatStatus::Pending) => {
            Err("The request is still being sent; try again in a moment")
        }
        ContactOp::Retract if !open => Err("That request has ended already"),
        ContactOp::Retract => Ok(Action::Retract),
        ContactOp::Delete if open && outgoing => Err("Retract the request instead: :retract"),
        ContactOp::Delete if open && incoming => Err("Decline the request instead: :decline"),
        ContactOp::Delete => Ok(Action::Delete),
    }
}

pub(super) async fn run(user: CoreUser, op: ContactOp, chat: CoreChatId) -> Result<(), Failure> {
    let Some(found) = user.chat(&chat).await else {
        return Err(Failure::Shown("The chat is gone".into()));
    };
    // The self chat is a group in the core: `check` refuses it.
    match check(op, &found.chat_type, found.status()).map_err(|m| Failure::Shown(m.into()))? {
        Action::Block(peer) => user.block_contact(peer).await?,
        Action::Unblock(peer) => user.unblock_contact(peer).await?,
        Action::Report(peer) => user.report_spam(peer).await?,
        Action::Retract => user.retract_contact_request(chat).await?,
        Action::Delete => user.delete_and_erase_chat(chat).await?,
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
        on_tokio(cx, op.what(), run(user, op, chat))
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

#[cfg(test)]
mod tests {
    use aircoreclient::ChatAttributes;

    use super::*;

    fn user(n: u128) -> CoreUserId {
        CoreUserId::new(uuid::Uuid::from_u128(n), "localhost".parse().unwrap())
    }

    #[test]
    fn each_operation_reaches_only_the_peer_it_may() {
        use ContactOp::*;
        let peer = user(1);
        let contact = ChatType::Connection(peer.clone());
        let incoming = ChatType::PendingConnection(peer.clone());
        let outgoing = ChatType::TargetedMessageConnection(peer.clone());
        let by_name = ChatType::HandleConnection(
            aircommon::identifiers::Username::new("ada-lovelace".into()).unwrap(),
        );
        let group = ChatType::Group(ChatAttributes {
            title: "Engines".into(),
            picture: None,
        });
        let ended = ChatStatus::inactive(vec![peer.clone()]);
        let (active, pending, blocked) =
            (ChatStatus::Active, ChatStatus::Pending, ChatStatus::Blocked);

        // Groups (and the self chat, a group in the core): none of these.
        for op in [Block, Unblock, Report, Retract, Delete] {
            assert!(check(op, &group, &active).is_err(), "{op:?}");
        }
        // A contact: block once, unblock only when blocked, report, delete.
        assert_eq!(
            check(Block, &contact, &active),
            Ok(Action::Block(peer.clone()))
        );
        assert!(check(Block, &contact, &blocked).is_err());
        assert!(check(Unblock, &contact, &active).is_err());
        assert_eq!(
            check(Unblock, &contact, &blocked),
            Ok(Action::Unblock(peer.clone()))
        );
        assert_eq!(
            check(Report, &contact, &active),
            Ok(Action::Report(peer.clone()))
        );
        assert!(check(Retract, &contact, &active).is_err());
        assert_eq!(check(Delete, &contact, &active), Ok(Action::Delete));
        assert_eq!(check(Delete, &contact, &blocked), Ok(Action::Delete));
        // An open incoming request: blocking declines it; deleting is
        // declining, which has its own command.
        assert_eq!(
            check(Block, &incoming, &active),
            Ok(Action::Block(peer.clone()))
        );
        assert!(check(Delete, &incoming, &active).is_err());
        assert!(check(Retract, &incoming, &active).is_err());
        // An open outgoing request: retracted, not blocked or deleted, and
        // not before it is sent.
        assert_eq!(check(Retract, &outgoing, &active), Ok(Action::Retract));
        assert_eq!(check(Retract, &by_name, &active), Ok(Action::Retract));
        assert!(check(Retract, &outgoing, &pending).is_err());
        assert!(check(Block, &outgoing, &active).is_err());
        assert!(check(Delete, &outgoing, &active).is_err());
        // Sent to a username: nobody known to report.
        assert!(check(Report, &by_name, &active).is_err());
        // Ended requests: only a record, deleted like a chat.
        for chat in [&incoming, &outgoing, &by_name] {
            assert_eq!(check(Delete, chat, &ended), Ok(Action::Delete));
            assert!(check(Retract, chat, &ended).is_err());
        }
    }
}
