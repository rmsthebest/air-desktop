//! Keymap loading. The bundled defaults and the user's `keymap.json` share
//! one format and one loader:
//!
//! ```json
//! [{ "context": "Workspace", "bindings": {
//!     "secondary-k": "air::OpenPalette",
//!     "secondary-1": ["air::OpenChatAt", { "index": 0 }],
//!     "alt-down": null
//! }}]
//! ```
//!
//! `assets/keymaps/helix.json` is always loaded on top of the defaults; its
//! contexts require the `helix` identifier, which views add to their key
//! context only in Helix mode, so switching modes needs no rebinding.
//! `null` unbinds. User bindings are added last, so they win.
//! A bad entry is reported and skipped; it never prevents startup.

use std::path::Path;
use std::rc::Rc;

use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate, NoAction};
use serde::Deserialize;
use serde_json::Value;

const DEFAULT_KEYMAP: &str = include_str!("../assets/keymaps/default.json");
const HELIX_KEYMAP: &str = include_str!("../assets/keymaps/helix.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    context: Option<String>,
    bindings: serde_json::Map<String, Value>,
}

/// Binds the default keymap, the Helix layer, then the user keymap at
/// `user_path` if it exists. Returns human-readable problems found along
/// the way.
pub fn load(user_path: Option<&Path>, cx: &mut App) -> Vec<String> {
    let mut errors = Vec::new();
    for (source, origin) in [
        (DEFAULT_KEYMAP, "default keymap"),
        (HELIX_KEYMAP, "helix keymap"),
    ] {
        let bindings = parse(source, origin, &mut errors, cx);
        cx.bind_keys(bindings);
    }

    if let Some(path) = user_path {
        match std::fs::read_to_string(path) {
            Ok(source) => {
                let user = parse(&source, &path.display().to_string(), &mut errors, cx);
                cx.bind_keys(user);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => errors.push(format!("{}: {err}", path.display())),
        }
    }
    errors
}

fn parse(source: &str, origin: &str, errors: &mut Vec<String>, cx: &App) -> Vec<KeyBinding> {
    let sections: Vec<Section> = match serde_json::from_str(source) {
        Ok(sections) => sections,
        Err(err) => {
            errors.push(format!("{origin}: {err}"));
            return Vec::new();
        }
    };

    let mut bindings = Vec::new();
    for section in sections {
        let predicate = match section
            .context
            .as_deref()
            .map(KeyBindingContextPredicate::parse)
        {
            None => None,
            Some(Ok(predicate)) => Some(Rc::new(predicate)),
            Some(Err(err)) => {
                errors.push(format!(
                    "{origin}: invalid context {:?}: {err}",
                    section.context
                ));
                continue;
            }
        };
        for (keystrokes, value) in section.bindings {
            let action = match build_action(&value, cx) {
                Ok(action) => action,
                Err(err) => {
                    errors.push(format!("{origin}: {keystrokes:?}: {err}"));
                    continue;
                }
            };
            match KeyBinding::load(&keystrokes, action, predicate.clone(), false, None, cx) {
                Ok(binding) => bindings.push(binding),
                Err(err) => errors.push(format!("{origin}: {keystrokes:?}: {err}")),
            }
        }
    }
    bindings
}

fn build_action(value: &Value, cx: &App) -> anyhow::Result<Box<dyn Action>> {
    let (name, data) = match value {
        Value::Null => return Ok(Box::new(NoAction)),
        Value::String(name) => (name.as_str(), None),
        Value::Array(items) => match items.as_slice() {
            [Value::String(name), data] => (name.as_str(), Some(data.clone())),
            _ => anyhow::bail!("expected [\"action::Name\", {{...}}]"),
        },
        _ => anyhow::bail!("expected an action name, [name, data] or null"),
    };
    // Only our own actions are bindable; this keeps a keymap from reaching
    // into library-internal actions.
    anyhow::ensure!(name.starts_with("air::"), "unknown action {name:?}");
    cx.build_action(name, data)
        .map_err(|err| anyhow::anyhow!("{err}"))
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn bundled_keymaps_are_valid(cx: &mut TestAppContext) {
        cx.update(|cx| {
            for source in [DEFAULT_KEYMAP, HELIX_KEYMAP] {
                let mut errors = Vec::new();
                let bindings = parse(source, "bundled", &mut errors, cx);
                assert_eq!(errors, Vec::<String>::new());
                assert!(!bindings.is_empty());
            }
        });
    }

    #[gpui::test]
    fn bad_user_entries_are_reported_and_skipped(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let source = r#"[
                { "context": "Workspace", "bindings": {
                    "ctrl-j": "air::NextChat",
                    "ctrl-nope-x": "air::NextChat",
                    "ctrl-u": "air::DoesNotExist",
                    "ctrl-o": "zed::SomethingElse",
                    "ctrl-p": ["air::OpenChatAt", { "index": "zero" }],
                    "ctrl-m": 42,
                    "alt-down": null
                }},
                { "context": "Workspace &&", "bindings": { "ctrl-y": "air::Quit" } }
            ]"#;
            let mut errors = Vec::new();
            let bindings = parse(source, "user", &mut errors, cx);
            // ctrl-j and the null unbinding survive; every other entry is an error.
            assert_eq!(bindings.len(), 2, "{errors:#?}");
            assert_eq!(errors.len(), 6, "{errors:#?}");
            assert!(
                bindings
                    .iter()
                    .any(|b| b.action().as_any().is::<NoAction>())
            );
        });
    }

    #[gpui::test]
    fn malformed_json_is_one_error(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut errors = Vec::new();
            assert!(parse("{ not json", "user", &mut errors, cx).is_empty());
            assert_eq!(errors.len(), 1);
        });
    }
}
