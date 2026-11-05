//! Command palette (jump to chat, run a command, pick a theme) and the
//! keyboard shortcuts overview.
//!
//! The palette lives in a dialog layer outside the workspace's element tree,
//! so actions dispatched from inside it would never reach the workspace's
//! handlers. Items therefore carry no action of their own: on confirm the
//! dialog closes first, which returns focus to where it was, and the entry
//! is dispatched from there.

use std::rc::Rc;

use gpui::{Action, AnyElement, AppContext as _, SharedString, WeakEntity, Window};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, IntoElement, div, px};

use crate::actions::{self, FocusChatList, OpenChatAt, SelectNext, SelectPrev};
use crate::model::{ChatId, ChatSummary};
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
pub fn open_palette(
    chats: Vec<ChatSummary>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let sections = Rc::new(vec![
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
    ]);

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

/// Read-only overview of the bindings, grouped by where they apply.
pub fn show_shortcuts(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, window, cx| {
        let theme = cx.theme();
        let mut rows: Vec<(&'static str, SharedString, Box<dyn Action>, &'static str)> =
            actions::commands()
                .into_iter()
                .map(|cmd| ("Anywhere", cmd.label, cmd.action, cmd.context))
                .collect();
        rows.push((
            "Anywhere",
            "Open chat 1–9".into(),
            Box::new(OpenChatAt { index: 0 }),
            "Workspace",
        ));
        rows.push((
            "Chat list",
            "Next chat".into(),
            Box::new(SelectNext),
            "ChatList",
        ));
        rows.push((
            "Chat list",
            "Previous chat".into(),
            Box::new(SelectPrev),
            "ChatList",
        ));
        rows.push((
            "Conversation",
            "Back to chat list".into(),
            Box::new(FocusChatList),
            "Conversation",
        ));

        let mut list = v_flex().gap_1();
        let mut current_group = "";
        for (group, label, action, context) in rows {
            if group != current_group {
                current_group = group;
                list = list.child(
                    div()
                        .pt_3()
                        .pb_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(group),
                );
            }
            list = list.child(command_row(label, &*action, context, window));
        }
        dialog.title("Keyboard shortcuts").w(px(520.)).child(list)
    });
}
