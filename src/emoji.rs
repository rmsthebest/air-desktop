//! Emoji by name: the picker's list and `:shortcode:` in the composer
//! (GitHub's shortcodes, as the `emojis` crate has them).

/// One emoji the picker offers.
pub struct Entry {
    pub emoji: &'static str,
    /// Its Unicode name, e.g. "grinning face".
    pub name: &'static str,
    /// Its shortcodes, e.g. "grinning".
    pub shortcodes: Vec<&'static str>,
}

/// Every emoji, in Unicode's order (default skin tone).
pub fn all() -> Vec<Entry> {
    emojis::iter()
        .map(|emoji| Entry {
            emoji: emoji.as_str(),
            name: emoji.name(),
            shortcodes: emoji.shortcodes().collect(),
        })
        .collect()
}

/// A shortcode may hold these, as GitHub's do.
fn shortcode_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '+' | '-')
}

/// If the text before `cursor` (a byte offset) ends in a known
/// `:shortcode:`, the text with it replaced by its emoji, and where the
/// cursor goes (just after the emoji). The opening colon must start the
/// text or follow a space, so `10:30:` or a URL's `a:b:` stay as typed.
pub fn expand_shortcode(text: &str, cursor: usize) -> Option<(String, usize)> {
    let before = text.get(..cursor)?;
    let body = before.strip_suffix(':')?;
    let start = body.rfind(':')?;
    let code = &body[start + 1..];
    if code.is_empty() || !code.chars().all(shortcode_char) {
        return None;
    }
    let opens_word = body[..start]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    if !opens_word {
        return None;
    }
    let emoji = emojis::get_by_shortcode(code)?.as_str();
    let mut expanded = String::with_capacity(text.len());
    expanded.push_str(&text[..start]);
    expanded.push_str(emoji);
    expanded.push_str(&text[cursor..]);
    Some((expanded, start + emoji.len()))
}

/// Whether the change from `before` to `text` typed one closing `:` just
/// before `cursor`: only then is a shortcode expanded, so an undo that
/// brings back `:smile:` (or a paste) leaves it as it is.
pub fn closed_by_typing(before: &str, text: &str, cursor: usize) -> bool {
    let Some(typed) = cursor.checked_sub(1) else {
        return false;
    };
    text.len() == before.len() + 1
        && text.get(typed..cursor) == Some(":")
        && text
            .get(..typed)
            .zip(text.get(cursor..))
            .is_some_and(|(head, tail)| {
                before.len() == head.len() + tail.len()
                    && before.starts_with(head)
                    && before.ends_with(tail)
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_shortcode_becomes_its_emoji() {
        let typed = "hi :smile:";
        assert_eq!(
            expand_shortcode(typed, typed.len()),
            Some(("hi 😄".to_string(), "hi 😄".len()))
        );
        // Mid-text: what follows the cursor stays, after the emoji.
        let typed = ":+1: and more";
        assert_eq!(
            expand_shortcode(typed, 4),
            Some(("👍 and more".to_string(), "👍".len()))
        );
    }

    #[test]
    fn anything_else_stays_as_typed() {
        for typed in [
            "meet at 10:30:",      // not after a space
            "see http://a:smile:", // inside a word
            "hi :nosuchcode:",     // unknown
            "hi :Smile:",          // shortcodes are lowercase
            "hi :smile",           // not closed
            "::",                  // empty
            "hi :sm ile:",         // not one word
        ] {
            assert_eq!(expand_shortcode(typed, typed.len()), None, "{typed}");
        }
        // The cursor isn't after the closing colon.
        assert_eq!(expand_shortcode("hi :smile: x", 12), None);
        // Not on a character boundary: nothing, never a panic.
        assert_eq!(expand_shortcode("é:", 1), None);
    }

    #[test]
    fn only_a_typed_closing_colon_expands() {
        assert!(closed_by_typing("hi :smile", "hi :smile:", 10));
        assert!(closed_by_typing(":smile x", ":smile: x", 7));
        // Undo (or a paste) bringing back a whole shortcode: kept.
        assert!(!closed_by_typing("hi 😄", "hi :smile:", 10));
        // A colon typed elsewhere, or a deletion.
        assert!(!closed_by_typing("hi :smile", "hi :smile:", 3));
        assert!(!closed_by_typing("hi :smile::", "hi :smile:", 10));
        assert!(!closed_by_typing("", "", 0));
    }
}
