//! The logged-in account as a GPUI entity. Owns the `CoreUser`, runs the
//! queue listener and forwards database changes to the UI. Views read plain
//! state from here and never touch `aircoreclient` themselves.

pub mod account;
pub mod account_settings;
pub mod alerts;
mod attachments;
mod avatars;
pub mod chats;
pub mod composer;
pub mod contact_safety;
mod contacts;
pub mod devices;
#[cfg(test)]
mod e2e;
pub mod groups;
pub mod invitations;
pub mod message_ops;
pub mod messages;
mod pending;
mod preview;
pub mod queue;
pub mod safety;
#[cfg(all(test, feature = "seam"))]
mod seam;

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
use queue::{QueueState, VersionStatus};

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

/// An account deletion was aborted without the user being told (it went
/// on in the background, or the app was closed meanwhile): the account
/// still exists, and this device keeps working.
pub struct DeletionAborted;

/// Clicks waiting for the UI. A handful at most: the user clicks one
/// notification at a time.
const CLICKS_BUFFER: usize = 8;

pub struct Session {
    user: CoreUser,
    display_name: SharedString,
    /// The user's own profile picture, if one could be drawn.
    picture: Option<crate::model::Picture>,
    own_avatar: Arc<tokio::sync::Mutex<avatars::Avatars<()>>>,
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
    /// The user's account deletion is running here: it reports its own
    /// outcome, an abort included.
    deleting: bool,
    /// An abort is being reported: not again until it is recorded.
    reporting_abort: bool,
    abort_check: Option<Task<()>>,
}

impl EventEmitter<SessionEvent> for Session {}
impl EventEmitter<OpenFromAlert> for Session {}
impl EventEmitter<Unlinked> for Session {}
impl EventEmitter<DeletionAborted> for Session {}

/// What the debug page shows of an account.
#[derive(Debug, Clone)]
pub struct DebugInfo {
    pub user_id: String,
    pub timed_tasks: usize,
    pub add_username_tokens: u32,
    pub invite_code_tokens: u32,
    pub connect_tokens: u32,
}

impl DebugInfo {
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        vec![
            ("User ID", self.user_id.clone()),
            ("Timed tasks", self.timed_tasks.to_string()),
            ("Add-username tokens", self.add_username_tokens.to_string()),
            ("Invite-code tokens", self.invite_code_tokens.to_string()),
            ("Connection-request tokens", self.connect_tokens.to_string()),
        ]
    }
}

impl Session {
    pub fn start(user: CoreUser, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let initial = QueueState {
                status: ConnectionStatus::Connecting,
                version: VersionStatus::Supported,
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
                    state_tx.send_modify(|state| state.apply(event))
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
            // The drawn picture lives on the GPU until dropped.
            cx.on_release(|this: &mut Self, cx| {
                if let Some(picture) = &this.picture {
                    cx.drop_image(picture.0.clone(), None);
                }
            })
            .detach();
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
                picture: None,
                own_avatar: Arc::default(),
                queue_state: initial,
                profile_task: None,
                user,
                _queue: queue,
                _forward: forward,
                _downloads: downloads,
                _receive: receive,
                deleting: false,
                reporting_abort: false,
                abort_check: None,
            };
            session.reload_profile(cx);
            // Durable: one aborted while the app was closed is told now.
            session.check_deletion_abort(cx);
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

    pub fn picture(&self) -> Option<&crate::model::Picture> {
        self.picture.as_ref()
    }

    pub fn status(&self) -> ConnectionStatus {
        self.queue_state.status
    }

    /// What the debug page shows of this account.
    pub fn debug_info(&self, cx: &App) -> impl Future<Output = Result<DebugInfo, String>> + use<> {
        let user = self.user.clone();
        const WHAT: &str = "load the debug info";
        contacts::on_tokio(cx, WHAT, async move {
            let info = user
                .user_debug_info()
                .await
                .map_err(|err| contacts::Failure::from(err).local(WHAT))?;
            Ok(DebugInfo {
                user_id: info.user_id,
                timed_tasks: info.timed_tasks.len(),
                add_username_tokens: info.add_username_token_count,
                invite_code_tokens: info.invitation_code_token_count,
                connect_tokens: info.connect_username_token_count,
            })
        })
    }

    /// Pretends the server said this about the version (seam tests).
    #[cfg(all(test, feature = "seam"))]
    pub fn set_version_state(&mut self, version: VersionStatus, cx: &mut Context<Self>) {
        self.queue_state.version = version;
        cx.notify();
    }

    /// What the alerts were last told (seam tests).
    #[cfg(all(test, feature = "seam"))]
    pub fn attention(&self) -> Attention {
        *self.attention.borrow()
    }

    /// Whether the server refuses this version: nothing works until updated.
    pub fn version_refused(&self) -> bool {
        self.queue_state.version == VersionStatus::Unsupported
    }

    /// When the server stops accepting this version, if it announced it.
    pub fn version_expires(&self) -> Option<DateTime<Utc>> {
        match self.queue_state.version {
            VersionStatus::ExpiresAt(at) => Some(at),
            _ => None,
        }
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

    /// Sets the user's profile picture from the file at `path` (`None`
    /// removes it), as their contacts see it. The core shrinks it to a
    /// small JPEG; then [`Self::reload_profile`] shows it.
    pub fn set_picture(
        &self,
        path: Option<std::path::PathBuf>,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let user = self.user.clone();
        let what = if path.is_some() {
            "change your picture"
        } else {
            "remove your picture"
        };
        contacts::on_tokio(cx, what, set_picture(user, path))
    }

    /// Deletes the account on the server, as upstream: the deletion is
    /// stored first and goes on in the background (also after a restart)
    /// if it takes longer than the core waits. Once it completes, the
    /// session reports [`Unlinked`] with `account_deleted`, and this
    /// device resets.
    pub fn delete_account(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<crate::model::AccountDeletion, String>> {
        use crate::model::AccountDeletion;
        // One at a time: each tells its own outcome.
        if self.deleting {
            return Task::ready(Err("Your account is being deleted already".into()));
        }
        self.deleting = true;
        let user = self.user.clone();
        let run = contacts::on_tokio(cx, "delete the account", async move {
            match user.delete_account().await {
                Ok(aircoreclient::AccountDeletionStatus::Completed) => {
                    Ok(AccountDeletion::Completed)
                }
                Ok(aircoreclient::AccountDeletionStatus::Pending) => Ok(AccountDeletion::Pending),
                // The core counts this as told: the caller tells it.
                Err(err) => {
                    error!("failed to delete the account: {err:#}");
                    Err(contacts::Failure::Shown(
                        "Your account couldn't be deleted. Wait a moment, then try again.".into(),
                    ))
                }
            }
        });
        cx.spawn(async move |this, cx| {
            let outcome = run.await;
            _ = this.update(cx, |this, cx| {
                this.deleting = false;
                // An abort recorded meanwhile was told by `outcome`; one
                // after it (the deletion went on) is told here.
                this.check_deletion_abort(cx);
            });
            outcome
        })
    }

    /// Tells the user ([`DeletionAborted`]) about an account deletion
    /// aborted in the background, once, and records that they were told.
    fn check_deletion_abort(&mut self, cx: &mut Context<Self>) {
        if self.deleting || self.reporting_abort {
            return;
        }
        let user = self.user.clone();
        let aborted = Tokio::spawn(cx, async move {
            user.has_unreported_account_deletion_abort().await
        });
        self.abort_check = Some(cx.spawn(async move |this, cx| {
            let aborted = match aborted.await.and_then(|aborted| aborted) {
                Ok(aborted) => aborted,
                Err(err) => return error!("failed to check for an aborted deletion: {err:#}"),
            };
            _ = this.update(cx, |this, cx| {
                if !aborted || this.deleting || this.reporting_abort {
                    return;
                }
                this.reporting_abort = true;
                cx.emit(DeletionAborted);
                let user = this.user.clone();
                let recorded = Tokio::spawn(cx, async move {
                    user.mark_account_deletion_abort_reported().await
                });
                cx.spawn(async move |this, cx| {
                    if let Err(err) = recorded.await.and_then(|recorded| recorded) {
                        error!("failed to record that the abort was told: {err:#}");
                    }
                    _ = this.update(cx, |this, _| this.reporting_abort = false);
                })
                .detach();
            });
        }));
    }

    pub(crate) fn reload_profile(&mut self, cx: &mut Context<Self>) {
        let (user, avatar) = (self.user.clone(), self.own_avatar.clone());
        let profile = Tokio::spawn(cx, async move {
            let profile = user.own_user_profile().await?;
            let bytes = profile.profile_picture.map(|picture| picture.into_bytes());
            let picture = avatar.lock().await.get(&(), bytes).await;
            anyhow::Ok((
                SharedString::from(profile.display_name.to_string()),
                picture,
            ))
        });
        self.profile_task =
            Some(cx.spawn(
                async move |this, cx| match profile.await.and_then(|profile| profile) {
                    Ok((name, picture)) => {
                        _ = this.update(cx, |this, cx| {
                            this.display_name = name;
                            let old = std::mem::replace(&mut this.picture, picture);
                            avatars::drop_unused(old.iter(), this.picture.iter(), cx);
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
            // The core records an abort on the own user.
            self.check_deletion_abort(cx);
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

/// See [`Session::set_picture`].
async fn set_picture(
    user: CoreUser,
    path: Option<std::path::PathBuf>,
) -> Result<(), contacts::Failure> {
    let picture = match path {
        Some(path) => Some(avatars::read_source(path).await?),
        None => None,
    };
    let mut profile = user.own_user_profile().await.map_err(anyhow::Error::from)?;
    profile.profile_picture = picture.map(aircoreclient::Asset::Value);
    user.set_own_user_profile(profile).await?;
    Ok(())
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
