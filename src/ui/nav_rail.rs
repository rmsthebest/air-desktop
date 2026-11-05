//! Narrow left rail: primary destinations and global toggles.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClickEvent, IntoElement, Window, px};

pub const RAIL_WIDTH: f32 = 56.;

pub fn nav_rail(
    on_toggle_theme: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let theme_icon = if theme.is_dark() {
        IconName::Sun
    } else {
        IconName::Moon
    };
    v_flex()
        .flex_none()
        .w(px(RAIL_WIDTH))
        .h_full()
        .items_center()
        .gap_1()
        .py_3()
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
        .child(
            Button::new("nav-chats")
                .icon(IconName::Inbox)
                .ghost()
                .selected(true)
                .tooltip("Chats"),
        )
        .child(
            Button::new("nav-contacts")
                .icon(IconName::CircleUser)
                .ghost()
                .tooltip("Contacts"),
        )
        .child(div_spacer())
        .child(
            Button::new("nav-theme")
                .icon(theme_icon)
                .ghost()
                .tooltip("Toggle light/dark")
                .on_click(on_toggle_theme),
        )
        .child(
            Button::new("nav-settings")
                .icon(IconName::Settings)
                .ghost()
                .tooltip("Settings"),
        )
}

fn div_spacer() -> impl IntoElement {
    gpui_kit::div().flex_1()
}
