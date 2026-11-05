//! Narrow left rail. Every button dispatches an action, so each one has the
//! same keyboard equivalent shown in its tooltip.

use gpui::Action;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, IntoElement, div, px};

use crate::actions::{FocusChatList, OpenPalette, ShowShortcuts, ToggleAppearance};
use crate::settings::Appearance;

pub const RAIL_WIDTH: f32 = 56.;

pub fn nav_rail(appearance: Appearance, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let (appearance_icon, appearance_label) = match appearance {
        Appearance::System => (IconName::Palette, "Appearance: system"),
        Appearance::Light => (IconName::Sun, "Appearance: light"),
        Appearance::Dark => (IconName::Moon, "Appearance: dark"),
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
        .child(rail_button("nav-chats", IconName::Inbox, "Chats", FocusChatList).selected(true))
        .child(rail_button(
            "nav-palette",
            IconName::Search,
            "Command palette",
            OpenPalette,
        ))
        .child(div().flex_1())
        .child(rail_button(
            "nav-appearance",
            appearance_icon,
            appearance_label,
            ToggleAppearance,
        ))
        .child(rail_button(
            "nav-shortcuts",
            IconName::Info,
            "Keyboard shortcuts",
            ShowShortcuts,
        ))
}

fn rail_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    action: impl Action,
) -> Button {
    Button::new(id)
        .icon(icon)
        .ghost()
        .tooltip_with_action(label, &action, Some("Workspace"))
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
}
