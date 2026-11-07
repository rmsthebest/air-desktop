//! The settings screen: one list of everything the commands can change
//! (appearance, themes, keymap, notifications, display name, usernames,
//! synced account settings, devices, invite codes, deleting the account).
//! Delete removes the selected username or unlinks the selected device.
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
    SettingRemove,
};
use crate::model::{Device, InviteSlot, SyncedSettings};
use crate::session::Session;
use crate::session::account_settings::AccountSettings;
use crate::session::devices::Devices;
use crate::session::invitations::Invitations;
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
    /// Pick a new picture (file dialog).
    SetPicture,
    /// Remove the user's picture (confirmed first).
    RemovePicture,
    AddUsername,
    RemoveUsername(SharedString),
    /// Type a new name for the device with this tag, starting from this one.
    EditDeviceName {
        tag: SharedString,
        name: SharedString,
    },
    /// Unlink the device with this tag (after confirming).
    UnlinkDevice(SharedString),
    /// Type the code a new device shows.
    LinkDevice,
    ReadReceipts(bool),
    DeleteExpiredRequests(bool),
    /// Copy these invite codes to give them out.
    CopyInviteCodes(Vec<SharedString>),
    /// Trade the token with this id for an invite code.
    GetInviteCode(i64),
    /// Forget the codes given out.
    RemoveUsedCodes,
    /// Start deleting the account (typed and confirmed first).
    DeleteAccount,
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
    /// The user's picture: whether there is one.
    Picture(bool),
    Username(SharedString),
    AddUsername,
    Device(Device),
    /// How many devices are linked, and the server's limit (0: none).
    LinkDevice {
        count: usize,
        max: u32,
    },
    /// `None` until the stored value loads: shown, so rows below keep
    /// their place, but nothing to flip (a default could be written).
    ReadReceipts(Option<bool>),
    DeleteExpiredRequests(Option<bool>),
    InviteCode {
        code: SharedString,
        copied: bool,
    },
    /// A token to trade for a code, by the core's id.
    GetInviteCode(i64),
    /// Every code shown, to copy at once.
    CopyInviteCodes(Vec<SharedString>),
    RemoveUsedCodes,
    DeleteAccount,
}

/// What the account adds to the screen; `None` without one.
#[derive(Clone, Default)]
pub struct AccountRows {
    pub display_name: SharedString,
    pub has_picture: bool,
    pub usernames: Vec<SharedString>,
    pub devices: Vec<Device>,
    pub max_devices: u32,
    /// `None` until loaded: the rows show it, and nothing flips a default.
    pub synced: Option<SyncedSettings>,
    pub invites: Vec<InviteSlot>,
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
        rows.push(Row::Picture(account.has_picture));
        rows.extend(account.usernames.iter().cloned().map(Row::Username));
        rows.push(Row::AddUsername);
        rows.push(Row::ReadReceipts(account.synced.map(|s| s.read_receipts)));
        rows.push(Row::DeleteExpiredRequests(
            account.synced.map(|s| s.delete_expired_requests),
        ));
        rows.extend(account.devices.iter().cloned().map(Row::Device));
        rows.push(Row::LinkDevice {
            count: account.devices.len(),
            max: account.max_devices,
        });
        let mut codes = Vec::new();
        let mut used = false;
        for slot in &account.invites {
            rows.push(match slot {
                InviteSlot::Code { code, copied } => {
                    codes.push(code.clone());
                    used |= copied;
                    Row::InviteCode {
                        code: code.clone(),
                        copied: *copied,
                    }
                }
                InviteSlot::Token(id) => Row::GetInviteCode(*id),
            });
        }
        if codes.len() > 1 {
            rows.push(Row::CopyInviteCodes(codes));
        }
        if used {
            rows.push(Row::RemoveUsedCodes);
        }
        rows.push(Row::DeleteAccount);
    }
    rows
}

/// What removing `row` (the Delete key) asks for: a username's removal, or
/// unlinking a device other than this one.
pub fn remove(row: &Row) -> Option<SettingsEvent> {
    match row {
        Row::Username(name) => Some(SettingsEvent::RemoveUsername(name.clone())),
        Row::Picture(true) => Some(SettingsEvent::RemovePicture),
        Row::Device(device) if !device.this_device => {
            Some(SettingsEvent::UnlinkDevice(device.tag.clone()))
        }
        _ => None,
    }
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
        // On or off: either way flips it.
        Row::ReadReceipts(on) => SettingsEvent::ReadReceipts(!(*on)?),
        Row::DeleteExpiredRequests(on) => SettingsEvent::DeleteExpiredRequests(!(*on)?),
        // Text and lists aren't stepped: only the forward key (enter,
        // right) acts on them, so stepping back never starts a removal.
        _ if !forward => return None,
        Row::DisplayName(name) => SettingsEvent::EditDisplayName(name.clone()),
        Row::Picture(_) => SettingsEvent::SetPicture,
        Row::Username(name) => SettingsEvent::RemoveUsername(name.clone()),
        Row::AddUsername => SettingsEvent::AddUsername,
        Row::Device(device) => SettingsEvent::EditDeviceName {
            tag: device.tag.clone(),
            name: device.name.clone(),
        },
        Row::LinkDevice { .. } => SettingsEvent::LinkDevice,
        Row::InviteCode { code, .. } => SettingsEvent::CopyInviteCodes(vec![code.clone()]),
        Row::GetInviteCode(id) => SettingsEvent::GetInviteCode(*id),
        Row::CopyInviteCodes(codes) => SettingsEvent::CopyInviteCodes(codes.clone()),
        Row::RemoveUsedCodes => SettingsEvent::RemoveUsedCodes,
        Row::DeleteAccount => SettingsEvent::DeleteAccount,
    })
}

fn on_off(on: Option<bool>) -> String {
    match on {
        Some(true) => "On",
        Some(false) => "Off",
        None => "…",
    }
    .into()
}

fn on_off_expired(delete: Option<bool>) -> String {
    match delete {
        Some(true) => "Deleted",
        Some(false) => "Kept",
        None => "…",
    }
    .into()
}

/// Whether `a` and `b` are the same row, whatever value it shows.
fn same_row(a: &Row, b: &Row) -> bool {
    match (a, b) {
        (Row::Username(a), Row::Username(b)) => a == b,
        (Row::Device(a), Row::Device(b)) => a.tag == b.tag,
        (Row::InviteCode { code: a, .. }, Row::InviteCode { code: b, .. }) => a == b,
        (Row::GetInviteCode(a), Row::GetInviteCode(b)) => a == b,
        _ => std::mem::discriminant(a) == std::mem::discriminant(b),
    }
}

/// Where the selection is in `rows`: on `anchor` wherever it moved, else
/// (it's gone) at `selected`, clamped.
fn reselect(rows: &[Row], selected: usize, anchor: Option<&Row>) -> usize {
    let last = rows.len().saturating_sub(1);
    let Some(anchor) = anchor else {
        return selected.min(last);
    };
    if rows.get(selected).is_some_and(|row| same_row(row, anchor)) {
        return selected;
    }
    rows.iter()
        .position(|row| same_row(row, anchor))
        .unwrap_or(selected.min(last))
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
    /// The row last selected: rows load late (devices, invite codes) and
    /// would move a selection kept by position onto another row, one
    /// that may act at once (spend an invite token).
    anchor: Option<Row>,
    /// `None` only in tests, on mock data.
    session: Option<Entity<Session>>,
    account: Option<AccountEntities>,
    themes: Themes,
}

/// What the account's rows come from.
#[derive(Clone)]
pub struct AccountEntities {
    pub devices: Entity<Devices>,
    pub settings: Entity<AccountSettings>,
    pub invitations: Entity<Invitations>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsView {
    pub fn new(
        session: Option<Entity<Session>>,
        account: Option<AccountEntities>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Settings change from the command line too, and the account's
        // name and usernames from other devices: show them as they are.
        cx.observe_global::<SettingsStore>(|_, cx| cx.notify())
            .detach();
        if let Some(session) = &session {
            cx.observe(session, |_, _, cx| cx.notify()).detach();
        }
        if let Some(account) = &account {
            cx.observe(&account.devices, |_, _, cx| cx.notify())
                .detach();
            cx.observe(&account.settings, |_, _, cx| cx.notify())
                .detach();
            cx.observe(&account.invitations, |_, _, cx| cx.notify())
                .detach();
        }
        Self {
            focus_handle: cx.focus_handle(),
            selected: 0,
            anchor: None,
            session,
            account,
            // Themes are read at startup only.
            themes: theme::by_mode(cx),
        }
    }

    fn account(&self, cx: &App) -> Option<AccountRows> {
        let session = self.session.as_ref()?.read(cx);
        let account = self.account.as_ref();
        let devices = account.map(|account| account.devices.read(cx));
        Some(AccountRows {
            display_name: session.display_name().clone(),
            has_picture: session.picture().is_some(),
            usernames: session.usernames().to_vec(),
            devices: devices.map(|d| d.devices().to_vec()).unwrap_or_default(),
            max_devices: devices.map_or(0, Devices::max_devices),
            synced: account.and_then(|account| account.settings.read(cx).settings()),
            invites: account
                .map(|account| account.invitations.read(cx).slots().to_vec())
                .unwrap_or_default(),
        })
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        rows(SettingsStore::get(cx), self.account(cx).as_ref())
    }

    #[cfg(test)]
    pub fn selected_row(&self, cx: &App) -> Option<Row> {
        self.rows(cx).get(self.selected).cloned()
    }

    /// The rows, with the selection back on the anchored row.
    fn current_rows(&mut self, cx: &App) -> Vec<Row> {
        let rows = self.rows(cx);
        self.selected = reselect(&rows, self.selected, self.anchor.as_ref());
        self.anchor = rows.get(self.selected).cloned();
        rows
    }

    /// Moves the selection by `by` rows (clamped).
    fn select(&mut self, by: isize, cx: &mut Context<Self>) {
        let rows = self.current_rows(cx);
        let last = rows.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize).saturating_add(by).clamp(0, last) as usize;
        self.anchor = rows.get(self.selected).cloned();
        cx.notify();
    }

    fn change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let rows = self.current_rows(cx);
        let Some(row) = rows.get(self.selected) else {
            return;
        };
        if let Some(event) = change(row, forward, &self.themes) {
            cx.emit(event);
        }
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let rows = self.current_rows(cx);
        if let Some(event) = rows.get(self.selected).and_then(remove) {
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
            Row::Picture(true) => ("Picture", "Set".into(), "enter to change, delete removes"),
            Row::Picture(false) => ("Picture", "None".into(), "enter to pick one"),
            Row::Username(name) => ("Username", name.to_string(), "enter or delete to remove"),
            Row::AddUsername => ("", "Add a username…".into(), "enter"),
            Row::Device(device) => {
                let mut value = format!("{}  #{}", device.name, device.tag);
                if !device.platform.is_empty() {
                    value.push_str(&format!(" · {}", device.platform));
                }
                if let Some(at) = device.linked_at {
                    value.push_str(&format!(" · linked {}", at.format("%Y-%m-%d")));
                }
                if device.this_device {
                    ("This device", value, "enter to rename")
                } else {
                    ("Device", value, "enter to rename, delete to unlink")
                }
            }
            Row::LinkDevice { count, max } => (
                "",
                match max {
                    0 => format!("Link a device… ({count} linked)"),
                    max => format!("Link a device… ({count} of {max} linked)"),
                },
                "enter",
            ),
            Row::ReadReceipts(on) => ("Read receipts", on_off(*on), "← →"),
            Row::DeleteExpiredRequests(on) => ("Expired requests", on_off_expired(*on), "← →"),
            Row::InviteCode { code, copied } => (
                "Invite code",
                if *copied {
                    format!("{code} (given out)")
                } else {
                    code.to_string()
                },
                "enter to copy",
            ),
            Row::GetInviteCode(_) => ("", "Get an invite code…".into(), "enter"),
            Row::CopyInviteCodes(codes) => ("", format!("Copy all {} codes", codes.len()), "enter"),
            Row::RemoveUsedCodes => ("", "Remove codes given out".into(), "enter"),
            Row::DeleteAccount => ("", "Delete your account…".into(), "enter"),
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
        let rows = self.current_rows(cx);
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
            if matches!(row, Row::Device(device) if device.this_device) {
                list = list.child(section("DEVICES"));
            }
            let first_invite =
                |row: &Row| matches!(row, Row::InviteCode { .. } | Row::GetInviteCode(_));
            if first_invite(row) && !rows[..ix].iter().any(first_invite) {
                list = list.child(section("INVITE CODES"));
            }
            if matches!(row, Row::DeleteAccount) {
                list = list.child(section("DANGER"));
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
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.select(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select(isize::MIN, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.select(isize::MAX, cx)))
            .on_action(cx.listener(|this, _: &SettingNext, _, cx| this.change(true, cx)))
            .on_action(cx.listener(|this, _: &SettingPrev, _, cx| this.change(false, cx)))
            .on_action(cx.listener(|this, _: &SettingRemove, _, cx| this.remove(cx)))
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
            ..AccountRows::default()
        };
        let rows = rows(&settings, Some(&account));
        let at = rows
            .iter()
            .position(|row| matches!(row, Row::DisplayName(_)))
            .unwrap();
        let tail: Vec<_> = rows[at..at + 5].to_vec();
        assert_eq!(
            tail,
            [
                Row::DisplayName("Ada".into()),
                Row::Picture(false),
                Row::Username("ada-1".into()),
                Row::Username("ada-2".into()),
                Row::AddUsername
            ]
        );
        // A username row removes that username, never another, and only
        // on the forward key.
        assert_eq!(
            change(&tail[3], true, &themes()),
            Some(SettingsEvent::RemoveUsername("ada-2".into()))
        );
        for row in &tail {
            assert_eq!(change(row, false, &themes()), None, "{row:?}");
        }
        assert_eq!(
            remove(&tail[2]),
            Some(SettingsEvent::RemoveUsername("ada-1".into()))
        );
        assert_eq!(remove(&tail[0]), None);
        // The picture: enter picks one; delete removes one there is.
        assert_eq!(
            change(&tail[1], true, &themes()),
            Some(SettingsEvent::SetPicture)
        );
        assert_eq!(remove(&tail[1]), None);
        assert_eq!(
            remove(&Row::Picture(true)),
            Some(SettingsEvent::RemovePicture)
        );
    }

    #[test]
    fn devices_are_renamed_by_tag_and_only_others_unlinked() {
        let device = |name: &str, tag: &str, this_device| Device {
            name: name.to_string().into(),
            platform: "Linux".into(),
            linked_at: None,
            this_device,
            tag: tag.to_string().into(),
        };
        let account = AccountRows {
            display_name: "Ada".into(),
            devices: vec![
                device("Linux", "1a2b", true),
                device("Linux", "9f00", false),
            ],
            max_devices: 3,
            ..AccountRows::default()
        };
        let rows = rows(&Settings::default(), Some(&account));
        let at = rows
            .iter()
            .position(|row| matches!(row, Row::Device(_)))
            .unwrap();
        let tail: Vec<_> = rows[at..at + 3].to_vec();
        assert_eq!(tail[2], Row::LinkDevice { count: 2, max: 3 });
        // Alike names: what is renamed or unlinked is the tag's device.
        assert_eq!(
            change(&tail[1], true, &themes()),
            Some(SettingsEvent::EditDeviceName {
                tag: "9f00".into(),
                name: "Linux".into()
            })
        );
        assert_eq!(
            remove(&tail[1]),
            Some(SettingsEvent::UnlinkDevice("9f00".into()))
        );
        // This device can't unlink itself; enter never unlinks.
        assert_eq!(remove(&tail[0]), None);
        for row in &tail {
            assert_eq!(change(row, false, &themes()), None, "{row:?}");
        }
        assert_eq!(
            change(&tail[2], true, &themes()),
            Some(SettingsEvent::LinkDevice)
        );
    }

    #[test]
    fn the_selection_stays_on_its_row_when_rows_load() {
        let account = |invites: Vec<InviteSlot>| AccountRows {
            display_name: "Ada".into(),
            invites,
            ..AccountRows::default()
        };
        let before = rows(&Settings::default(), Some(&account(Vec::new())));
        let last = before.len() - 1;
        assert_eq!(before[last], Row::DeleteAccount);
        // A token loads above Delete account: by position the selection
        // would be on it, and enter would spend it unasked.
        let after = rows(
            &Settings::default(),
            Some(&account(vec![InviteSlot::Token(7)])),
        );
        assert_eq!(after[last], Row::GetInviteCode(7));
        let ix = reselect(&after, last, before.get(last));
        assert_eq!(after[ix], Row::DeleteAccount);
        // A row whose value changed is the same row.
        let flipped = [Row::ReadReceipts(Some(true))];
        assert_eq!(reselect(&flipped, 0, Some(&Row::ReadReceipts(None))), 0);
        // A row that's gone leaves the selection where it was, clamped.
        assert_eq!(reselect(&before, 99, Some(&Row::GetInviteCode(7))), last);
    }

    #[test]
    fn synced_settings_flip_and_codes_are_given_out_one_or_all() {
        let account = |invites: Vec<InviteSlot>| AccountRows {
            display_name: "Ada".into(),
            invites,
            synced: Some(SyncedSettings {
                read_receipts: false,
                delete_expired_requests: false,
            }),
            ..AccountRows::default()
        };
        // Until the stored settings load, there is nothing to flip: a
        // flip from the default could write the opposite of what is meant.
        // The rows are there all the same, so the load moves no row.
        let unloaded = AccountRows {
            synced: None,
            ..account(Vec::new())
        };
        let before = rows(&Settings::default(), Some(&unloaded));
        let all = rows(&Settings::default(), Some(&account(Vec::new())));
        assert_eq!(before.len(), all.len());
        for row in &before {
            if matches!(row, Row::ReadReceipts(_) | Row::DeleteExpiredRequests(_)) {
                assert_eq!(change(row, true, &themes()), None);
                assert_eq!(change(row, false, &themes()), None);
            }
        }
        // Shown as stored, and flipped from it.
        let receipts = all
            .iter()
            .find(|row| matches!(row, Row::ReadReceipts(_)))
            .unwrap();
        assert_eq!(
            change(receipts, true, &themes()),
            Some(SettingsEvent::ReadReceipts(true))
        );
        // Deleting the account is the last row, and only enter starts it
        // (it asks for the word, then confirms).
        assert_eq!(all.last(), Some(&Row::DeleteAccount));
        assert_eq!(change(&Row::DeleteAccount, false, &themes()), None);
        assert_eq!(remove(&Row::DeleteAccount), None);

        let code = |code: &str, copied| InviteSlot::Code {
            code: code.to_string().into(),
            copied,
        };
        // One fresh code and a token: no copy-all, nothing to clear.
        let one = rows(
            &Settings::default(),
            Some(&account(vec![
                code("AAAA1111", false),
                InviteSlot::Token(7),
            ])),
        );
        let invites: Vec<_> = one
            .iter()
            .filter(|row| {
                matches!(
                    row,
                    Row::InviteCode { .. }
                        | Row::GetInviteCode(_)
                        | Row::CopyInviteCodes(_)
                        | Row::RemoveUsedCodes
                )
            })
            .cloned()
            .collect();
        assert_eq!(invites.len(), 2);
        assert_eq!(
            change(&invites[0], true, &themes()),
            Some(SettingsEvent::CopyInviteCodes(vec!["AAAA1111".into()]))
        );
        assert_eq!(
            change(&invites[1], true, &themes()),
            Some(SettingsEvent::GetInviteCode(7))
        );
        assert_eq!(change(&invites[1], false, &themes()), None);
        // Two codes, one given out: copy all of them, clear the used.
        let two = rows(
            &Settings::default(),
            Some(&account(vec![
                code("AAAA1111", true),
                code("BBBB2222", false),
            ])),
        );
        assert!(two.contains(&Row::CopyInviteCodes(vec![
            "AAAA1111".into(),
            "BBBB2222".into()
        ])));
        assert!(two.contains(&Row::RemoveUsedCodes));
    }
}
