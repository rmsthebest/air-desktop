//! The user's devices. Ports of upstream's `LinkedDevicesCubit` (the list:
//! the self group's members decide which devices are linked, the synced
//! `LinkedDevicesSetting` names them), its rename and unlink, and of
//! `multi_device.rs`: linking a new device from this one (the existing
//! device's side) and linking this device into an existing account (the new
//! device's side, at onboarding).
//!
//! Also how this device learns it was unlinked (or the account deleted)
//! and tears itself down, as upstream's `UnlinkedDeviceHandler`.

use aircommon::crypto::mdl::code::{LinkingCode, LinkingCodeError, MIN_CODE_DIGITS};
use aircommon::identifiers::Fqdn;
use aircoreclient::clients::CoreUser;
use aircoreclient::clients::multi_device::{MultiDeviceLinkClientError, MultiDeviceProvisionStep};
use aircoreclient::clients::store::ClientRecord;
use aircoreclient::db::notification::{DbEntityId, DbNotification};
use aircoreclient::{ChatId as CoreChatId, LinkedDevicesSetting, UnlinkReason, UserSetting as _};
use chrono::{TimeZone as _, Utc};
use gpui::SharedString;
use gpui_kit::{App, Context, Entity, Subscription, Task};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::{Stream, StreamExt as _};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::contacts::{Failure, on_tokio};
use super::groups::as_tag;
use super::{Session, SessionEvent};
use crate::command_line::{NameMatch, matches_name};
use crate::model::{Device, name_line, unique_prefix_len};
use crate::runtime::Tokio;

/// Fewest hex digits of a device's tag. The ids are random and few, so
/// four almost always tell them apart; tags grow when they don't.
const TAG_LEN: usize = 4;

/// Longest device name accepted, in characters (it is shown as one bounded
/// line anyway).
const MAX_DEVICE_NAME: usize = 64;

/// As upstream: what a new device's QR code carries for a phone to scan.
const LINKING_URL_SCHEME: &str = "air";
const LINKING_URL_PATH: &str = "multiDeviceLinkingCode";

/// One stored device entry, as the join needs it.
#[derive(Debug, Clone)]
struct Entry {
    client_id: Uuid,
    name: String,
    platform: String,
    linked_at: u64,
}

/// The device list: one row per self-group member, named from its entry
/// (upstream's `try_load`). This device first, then oldest link first.
/// Without a self group, this device is the only one.
fn join(own: Uuid, mut members: Vec<Uuid>, entries: &[Entry]) -> Vec<(Device, Uuid)> {
    if members.is_empty() {
        members.push(own);
    }
    members.sort_unstable();
    members.dedup();
    let ids: Vec<String> = members.iter().map(|id| id.simple().to_string()).collect();
    let len = unique_prefix_len(&ids, TAG_LEN);
    let mut devices: Vec<(Device, Uuid)> = members
        .iter()
        .zip(&ids)
        .map(|(&client_id, hex)| {
            let entry = entries.iter().find(|entry| entry.client_id == client_id);
            let name = entry
                .map(|entry| entry.name.trim())
                .filter(|name| !name.is_empty());
            let device = Device {
                name: name.map_or_else(|| "Unknown device".into(), name_line),
                platform: entry
                    .map(|entry| entry.platform.as_str())
                    .filter(|platform| *platform != "Unknown")
                    .map(name_line)
                    .unwrap_or_default(),
                linked_at: entry
                    .and_then(|entry| i64::try_from(entry.linked_at).ok())
                    .and_then(|at| Utc.timestamp_opt(at, 0).single()),
                this_device: client_id == own,
                tag: hex[..len].to_string().into(),
            };
            (device, client_id)
        })
        .collect();
    devices.sort_by_key(|(device, _)| (!device.this_device, device.linked_at));
    devices
}

/// Which device `query` names (its index): by `#tag`, else by name (see
/// [`matches_name`]). `others` leaves this device out.
fn find(devices: &[Device], query: &str, others: bool) -> Result<usize, String> {
    let candidates: Vec<usize> = (0..devices.len())
        .filter(|&ix| !(others && devices[ix].this_device))
        .collect();
    if let Some(tag) = as_tag(query) {
        let tag = tag.to_lowercase();
        let mut tagged = candidates
            .iter()
            .copied()
            .filter(|&ix| devices[ix].tag.as_ref() == tag);
        return match (tagged.next(), tagged.next()) {
            (Some(ix), None) => Ok(ix),
            (None, _) => Err(format!("No device is tagged #{tag}")),
            (Some(_), Some(_)) => Err(format!("Several devices are tagged #{tag}")),
        };
    }
    match matches_name(
        query,
        candidates.iter().map(|&ix| devices[ix].name.as_ref()),
    ) {
        NameMatch::One(name) => candidates
            .iter()
            .copied()
            .find(|&ix| devices[ix].name.as_ref() == name)
            .ok_or_else(|| "That device is gone".to_string()),
        NameMatch::None => Err(format!("No device matches {query:?}")),
        NameMatch::Ambiguous(n) => Err(format!(
            "{n} devices match {query:?}: use the #tag the settings show"
        )),
    }
}

/// A new name for a device, as typed.
fn parse_device_name(name: &str) -> Result<String, Failure> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Failure::Shown("Devices need a name".into()));
    }
    if name.chars().count() > MAX_DEVICE_NAME {
        return Err(Failure::Shown(format!(
            "Device names have at most {MAX_DEVICE_NAME} characters"
        )));
    }
    Ok(name.to_string())
}

/// What `:device-name` was given: an optional `#tag` first (else this
/// device), then the new name.
pub fn split_rename(input: &str) -> (Option<&str>, &str) {
    let input = input.trim();
    match input.split_once(char::is_whitespace) {
        Some((first, rest)) if as_tag(first).is_some() => (Some(first), rest.trim()),
        _ if as_tag(input).is_some() => (Some(input), ""),
        _ => (None, input),
    }
}

/// A device a command resolved, renamed or unlinked once confirmed.
#[derive(Debug, Clone)]
pub struct DeviceRef {
    pub name: SharedString,
    pub tag: SharedString,
    id: Uuid,
}

/// The user's devices, kept up to date.
pub struct Devices {
    user: CoreUser,
    devices: Vec<Device>,
    /// The client id of each device, in the same order.
    ids: Vec<Uuid>,
    /// The server's limit, 0 if none (or not known yet).
    max_devices: u32,
    /// Membership changes come with the self chat.
    self_chat: Option<CoreChatId>,
    loaded: bool,
    /// The last load failed: what is shown may be stale.
    failed: bool,
    /// The latest load; a newer one replaces (cancels) it.
    load: Option<Task<()>>,
    /// Looking up the self chat while it is unknown.
    probe: Option<Task<()>>,
    /// A chat changed during the lookup: look again once it is done.
    reprobe: bool,
    _changes: Subscription,
}

pub(super) struct Loaded {
    pub(super) devices: Vec<(Device, Uuid)>,
    max_devices: u32,
    self_chat: Option<CoreChatId>,
}

pub(super) async fn load(user: CoreUser) -> anyhow::Result<Loaded> {
    let own = user.own_client_id().await?;
    let entries: Vec<Entry> = user
        .linked_devices()
        .await?
        .into_iter()
        .map(|device| Entry {
            client_id: device.client_id,
            platform: device.platform.label().to_string(),
            name: device.name,
            linked_at: device.linked_at,
        })
        .collect();
    let members = user.self_group_client_ids().await?;
    let max_devices = user.max_devices().await.unwrap_or_else(|err| {
        warn!("couldn't read the device limit: {err:#}");
        0
    });
    Ok(Loaded {
        devices: join(own, members, &entries),
        max_devices,
        self_chat: user.self_chat_id().await?,
    })
}

/// What a notification means for the device list.
#[derive(Debug, PartialEq)]
enum Touch {
    None,
    Reload,
    /// A chat changed while the self chat is unknown: it may be the self
    /// group, joined only now (a new device processes its Welcome after
    /// loading). Look the self chat up again, as upstream.
    Probe,
}

/// What follows a self-chat lookup.
#[derive(Debug, PartialEq)]
enum AfterProbe {
    Reload,
    /// A chat changed while looking: it may have brought the self chat.
    Again,
    Done,
}

fn after_probe(found: bool, known: bool, reprobe: &mut bool) -> AfterProbe {
    let again = std::mem::take(reprobe);
    match (found, known) {
        (true, false) => AfterProbe::Reload,
        (false, false) if again => AfterProbe::Again,
        _ => AfterProbe::Done,
    }
}

/// The synced entries and the self chat (its members) change the list.
/// Before the first load, any chat might.
fn touches(notification: &DbNotification, loaded: bool, self_chat: Option<CoreChatId>) -> Touch {
    let mut touch = Touch::None;
    for entity in notification.ops.keys() {
        match entity {
            DbEntityId::UserSetting(key) if key == LinkedDevicesSetting::KEY => {
                return Touch::Reload;
            }
            DbEntityId::Chat(_) if !loaded => return Touch::Reload,
            DbEntityId::Chat(id) => match self_chat {
                Some(chat) if chat == *id => return Touch::Reload,
                Some(_) => {}
                None => touch = Touch::Probe,
            },
            _ => {}
        }
    }
    touch
}

impl Devices {
    pub fn new(session: &Entity<Session>, cx: &mut Context<Self>) -> Self {
        let changes = cx.subscribe(session, |this: &mut Self, _, event, cx| match event {
            SessionEvent::Changed {
                notifications,
                overflowed,
            } => {
                let touches: Vec<Touch> = notifications
                    .iter()
                    .map(|notification| touches(notification, this.loaded, this.self_chat))
                    .collect();
                if *overflowed || touches.contains(&Touch::Reload) {
                    this.reload(cx);
                } else if touches.contains(&Touch::Probe) {
                    // A running lookup may have read before this change:
                    // one more follows it (never aborted, so busy chats
                    // can't starve it).
                    if this.probe.is_some() {
                        this.reprobe = true;
                    } else {
                        this.probe_self_chat(cx);
                    }
                }
            }
        });
        let mut devices = Self {
            user: session.read(cx).user().clone(),
            devices: Vec::new(),
            ids: Vec::new(),
            max_devices: 0,
            self_chat: None,
            loaded: false,
            failed: false,
            load: None,
            probe: None,
            reprobe: false,
            _changes: changes,
        };
        devices.reload(cx);
        devices
    }

    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    pub fn max_devices(&self) -> u32 {
        self.max_devices
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let task = Tokio::spawn(cx, load(self.user.clone()));
        self.load = Some(cx.spawn(async move |this, cx| {
            let result = task.await.and_then(|loaded| loaded);
            _ = this.update(cx, |this, cx| {
                match result {
                    Ok(loaded) => {
                        (this.devices, this.ids) = loaded.devices.into_iter().unzip();
                        this.max_devices = loaded.max_devices;
                        this.self_chat = loaded.self_chat;
                        this.loaded = true;
                        this.failed = false;
                    }
                    Err(err) => {
                        error!("failed to load the devices: {err:#}");
                        this.failed = true;
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Reloads once the self chat exists: one query per chat change until
    /// then, rather than a full load.
    fn probe_self_chat(&mut self, cx: &mut Context<Self>) {
        let user = self.user.clone();
        let task = Tokio::spawn(cx, async move { user.self_chat_id().await });
        self.probe = Some(cx.spawn(async move |this, cx| {
            let found = task.await.and_then(|found| found);
            _ = this.update(cx, |this, cx| {
                this.probe = None;
                let found = found
                    .inspect_err(|err| error!("failed to look up the self chat: {err:#}"))
                    .is_ok_and(|found| found.is_some());
                match after_probe(found, this.self_chat.is_some(), &mut this.reprobe) {
                    AfterProbe::Reload => this.reload(cx),
                    AfterProbe::Again => this.probe_self_chat(cx),
                    AfterProbe::Done => {}
                }
            });
        }));
    }

    /// The device `query` names (see [`find`]); `others` for operations
    /// this device can't do on itself (unlinking).
    pub fn resolve(&self, query: &str, others: bool) -> Result<DeviceRef, String> {
        if self.failed {
            return Err("Couldn't load the devices; open the settings again".into());
        }
        if !self.loaded {
            return Err("Still loading the devices".into());
        }
        let ix = if query.trim().is_empty() && !others {
            self.devices
                .iter()
                .position(|device| device.this_device)
                .ok_or("This device isn't listed")?
        } else {
            find(&self.devices, query, others)?
        };
        Ok(DeviceRef {
            name: self.devices[ix].name.clone(),
            tag: self.devices[ix].tag.clone(),
            id: self.ids[ix],
        })
    }

    /// Renames `device` on all of the user's devices.
    pub fn rename(
        &self,
        device: &DeviceRef,
        name: &str,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, id, name) = (self.user.clone(), device.id, name.to_string());
        on_tokio(cx, "rename the device", async move {
            let name = parse_device_name(&name)?;
            Ok(user.rename_device(id, name).await?)
        })
    }

    /// Takes `device` out of the self group. It deletes its own data once
    /// it processes the removal (it keeps the account's keys until then:
    /// this is not revocation).
    pub fn unlink(
        &self,
        device: &DeviceRef,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, id) = (self.user.clone(), device.id);
        on_tokio(cx, "unlink the device", async move {
            Ok(user.unlink_device(id).await?)
        })
    }
}

/// A linking code as typed: separators are ignored, as the core does.
pub fn parse_code(input: &str) -> Result<String, String> {
    match LinkingCode::parse(input) {
        Ok(code) => Ok(code.to_digits()),
        Err(LinkingCodeError::TooShort) => Err(format!(
            "Linking codes have at least {MIN_CODE_DIGITS} digits"
        )),
        Err(LinkingCodeError::InvalidRendezvousId) => Err("That isn't a linking code".into()),
    }
}

/// A code shown in groups of four digits, as upstream.
pub fn spaced(code: &str) -> String {
    let digits: Vec<char> = code.chars().collect();
    digits
        .chunks(4)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a new device's QR code carries: upstream's linking URL, which the
/// phone app's scanner accepts only for its own server.
pub fn linking_url(domain: &Fqdn, code: &str) -> String {
    format!("{LINKING_URL_SCHEME}://{domain}/{LINKING_URL_PATH}?code={code}")
}

/// How linking a new device from this one goes.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkEvent {
    /// The new device proved it knows the code: ask the user to confirm.
    AwaitingConfirmation,
    Linked,
    /// Linking ended without a new device; what to tell the user.
    Failed(String),
}

/// Linking a new device from this one. Dropping it before
/// [`confirm`](Self::confirm) turns the link down (the core tells the new
/// device); after, the link runs to its end on its own.
pub struct Link {
    confirm: Option<oneshot::Sender<String>>,
}

impl Link {
    /// A link whose confirmation goes to `confirm`, for view tests.
    #[cfg(test)]
    pub fn fake(confirm: oneshot::Sender<String>) -> Self {
        Self {
            confirm: Some(confirm),
        }
    }

    /// Lets the link go ahead, naming the new device `name` (empty keeps
    /// the name it gave itself). Once only.
    pub fn confirm(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.chars().count() > MAX_DEVICE_NAME {
            return Err(format!(
                "Device names have at most {MAX_DEVICE_NAME} characters"
            ));
        }
        let confirm = self.confirm.take().ok_or("Already confirmed")?;
        // A closed channel means the link already ended; its event says why.
        _ = confirm.send(name.to_string());
        Ok(())
    }
}

impl Session {
    /// Starts linking the new device that shows `code` (as [`parse_code`]
    /// returns it). Detached: once confirmed, stopping midway would leave
    /// the core's undo to its deadline.
    pub fn link_device(
        &self,
        code: String,
        cx: &App,
    ) -> (Link, mpsc::UnboundedReceiver<LinkEvent>) {
        link_on(&Tokio::handle(cx), self.user.clone(), code)
    }
}

/// [`Session::link_device`] on `handle`.
pub(super) fn link_on(
    handle: &tokio::runtime::Handle,
    user: CoreUser,
    code: String,
) -> (Link, mpsc::UnboundedReceiver<LinkEvent>) {
    let (events_tx, events) = mpsc::unbounded_channel();
    let (confirm, confirmation_rx) = oneshot::channel();
    handle.spawn(async move {
        let (connected_tx, connected_rx) = oneshot::channel();
        let forward = {
            let events_tx = events_tx.clone();
            async move {
                if connected_rx.await.is_ok() {
                    _ = events_tx.send(LinkEvent::AwaitingConfirmation);
                }
            }
        };
        let link = user.multi_device_link_client(code, connected_tx, confirmation_rx);
        let (_, result) = tokio::join!(forward, Box::pin(link));
        _ = events_tx.send(link_outcome(result));
    });
    let link = Link {
        confirm: Some(confirm),
    };
    (link, events)
}

fn link_outcome(result: anyhow::Result<Result<(), MultiDeviceLinkClientError>>) -> LinkEvent {
    use MultiDeviceLinkClientError::*;
    match result {
        Ok(Ok(())) => LinkEvent::Linked,
        Ok(Err(SessionNotFound | InvalidCode)) => LinkEvent::Failed(
            "No device is showing that code. Check it, or wait for the next one.".into(),
        ),
        Ok(Err(DeviceLimitReached { max_devices })) => LinkEvent::Failed(format!(
            "You can link up to {max_devices} devices. Unlink one, then try again."
        )),
        Err(err) => {
            // Also a wrong code (the pairing doesn't authenticate) or a
            // link the user turned down.
            info!("linking a device failed: {err:#}");
            LinkEvent::Failed(
                "Linking failed. Check the code and your connection, then try again.".into(),
            )
        }
    }
}

/// How linking this device into an existing account goes.
pub enum ProvisionEvent {
    /// Show this code; a new one replaces it when it expires unanswered.
    Code(String),
    /// An existing device answered the code; the user confirms there.
    Linking,
    Linked(CoreUser),
    Failed,
}

/// Linking this device into an existing account. Dropping it cancels the
/// link (the core then deletes the half-made database), unless it is done.
pub struct Provision {
    cancel: CancellationToken,
}

impl Drop for Provision {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Starts linking this device into an account on `domain`. Detached and
/// cancelled through the token (never aborted): cancelling is what rolls
/// the new database back.
pub fn provision(
    db_path: String,
    domain: Fqdn,
    cx: &App,
) -> (Provision, mpsc::UnboundedReceiver<ProvisionEvent>) {
    provision_on(&Tokio::handle(cx), db_path, domain)
}

/// [`provision`] on `handle`.
pub(super) fn provision_on(
    handle: &tokio::runtime::Handle,
    db_path: String,
    domain: Fqdn,
) -> (Provision, mpsc::UnboundedReceiver<ProvisionEvent>) {
    let (events_tx, events) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    handle.spawn(async move {
        let (steps_tx, mut steps) = mpsc::channel(1);
        let forward = {
            let events_tx = events_tx.clone();
            async move {
                while let Some(step) = steps.recv().await {
                    _ = events_tx.send(match step {
                        MultiDeviceProvisionStep::Code(code) => ProvisionEvent::Code(code),
                        MultiDeviceProvisionStep::Linking => ProvisionEvent::Linking,
                    });
                }
            }
        };
        let link = CoreUser::new_linked(&db_path, domain, None, steps_tx, token);
        let (_, result) = tokio::join!(forward, Box::pin(link));
        _ = events_tx.send(match result {
            Ok(user) => ProvisionEvent::Linked(user),
            Err(err) => {
                info!("linking this device failed: {err:#}");
                ProvisionEvent::Failed
            }
        });
    });
    (Provision { cancel }, events)
}

/// Resolves once this device may no longer use the account: a sibling
/// unlinked it, or the account was deleted. Checks first, then on every
/// change to the own user (where the core reports it), as upstream.
pub(super) async fn unlinked(
    user: CoreUser,
    mut notifications: impl Stream<Item = std::sync::Arc<DbNotification>> + Unpin,
) -> Option<UnlinkReason> {
    let own = DbEntityId::User(user.user_id().clone());
    loop {
        match user.account_unlink_reason().await {
            Ok(Some(reason)) => return Some(reason),
            Ok(None) => {}
            Err(err) => error!("failed to read the unlink reason: {err:#}"),
        }
        loop {
            let notification = notifications.next().await?;
            if notification.ops.contains_key(&own) {
                break;
            }
        }
    }
}

/// How deleting an unlinked account's data went.
#[derive(Debug, PartialEq)]
pub enum TornDown {
    Deleted,
    /// Some of it failed. The record decides what happens next: kept, the
    /// account loads again (and fails again); gone, it never loads again,
    /// and the files left are removed by the next load (they were marked
    /// first, see [`super::account::remove_marked`]).
    Failed {
        record_kept: bool,
    },
}

/// Deletes this device's account data after it was unlinked: closes the
/// database (stopping the core's background work), then removes it and its
/// record, as upstream's `deleteCurrentDatabase`. The core removes the
/// record even when removing a file failed.
pub async fn tear_down(user: CoreUser, db_path: String) -> TornDown {
    let record = user.client_record_id();
    user.close_local_database().await;
    drop(user);
    // Marked first: a quit (or a failed read below) between the core
    // dropping the record and marking would leave the files unmarked for
    // good. A mark on an id that keeps its record is skipped.
    // Blocking (synced) file work, off the tokio workers.
    let dir = std::path::PathBuf::from(&db_path);
    let marking = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || super::account::mark_deleted(&dir, record))
    };
    if let Err(err) = marking
        .await
        .map_err(std::io::Error::other)
        .and_then(|done| done)
    {
        error!(
            "failed to mark the unlinked account's data for removal \
             (if the deletion is cut short, files may need removing by hand): {err}"
        );
    }
    let Err(err) = aircoreclient::delete_client_database(&db_path, record).await else {
        _ = tokio::task::spawn_blocking(move || super::account::unmark_deleted(&dir, record)).await;
        return TornDown::Deleted;
    };
    error!("failed to delete the unlinked account's data: {err:#}");
    let record_kept = match ClientRecord::load_all_from_air_db(&db_path).await {
        Ok(records) => records.iter().any(|r| r.client_record_id == record),
        // Can't tell: assume the worst (don't load again).
        Err(_) => true,
    };
    TornDown::Failed { record_kept }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn entry(client: u128, name: &str, linked_at: u64) -> Entry {
        Entry {
            client_id: id(client),
            name: name.into(),
            platform: "Linux".into(),
            linked_at,
        }
    }

    #[test]
    fn members_decide_what_is_listed_entries_only_name_them() {
        let entries = [
            entry(2, "Phone", 20),
            entry(1, "Laptop", 30),
            // Unlinked already: its entry may linger, it isn't listed.
            entry(9, "Old tablet", 5),
            // A name that tries to look like two lines.
            entry(3, "Work\n\u{202e}desk", 10),
        ];
        let devices = join(id(1), vec![id(3), id(2), id(1), id(4)], &entries);
        let names: Vec<_> = devices.iter().map(|(d, _)| d.name.to_string()).collect();
        // This device first; then by link time; a member with no entry
        // (yet) has no time and an unknown name.
        assert_eq!(names, ["Laptop", "Unknown device", "Work desk", "Phone"]);
        assert!(devices[0].0.this_device && devices.iter().skip(1).all(|(d, _)| !d.this_device));
        assert_eq!(devices[0].1, id(1));

        // No self group yet: this device alone, linked by definition.
        let alone = join(id(1), Vec::new(), &entries);
        assert_eq!(alone.len(), 1);
        assert!(alone[0].0.this_device);
    }

    #[test]
    fn tags_grow_until_no_two_devices_share_one() {
        let a = Uuid::parse_str("1a2b3c4d-0000-0000-0000-000000000001").unwrap();
        let b = Uuid::parse_str("1a2b3c4e-0000-0000-0000-000000000002").unwrap();
        let c = Uuid::parse_str("99990000-0000-0000-0000-000000000003").unwrap();
        let devices = join(a, vec![a, b, c], &[]);
        let tags: Vec<_> = devices.iter().map(|(d, _)| d.tag.to_string()).collect();
        assert_eq!(tags, ["1a2b3c4d", "1a2b3c4e", "99990000"]);
        let devices = join(a, vec![a, c], &[]);
        let tags: Vec<_> = devices.iter().map(|(d, _)| d.tag.to_string()).collect();
        assert_eq!(tags, ["1a2b", "9999"]);
    }

    #[test]
    fn alike_names_need_the_tag_and_unlinking_never_finds_this_device() {
        let entries = [
            entry(1, "Linux", 1),
            entry(2, "Linux", 2),
            entry(3, "Phone", 3),
        ];
        let devices: Vec<Device> = join(id(1), vec![id(1), id(2), id(3)], &entries)
            .into_iter()
            .map(|(d, _)| d)
            .collect();
        assert!(find(&devices, "linux", false).unwrap_err().contains("#tag"));
        // Among the others, the name is unique.
        assert_eq!(find(&devices, "linux", true), Ok(1));
        assert_eq!(find(&devices, "phone", true), Ok(2));
        let own_tag = format!("#{}", devices[0].tag);
        assert_eq!(find(&devices, &own_tag, false), Ok(0));
        assert!(find(&devices, &own_tag, true).is_err());
        // A name is never read as a tag unless all of it is one.
        assert!(find(&devices, &format!("{own_tag} x"), false).is_err());
    }

    #[test]
    fn rename_takes_an_optional_tag_first() {
        assert_eq!(split_rename("Work laptop"), (None, "Work laptop"));
        assert_eq!(
            split_rename("#1a2b  Work laptop "),
            (Some("#1a2b"), "Work laptop")
        );
        assert_eq!(split_rename("#1a2b"), (Some("#1a2b"), ""));
        // `#` alone or with other characters is part of the name.
        assert_eq!(split_rename("#1 phone"), (Some("#1"), "phone"));
        assert_eq!(split_rename("#phone mine"), (None, "#phone mine"));
    }

    #[test]
    fn codes_are_read_as_the_core_reads_them() {
        assert_eq!(parse_code("1234 5678 9"), Ok("123456789".into()));
        assert!(parse_code("1234567").is_err());
        assert!(parse_code("").is_err());
        assert_eq!(spaced("123456789"), "1234 5678 9");
        let domain: Fqdn = "example.com".parse().unwrap();
        assert_eq!(
            linking_url(&domain, "123456789"),
            "air://example.com/multiDeviceLinkingCode?code=123456789"
        );
    }

    #[test]
    fn a_change_during_a_lookup_gets_one_more_lookup() {
        let mut reprobe = true;
        assert_eq!(after_probe(false, false, &mut reprobe), AfterProbe::Again);
        // That one found nothing either, and nothing changed meanwhile.
        assert_eq!(after_probe(false, false, &mut reprobe), AfterProbe::Done);
        assert_eq!(after_probe(true, false, &mut true), AfterProbe::Reload);
        // A load found it meanwhile: nothing more to do.
        let mut reprobe = true;
        assert_eq!(after_probe(true, true, &mut reprobe), AfterProbe::Done);
        assert!(!reprobe);
    }

    #[test]
    fn internal_link_errors_are_told_without_their_detail() {
        let LinkEvent::Failed(message) =
            link_outcome(Err(anyhow::anyhow!("relay said 10.0.0.7 refused")))
        else {
            panic!("not a failure");
        };
        assert!(!message.contains("10.0.0.7"), "{message}");
        assert_eq!(link_outcome(Ok(Ok(()))), LinkEvent::Linked);
    }

    #[test]
    fn the_list_reloads_for_its_setting_and_the_self_chat_only() {
        let chat = |n: u128| CoreChatId::new(Uuid::from_u128(n));
        let changed = |entity: DbEntityId| DbNotification {
            ops: [(entity, Default::default())].into(),
        };
        let setting = changed(DbEntityId::UserSetting(LinkedDevicesSetting::KEY.into()));
        let other_setting = changed(DbEntityId::UserSetting("read_receipts".into()));
        let self_chat = changed(DbEntityId::Chat(chat(1)));
        let other_chat = changed(DbEntityId::Chat(chat(2)));
        for (loaded, known) in [(false, None), (true, None), (true, Some(chat(1)))] {
            assert_eq!(touches(&setting, loaded, known), Touch::Reload);
            assert_eq!(touches(&other_setting, loaded, known), Touch::None);
        }
        // Before the first load, any chat might be the self chat.
        assert_eq!(touches(&other_chat, false, None), Touch::Reload);
        assert_eq!(touches(&self_chat, true, Some(chat(1))), Touch::Reload);
        assert_eq!(touches(&other_chat, true, Some(chat(1))), Touch::None);
        // Loaded without one: a chat change may be the self group joined
        // since, so it is looked up (not a full reload).
        assert_eq!(touches(&other_chat, true, None), Touch::Probe);
    }

    #[test]
    fn a_link_is_confirmed_once_with_a_bounded_name() {
        let (confirm, mut rx) = oneshot::channel();
        let mut link = Link {
            confirm: Some(confirm),
        };
        assert!(link.confirm(&"x".repeat(MAX_DEVICE_NAME + 1)).is_err());
        assert_eq!(link.confirm("  Phone "), Ok(()));
        assert_eq!(rx.try_recv().as_deref(), Ok("Phone"));
        assert!(link.confirm("again").is_err());
    }
}
