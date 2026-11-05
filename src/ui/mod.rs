mod app_root;
mod chat_list;
mod command_line;
mod conversation;
mod details_pane;
mod nav_rail;
mod onboarding;
mod palette;
mod widgets;
mod workspace;

pub use app_root::AppRoot;
pub use workspace::Workspace;

use gpui::{App, KeyContext, Window};
use gpui_kit::component::WindowExt as _;
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
