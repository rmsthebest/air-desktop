mod app_root;
mod chat_list;
mod command_line;
mod conversation;
mod details_pane;
mod emoji_picker;
mod link_view;
mod markdown_view;
mod nav_rail;
mod onboarding;
mod palette;
mod qr;
mod settings_view;
mod widgets;
mod workspace;

pub use app_root::AppRoot;
pub use settings_view::Themes;
pub use workspace::Workspace;

use gpui::{App, KeyContext, Window};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::notification::Notification;

use crate::settings::{Settings, SettingsStore};

/// Key context for a pane, with the `helix` identifier added in Helix mode
/// so the Helix keymap layer applies (see `keymap.rs`).
pub fn key_context(name: &str, cx: &App) -> KeyContext {
    let mut context = KeyContext::default();
    context.add(name.to_string());
    if SettingsStore::get(cx).keymap_mode == crate::settings::KeymapMode::Helix {
        context.add("helix");
    }
    context
}

/// Changes settings from the UI, telling the user if they couldn't be saved.
pub fn update_settings(window: &mut Window, cx: &mut App, edit: impl FnOnce(&mut Settings)) {
    if let Err(err) = SettingsStore::update(cx, edit) {
        show_error(format!("{err:#}"), window, cx);
    }
}

pub fn show_error(message: String, window: &mut Window, cx: &mut App) {
    tracing::warn!("{message}");
    window.push_notification(Notification::error(message), cx);
}

/// Tells the user why something they asked for didn't happen, without
/// logging it: the message may name a contact, and the session logged any
/// internal cause where it happened.
pub fn show_refusal(message: String, window: &mut Window, cx: &mut App) {
    #[cfg(test)]
    TOLD.with(|told| told.borrow_mut().push(message.clone()));
    window.push_notification(Notification::error(message), cx);
}

pub fn show_info(message: String, window: &mut Window, cx: &mut App) {
    #[cfg(test)]
    TOLD.with(|told| told.borrow_mut().push(message.clone()));
    window.push_notification(Notification::info(message), cx);
}

#[cfg(test)]
thread_local! {
    /// What tests' windows were told, in order (toasts don't show their
    /// text to tests).
    static TOLD: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The latest refusal or info shown on this thread since the last take
/// (tests only): taking it clears the record, so no check passes on what
/// an earlier step was told.
#[cfg(test)]
pub fn last_told() -> Option<String> {
    take_told().pop()
}

/// Everything shown on this thread since the last take, in order (tests
/// only).
#[cfg(test)]
pub fn take_told() -> Vec<String> {
    TOLD.with(|told| std::mem::take(&mut *told.borrow_mut()))
}

/// Asks before running `run`: `ok` confirms (looking like `variant`),
/// Escape or Cancel doesn't. The dialog closes and focus goes back where
/// it was before `run` (which may report): gpui-component's OK path
/// doesn't restore it.
pub fn confirm(
    title: String,
    description: String,
    ok: &'static str,
    variant: ButtonVariant,
    run: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let run = std::rc::Rc::new(run);
    let previous_focus = window.focused(cx);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (run, previous_focus) = (run.clone(), previous_focus.clone());
        alert
            .confirm()
            .title(title.clone())
            .description(description.clone())
            .ok_text(ok)
            .ok_variant(variant)
            .on_ok(move |_, window, cx| {
                window.close_dialog(cx);
                if let Some(focus) = &previous_focus {
                    focus.focus(window, cx);
                }
                run(window, cx);
                false
            })
    });
}
