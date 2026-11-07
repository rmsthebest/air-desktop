//! Safety codes: what the user and a contact (or group member) compare, on
//! their screens or out loud, to know their messages are end-to-end
//! encrypted with each other and nobody in between. Port of upstream's
//! `UserCubit::safety_codes` and the Flutter safety code pane.
//!
//! Each user's code hashes their credential (the core's `SafetyCode`).
//! The pair is ordered, so both sides see the same 60 digits: twelve
//! blocks of five, three lines of four.

use aircommon::identifiers::UserId as CoreUserId;
use aircoreclient::ChatType;
use aircoreclient::SafetyCode;
use aircoreclient::clients::CoreUser;
use gpui::SharedString;
use gpui_kit::App;
use tracing::warn;

use super::Session;
use super::contacts::{Failure, on_tokio};
use super::groups::Members;
use super::messages::core_chat_id;
use crate::model::ChatId;

/// A safety code as shown: three lines of four five-digit blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct SafetyLines(pub [String; 3]);

impl SafetyLines {
    /// The code as copied: its lines, one per line, as upstream.
    pub fn text(&self) -> String {
        self.0.join("\n")
    }
}

/// The lines for a pair of codes, the same whichever side computes them.
fn lines(a: SafetyCode, b: SafetyCode) -> SafetyLines {
    let (first, second) = if a <= b { (a, b) } else { (b, a) };
    let blocks: Vec<String> = first
        .to_chunks()
        .into_iter()
        .chain(second.to_chunks())
        .map(|block| format!("{block:05}"))
        .collect();
    SafetyLines(std::array::from_fn(|line| {
        blocks[line * 4..line * 4 + 4].join(" ")
    }))
}

/// The safety code the user shares with `peer`.
pub(super) async fn with(user: &CoreUser, peer: &CoreUserId) -> Result<SafetyLines, Failure> {
    let own = user.safety_code(user.user_id()).await?;
    // A local lookup: the connection is no reason it fails. The credential
    // arrives with the peer's first message or a group's member list.
    let theirs = user.safety_code(peer).await.map_err(|err| {
        warn!("no safety code for a peer: {err:#}");
        Failure::Shown("Their credential isn't on this device yet; try again later".into())
    })?;
    Ok(lines(own, theirs))
}

impl Session {
    /// The safety code shared with the contact of the one-to-one `chat`.
    pub fn contact_safety_code(
        &self,
        chat: &ChatId,
        cx: &App,
    ) -> impl Future<Output = Result<SafetyLines, String>> + use<> {
        let (user, chat) = (self.user.clone(), core_chat_id(chat));
        on_tokio(cx, "compute the safety code", async move {
            let Some(chat) = user.chat(&chat).await else {
                return Err(Failure::Shown("The chat is gone".into()));
            };
            match chat.chat_type {
                ChatType::Connection(peer) => with(&user, &peer).await,
                ChatType::Group(_) => Err(Failure::Shown(
                    "In a group, name the member: :safety-code <name or #tag>".into(),
                )),
                // Not a contact yet: there is nobody confirmed to compare with.
                _ => Err(Failure::Shown(
                    "Safety codes are compared once the contact request is accepted".into(),
                )),
            }
        })
    }
}

impl Members {
    /// The member `query` names (by `#tag` or name) and the safety code the
    /// user shares with them.
    pub fn safety_code(
        &self,
        query: &str,
        cx: &App,
    ) -> Result<
        (
            SharedString,
            impl Future<Output = Result<SafetyLines, String>> + use<>,
        ),
        String,
    > {
        let (name, tag, peer) = self.find(query)?;
        let user = self.user().clone();
        let code = on_tokio(cx, "compute the safety code", async move {
            with(&user, &peer).await
        });
        Ok((format!("{name} (#{tag})").into(), code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(seed: u8) -> SafetyCode {
        SafetyCode(std::array::from_fn(|i| seed.wrapping_add(i as u8)))
    }

    #[test]
    fn both_sides_see_the_same_sixty_digits() {
        let (ours, theirs) = (code(1), code(200));
        let shown = lines(ours, theirs);
        assert_eq!(shown, lines(theirs, ours));
        for line in &shown.0 {
            let blocks: Vec<&str> = line.split(' ').collect();
            assert_eq!(blocks.len(), 4, "{line}");
            assert!(
                blocks
                    .iter()
                    .all(|b| b.len() == 5 && b.bytes().all(|c| c.is_ascii_digit()))
            );
        }
        // The lower code comes first, block for block, zero-padded.
        let first: Vec<String> = ours
            .to_chunks()
            .iter()
            .map(|block| format!("{block:05}"))
            .collect();
        assert_eq!(shown.0[0], first[..4].join(" "));
        assert_eq!(shown.text().lines().count(), 3);
        // Another peer, another code.
        assert_ne!(shown, lines(ours, code(7)));
    }

    #[test]
    fn small_blocks_keep_their_leading_zeros() {
        let zeros = SafetyCode([0; 32]);
        assert_eq!(lines(zeros, zeros).0[0], "00000 00000 00000 00000");
    }
}
