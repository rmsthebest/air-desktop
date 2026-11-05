//! Command palette (jump to chat, run a command, pick a theme) and the
//! keyboard shortcuts overview.
//!
//! The palette lives in a dialog layer outside the workspace's element tree,
//! so actions dispatched from inside it would never reach the workspace's
//! handlers. Items therefore carry no action of their own: on confirm the
//! dialog closes first, which returns focus to where it was, and the entry
//! is dispatched from there.

use std::rc::Rc;

use gpui::{
    Action, AnyElement, AppContext as _, AsKeystroke as _, SharedString, WeakEntity, Window,
};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, IntoElement, div, px};

use crate::actions;
use crate::model::{ChatId, ChatSummary};
use crate::settings::{KeymapMode, SettingsStore};
use crate::theme;
use crate::ui::{Workspace, show_error};

enum Entry {
    Chat { id: ChatId, title: SharedString },
    Command(actions::Command),
    Theme(String),
}

struct Section {
    label: &'static str,
    entries: Vec<Entry>,
}

/// Called from the workspace's own action handler, so it receives the chats
/// instead of reading the workspace (which is mid-update).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    All,
    Chats,
    Themes,
}

pub fn open_palette(
    scope: Scope,
    chats: Vec<ChatSummary>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let mut sections = vec![
        Section {
            label: "Chats",
            entries: chats
                .into_iter()
                .map(|chat| Entry::Chat {
                    id: chat.id,
                    title: chat.title,
                })
                .collect(),
        },
        Section {
            label: "Commands",
            entries: actions::commands()
                .into_iter()
                .map(Entry::Command)
                .collect(),
        },
        Section {
            label: "Themes",
            entries: theme::available(cx).into_iter().map(Entry::Theme).collect(),
        },
    ];
    sections.retain(|section| match scope {
        Scope::All => true,
        Scope::Chats => section.label == "Chats",
        Scope::Themes => section.label == "Themes",
    });
    let sections = Rc::new(sections);

    let state = cx.new(|cx| CommandState::new(window, cx));
    let palette_state = state.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(560.))
            .child(build(&palette_state, &sections, &workspace))
    });
    state.update(cx, |state, cx| state.focus(window, cx));
}

fn build(
    state: &gpui::Entity<CommandState>,
    sections: &Rc<Vec<Section>>,
    workspace: &WeakEntity<Workspace>,
) -> Command {
    let mut command = Command::new(state).placeholder("Jump to a chat or run a command…");
    for section in sections.iter() {
        let items = section.entries.iter().map(|entry| match entry {
            Entry::Chat { title, .. } => CommandItem::new().label(title.clone()),
            Entry::Theme(name) => CommandItem::new()
                .label(format!("Theme: {name}"))
                .keywords(["theme", "appearance"]),
            Entry::Command(cmd) => {
                let label = cmd.label.clone();
                let action = cmd.action.boxed_clone();
                let context = cmd.context;
                CommandItem::new()
                    .label(label.clone())
                    .child(move |window, _| command_row(label.clone(), &*action, context, window))
            }
        });
        command = command.group(CommandGroup::new().label(section.label).items(items));
    }

    let sections = sections.clone();
    let workspace = workspace.clone();
    command.on_confirm(move |path, window, cx| {
        let Some(entry) = sections
            .get(path.section)
            .and_then(|section| section.entries.get(path.row))
        else {
            return;
        };
        window.close_dialog(cx);
        match entry {
            Entry::Command(cmd) => window.dispatch_action(cmd.action.boxed_clone(), cx),
            Entry::Chat { id, .. } => {
                let id = id.clone();
                _ = workspace.update(cx, |workspace, cx| workspace.jump_to_chat(id, window, cx));
            }
            Entry::Theme(name) => {
                if let Err(err) = theme::select_theme(name, window, cx) {
                    show_error(format!("Couldn't switch theme: {err:#}"), window, cx);
                }
            }
        }
    })
}

fn command_row(
    label: SharedString,
    action: &dyn Action,
    context: &str,
    window: &Window,
) -> AnyElement {
    h_flex()
        .w_full()
        .justify_between()
        .gap_4()
        .child(label)
        .children(Kbd::binding_for_action(action, Some(context), window))
        .into_any_element()
}

/// Read-only overview of the active bindings, read from the live keymap so
/// it always matches (including user overrides), grouped by where they apply.
pub fn show_shortcuts(window: &mut Window, cx: &mut App) {
    let rows = Rc::new(shortcut_rows(cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        let theme = cx.theme();
        let mut list = v_flex().gap_0p5();
        let mut current_group = "";
        for row in rows.iter() {
            if row.group != current_group {
                current_group = row.group;
                list = list.child(
                    div()
                        .pt_3()
                        .pb_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(row.group),
                );
            }
            list = list.child(
                h_flex()
                    .justify_between()
                    .gap_4()
                    .child(row.label.clone())
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(row.keys.clone()),
                    ),
            );
        }
        dialog.title("Keyboard shortcuts").w(px(520.)).child(
            div()
                .id("shortcuts")
                .max_h(px(560.))
                .overflow_y_scroll()
                .child(list),
        )
    });
}

struct ShortcutRow {
    group: &'static str,
    keys: String,
    label: String,
}

const GROUPS: [&str; 7] = [
    "Anywhere",
    "Chat list",
    "Messages",
    "Composer",
    "Conversation",
    "Command line",
    "Other",
];

fn shortcut_rows(cx: &App) -> Vec<ShortcutRow> {
    let helix = SettingsStore::get(cx).keymap_mode == KeymapMode::Helix;
    let os = cx.platform_info().os;
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    // Later bindings win, so walk backwards and keep the first of each
    // (keys, context) pair; an unbinding hides what it shadows.
    for binding in keymap.bindings().rev() {
        let context = binding
            .predicate()
            .map(|p| p.to_string())
            .unwrap_or_default();
        let keys = binding
            .keystrokes()
            .iter()
            .map(|k| k.as_keystroke().unparse(os))
            .collect::<Vec<_>>()
            .join(" ");
        if !seen.insert((keys.clone(), context.clone())) {
            continue;
        }
        let action = binding.action();
        if gpui::is_no_action(action) || !action.name().starts_with("air::") {
            continue;
        }
        if context.contains("helix") && !helix {
            continue;
        }
        let group = if context.contains("ChatList") {
            "Chat list"
        } else if context.contains("MessageList") {
            "Messages"
        } else if context.contains("Composer") {
            "Composer"
        } else if context.contains("CommandLine") {
            "Command line"
        } else if context.contains("Conversation") {
            "Conversation"
        } else if context.is_empty() || context.contains("Workspace") {
            "Anywhere"
        } else {
            "Other"
        };
        rows.push(ShortcutRow {
            group,
            keys,
            label: actions::label(action),
        });
    }
    rows.reverse();
    rows.sort_by_key(|row| GROUPS.iter().position(|group| *group == row.group));
    rows
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::keymap;
    use crate::settings::Settings;

    #[gpui::test]
    fn shortcut_rows_follow_mode_and_user_overrides(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("keymap.json");
        std::fs::write(
            &user,
            r#"[{ "context": "Workspace", "bindings": { "alt-down": null } }]"#,
        )
        .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            SettingsStore::init(None, Settings::default(), cx);
            assert_eq!(keymap::load(Some(&user), cx), Vec::<String>::new());

            let keys = |cx: &App| -> Vec<String> {
                shortcut_rows(cx).into_iter().map(|row| row.keys).collect()
            };
            let standard = keys(cx);
            assert!(standard.iter().any(|k| k == "alt-up"));
            assert!(
                !standard.iter().any(|k| k == "alt-down"),
                "unbound key listed"
            );
            assert!(
                !standard.iter().any(|k| k == "space f"),
                "helix key in standard mode"
            );

            SettingsStore::update(cx, |s| s.keymap_mode = KeymapMode::Helix).unwrap();
            assert!(keys(cx).iter().any(|k| k == "space f"));
        });
    }
}
