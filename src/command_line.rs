//! Helix-style `:` commands: parsing and completion. Pure, so it is tested
//! without a UI; the workspace executes the parsed [`Command`].

use crate::settings::{Appearance, KeymapMode, Notifications};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Quit,
    /// Theme name as typed; resolved with [`matches_name`] when executed.
    Theme(String),
    /// Chat title query; resolved with [`matches_name`] when executed.
    Open(String),
    Appearance(Appearance),
    ToggleDetails,
    Keymap(KeymapMode),
    Notifications(Notifications),
    Shortcuts,
    /// Send a contact request to a username, as typed.
    AddContact(String),
    /// Register a username, as typed.
    AddUsername(String),
    RemoveUsername(String),
    /// Accept or decline the open chat's contact request.
    Accept,
    Decline,
    /// Create a group with this name.
    Group(String),
    /// Invite a contact to the open group, by name as typed.
    Invite(String),
    /// Remove a member from the open group, by name as typed.
    Kick(String),
    /// Rename the open group.
    Rename(String),
    Leave,
    /// Delete the open chat (after confirming): a group, a contact's chat
    /// or an ended request.
    DeleteChat,
    /// Block or unblock the open chat's contact (or request's sender).
    Block,
    Unblock,
    /// Report the open chat's contact for spam, or the open group's member
    /// named (as typed).
    Report(String),
    /// Retract the open chat's contact request.
    Retract,
    /// Send the file at this path, as typed, to the open chat.
    Attach(String),
    /// Change the user's display name.
    DisplayName(String),
    /// Link the new device that shows this code (as typed).
    Link(String),
    /// Rename a device: `[#tag] <name>`, as typed (no tag: this device).
    DeviceName(String),
    /// Unlink a device, by `#tag` or name as typed (after confirming).
    Unlink(String),
    /// Show the safety code shared with the open chat's contact, or with
    /// the open group's member named (as typed).
    SafetyCode(String),
}

/// What arguments complete from.
#[derive(Default)]
pub struct Candidates {
    pub themes: Vec<String>,
    pub chats: Vec<String>,
    /// Contacts' names (one-to-one chats).
    pub contacts: Vec<String>,
    /// The open group's members other than the user: what to insert, and
    /// what to show (and match) for it.
    pub members: Vec<(String, String)>,
    /// The user's other devices: their `#tag` to insert, and what to show.
    pub devices: Vec<(String, String)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Arg {
    None,
    Theme,
    Chat,
    Appearance,
    Keymap,
    Notifications,
    Contact,
    Member,
    /// One of the user's other devices.
    Device,
    /// Free text, not completed (e.g. a username).
    Text,
}

struct Spec {
    names: &'static [&'static str],
    arg: Arg,
    help: &'static str,
}

const SPECS: &[Spec] = &[
    Spec {
        names: &["quit", "q", "quit-all", "qa"],
        arg: Arg::None,
        help: "Quit Air",
    },
    Spec {
        names: &["open", "o"],
        arg: Arg::Chat,
        help: "Open a chat by name",
    },
    Spec {
        names: &["theme"],
        arg: Arg::Theme,
        help: "Switch theme",
    },
    Spec {
        names: &["appearance"],
        arg: Arg::Appearance,
        help: "system, light or dark",
    },
    Spec {
        names: &["details"],
        arg: Arg::None,
        help: "Toggle the details pane",
    },
    Spec {
        names: &["keymap"],
        arg: Arg::Keymap,
        help: "standard or helix",
    },
    Spec {
        names: &["notifications"],
        arg: Arg::Notifications,
        help: "full, private (no names or text) or off",
    },
    Spec {
        names: &["contact", "add"],
        arg: Arg::Text,
        help: "Send a contact request to a username",
    },
    Spec {
        names: &["accept"],
        arg: Arg::None,
        help: "Accept the open contact request",
    },
    Spec {
        names: &["decline"],
        arg: Arg::None,
        help: "Decline the open contact request",
    },
    Spec {
        names: &["group"],
        arg: Arg::Text,
        help: "Create a group with this name",
    },
    Spec {
        names: &["invite"],
        arg: Arg::Contact,
        help: "Invite a contact to the open group",
    },
    Spec {
        names: &["kick", "remove"],
        arg: Arg::Member,
        help: "Remove a member from the open group",
    },
    Spec {
        names: &["rename"],
        arg: Arg::Text,
        help: "Rename the open group",
    },
    Spec {
        names: &["leave"],
        arg: Arg::None,
        help: "Leave the open group",
    },
    Spec {
        names: &["delete-chat"],
        arg: Arg::None,
        help: "Delete the open chat (a group, contact or ended request)",
    },
    Spec {
        names: &["block"],
        arg: Arg::None,
        help: "Block the contact (declines their request)",
    },
    Spec {
        names: &["unblock"],
        arg: Arg::None,
        help: "Unblock the contact",
    },
    Spec {
        names: &["report"],
        arg: Arg::Member,
        help: "Report the contact (or a group member) for spam",
    },
    Spec {
        names: &["retract"],
        arg: Arg::None,
        help: "Retract the contact request you sent",
    },
    Spec {
        names: &["attach"],
        arg: Arg::Text,
        help: "Send a file (a path) to the open chat",
    },
    Spec {
        names: &["name"],
        arg: Arg::Text,
        help: "Change your display name",
    },
    Spec {
        names: &["username"],
        arg: Arg::Text,
        help: "Add a username others can find you by",
    },
    Spec {
        names: &["remove-username"],
        arg: Arg::Text,
        help: "Remove one of your usernames",
    },
    Spec {
        names: &["link"],
        arg: Arg::Text,
        help: "Link a new device: the code it shows",
    },
    Spec {
        names: &["device-name"],
        arg: Arg::Text,
        help: "Rename this device, or another: #tag name",
    },
    Spec {
        names: &["unlink"],
        arg: Arg::Device,
        help: "Unlink one of your other devices",
    },
    Spec {
        names: &["safety-code"],
        arg: Arg::Member,
        help: "Compare safety codes with the contact (or a group member)",
    },
    Spec {
        names: &["help", "shortcuts"],
        arg: Arg::None,
        help: "Show keyboard shortcuts",
    },
];

fn spec(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|spec| spec.names.contains(&name))
}

pub fn parse(input: &str) -> Result<Command, String> {
    let input = input.trim().trim_start_matches(':').trim_start();
    let (name, arg) = match input.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, arg.trim()),
        None => (input, ""),
    };
    let spec = spec(name).ok_or_else(|| format!("no such command: {name:?}"))?;
    let need = |what: &str| {
        if arg.is_empty() {
            Err(format!(":{name} needs {what}"))
        } else {
            Ok(arg.to_string())
        }
    };
    if spec.arg == Arg::None && !arg.is_empty() {
        return Err(format!(":{name} takes no argument"));
    }
    Ok(match spec.names[0] {
        "quit" => Command::Quit,
        "open" => Command::Open(need("a chat name")?),
        "theme" => Command::Theme(need("a theme name")?),
        "appearance" => Command::Appearance(match normalize(arg).as_str() {
            "system" => Appearance::System,
            "light" => Appearance::Light,
            "dark" => Appearance::Dark,
            _ => return Err(":appearance takes system, light or dark".into()),
        }),
        "details" => Command::ToggleDetails,
        "keymap" => Command::Keymap(match normalize(arg).as_str() {
            "standard" => KeymapMode::Standard,
            "helix" => KeymapMode::Helix,
            _ => return Err(":keymap takes standard or helix".into()),
        }),
        "notifications" => Command::Notifications(match normalize(arg).as_str() {
            "full" => Notifications::Full,
            "private" => Notifications::Private,
            "off" => Notifications::Off,
            _ => return Err(":notifications takes full, private or off".into()),
        }),
        "help" => Command::Shortcuts,
        "contact" => Command::AddContact(need("a username")?),
        "accept" => Command::Accept,
        "decline" => Command::Decline,
        "username" => Command::AddUsername(need("a username")?),
        "remove-username" => Command::RemoveUsername(need("a username")?),
        "group" => Command::Group(need("a name")?),
        "invite" => Command::Invite(need("a contact")?),
        "kick" => Command::Kick(need("a member")?),
        "rename" => Command::Rename(need("a name")?),
        "leave" => Command::Leave,
        "delete-chat" => Command::DeleteChat,
        "block" => Command::Block,
        "unblock" => Command::Unblock,
        "report" => Command::Report(arg.to_string()),
        "retract" => Command::Retract,
        "attach" => Command::Attach(need("a file path")?),
        "name" => Command::DisplayName(need("a name")?),
        "link" => Command::Link(need("the code the new device shows")?),
        "device-name" => Command::DeviceName(need("a name")?),
        "unlink" => Command::Unlink(need("a device")?),
        "safety-code" => Command::SafetyCode(arg.to_string()),
        other => unreachable!("spec {other} has no parser"),
    })
}

/// Completions for the current input, each a full replacement line, plus a
/// short description for display.
pub fn complete(input: &str, candidates: &Candidates) -> Vec<(String, String)> {
    let input = input.trim_start_matches(':').trim_start();
    let Some((name, arg)) = input.split_once(char::is_whitespace) else {
        return SPECS
            .iter()
            .flat_map(|spec| spec.names.iter().map(move |name| (*name, spec.help)))
            .filter(|(name, _)| name.starts_with(input))
            .map(|(name, help)| (name.to_string(), help.to_string()))
            .collect();
    };
    let Some(spec) = spec(name) else {
        return Vec::new();
    };
    let arg = normalize(arg.trim_start());
    let candidates: Vec<String> = match spec.arg {
        Arg::None | Arg::Text => return Vec::new(),
        // Matched on what is shown, which names the member.
        Arg::Member | Arg::Device => {
            let named = if spec.arg == Arg::Member {
                &candidates.members
            } else {
                &candidates.devices
            };
            return named
                .iter()
                .filter(|(_, shown)| normalize(shown).contains(&arg))
                .map(|(insert, shown)| (format!("{name} {insert}"), shown.clone()))
                .collect();
        }
        Arg::Theme => candidates.themes.clone(),
        Arg::Chat => candidates.chats.clone(),
        Arg::Contact => candidates.contacts.clone(),
        Arg::Appearance => ["system", "light", "dark"].map(String::from).to_vec(),
        Arg::Keymap => ["standard", "helix"].map(String::from).to_vec(),
        Arg::Notifications => ["full", "private", "off"].map(String::from).to_vec(),
    };
    candidates
        .into_iter()
        .filter(|candidate| normalize(candidate).contains(&arg))
        .map(|candidate| (format!("{name} {candidate}"), String::new()))
        .collect()
}

/// Lowercase with spaces, dashes and underscores removed, so `air-dark`
/// matches "Air Dark".
pub fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Result of resolving a typed name against candidates.
#[derive(Debug, PartialEq)]
pub enum NameMatch<'a> {
    One(&'a str),
    None,
    /// Several candidates match; acting on any of them would be a guess.
    Ambiguous(usize),
}

/// Resolves `query` to a single candidate: the only exact normalized match,
/// or else the only candidate containing it. Never guesses between several,
/// since names (e.g. peers' display names) may be chosen to collide.
pub fn matches_name<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> NameMatch<'a> {
    let query = normalize(query);
    if query.is_empty() {
        return NameMatch::None;
    }
    let candidates: Vec<_> = candidates.into_iter().collect();
    let pick = |hits: Vec<&'a str>| match hits.as_slice() {
        [] => NameMatch::None,
        [one] => NameMatch::One(one),
        many => NameMatch::Ambiguous(many.len()),
    };
    let exact: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|candidate| normalize(candidate) == query)
        .collect();
    if !exact.is_empty() {
        return pick(exact);
    }
    pick(
        candidates
            .into_iter()
            .filter(|candidate| normalize(candidate).contains(&query))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_aliases_and_arguments() {
        assert_eq!(parse(":q"), Ok(Command::Quit));
        assert_eq!(parse("  qa "), Ok(Command::Quit));
        assert_eq!(
            parse(":o grace hopper"),
            Ok(Command::Open("grace hopper".into()))
        );
        assert_eq!(
            parse(":theme air-dark"),
            Ok(Command::Theme("air-dark".into()))
        );
        assert_eq!(
            parse(":appearance Dark"),
            Ok(Command::Appearance(Appearance::Dark))
        );
        assert_eq!(
            parse(":keymap helix"),
            Ok(Command::Keymap(KeymapMode::Helix))
        );
        assert_eq!(parse(":details"), Ok(Command::ToggleDetails));
        // Usernames go to the session as typed; it normalizes them.
        assert_eq!(
            parse(":add  @Ada-L "),
            Ok(Command::AddContact("@Ada-L".into()))
        );
    }

    #[test]
    fn rejects_bad_input_with_a_message() {
        for input in [
            ":",
            ":nope",
            ":theme",
            ":q now",
            ":appearance purple",
            ":keymap emacs",
            ":notifications loud",
            ":contact",
            ":accept now",
            ":group",
            ":group  ",
            ":leave now",
        ] {
            assert!(parse(input).is_err(), "{input:?} should fail");
        }
    }

    #[test]
    fn completes_names_then_arguments() {
        let candidates = Candidates {
            themes: vec!["Air Dark".to_string(), "Air Light".to_string()],
            contacts: vec!["Grace Hopper".to_string()],
            members: vec![("#1a2b3c4d".to_string(), "Alan Turing #1a2b3c4d".to_string())],
            devices: vec![("#9f00".to_string(), "Phone #9f00".to_string())],
            ..Candidates::default()
        };
        let first = |input| -> Vec<String> {
            complete(input, &candidates)
                .into_iter()
                .map(|c| c.0)
                .collect()
        };
        assert_eq!(first(":th"), ["theme"]);
        assert_eq!(first(":theme dark"), ["theme Air Dark"]);
        // Invites complete from contacts, removals from members.
        assert_eq!(first(":invite gr"), ["invite Grace Hopper"]);
        assert_eq!(first(":kick al"), ["kick #1a2b3c4d"]);
        assert!(first(":kick gr").is_empty());
        // Devices complete to their tag, matched on name or tag.
        assert_eq!(first(":unlink ph"), ["unlink #9f00"]);
        assert_eq!(first(":unlink 9f"), ["unlink #9f00"]);
        assert!(first(":unlink al").is_empty());
        assert!(first(":q x").is_empty());
        assert!(first(":nope x").is_empty());
    }

    #[test]
    fn every_command_name_parses() {
        for spec in SPECS {
            for name in spec.names {
                let line = match spec.arg {
                    Arg::None => name.to_string(),
                    Arg::Appearance => format!("{name} dark"),
                    Arg::Keymap => format!("{name} helix"),
                    Arg::Notifications => format!("{name} private"),
                    Arg::Theme
                    | Arg::Chat
                    | Arg::Contact
                    | Arg::Member
                    | Arg::Device
                    | Arg::Text => {
                        format!("{name} x")
                    }
                };
                assert!(parse(&line).is_ok(), "{line:?} should parse");
            }
        }
    }

    #[test]
    fn name_matching_never_guesses() {
        let names = ["Grace Hopper", "Grace", "Alan Turing"];
        assert_eq!(matches_name("grace", names), NameMatch::One("Grace"));
        assert_eq!(
            matches_name("hopper", names),
            NameMatch::One("Grace Hopper")
        );
        assert_eq!(
            matches_name("air-dark", ["Air Dark"]),
            NameMatch::One("Air Dark")
        );
        assert_eq!(matches_name("nobody", names), NameMatch::None);
        // Look-alike names (normalizing equal) are ambiguous, as are
        // several partial matches.
        assert_eq!(
            matches_name("grace hopper", ["grace-hopper", "Grace Hopper"]),
            NameMatch::Ambiguous(2)
        );
        assert_eq!(matches_name("a", names), NameMatch::Ambiguous(3));
        // A query that normalizes to nothing matches nothing.
        assert_eq!(matches_name(" -_", names), NameMatch::None);
    }
}
