//! The emoji picker: every emoji, found by name or shortcode. Used to react
//! with any emoji and to insert one into the composer.
//!
//! Like the palette, it lives in a dialog layer outside the workspace's
//! element tree: on confirm it closes first (focus returns to where it was)
//! and then hands the emoji to `on_pick`.

use std::rc::Rc;

use gpui::{AppContext as _, Window};
use gpui_kit::App;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::prelude::*;
use gpui_kit::px;

use crate::emoji;

/// What to do with the emoji picked.
pub type OnPick = Rc<dyn Fn(&'static str, &mut Window, &mut App)>;

pub fn open(title: &'static str, on_pick: OnPick, window: &mut Window, cx: &mut App) {
    let entries: Rc<Vec<emoji::Entry>> = Rc::new(emoji::all());
    // Made once: the dialog is rebuilt on every render.
    let items: Rc<Vec<CommandItem>> = Rc::new(entries.iter().map(item).collect());
    let state = cx.new(|cx| CommandState::new(window, cx));
    let picker_state = state.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(480.))
            .child(build(&picker_state, title, &entries, &items, &on_pick))
    });
    state.update(cx, |state, cx| state.focus(window, cx));
}

fn item(entry: &emoji::Entry) -> CommandItem {
    let label = match entry.shortcodes.first() {
        Some(code) => format!("{}  {}  :{code}:", entry.emoji, entry.name),
        None => format!("{}  {}", entry.emoji, entry.name),
    };
    CommandItem::new()
        .label(label)
        .keywords(entry.shortcodes.iter().copied())
}

fn build(
    state: &gpui::Entity<CommandState>,
    title: &'static str,
    entries: &Rc<Vec<emoji::Entry>>,
    items: &Rc<Vec<CommandItem>>,
    on_pick: &OnPick,
) -> Command {
    let items = items.iter().cloned();
    let entries = entries.clone();
    let on_pick = on_pick.clone();
    Command::new(state)
        .placeholder("Find an emoji by name…")
        .group(CommandGroup::new().label(title).items(items))
        .on_confirm(move |path, window, cx| {
            let Some(entry) = entries.get(path.row) else {
                return;
            };
            window.close_dialog(cx);
            on_pick(entry.emoji, window, cx);
        })
}
