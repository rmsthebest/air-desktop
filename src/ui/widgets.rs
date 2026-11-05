//! Small shared building blocks.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{App, Div, Pixels, div, px};

use crate::model::initials;

/// Round avatar placeholder showing the initials of `name`.
pub fn avatar(name: &str, size: Pixels, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded_full()
        .bg(theme.primary.opacity(0.14))
        .text_color(theme.primary)
        .text_size(size * 0.4)
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .child(initials(name))
}

/// Pill-shaped unread counter.
pub fn badge(count: u32, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(20.))
        .h(px(20.))
        .px(px(6.))
        .rounded_full()
        .bg(theme.primary)
        .text_color(theme.primary_foreground)
        .text_xs()
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .child(if count > 99 {
            "99+".to_string()
        } else {
            count.to_string()
        })
}
