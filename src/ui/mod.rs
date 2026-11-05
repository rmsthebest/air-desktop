mod chat_list;
mod conversation;
mod details_pane;
mod nav_rail;
mod palette;
mod widgets;
mod workspace;

pub use workspace::Workspace;

use gpui::{App, Window};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;

use crate::settings::{Settings, SettingsStore};

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
