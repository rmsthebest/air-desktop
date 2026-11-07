//! The logged-in account as a GPUI entity. Owns the `CoreUser`, runs the
//! queue listener and forwards database changes to the UI. Views read plain
//! state from here and never touch `aircoreclient` themselves.

pub mod account;
pub mod alerts;
mod attachments;
pub mod chats;
pub mod composer;
pub mod contact_safety;
mod contacts;
pub mod devices;
#[cfg(test)]
mod e2e;
pub mod groups;
pub mod messages;
mod pending;
mod preview;
pub mod queue;
pub mod safety;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aircommon::identifiers::UserId as CoreUserId;
use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::{DbEntityId, DbNotification};
use chrono::{DateTime, Utc};
use gpui::{EventEmitter, SharedString};
use gpui_kit::{App, AppContext as _, AsyncApp, Context, Entity, Subscription, Task, WeakEntity};
use tokio::sync::{mpsc, watch};
use tokio_stream::StreamExt as _;
use tracing::error;

pub use queue::ConnectionStatus;

/// How the views name a user: "You" for the local user, else their display
/// name as one bounded line (it is peer-chosen).
pub(crate) async fn display_name(user: &CoreUser, id: &CoreUserId) -> SharedString {
    if id == user.user_id() {
        return "You".into();
    }
    let profile = user.user_profile(id).await;
    crate::model::name_line(profile.display_name.as_ref())
}

use crate::model::ChatId;
use crate::runtime::{Tokio, TokioTask};
use crate::settings::SettingsStore;
use alerts::{Alerts, Attention};
use queue::QueueEvent;

/// Database changes buffered for the UI. Peers control how many arrive, so
/// beyond this the forwarder stops queueing and flags a full reload.
const CHANGES_BUFFER: usize = 256;

pub enum SessionEvent {
    /// Data changed in the database (new messages, chats, profiles…).
    /// Batched: one event may carry several notifications.
    Changed {
        notifications: Vec<Arc<DbNotification>>,
        /// Some notifications were dropped: reload everything shown.
        overflowed: bool,
    },
}

/// The user clicked a chat's desktop notification: show it, raising the
/// window.
pub struct OpenFromAlert(pub ChatId);

/// This device may no longer use the account: it must delete its data
/// (see [`devices::tear_down`]) and start over.
pub struct Unlinked {
    /// The account itself is gone (deleted from this or another device),
    /// rather than only this device unlinked.
    pub account_deleted: bool,
}

/// Clicks waiting for the UI. A handful at most: the user clicks one
/// notification at a time.
const CLICKS_BUFFER: usize = 8;

#[derive(Clone, Copy, PartialEq)]
struct QueueState {
    status: ConnectionStatus,
    version_expires: Option<DateTime<Utc>>,
}

pub struct Session {
    user: CoreUser,
    display_name: SharedString,
    queue_state: QueueState,
    /// The latest profile load; a newer one replaces (cancels) it, so an
    /// older result can't overwrite a newer one.
    profile_task: Option<Task<()>>,
    drafts: composer::DraftQueue,
    usernames: Vec<SharedString>,
    /// What the user is looking at, read by the alerts.
    attention: watch::Sender<Attention>,
    alerts: Alerts,
    _notifier: TokioTask<()>,
    _settings: Subscription,
    _queue: TokioTask<()>,
    _usernames: TokioTask<()>,
    _drafts: TokioTask<()>,
    _forward: TokioTask<()>,
    _downloads: TokioTask<()>,
    _receive: [Task<()>; 5],
}

impl EventEmitter<SessionEvent> for Session {}
impl EventEmitter<OpenFromAlert> for Session {}
impl EventEmitter<Unlinked> for Session {}

impl Session {
    pub fn start(user: CoreUser, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let initial = QueueState {
                status: ConnectionStatus::Connecting,
                version_expires: None,
            };
            let (state_tx, state_rx) = watch::channel(initial);

            let notices = Arc::new(alerts::NoticeQueue::default());
            let (attention, attention_rx) = watch::channel(Attention {
                content: SettingsStore::get(cx).notifications,
                ..Attention::default()
            });
            let alerts = Alerts::new(attention_rx.clone(), notices.clone());
            let (clicks_tx, mut clicks_rx) = mpsc::channel(CLICKS_BUFFER);
            let notifier = Tokio::spawn(
                cx,
                crate::notifier::run(
                    notices,
                    attention_rx,
                    crate::notifier::system(),
                    alert_outcome(user.clone(), clicks_tx),
                ),
            );
            let settings = cx.observe_global::<SettingsStore>(|this: &mut Self, cx| {
                let content = SettingsStore::get(cx).notifications;
                let next = Attention {
                    content,
                    ..*this.attention.borrow()
                };
                this.attend(next);
            });

            let queue = Tokio::spawn(
                cx,
                queue::run(user.clone(), alerts.clone(), move |event| {
                    state_tx.send_modify(|state| match event {
                        QueueEvent::Status(status) => state.status = status,
                        QueueEvent::VersionExpires(at) => state.version_expires = at,
                    })
                }),
            );

            let (changes_tx, changes_rx) = mpsc::channel(CHANGES_BUFFER);
            let overflowed = Arc::new(AtomicBool::new(false));
            // Subscribe before anything runs, so no change is missed.
            let mut notifications = user.db_notifications();
            let previews_tx = changes_tx.clone();
            let forward = Tokio::spawn(cx, {
                let overflowed = overflowed.clone();
                async move {
                    while let Some(notification) = notifications.next().await {
                        match changes_tx.try_send(notification) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                overflowed.store(true, Ordering::Relaxed)
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => return,
                        }
                    }
                }
            });

            // Previews made after a download reach the views like any
            // other change.
            let downloads = Tokio::spawn(cx, {
                let overflowed = overflowed.clone();
                attachments::download_all(user.clone(), move |id| {
                    let full = previews_tx.try_send(attachments::previewed(id));
                    if let Err(mpsc::error::TrySendError::Full(_)) = full {
                        overflowed.store(true, Ordering::Relaxed);
                    }
                })
            });

            let (usernames_tx, mut usernames_rx) = watch::channel(Vec::new());
            let usernames = Tokio::spawn(
                cx,
                contacts::run(user.clone(), alerts.clone(), move |usernames| {
                    usernames_tx.send_if_modified(|old| {
                        let changed = *old != usernames;
                        *old = usernames;
                        changed
                    });
                }),
            );

            // Subscribed now, so a removal processed meanwhile isn't missed.
            let unlinked =
                Tokio::spawn(cx, devices::unlinked(user.clone(), user.db_notifications()));

            let receive = [
                cx.spawn(async move |this, cx| {
                    if let Ok(Some(reason)) = unlinked.await {
                        let account_deleted = reason == aircoreclient::UnlinkReason::AccountDeleted;
                        _ = this.update(cx, |_, cx| cx.emit(Unlinked { account_deleted }));
                    }
                }),
                cx.spawn(async move |this, cx| {
                    receive_changes(this, changes_rx, overflowed, cx).await
                }),
                cx.spawn(async move |this, cx| receive_queue_state(this, state_rx, cx).await),
                cx.spawn(async move |this, cx| {
                    while let Some(chat) = clicks_rx.recv().await {
                        let chat = chats::model_id(&chat);
                        if this
                            .update(cx, |_, cx| cx.emit(OpenFromAlert(chat)))
                            .is_err()
                        {
                            return;
                        }
                    }
                }),
                cx.spawn(async move |this, cx| {
                    while usernames_rx.changed().await.is_ok() {
                        let usernames = usernames_rx.borrow_and_update().clone();
                        let updated = this.update(cx, |this, cx| {
                            this.usernames = usernames;
                            cx.notify();
                        });
                        if updated.is_err() {
                            return;
                        }
                    }
                }),
            ];
            let (drafts, drafts_task) = composer::DraftQueue::start(user.clone(), cx);
            let mut session = Self {
                drafts,
                _drafts: drafts_task,
                usernames: Vec::new(),
                _usernames: usernames,
                attention,
                alerts,
                _notifier: notifier,
                _settings: settings,
                display_name: SharedString::default(),
                queue_state: initial,
                profile_task: None,
                user,
                _queue: queue,
                _forward: forward,
                _downloads: downloads,
                _receive: receive,
            };
            session.reload_profile(cx);
            session
        })
    }

    pub(crate) fn user(&self) -> &CoreUser {
        &self.user
    }

    pub(crate) fn drafts(&self) -> &composer::DraftQueue {
        &self.drafts
    }

    pub fn display_name(&self) -> &SharedString {
        &self.display_name
    }

    pub fn status(&self) -> ConnectionStatus {
        self.queue_state.status
    }

    /// When the server stops accepting this version, if it announced it.
    pub fn version_expires(&self) -> Option<DateTime<Utc>> {
        self.queue_state.version_expires
    }

    /// Tells the alerts what the user is looking at: the open chat, and
    /// whether the window is active. Looking at a chat dismisses its alert.
    pub fn set_attention(&mut self, window_active: bool, open_chat: Option<&ChatId>) {
        let next = Attention {
            window_active,
            open_chat: open_chat.map(messages::core_chat_id),
            ..*self.attention.borrow()
        };
        self.attend(next);
    }

    fn attend(&mut self, next: Attention) {
        let mut stale = None;
        self.attention.send_if_modified(|attention| {
            let changed = *attention != next;
            stale = alerts::attend(attention, next);
            changed
        });
        if let Some(stale) = stale {
            self.alerts.dismiss(stale);
        }
    }

    /// Changes the user's display name, as their contacts see it.
    /// Resolves to the name as shown once it is stored and sent; then
    /// [`Self::reload_profile`] shows it (the core may not report its own
    /// change).
    pub fn set_display_name(
        &self,
        name: &str,
        cx: &App,
    ) -> impl Future<Output = Result<SharedString, String>> + use<> {
        let (user, name) = (self.user.clone(), name.to_string());
        contacts::on_tokio(cx, "change your name", set_display_name(user, name))
    }

    pub(crate) fn reload_profile(&mut self, cx: &mut Context<Self>) {
        let user = self.user.clone();
        let profile = Tokio::spawn(cx, async move { user.own_user_profile().await });
        self.profile_task =
            Some(cx.spawn(
                async move |this, cx| match profile.await.and_then(|profile| Ok(profile?)) {
                    Ok(profile) => {
                        let name = SharedString::from(profile.display_name.to_string());
                        _ = this.update(cx, |this, cx| {
                            this.display_name = name;
                            cx.notify();
                        });
                    }
                    Err(err) => error!("failed to load own profile: {err:#}"),
                },
            ));
    }

    fn changed(
        &mut self,
        notifications: Vec<Arc<DbNotification>>,
        overflowed: bool,
        cx: &mut Context<Self>,
    ) {
        let own = DbEntityId::User(self.user.user_id().clone());
        if overflowed || notifications.iter().any(|n| n.ops.contains_key(&own)) {
            self.reload_profile(cx);
        }
        cx.emit(SessionEvent::Changed {
            notifications,
            overflowed,
        });
    }
}

/// Carries out what the user did with a desktop notification. Runs on the
/// notifier's tokio task.
fn alert_outcome(
    user: CoreUser,
    clicks: mpsc::Sender<aircoreclient::ChatId>,
) -> impl FnMut(crate::notifier::Outcome) + Send {
    use crate::notifier::Outcome;
    move |outcome| match outcome {
        // Dropped if the UI is that far behind: the user can click again.
        Outcome::Open(chat) => _ = clicks.try_send(chat),
        // Detached: the notifier mustn't wait on the database. The core
        // never moves the watermark back.
        Outcome::Dismissed { chat, until } => {
            let user = user.clone();
            tokio::spawn(async move {
                if let Err(err) = user.set_chat_notified_until(chat, until).await {
                    error!("failed to mark a dismissed alert's chat as notified: {err:#}");
                }
            });
        }
    }
}

/// See [`Session::set_display_name`].
async fn set_display_name(user: CoreUser, name: String) -> Result<SharedString, contacts::Failure> {
    let display_name: aircoreclient::DisplayName = name
        .trim()
        .parse()
        .map_err(|_| contacts::Failure::Shown("A name can't be blank".into()))?;
    let mut profile = user.own_user_profile().await.map_err(anyhow::Error::from)?;
    profile.display_name = display_name;
    let profile = user.set_own_user_profile(profile).await?;
    Ok(crate::model::name_line(profile.display_name.as_ref()))
}

/// Drains database changes in batches, so a burst costs one UI update
/// rather than one per row.
async fn receive_changes(
    this: WeakEntity<Session>,
    mut rx: mpsc::Receiver<Arc<DbNotification>>,
    overflowed: Arc<AtomicBool>,
    cx: &mut AsyncApp,
) {
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(notification) = rx.try_recv() {
            batch.push(notification);
        }
        let overflowed = overflowed.swap(false, Ordering::Relaxed);
        if this
            .update(cx, |this, cx| this.changed(batch, overflowed, cx))
            .is_err()
        {
            return;
        }
    }
}

async fn receive_queue_state(
    this: WeakEntity<Session>,
    mut rx: watch::Receiver<QueueState>,
    cx: &mut AsyncApp,
) {
    while rx.changed().await.is_ok() {
        let state = *rx.borrow_and_update();
        let updated = this.update(cx, |this, cx| {
            this.queue_state = state;
            cx.notify();
        });
        if updated.is_err() {
            return;
        }
    }
}
