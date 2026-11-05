//! The chat list's preview line: the draft, or a description of the last
//! message. Port of the app's `_LastMessage` widget and applogic's
//! `preview_text` (markdown flattened to plain text).

use aircoreclient::clients::CoreUser;
use aircoreclient::{ChatMessage, EventMessage, Message};
use gpui::SharedString;
use mimi_content::content_container::{Disposition, NestedPart};
use mimi_content::{MessageStatus, MimiContent};
use pulldown_cmark::{Event, Options, Parser, TagEnd};

use crate::model::{Prefix, Preview, PreviewKind, name_line, preview_line};

/// What the preview needs to know about a chat's last message.
#[derive(Debug, PartialEq)]
pub enum LastMessage {
    Hidden,
    Deleted,
    Content {
        /// `None` when the local user sent it.
        sender_name: Option<String>,
        body: Body,
    },
    /// A system message, already put into words.
    Event(String),
}

#[derive(Debug, PartialEq)]
pub enum Body {
    Markdown(String),
    Attachment,
    Unsupported,
}

/// Describes a message, loading the names it mentions. `None` for messages
/// the list doesn't preview (errors), as upstream.
pub async fn describe(user: &CoreUser, message: &ChatMessage) -> Option<LastMessage> {
    if message.status() == MessageStatus::Hidden {
        return Some(LastMessage::Hidden);
    }
    if message.message().is_deleted() {
        return Some(LastMessage::Deleted);
    }
    match message.message() {
        Message::Content(content) => {
            let sender = content.sender();
            let sender_name = if sender == user.user_id() {
                None
            } else {
                let name = user.user_profile(sender).await.display_name;
                Some(name_line(name.as_ref()).to_string())
            };
            let body = body_of(content.content());
            Some(LastMessage::Content { sender_name, body })
        }
        Message::Event(event @ EventMessage::System(_)) => {
            Some(LastMessage::Event(event.string_representation(user).await))
        }
        Message::Event(EventMessage::Error(_)) => None,
    }
}

/// What a message's content is, as far as this client can show it.
pub fn body_of(content: &MimiContent) -> Body {
    match &content.nested_part {
        NestedPart::SinglePart {
            content_type,
            content,
            ..
        } if content_type == "text/markdown" => {
            Body::Markdown(String::from_utf8_lossy(content).into_owned())
        }
        NestedPart::MultiPart {
            disposition: Disposition::Attachment,
            ..
        } => Body::Attachment,
        _ => Body::Unsupported,
    }
}

/// The exact text of a markdown message, as editing it starts from (and
/// compares against): no lossy decoding.
pub fn plain_body(content: &MimiContent) -> Option<&str> {
    match &content.nested_part {
        NestedPart::SinglePart {
            content_type,
            content,
            ..
        } if content_type == "text/markdown" => std::str::from_utf8(content).ok(),
        _ => None,
    }
}

/// One line of a replied-to message, or what stands in for it.
pub fn quote_line(content: Option<&MimiContent>) -> SharedString {
    match content.map(body_of) {
        Some(Body::Markdown(source)) => preview_line(&plain_text(&source)),
        Some(Body::Attachment) => "Attachment".into(),
        Some(Body::Unsupported) => "Unsupported message".into(),
        None => "Message unavailable".into(),
    }
}

/// The preview of a chat: a non-empty draft wins over the last message.
/// Senders are named in groups only; the local user is always "You".
pub fn preview(draft: Option<&str>, last: Option<LastMessage>, is_group: bool) -> Option<Preview> {
    let make = |prefix: Option<Prefix>, text: &str, kind| {
        Some(Preview {
            prefix,
            text: preview_line(text),
            kind,
        })
    };
    if let Some(draft) = draft.map(plain_text).filter(|text| !text.is_empty()) {
        return make(Some(Prefix::Draft), &draft, PreviewKind::Draft);
    }
    match last? {
        LastMessage::Hidden => make(None, "Hidden message", PreviewKind::Notice),
        LastMessage::Deleted => make(None, "Message deleted", PreviewKind::Notice),
        LastMessage::Event(text) => make(None, &text, PreviewKind::Message),
        LastMessage::Content { sender_name, body } => {
            let prefix = match sender_name {
                None => Some(Prefix::You),
                Some(name) if is_group => Some(Prefix::Sender(name.into())),
                Some(_) => None,
            };
            let text = match body {
                Body::Markdown(markdown) => plain_text(&markdown),
                Body::Attachment => "Attachment".into(),
                Body::Unsupported => "Unsupported message".into(),
            };
            make(prefix, &text, PreviewKind::Message)
        }
    }
}

/// Markdown flattened to its text, falling back to the raw markdown when
/// that is empty (e.g. only an image). As upstream's `preview_text`.
fn plain_text(markdown: &str) -> String {
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let mut text = String::new();
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Text(value) | Event::Code(value) => text.push_str(&value),
            Event::SoftBreak
            | Event::HardBreak
            | Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::CodeBlock
                | TagEnd::TableCell,
            ) => text.push(' '),
            _ => {}
        }
    }
    let text = text.trim();
    if text.is_empty() {
        markdown.trim().to_string()
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(preview: Option<Preview>) -> Option<(Option<Prefix>, String, PreviewKind)> {
        preview.map(|p| (p.prefix, p.text.to_string(), p.kind))
    }

    fn content(sender_name: Option<&str>, markdown: &str) -> Option<LastMessage> {
        Some(LastMessage::Content {
            sender_name: sender_name.map(Into::into),
            body: Body::Markdown(markdown.into()),
        })
    }

    #[test]
    fn markdown_is_flattened_to_one_line() {
        assert_eq!(
            plain_text("# Title\n\n**bold** and `code`\n\n- one\n- two"),
            "Title bold and code one two"
        );
        assert_eq!(
            text(preview(
                None,
                content(None, "a\nb [link](https://x)"),
                false
            ))
            .unwrap()
            .1,
            "a b link"
        );
        // Nothing but markup: show the source rather than nothing.
        assert_eq!(plain_text("![](x.png)"), "![](x.png)");
    }

    #[test]
    fn prefixes_follow_who_sent_it_and_where() {
        let sender = |name: &str| Some(Prefix::Sender(name.to_string().into()));
        let message = PreviewKind::Message;
        assert_eq!(
            text(preview(None, content(None, "hi"), false)),
            Some((Some(Prefix::You), "hi".into(), message))
        );
        assert_eq!(
            text(preview(None, content(Some("Ada"), "hi"), false)),
            Some((None, "hi".into(), message))
        );
        assert_eq!(
            text(preview(None, content(Some("Ada"), "hi"), true)),
            Some((sender("Ada"), "hi".into(), message))
        );
        // A peer named "You" stays a peer: only the local user gets the label.
        let prefix = text(preview(None, content(Some("You"), "hi"), true))
            .unwrap()
            .0;
        assert_eq!(prefix, sender("You"));
        assert!(!prefix.unwrap().is_label());
    }

    #[test]
    fn a_draft_wins_unless_it_is_blank() {
        let last = || content(Some("Ada"), "hi");
        assert_eq!(
            text(preview(Some("*soon*"), last(), false)),
            Some((Some(Prefix::Draft), "soon".into(), PreviewKind::Draft))
        );
        assert_eq!(text(preview(Some(" \n "), last(), false)).unwrap().1, "hi");
        assert_eq!(preview(Some(""), None, false), None);
    }

    #[test]
    fn unshowable_messages_get_a_notice() {
        for (last, notice) in [
            (LastMessage::Hidden, "Hidden message"),
            (LastMessage::Deleted, "Message deleted"),
        ] {
            assert_eq!(
                text(preview(None, Some(last), true)),
                Some((None, notice.into(), PreviewKind::Notice))
            );
        }
    }
}
