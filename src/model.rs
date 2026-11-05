//! View-model types the UI renders. They are deliberately plain data so views
//! never reach into `aircoreclient` directly; the session layer (milestone 3)
//! produces them from core state. Until then, [`mock`] supplies sample data.

use gpui_kit::SharedString;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatId(pub SharedString);

#[derive(Debug, Clone)]
pub struct ChatSummary {
    pub id: ChatId,
    pub title: SharedString,
    pub is_group: bool,
    pub last_message: Option<SharedString>,
    /// Pre-formatted relative time of the last activity, e.g. "14:02".
    pub last_activity: SharedString,
    pub unread: u32,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub sender: SharedString,
    pub own: bool,
    pub body: SharedString,
    pub time: SharedString,
}

/// Initials for an avatar placeholder: first letters of the first two words.
pub fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

pub mod mock {
    use super::*;

    pub fn chats() -> Vec<ChatSummary> {
        let chat = |id: &'static str, title: &'static str, group, last, time, unread| ChatSummary {
            id: ChatId(id.into()),
            title: title.into(),
            is_group: group,
            last_message: Some(SharedString::from(last)),
            last_activity: SharedString::from(time),
            unread,
        };
        vec![
            chat(
                "ada",
                "Ada Lovelace",
                false,
                "The engine can compose music too.",
                "14:02",
                2,
            ),
            chat(
                "design",
                "Design Crew",
                true,
                "Shipped the new palette 🎨",
                "13:47",
                5,
            ),
            chat(
                "grace",
                "Grace Hopper",
                false,
                "Found the bug. It was a moth.",
                "11:20",
                0,
            ),
            chat(
                "alan",
                "Alan Turing",
                false,
                "Can machines think?",
                "Yesterday",
                0,
            ),
            chat("rust", "Rustaceans", true, "New release is out", "Mon", 0),
        ]
    }

    pub fn messages(chat: &ChatId) -> Vec<Message> {
        let peer: SharedString = match chat.0.as_ref() {
            "ada" => "Ada Lovelace",
            "grace" => "Grace Hopper",
            "alan" => "Alan Turing",
            _ => "Someone",
        }
        .into();
        let msg = |own: bool, body: &'static str, time: &'static str| Message {
            sender: if own { "You".into() } else { peer.clone() },
            own,
            body: body.into(),
            time: time.into(),
        };
        vec![
            msg(
                false,
                "Hey! Did you get a chance to look at the new client?",
                "13:55",
            ),
            msg(true, "Yes, it is so much nicer on a big screen.", "13:56"),
            msg(
                true,
                "And everything is reachable from the keyboard.",
                "13:56",
            ),
            msg(
                false,
                "Ctrl+K to jump between chats is my favourite.",
                "13:58",
            ),
            msg(false, "The engine can compose music too.", "14:02"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::initials;

    #[test]
    fn initials_handles_empty_unicode_and_long_names() {
        assert_eq!(initials(""), "");
        assert_eq!(initials("  ada  "), "A");
        assert_eq!(initials("grace brewster hopper"), "GB");
        assert_eq!(initials("élodie ßtraße"), "ÉSS");
    }
}
