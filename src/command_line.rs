//! Helix-style `:` commands: parsing and completion. Pure, so it is tested
//! without a UI; the workspace executes the parsed [`Command`].

use crate::settings::{Appearance, KeymapMode};

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
    Shortcuts,
}

#[derive(Clone, Copy, PartialEq)]
enum Arg {
    None,
    Theme,
    Chat,
    Appearance,
    Keymap,
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
        "help" => Command::Shortcuts,
        other => unreachable!("spec {other} has no parser"),
    })
}

/// Completions for the current input, each a full replacement line, plus a
/// short description for display.
pub fn complete(input: &str, themes: &[String], chats: &[String]) -> Vec<(String, String)> {
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
    let candidates: Vec<String> = match spec.arg {
        Arg::None => return Vec::new(),
        Arg::Theme => themes.to_vec(),
        Arg::Chat => chats.to_vec(),
        Arg::Appearance => ["system", "light", "dark"].map(String::from).to_vec(),
        Arg::Keymap => ["standard", "helix"].map(String::from).to_vec(),
    };
    let arg = normalize(arg.trim_start());
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
        ] {
            assert!(parse(input).is_err(), "{input:?} should fail");
        }
    }

    #[test]
    fn completes_names_then_arguments() {
        let themes = vec!["Air Dark".to_string(), "Air Light".to_string()];
        let names: Vec<_> = complete(":th", &themes, &[])
            .into_iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(names, ["theme"]);
        let args: Vec<_> = complete(":theme dark", &themes, &[])
            .into_iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(args, ["theme Air Dark"]);
        assert!(complete(":q x", &themes, &[]).is_empty());
        assert!(complete(":nope x", &themes, &[]).is_empty());
    }

    #[test]
    fn every_command_name_parses() {
        for spec in SPECS {
            for name in spec.names {
                let line = match spec.arg {
                    Arg::None => name.to_string(),
                    Arg::Appearance => format!("{name} dark"),
                    Arg::Keymap => format!("{name} helix"),
                    Arg::Theme | Arg::Chat => format!("{name} x"),
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
