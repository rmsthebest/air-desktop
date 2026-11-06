//! The settings screen: one list of everything the commands can change
//! (appearance, themes, keymap, notifications, display name, usernames).
//! Up and down pick a row; left and right (or enter) change it. Text is
//! typed on the command line, prefilled with the command, so the screen
//! and the commands stay one thing. The view only says what was asked for
//! ([`SettingsEvent`]); the workspace does it.

use gpui::{EventEmitter, SharedString};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    Window, div, px,
};

use crate::actions::{
    CloseSettings, SelectFirst, SelectLast, SelectNext, SelectPrev, SettingNext, SettingPrev,
};
use crate::session::Session;
use crate::settings::{Appearance, KeymapMode, Notifications, Settings, SettingsStore};
use crate::theme;
use crate::ui::key_context;

/// What the user asked the screen for.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingsEvent {
    Appearance(Appearance),
    /// Use this theme for its mode (light or dark).
    Theme(String),
    Keymap(KeymapMode),
    Notifications(Notifications),
    /// Type a new display name, starting from this one.
    EditDisplayName(SharedString),
    AddUsername,
    RemoveUsername(SharedString),
    Close,
}

/// One row of the screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Appearance(Appearance),
    LightTheme(String),
    DarkTheme(String),
    Keymap(KeymapMode),
    Notifications(Notifications),
    DisplayName(SharedString),
    Username(SharedString),
    AddUsername,
}

/// What the account adds to the screen; `None` without one.
#[derive(Clone, Default)]
pub struct AccountRows {
    pub display_name: SharedString,
    pub usernames: Vec<SharedString>,
}

/// The rows, in order: the app's settings, then the account's.
pub fn rows(settings: &Settings, account: Option<&AccountRows>) -> Vec<Row> {
    let mut rows = vec![
        Row::Appearance(settings.appearance),
        Row::LightTheme(settings.light_theme.clone()),
        Row::DarkTheme(settings.dark_theme.clone()),
        Row::Keymap(settings.keymap_mode),
        Row::Notifications(settings.notifications),
    ];
    if let Some(account) = account {
        rows.push(Row::DisplayName(account.display_name.clone()));
        rows.extend(account.usernames.iter().cloned().map(Row::Username));
        rows.push(Row::AddUsername);
    }
    rows
}

/// What changing `row` asks for: the next (`forward`) or previous value of
/// a choice, wrapping; the edit for a text row. `themes` is each mode's
/// theme names, sorted.
pub fn change(row: &Row, forward: bool, themes: &Themes) -> Option<SettingsEvent> {
    fn step<T: Clone + PartialEq>(all: &[T], current: &T, forward: bool) -> Option<T> {
        if all.is_empty() {
            return None;
        }
        let at = all.iter().position(|value| value == current);
        let next = match (at, forward) {
            (None, _) => 0,
            (Some(at), true) => (at + 1) % all.len(),
            (Some(at), false) => (at + all.len() - 1) % all.len(),
        };
        Some(all[next].clone())
    }
    use Appearance::*;
    use Notifications::{Full, Off, Private};
    Some(match row {
        Row::Appearance(current) => {
            SettingsEvent::Appearance(step(&[System, Light, Dark], current, forward)?)
        }
        Row::LightTheme(current) => SettingsEvent::Theme(step(&themes.light, current, forward)?),
        Row::DarkTheme(current) => SettingsEvent::Theme(step(&themes.dark, current, forward)?),
        Row::Keymap(current) => SettingsEvent::Keymap(step(
            &[KeymapMode::Standard, KeymapMode::Helix],
            current,
            forward,
        )?),
        Row::Notifications(current) => {
            SettingsEvent::Notifications(step(&[Full, Private, Off], current, forward)?)
        }
        // Text and lists aren't stepped: only the forward key (enter,
        // right) acts on them, so stepping back never starts a removal.
        _ if !forward => return None,
        Row::DisplayName(name) => SettingsEvent::EditDisplayName(name.clone()),
        Row::Username(name) => SettingsEvent::RemoveUsername(name.clone()),
        Row::AddUsername => SettingsEvent::AddUsername,
    })
}

/// Registered theme names by mode, sorted.
#[derive(Default)]
pub struct Themes {
    pub light: Vec<String>,
    pub dark: Vec<String>,
}

pub struct SettingsView {
    focus_handle: FocusHandle,
    selected: usize,
    /// `None` only in tests, on mock data.
    session: Option<Entity<Session>>,
    themes: Themes,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsView {
    pub fn new(session: Option<Entity<Session>>, cx: &mut Context<Self>) -> Self {
        // Settings change from the command line too, and the account's
        // name and usernames from other devices: show them as they are.
        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
            .detach();
        if let Some(session) = &session {
            cx.observe(session, |_, _, cx| cx.notify()).detach();
        }
        Self {
            focus_handle: cx.focus_handle(),
            selected: 0,
            session,
            // Themes are read at startup only.
            themes: theme::by_mode(cx),
        }
    }

    fn account(&self, cx: &App) -> Option<AccountRows> {
        let session = self.session.as_ref()?.read(cx);
        Some(AccountRows {
            display_name: session.display_name().clone(),
            usernames: session.usernames().to_vec(),
        })
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        rows(SettingsStore::get(cx), self.account(cx).as_ref())
    }

    #[cfg(test)]
    pub fn selected_row(&self, cx: &App) -> Option<Row> {
        self.rows(cx).get(self.selected).cloned()
    }

    fn select(&mut self, ix: isize, cx: &mut Context<Self>) {
        let last = self.rows(cx).len().saturating_sub(1);
        self.selected = ix.clamp(0, last as isize) as usize;
        cx.notify();
    }

    fn change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let rows = self.rows(cx);
        let Some(row) = rows.get(self.selected.min(rows.len().saturating_sub(1))) else {
            return;
        };
        if let Some(event) = change(row, forward, &self.themes) {
            cx.emit(event);
        }
    }

    fn render_row(&self, ix: usize, row: &Row, focused: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (label, value, hint) = match row {
            Row::Appearance(appearance) => (
                "Appearance",
                match appearance {
                    Appearance::System => "System",
                    Appearance::Light => "Light",
                    Appearance::Dark => "Dark",
                }
                .into(),
                "← →",
            ),
            Row::LightTheme(name) => ("Light theme", name.clone(), "← →"),
            Row::DarkTheme(name) => ("Dark theme", name.clone(), "← →"),
            Row::Keymap(mode) => (
                "Keymap",
                match mode {
                    KeymapMode::Standard => "Standard",
                    KeymapMode::Helix => "Helix (modal)",
                }
                .into(),
                "← →",
            ),
            Row::Notifications(level) => (
                "Notifications",
                match level {
                    Notifications::Full => "Who wrote and what",
                    Notifications::Private => "Private (no names or text)",
                    Notifications::Off => "Off",
                }
                .into(),
                "← →",
            ),
            Row::DisplayName(name) => ("Display name", name.to_string(), "enter to change"),
            Row::Username(name) => ("Username", name.to_string(), "enter to remove"),
            Row::AddUsername => ("", "Add a username…".into(), "enter"),
        };
        let selected = ix == self.selected;
        h_flex()
            .id(("setting", ix))
            .px_3()
            .py_2()
            .gap_4()
            .rounded(theme.radius)
            .when(selected, |row| {
                row.bg(if focused {
                    theme.list_active
                } else {
                    theme.list_hover
                })
            })
            .child(
                div()
                    .w(px(140.))
                    .flex_none()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().truncate().child(value))
            .when(selected, |row| {
                row.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(hint),
                )
            })
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        // A removed username can leave the selection past the end.
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        let focused = self.focus_handle.is_focused(window);
        let theme = cx.theme();
        let section = |title: &'static str| {
            div()
                .pt_4()
                .pb_1()
                .px_3()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .child(title)
        };
        let mut list = v_flex().gap_0p5().child(section("APP"));
        for (ix, row) in rows.iter().enumerate() {
            if matches!(row, Row::DisplayName(_)) {
                list = list.child(section("ACCOUNT"));
            }
            list = list.child(self.render_row(ix, row, focused, cx));
        }
        if self.session.is_none() {
            list = list.child(
                div()
                    .px_3()
                    .pt_4()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Sign in to change your name and usernames."),
            );
        }
        v_flex()
            .key_context(key_context("Settings", cx))
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.select(this.selected as isize + 1, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| {
                this.select(this.selected as isize - 1, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select(0, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.select(isize::MAX, cx)))
            .on_action(cx.listener(|this, _: &SettingNext, _, cx| this.change(true, cx)))
            .on_action(cx.listener(|this, _: &SettingPrev, _, cx| this.change(false, cx)))
            .on_action(cx.listener(|_, _: &CloseSettings, _, cx| cx.emit(SettingsEvent::Close)))
            .size_full()
            .bg(theme.background)
            .child(
                h_flex()
                    .flex_none()
                    .h(px(52.))
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Settings")
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme.muted_foreground)
                            .child("esc to close"),
                    ),
            )
            .child(
                div()
                    .id("settings-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .pb_4()
                    .child(div().max_w(px(640.)).child(list)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn themes() -> Themes {
        Themes {
            light: vec!["A Light".into(), "B Light".into()],
            dark: vec!["A Dark".into()],
        }
    }

    #[test]
    fn choices_step_both_ways_and_wrap() {
        let row = Row::Notifications(Notifications::Off);
        assert_eq!(
            change(&row, true, &themes()),
            Some(SettingsEvent::Notifications(Notifications::Full))
        );
        assert_eq!(
            change(&row, false, &themes()),
            Some(SettingsEvent::Notifications(Notifications::Private))
        );
        // A theme steps only through themes of its own mode; a setting
        // naming a theme that isn't there starts from the first.
        let light = Row::LightTheme("B Light".into());
        assert_eq!(
            change(&light, true, &themes()),
            Some(SettingsEvent::Theme("A Light".into()))
        );
        let missing = Row::DarkTheme("Gone".into());
        assert_eq!(
            change(&missing, false, &themes()),
            Some(SettingsEvent::Theme("A Dark".into()))
        );
        assert_eq!(change(&missing, true, &Themes::default()), None);
    }

    #[test]
    fn account_rows_come_only_with_an_account() {
        let settings = Settings::default();
        assert!(
            !rows(&settings, None)
                .iter()
                .any(|row| matches!(row, Row::DisplayName(_) | Row::AddUsername))
        );
        let account = AccountRows {
            display_name: "Ada".into(),
            usernames: vec!["ada-1".into(), "ada-2".into()],
        };
        let rows = rows(&settings, Some(&account));
        let tail: Vec<_> = rows[rows.len() - 4..].to_vec();
        assert_eq!(
            tail,
            [
                Row::DisplayName("Ada".into()),
                Row::Username("ada-1".into()),
                Row::Username("ada-2".into()),
                Row::AddUsername
            ]
        );
        // A username row removes that username, never another, and only
        // on the forward key.
        assert_eq!(
            change(&tail[2], true, &themes()),
            Some(SettingsEvent::RemoveUsername("ada-2".into()))
        );
        assert_eq!(change(&tail[2], false, &themes()), None);
    }
}
