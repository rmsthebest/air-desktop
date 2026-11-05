//! View-model types the UI renders. They are deliberately plain data so views
//! never reach into `aircoreclient` directly; the session layer (milestone 3)
//! produces them from core state. Until then, [`mock`] supplies sample data.
//!
//! Identity always comes from IDs. Display names are peer-controlled and are
//! only ever shown, never compared.

use chrono::{DateTime, Datelike as _, Local, Utc};
use gpui_kit::SharedString;
use unicode_segmentation::UnicodeSegmentation as _;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatId(pub SharedString);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserId(pub SharedString);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageId(pub SharedString);

#[derive(Debug, Clone)]
pub struct ChatSummary {
    pub id: ChatId,
    pub title: SharedString,
    pub is_group: bool,
    pub last_message: Option<SharedString>,
    pub last_activity: DateTime<Utc>,
    pub unread: u32,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: MessageId,
    pub sender_id: UserId,
    /// Display name at render time; never used for identity.
    pub sender_name: SharedString,
    /// Set by the producer by comparing `sender_id` with the local user's ID.
    pub own: bool,
    pub body: SharedString,
    pub sent_at: DateTime<Utc>,
}

/// Whether the message at `ix` starts a new visual group, i.e. its sender
/// differs from the previous message's sender.
pub fn starts_group(messages: &[Message], ix: usize) -> bool {
    match ix.checked_sub(1).and_then(|prev| messages.get(prev)) {
        Some(prev) => prev.sender_id != messages[ix].sender_id,
        None => true,
    }
}

/// Chat-list timestamp: time today, "Yesterday", weekday within a week,
/// otherwise the date.
pub fn format_activity(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let at = at.with_timezone(&Local);
    let now = now.with_timezone(&Local);
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        ..=0 => at.format("%H:%M").to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => at.format("%a").to_string(),
        _ if at.year() == now.year() => at.format("%b %-d").to_string(),
        _ => at.format("%Y-%m-%d").to_string(),
    }
}

/// Message timestamp shown in a bubble.
pub fn format_message_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%H:%M").to_string()
}

/// Initials for an avatar placeholder: the first visible grapheme of the
/// first two words. Names are peer-controlled, so invisible format and
/// control characters (e.g. bidi overrides) are ignored.
pub fn initials(name: &str) -> String {
    let visible: String = name.chars().filter(|c| !is_invisible(*c)).collect();
    visible
        .split_whitespace()
        .filter_map(|word| word.graphemes(true).next())
        .take(2)
        .map(|grapheme| {
            // Keep graphemes whose uppercase form would change length
            // (e.g. ß → SS), so each word contributes exactly one grapheme.
            let upper = grapheme.to_uppercase();
            if upper.graphemes(true).count() == 1 {
                upper
            } else {
                grapheme.to_string()
            }
        })
        .collect()
}

fn is_invisible(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}'                      // soft hyphen
            | '\u{200B}'                     // zero-width space (ZWJ/ZWNJ kept: emoji)
            | '\u{200E}' | '\u{200F}'         // LRM/RLM
            | '\u{202A}'..='\u{202E}'       // bidi embeddings/overrides
            | '\u{2060}'..='\u{2069}'       // word joiner, bidi isolates
            | '\u{FEFF}')
}

pub mod mock {
    use chrono::Duration;

    use super::*;

    pub fn chats() -> Vec<ChatSummary> {
        let now = Utc::now();
        let chat = |id: &'static str, title: &'static str, group, last, ago: Duration, unread| {
            ChatSummary {
                id: ChatId(id.into()),
                title: title.into(),
                is_group: group,
                last_message: Some(SharedString::from(last)),
                last_activity: now - ago,
                unread,
            }
        };
        vec![
            chat(
                "ada",
                "Ada Lovelace",
                false,
                "The engine can compose music too.",
                Duration::minutes(3),
                2,
            ),
            chat(
                "design",
                "Design Crew",
                true,
                "Shipped the new palette 🎨",
                Duration::minutes(18),
                5,
            ),
            chat(
                "grace",
                "Grace Hopper",
                false,
                "Found the bug. It was a moth.",
                Duration::hours(3),
                0,
            ),
            chat(
                "alan",
                "Alan Turing",
                false,
                "Can machines think?",
                Duration::days(1),
                0,
            ),
            chat(
                "rust",
                "Rustaceans",
                true,
                "New release is out",
                Duration::days(4),
                0,
            ),
        ]
    }

    pub fn messages(chat: &ChatId) -> Vec<Message> {
        let peer_name: SharedString = match chat.0.as_ref() {
            "ada" => "Ada Lovelace",
            "grace" => "Grace Hopper",
            "alan" => "Alan Turing",
            _ => "Someone",
        }
        .into();
        let now = Utc::now();
        let mut next_id = 0;
        let mut msg = |own: bool, body: &'static str, minutes_ago: i64| {
            next_id += 1;
            Message {
                id: MessageId(format!("{}-{next_id}", chat.0).into()),
                sender_id: UserId(if own { "me" } else { "peer" }.into()),
                sender_name: if own { "You".into() } else { peer_name.clone() },
                own,
                body: body.into(),
                sent_at: now - Duration::minutes(minutes_ago),
            }
        };
        vec![
            msg(
                false,
                "Hey! Did you get a chance to look at the new client?",
                10,
            ),
            msg(true, "Yes, it is so much nicer on a big screen.", 9),
            msg(true, "And everything is reachable from the keyboard.", 9),
            msg(false, "Ctrl+K to jump between chats is my favourite.", 7),
            msg(false, "The engine can compose music too.", 3),
        ]
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone as _};

    use super::*;

    fn message(sender_id: &str, sender_name: &str) -> Message {
        Message {
            id: MessageId("m".into()),
            sender_id: UserId(sender_id.to_string().into()),
            sender_name: sender_name.to_string().into(),
            own: false,
            body: "hi".into(),
            sent_at: Utc::now(),
        }
    }

    #[test]
    fn grouping_uses_sender_id_not_display_name() {
        let messages = [
            message("alice", "Alice"),
            message("alice", "Alice"),
            // Impersonator copying Alice's display name must start a new group.
            message("mallory", "Alice"),
            // A peer calling themselves "You" is just another sender.
            message("eve", "You"),
            message("eve", "You"),
        ];
        let groups: Vec<_> = (0..messages.len())
            .map(|ix| starts_group(&messages, ix))
            .collect();
        assert_eq!(groups, [true, false, true, true, false]);
    }

    #[test]
    fn activity_format_by_age() {
        let now = Local
            .with_ymd_and_hms(2026, 11, 5, 15, 0, 0)
            .unwrap()
            .to_utc();
        let at = |delta: Duration| format_activity(now - delta, now);
        assert_eq!(at(Duration::hours(1)), "14:00");
        assert_eq!(at(Duration::days(1)), "Yesterday");
        assert_eq!(at(Duration::days(3)), "Mon");
        assert_eq!(at(Duration::days(30)), "Oct 6");
        assert_eq!(at(Duration::days(400)), "2025-10-01");
        // Clock skew: a peer timestamp in the future still renders as a time.
        assert_eq!(at(Duration::hours(-2)), "17:00");
    }

    #[test]
    fn initials_are_visible_graphemes() {
        assert_eq!(initials(""), "");
        assert_eq!(initials("  ada  "), "A");
        assert_eq!(initials("grace brewster hopper"), "GB");
        // Combining marks and ZWJ emoji stay whole.
        assert_eq!(initials("e\u{301}lodie x"), "E\u{301}X");
        assert_eq!(initials("👩‍💻 coder"), "👩‍💻C");
        // ß would uppercase to two letters; keep it as one grapheme.
        assert_eq!(initials("ßtraße"), "ß");
        // Bidi overrides and zero-width chars can't become the initial.
        assert_eq!(initials("\u{202E}mallory \u{200B}eve"), "ME");
        assert_eq!(initials("\u{202E}\u{200B}"), "");
    }
}
