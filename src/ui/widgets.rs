//! Small shared building blocks.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::*;
use gpui_kit::{App, Div, ImageSource, ObjectFit, Pixels, div, img, px};

use crate::model::{Picture, initials};

/// Round avatar: the picture if there is one, else the initials of `name`.
pub fn avatar(name: &str, picture: Option<&Picture>, size: Pixels, cx: &App) -> Div {
    let theme = cx.theme();
    if let Some(picture) = picture {
        return div()
            .flex_none()
            .size(size)
            .rounded_full()
            .overflow_hidden()
            .bg(theme.primary.opacity(0.14))
            .child(
                img(ImageSource::Render(picture.0.clone()))
                    .size(size)
                    .object_fit(ObjectFit::Cover),
            );
    }
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
