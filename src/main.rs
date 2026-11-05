#[cfg(test)]
extern crate gpui_kit as gpui;

mod model;
mod runtime;
mod theme;
mod ui;

use gpui_kit::{AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

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
            if let Err(err) = runtime::init(cx).and_then(|()| theme::init(cx)) {
                tracing::error!("startup failed: {err:#}");
                cx.quit();
                return;
            }

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
            if let Err(err) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Workspace::new(window, cx))
            }) {
                tracing::error!("failed to open window: {err:#}");
                cx.quit();
            }
        });
}
