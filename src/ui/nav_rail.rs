//! Narrow left rail. Every button dispatches an action, so each one has the
//! same keyboard equivalent shown in its tooltip.

use gpui::{Action, SharedString};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, IntoElement, div, px};

use crate::actions::{FocusChatList, OpenPalette, OpenSettings, ShowShortcuts, ToggleAppearance};
use crate::session::ConnectionStatus;
use crate::settings::Appearance;
use crate::ui::widgets::avatar;

pub const RAIL_WIDTH: f32 = 56.;

/// The signed-in account, shown at the bottom of the rail.
pub struct Account {
    pub name: SharedString,
    pub status: ConnectionStatus,
    /// When the server stops accepting this client version, if announced.
    pub version_expires: Option<chrono::DateTime<chrono::Utc>>,
    /// What others can add the user by.
    pub usernames: Vec<SharedString>,
}

pub fn nav_rail(appearance: Appearance, account: Option<Account>, cx: &App) -> impl IntoElement {
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
            "nav-settings",
            IconName::Settings,
            "Settings",
            OpenSettings,
        ))
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
        .when_some(account, |rail, account| {
            rail.child(account_badge(account, cx))
        })
}

/// Avatar with a connection dot; the tooltip spells the status out.
fn account_badge(account: Account, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (color, status) = match account.status {
        ConnectionStatus::Connected => (theme.success, "connected"),
        ConnectionStatus::Connecting => (theme.warning, "connecting…"),
        ConnectionStatus::Offline => (theme.danger, "offline, retrying"),
        ConnectionStatus::UnsupportedVersion => (
            theme.danger,
            "this version is no longer supported; please update",
        ),
    };
    let (color, tooltip) = match account.version_expires {
        Some(at) if account.status == ConnectionStatus::Connected => (
            theme.warning,
            format!(
                "{} — connected; this version stops working on {}, please update",
                account.name,
                at.with_timezone(&chrono::Local).format("%Y-%m-%d")
            ),
        ),
        _ => (color, format!("{} — {status}", account.name)),
    };
    let usernames = if account.usernames.is_empty() {
        "no username yet (:username <name> adds one)".to_string()
    } else {
        account.usernames.join(", ")
    };
    let tooltip = SharedString::from(format!("{tooltip}\n{usernames}"));
    div()
        .id("nav-account")
        .relative()
        .mt_2()
        .child(avatar(&account.name, px(32.), cx))
        .child(
            div()
                .absolute()
                .right_0()
                .bottom_0()
                .size(px(10.))
                .rounded_full()
                .border_2()
                .border_color(theme.sidebar)
                .bg(color),
        )
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .into_any_element()
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
