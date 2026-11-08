//! A username to suggest from the display name. Port of upstream's
//! `username_suggestions.rs`; the user still sees and confirms it.

const MAX_USERNAME_LENGTH: usize = 63;
const SUFFIX_LENGTH: usize = 5;
const HYPHEN_LENGTH: usize = 1;
const MAX_BASE_LENGTH: usize = 58;

/// `display` as a valid username, with a random numeric suffix.
pub fn from_display(display: &str) -> String {
    let suffix = (uuid::Uuid::new_v4().as_u128() % 90_000 + 10_000) as u32;
    format!("{}-{suffix}", base_of(display))
}

fn base_of(display: &str) -> String {
    // Only the first three tokens, so a long name doesn't dominate.
    let ascii = deunicode::deunicode(&first_tokens(display, 3).join(" ")).to_lowercase();
    let mut out = String::with_capacity(ascii.len());
    let mut last_was_dash = false;
    let mut started = false; // The first character can't be a digit.
    for ch in ascii.chars() {
        if ch.is_ascii_alphanumeric() {
            if !started && ch.is_ascii_digit() {
                continue;
            }
            started = true;
            out.push(ch);
            last_was_dash = false;
        } else if started && !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    if last_was_dash {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("user");
    }
    truncate_clean(&mut out, MAX_BASE_LENGTH);
    truncate_clean(
        &mut out,
        MAX_USERNAME_LENGTH - SUFFIX_LENGTH - HYPHEN_LENGTH,
    );
    out
}

fn truncate_clean(out: &mut String, max_len: usize) {
    if out.len() > max_len {
        out.truncate(max_len); // ASCII by now.
        while out.ends_with('-') {
            out.pop();
        }
        if out.is_empty() {
            out.push_str("user");
        }
    }
}

/// At most `limit` tokens: alphanumeric runs (across scripts) are words, ASCII
/// punctuation only separates, and any other symbol is a token of its own.
fn first_tokens(display: &str, limit: usize) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in display.chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
            continue;
        }
        if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
            if tokens.len() == limit {
                return tokens;
            }
        }
        if !ch.is_whitespace() && !ch.is_ascii_punctuation() {
            tokens.push(ch.to_string());
            if tokens.len() == limit {
                return tokens;
            }
        }
    }
    if !current.is_empty() && tokens.len() < limit {
        tokens.push(current);
    }
    tokens
}

/// What the command line starts with to add a username: a suggestion only for
/// the first one (someone with usernames knows what they want).
pub fn prompt(display_name: &str, usernames: &[impl AsRef<str>]) -> String {
    if usernames.is_empty() {
        format!("username {}", from_display(display_name))
    } else {
        "username ".to_string()
    }
}

#[cfg(test)]
mod tests {
    use aircommon::identifiers::Username;

    use super::*;

    fn assert_valid(username: &str) {
        Username::new(username.to_string()).unwrap();
        let suffix = username.rsplit('-').next().unwrap();
        assert_eq!(suffix.len(), SUFFIX_LENGTH);
        assert!(suffix.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn suggestions_are_valid_usernames() {
        let out = from_display("Very Long Display Name With Many Extra Words");
        assert!(out.starts_with("very-long-display-"), "{out}");
        assert_valid(&out);
        assert_valid(&from_display(&"a".repeat(200)));
        let digits = from_display("12345");
        assert!(digits.starts_with("user-"), "{digits}");
        assert_valid(&digits);
        let dashes = from_display("ééé---");
        assert_valid(&dashes);
        assert!(!dashes.contains("--"));
        assert_valid(&from_display("🎉🎉🎉"));
        assert_valid(&from_display("Åsa Öberg"));
        assert!(from_display("Åsa Öberg").starts_with("asa-oberg-"));
    }

    #[test]
    fn only_the_first_username_is_suggested() {
        let none: [&str; 0] = [];
        assert!(prompt("Ada", &none).starts_with("username ada-"));
        assert_eq!(prompt("Ada", &["ada-lovelace"]), "username ");
    }
}
