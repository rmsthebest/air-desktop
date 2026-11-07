//! The account's settings the core syncs to the user's other devices: read
//! receipts, and deleting contact requests that expired. Port of the
//! settings parts of upstream's `UserSettingsCubit`.
//!
//! Each is shown as stored and reloaded when the core reports it changed:
//! a change made here is applied at once and synced; one that loses to
//! another device's, or fails to sync, is rolled back by the core, which
//! reports that too.

use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::DbEntityId;
use aircoreclient::{DeleteExpiredContactRequestsSetting, ReadReceiptsSetting, UserSetting as _};
use gpui_kit::{App, Context, Entity, Subscription, Task};

use super::contacts::{Failure, on_tokio};
use super::{Session, SessionEvent};
use crate::model::SyncedSettings;
use crate::runtime::Tokio;

pub struct AccountSettings {
    user: CoreUser,
    /// `None` until first loaded: nothing is changed from a guess.
    settings: Option<SyncedSettings>,
    /// The latest load; a newer one replaces (cancels) it.
    load: Option<Task<()>>,
    _changes: Subscription,
}

/// Whether a change to the setting stored under `key` is one shown here.
fn shown(key: &str) -> bool {
    key == ReadReceiptsSetting::KEY || key == DeleteExpiredContactRequestsSetting::KEY
}

/// The settings as stored; each unset one has the core's default (read
/// receipts on, expired requests kept).
pub(super) async fn load(user: CoreUser) -> SyncedSettings {
    let defaults = SyncedSettings::default();
    SyncedSettings {
        read_receipts: user
            .user_setting::<ReadReceiptsSetting>()
            .await
            .map_or(defaults.read_receipts, |ReadReceiptsSetting(on)| on),
        delete_expired_requests: user
            .user_setting::<DeleteExpiredContactRequestsSetting>()
            .await
            .map_or(defaults.delete_expired_requests, |setting| setting.0),
    }
}

impl AccountSettings {
    pub fn new(session: &Entity<Session>, cx: &mut Context<Self>) -> Self {
        let changes = cx.subscribe(session, |this: &mut Self, _, event, cx| match event {
            SessionEvent::Changed {
                notifications,
                overflowed,
            } => {
                let touched = notifications.iter().any(|notification| {
                    notification
                        .ops
                        .keys()
                        .any(|entity| matches!(entity, DbEntityId::UserSetting(key) if shown(key)))
                });
                if *overflowed || touched {
                    this.reload(cx);
                }
            }
        });
        let mut settings = Self {
            user: session.read(cx).user().clone(),
            settings: None,
            load: None,
            _changes: changes,
        };
        settings.reload(cx);
        settings
    }

    pub fn settings(&self) -> Option<SyncedSettings> {
        self.settings
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let task = Tokio::spawn(cx, load(self.user.clone()));
        self.load = Some(cx.spawn(async move |this, cx| {
            if let Ok(settings) = task.await {
                _ = this.update(cx, |this, cx| {
                    this.settings = Some(settings);
                    cx.notify();
                });
            }
        }));
    }

    /// Turns read receipts on or off, on all the user's devices.
    pub fn set_read_receipts(
        &self,
        on: bool,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let user = self.user.clone();
        set(cx, async move {
            user.set_synced_user_setting(&ReadReceiptsSetting(on)).await
        })
    }

    /// Whether contact requests that expired are deleted (else kept as a
    /// record), on all the user's devices.
    pub fn set_delete_expired_requests(
        &self,
        on: bool,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let user = self.user.clone();
        set(cx, async move {
            let setting = DeleteExpiredContactRequestsSetting(on);
            user.set_synced_user_setting(&setting).await
        })
    }
}

/// Runs a store of a synced setting (local; the core queues the sync).
fn set<F>(cx: &App, store: F) -> impl Future<Output = Result<(), String>> + use<F>
where
    F: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    let what = "change the setting";
    on_tokio(cx, what, async move {
        store.await.map_err(|err| Failure::from(err).local(what))
    })
}
