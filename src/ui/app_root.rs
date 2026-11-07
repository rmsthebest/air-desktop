//! Window root: loads the account at startup, then shows sign-up or the
//! workspace.

use std::path::PathBuf;

use aircoreclient::clients::CoreUser;
use gpui::SharedString;
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, Focusable as _, IntoElement, Render, Subscription, Window, div,
};

use crate::paths;
use crate::runtime::Tokio;
use crate::session::{Session, Unlinked, account, devices};
use crate::settings::{Appearance, SettingsStore};
use crate::theme;
use crate::ui::Workspace;
use crate::ui::onboarding::{Onboarding, OnboardingEvent};

enum Screen {
    Loading,
    Onboarding {
        view: Entity<Onboarding>,
        _subscription: Subscription,
    },
    Ready {
        workspace: Entity<Workspace>,
        _unlinked: Subscription,
    },
    /// Deleting the data of an account this device may no longer use.
    TearingDown,
    Failed(SharedString),
}

pub struct AppRoot {
    data_dir: PathBuf,
    /// The core's `db_path` (the data dir), once prepared.
    db_path: String,
    screen: Screen,
    _subscriptions: Vec<Subscription>,
}

impl AppRoot {
    pub fn new(data_dir: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        theme::apply_appearance(window, cx);
        let subscriptions = vec![cx.observe_window_appearance(window, |_, window, cx| {
            if SettingsStore::get(cx).appearance == Appearance::System {
                theme::apply_appearance(window, cx);
            }
        })];
        let mut this = Self {
            data_dir: data_dir.clone().unwrap_or_default(),
            db_path: String::new(),
            screen: Screen::Loading,
            _subscriptions: subscriptions,
        };
        match data_dir
            .ok_or_else(|| "No data directory: is $HOME set?".to_string())
            .and_then(|dir| prepare_data_dir(&dir))
        {
            Ok(db_path) => {
                this.db_path = db_path.clone();
                this.load(db_path, None, window, cx)
            }
            Err(err) => this.screen = Screen::Failed(err.into()),
        }
        this
    }

    fn load(
        &mut self,
        db_path: String,
        notice: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = Tokio::spawn(cx, account::load_default(db_path.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.and_then(|loaded| loaded);
            _ = this.update_in(cx, |this, window, cx| {
                // Opening the account database creates it with the umask.
                this.restrict_files();
                match result {
                    Ok(Some(user)) => this.start(user, notice, window, cx),
                    Ok(None) => this.onboard(db_path, notice, window, cx),
                    Err(err) => {
                        tracing::error!("failed to load account: {err:#}");
                        this.screen =
                            Screen::Failed(format!("Couldn't open your account: {err:#}").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn onboard(
        &mut self,
        db_path: String,
        notice: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let onboarding = cx.new(|cx| Onboarding::new(db_path, notice, window, cx));
        let subscription =
            cx.subscribe_in(
                &onboarding,
                window,
                |this, _, event, window, cx| match event {
                    OnboardingEvent::SignedUp(user) => this.start(user.clone(), None, window, cx),
                },
            );
        onboarding.focus_handle(cx).focus(window, cx);
        self.screen = Screen::Onboarding {
            view: onboarding,
            _subscription: subscription,
        };
    }

    /// Opens `user`'s workspace, telling `notice` (why the last account
    /// went) if there is one.
    fn start(
        &mut self,
        user: CoreUser,
        notice: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Sign-up created the account's databases with the umask.
        self.restrict_files();
        let session = Session::start(user.clone(), cx);
        let unlinked = cx.subscribe_in(&session, window, move |this, _, event, window, cx| {
            this.tear_down(user.clone(), event, window, cx)
        });
        let workspace = cx.new(|cx| Workspace::new(Some(session), window, cx));
        self.screen = Screen::Ready {
            workspace,
            _unlinked: unlinked,
        };
        if let Some(notice) = notice {
            crate::ui::show_info(notice.to_string(), window, cx);
        }
        cx.notify();
    }

    /// Drops the account (the workspace and session go, stopping their
    /// work), deletes its data and starts over, saying why: with another
    /// account left in the data dir if one loads, else onboarding. As
    /// upstream, a failed deletion still signs out: staying in an account
    /// the user took this device out of is worse than data left behind.
    /// If its record is gone (the core drops it even when a file stays),
    /// loading again is safe and the next start sweeps the files; if the
    /// record is kept, loading would find the same account and fail again
    /// in a loop, so onboarding shows and the next start retries.
    fn tear_down(
        &mut self,
        user: CoreUser,
        event: &Unlinked,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.screen, Screen::Ready { .. }) {
            return;
        }
        let notice: SharedString = if event.account_deleted {
            "Your Air account was deleted. This device has been reset.".into()
        } else {
            "This device was unlinked from your account, and its data deleted.".into()
        };
        self.screen = Screen::TearingDown;
        cx.notify();
        let db_path = self.db_path.clone();
        // Detached, so dropping this view doesn't stop the deletion between
        // removing the database and its record. Quitting still can (the
        // runtime shuts down); the next start then skips the bare record.
        let task = Tokio::handle(cx).spawn(devices::tear_down(user, db_path.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let torn_down = task
                .await
                .unwrap_or(devices::TornDown::Failed { record_kept: true });
            _ = this.update_in(cx, |this, window, cx| {
                match torn_down {
                    devices::TornDown::Failed { record_kept: true } => {
                        this.onboard(db_path, Some(notice), window, cx)
                    }
                    // Another account may be left: that one opens;
                    // onboarding only if none does.
                    _ => this.load(db_path, Some(notice), window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
impl AppRoot {
    /// Opens `user`'s workspace at once (the user's databases in
    /// `data_dir`): a loaded test account wouldn't find the test server.
    pub fn with_account(
        data_dir: PathBuf,
        user: CoreUser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let db_path = prepare_data_dir(&data_dir).unwrap();
        let mut this = Self {
            data_dir,
            db_path,
            screen: Screen::Loading,
            _subscriptions: Vec::new(),
        };
        this.start(user, None, window, cx);
        this
    }
}

impl AppRoot {
    /// The directory is 0700 already; this keeps the files 0600 as well.
    fn restrict_files(&self) {
        if let Err(err) = paths::restrict_files(&self.data_dir) {
            tracing::error!("failed to restrict data file permissions: {err}");
        }
    }
}

/// Creates the private data directory and returns it as the core's
/// `db_path`.
fn prepare_data_dir(dir: &std::path::Path) -> Result<String, String> {
    // The core builds `sqlite://{db_path}/…` URLs from it, unescaped.
    let path = dir
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", dir.display()))?;
    if path.contains(['?', '#', '%']) {
        return Err(format!(
            "{path} contains ?, # or %, which the database can't open; \
             set AIR_DESKTOP_DATA_DIR to another directory"
        ));
    }
    paths::ensure_private_dir(dir)
        .map_err(|err| format!("Couldn't create {}: {err}", dir.display()))?;
    paths::restrict_files(dir)
        .map_err(|err| format!("Couldn't secure {}: {err}", dir.display()))?;
    Ok(path.to_string())
}

impl Render for AppRoot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let content: AnyElement = match &self.screen {
            Screen::Loading => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("Opening your account…")
                .into_any_element(),
            Screen::Onboarding { view, .. } => view.clone().into_any_element(),
            Screen::Ready { workspace, .. } => workspace.clone().into_any_element(),
            Screen::TearingDown => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child("Removing this device's data…")
                .into_any_element(),
            Screen::Failed(message) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(div().text_lg().child("Air couldn't start"))
                .child(
                    div()
                        .max_w(gpui_kit::px(560.))
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(message.clone()),
                )
                .into_any_element(),
        };
        div()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn open(data_dir: Option<PathBuf>, cx: &mut TestAppContext) -> Entity<AppRoot> {
        let (_, root) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::runtime::init(cx).unwrap();
            crate::settings::SettingsStore::init(None, Default::default(), cx);
            theme::init(None, cx).unwrap();
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| AppRoot::new(data_dir, window, cx))
            })
            .unwrap()
        });
        root
    }

    #[gpui::test]
    fn unusable_data_dirs_fail_visibly_without_touching_disk(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        // `#` would cut the sqlite URL short and open a different database.
        let bad = root.path().join("air#desktop");
        for data_dir in [None, Some(bad.clone())] {
            let app = open(data_dir, cx);
            assert!(app.read_with(cx, |app, _| matches!(app.screen, Screen::Failed(_))));
        }
        assert!(!bad.exists());
    }

    /// The app root on a real account (see `session::seam`).
    mod seam {
        use std::time::{Duration, Instant};

        use airserver_test_harness::utils::setup::TestBackend;

        use super::*;

        /// Deleting the account runs once at a time; done, the device is
        /// reset and onboarding says why (`tear_down`'s routing).
        #[gpui::test]
        #[ignore = "needs Postgres and MinIO (Air's compose file)"]
        fn seam_a_deleted_account_lands_on_onboarding(cx: &mut TestAppContext) {
            cx.executor().allow_parking();
            cx.update(|cx| {
                gpui_kit::init(cx);
                crate::runtime::init(cx).unwrap();
                crate::settings::SettingsStore::init(None, Default::default(), cx);
                theme::init(None, cx).unwrap();
            });
            let tokio = cx.update(|cx| Tokio::handle(cx));
            let (backend, user) = tokio.block_on(async {
                let mut backend = TestBackend::single().await;
                let user = backend.add_persisted_user().await;
                (backend, user)
            });
            let dir = backend.temp_dir().to_path_buf();
            let (_, root) = cx.update(|cx| {
                gpui_kit::open_window(Default::default(), cx, |window, cx| {
                    cx.new(|cx| AppRoot::with_account(dir.clone(), user, window, cx))
                })
                .unwrap()
            });
            let session = root.read_with(cx, |root, cx| match &root.screen {
                Screen::Ready { workspace, .. } => workspace.read(cx).session().cloned().unwrap(),
                _ => panic!("the workspace shows"),
            });
            let (first, second) = session.update(cx, |session, cx| {
                (session.delete_account(cx), session.delete_account(cx))
            });
            // Refused at once, while the first runs.
            assert_eq!(
                futures_util::FutureExt::now_or_never(second),
                Some(Err("Your account is being deleted already".to_string()))
            );
            drop(first);

            let deadline = Instant::now() + Duration::from_secs(20);
            let notice = loop {
                cx.run_until_parked();
                let notice = root.read_with(cx, |root, cx| match &root.screen {
                    Screen::Onboarding { view, .. } => Some(view.read(cx).notice().cloned()),
                    _ => None,
                });
                if let Some(notice) = notice {
                    break notice;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for onboarding"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            assert_eq!(
                notice.as_deref(),
                Some("Your Air account was deleted. This device has been reset.")
            );
            // Its data went with it.
            let records = tokio
                .block_on(
                    aircoreclient::clients::store::ClientRecord::load_all_from_air_db(
                        dir.to_str().unwrap(),
                    ),
                )
                .unwrap();
            assert!(records.is_empty(), "{records:?}");
            let _entered = tokio.enter();
            drop(backend);
        }
    }
}
