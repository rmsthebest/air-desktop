// GPUI's derive macros emit `gpui::` paths; gpui-kit documents this alias.
extern crate gpui_kit as gpui;

mod actions;
mod command_line;
mod keymap;
mod model;
mod paths;
mod runtime;
mod settings;
mod theme;
mod ui;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

use crate::settings::SettingsStore;
use crate::ui::Workspace;

/// Air's core logs user, chat and attachment IDs at `info`; keep dependencies
/// at `warn` so identifiers don't end up in the journal by default.
/// Override with `RUST_LOG` when debugging.
const DEFAULT_LOG_FILTER: &str = "warn,air_desktop=info";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| DEFAULT_LOG_FILTER.into()),
        )
        .init();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            if let Err(err) = runtime::init(cx) {
                tracing::error!("startup failed: {err:#}");
                cx.quit();
                return;
            }
            // Problems with user config are reported in the window, never fatal.
            let warnings = match load_config(cx) {
                Ok(warnings) => warnings,
                Err(err) => {
                    tracing::error!("startup failed: {err:#}");
                    cx.quit();
                    return;
                }
            };
            cx.on_action(|_: &actions::Quit, cx| cx.quit());

            let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(720.), px(480.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Air".into()),
                    ..Default::default()
                }),
                app_id: Some("air-desktop".into()),
                ..Default::default()
            };
            let window = match gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Workspace::new(window, cx))
            }) {
                Ok((window, _)) => window,
                Err(err) => {
                    tracing::error!("failed to open window: {err:#}");
                    cx.quit();
                    return;
                }
            };
            for warning in warnings {
                tracing::warn!("{warning}");
                _ = window.update(cx, |_, window, cx| {
                    window.push_notification(Notification::warning(warning), cx);
                });
            }
        });
}

/// Loads settings, keymap and themes from the config dir. Only a broken
/// bundled asset is an error; user-file problems come back as warnings.
fn load_config(cx: &mut gpui::App) -> anyhow::Result<Vec<String>> {
    let config_dir = paths::config_dir();
    let mut warnings = Vec::new();

    let settings_path = config_dir.as_ref().map(|dir| dir.join("settings.json"));
    warnings.extend(SettingsStore::open(settings_path, cx));

    let keymap_path = config_dir.as_ref().map(|dir| dir.join("keymap.json"));
    warnings.extend(keymap::load(keymap_path.as_deref(), cx));

    let themes_dir = config_dir.as_ref().map(|dir| dir.join("themes"));
    warnings.extend(theme::init(themes_dir.as_deref(), cx)?);
    Ok(warnings)
}
