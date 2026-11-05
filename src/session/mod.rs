//! The logged-in account as a GPUI entity. Owns the `CoreUser`, runs the
//! queue listener and forwards database changes to the UI. Views read plain
//! state from here and never touch `aircoreclient` themselves.

pub mod account;
#[cfg(test)]
mod e2e;
pub mod queue;

use std::sync::Arc;

use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::{DbEntityId, DbNotification};
use gpui::{EventEmitter, SharedString};
use gpui_kit::{App, AppContext as _, Context, Entity, Task};
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tracing::error;

pub use queue::ConnectionStatus;

use crate::runtime::{Tokio, TokioTask};

pub enum SessionEvent {
    /// Data changed in the database (new messages, chats, profiles…).
    /// Batched: one event may carry several notifications.
    #[allow(dead_code, reason = "the chat list consumes it from milestone 4")]
    Changed(Vec<Arc<DbNotification>>),
}

enum Update {
    Db(Arc<DbNotification>),
    Status(ConnectionStatus),
}

pub struct Session {
    user: CoreUser,
    display_name: SharedString,
    status: ConnectionStatus,
    _queue: TokioTask<()>,
    _forward: TokioTask<()>,
    _receive: Task<()>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn start(user: CoreUser, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let (tx, rx) = mpsc::unbounded_channel();
            // Subscribe before anything runs, so no change is missed.
            let mut notifications = user.db_notifications();
            let db_tx = tx.clone();
            let forward = Tokio::spawn(cx, async move {
                while let Some(notification) = notifications.next().await {
                    if db_tx.send(Update::Db(notification)).is_err() {
                        return;
                    }
                }
            });
            let queue = Tokio::spawn(
                cx,
                queue::run(user.clone(), move |status| {
                    let _ = tx.send(Update::Status(status));
                }),
            );
            let receive = cx.spawn(async move |this, cx| receive(this, rx, cx).await);
            let session = Self {
                display_name: SharedString::default(),
                status: ConnectionStatus::Connecting,
                user,
                _queue: queue,
                _forward: forward,
                _receive: receive,
            };
            session.reload_profile(cx);
            session
        })
    }

    pub fn display_name(&self) -> &SharedString {
        &self.display_name
    }

    pub fn status(&self) -> ConnectionStatus {
        self.status
    }

    fn reload_profile(&self, cx: &mut Context<Self>) {
        let user = self.user.clone();
        let profile = Tokio::spawn(cx, async move { user.own_user_profile().await });
        cx.spawn(
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
        )
        .detach();
    }

    fn apply(&mut self, updates: Vec<Update>, cx: &mut Context<Self>) {
        let mut changed = Vec::new();
        for update in updates {
            match update {
                Update::Status(status) => self.status = status,
                Update::Db(notification) => changed.push(notification),
            }
        }
        let own = DbEntityId::User(self.user.user_id().clone());
        if changed.iter().any(|n| n.ops.contains_key(&own)) {
            self.reload_profile(cx);
        }
        if !changed.is_empty() {
            cx.emit(SessionEvent::Changed(changed));
        }
        cx.notify();
    }
}

/// Drains updates in batches, so a burst of database changes costs one UI
/// update rather than one per row.
async fn receive(
    this: gpui::WeakEntity<Session>,
    mut rx: mpsc::UnboundedReceiver<Update>,
    cx: &mut gpui::AsyncApp,
) {
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(update) = rx.try_recv() {
            batch.push(update);
        }
        if this.update(cx, |this, cx| this.apply(batch, cx)).is_err() {
            return;
        }
    }
}
