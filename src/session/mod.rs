//! The logged-in account as a GPUI entity. Owns the `CoreUser`, runs the
//! queue listener and forwards database changes to the UI. Views read plain
//! state from here and never touch `aircoreclient` themselves.

pub mod account;
pub mod alerts;
pub mod chats;
pub mod composer;
mod contacts;
#[cfg(test)]
mod e2e;
pub mod messages;
mod pending;
mod preview;
pub mod queue;

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
    _receive: [Task<()>; 3],
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn start(user: CoreUser, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let initial = QueueState {
                status: ConnectionStatus::Connecting,
                version_expires: None,
            };
            let (state_tx, state_rx) = watch::channel(initial);

            let (notices_tx, notices_rx) = mpsc::channel(alerts::NOTICES_BUFFER);
            let (attention, attention_rx) = watch::channel(Attention {
                content: SettingsStore::get(cx).notifications,
                ..Attention::default()
            });
            let alerts = Alerts::new(attention_rx, notices_tx);
            let notifier = Tokio::spawn(cx, crate::notifier::run(notices_rx));
            let settings = cx.observe_global::<SettingsStore>(|this: &mut Self, cx| {
                let content = SettingsStore::get(cx).notifications;
                this.attention.send_if_modified(|attention| {
                    let changed = attention.content != content;
                    attention.content = content;
                    changed
                });
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

            let receive = [
                cx.spawn(async move |this, cx| {
                    receive_changes(this, changes_rx, overflowed, cx).await
                }),
                cx.spawn(async move |this, cx| receive_queue_state(this, state_rx, cx).await),
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
        let open_chat = open_chat.map(messages::core_chat_id);
        let changed = self.attention.send_if_modified(|attention| {
            let changed =
                (attention.window_active, attention.open_chat) != (window_active, open_chat);
            attention.window_active = window_active;
            attention.open_chat = open_chat;
            changed
        });
        if let (true, true, Some(chat)) = (changed, window_active, open_chat) {
            self.alerts.dismiss(chat);
        }
    }

    fn reload_profile(&mut self, cx: &mut Context<Self>) {
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
