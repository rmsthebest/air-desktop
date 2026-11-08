// GPUI's derive macros emit `gpui::` paths; gpui-kit documents this alias.
extern crate gpui_kit as gpui;

mod about;
mod actions;
mod command_line;
mod emoji;
mod keymap;
mod logs;
mod markdown;
mod model;
mod notifier;
mod paths;
mod runtime;
mod session;
mod settings;
mod theme;
mod ui;
mod username;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

use crate::settings::SettingsStore;
use crate::ui::AppRoot;

/// Air's core logs user, chat and attachment IDs at `info`; keep dependencies
/// at `warn` so identifiers don't end up in the journal by default.
/// Override with `RUST_LOG` when debugging.
const DEFAULT_LOG_FILTER: &str = "warn,air_desktop=info";

// A release with the `seam` test feature would ship the core in test mode.
#[cfg(all(feature = "seam", not(debug_assertions)))]
compile_error!("the `seam` feature is for tests only; build releases without it");

fn main() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| DEFAULT_LOG_FILTER.into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(|| logs::Writer),
        )
        .init();

    // The `seam` feature (tests only) puts the core in test mode: fewer
    // connection packages, shorter link codes, faster retries. Never run
    // a binary built with it.
    if cfg!(feature = "seam") {
        eprintln!("air-desktop was built with the `seam` test feature and won't run");
        std::process::exit(1);
    }

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
                cx.new(|cx| AppRoot::new(paths::data_dir(), window, cx))
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
