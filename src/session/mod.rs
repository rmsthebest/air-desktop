//! The logged-in account as a GPUI entity. Owns the `CoreUser`, runs the
//! queue listener and forwards database changes to the UI. Views read plain
//! state from here and never touch `aircoreclient` themselves.

pub mod account;
pub mod chats;
#[cfg(test)]
mod e2e;
mod preview;
pub mod queue;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::{DbEntityId, DbNotification};
use chrono::{DateTime, Utc};
use gpui::{EventEmitter, SharedString};
use gpui_kit::{App, AppContext as _, AsyncApp, Context, Entity, Task, WeakEntity};
use tokio::sync::{mpsc, watch};
use tokio_stream::StreamExt as _;
use tracing::error;

pub use queue::ConnectionStatus;

use crate::runtime::{Tokio, TokioTask};
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
    _queue: TokioTask<()>,
    _forward: TokioTask<()>,
    _receive: [Task<()>; 2],
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
            let queue = Tokio::spawn(
                cx,
                queue::run(user.clone(), move |event| {
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

            let receive = [
                cx.spawn(async move |this, cx| {
                    receive_changes(this, changes_rx, overflowed, cx).await
                }),
                cx.spawn(async move |this, cx| receive_queue_state(this, state_rx, cx).await),
            ];
            let mut session = Self {
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
