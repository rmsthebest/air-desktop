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
use crate::session::{Session, account};
use crate::settings::{Appearance, SettingsStore};
use crate::theme;
use crate::ui::Workspace;
use crate::ui::onboarding::{DEFAULT_DOMAIN, Onboarding, OnboardingEvent};

enum Screen {
    Loading,
    Onboarding {
        view: Entity<Onboarding>,
        _subscription: Subscription,
    },
    Ready(Entity<Workspace>),
    Failed(SharedString),
}

pub struct AppRoot {
    data_dir: PathBuf,
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
            screen: Screen::Loading,
            _subscriptions: subscriptions,
        };
        match data_dir
            .ok_or_else(|| "No data directory: is $HOME set?".to_string())
            .and_then(|dir| prepare_data_dir(&dir))
        {
            Ok(db_path) => this.load(db_path, window, cx),
            Err(err) => this.screen = Screen::Failed(err.into()),
        }
        this
    }

    fn load(&mut self, db_path: String, window: &mut Window, cx: &mut Context<Self>) {
        let task = Tokio::spawn(cx, account::load_default(db_path.clone()));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await.and_then(|loaded| loaded);
            _ = this.update_in(cx, |this, window, cx| {
                // Opening the account database creates it with the umask.
                this.restrict_files();
                match result {
                    Ok(Some(user)) => this.start(user, window, cx),
                    Ok(None) => this.onboard(db_path, window, cx),
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

    fn onboard(&mut self, db_path: String, window: &mut Window, cx: &mut Context<Self>) {
        let onboarding = cx.new(|cx| Onboarding::new(db_path, DEFAULT_DOMAIN, window, cx));
        let subscription =
            cx.subscribe_in(
                &onboarding,
                window,
                |this, _, event, window, cx| match event {
                    OnboardingEvent::SignedUp(user) => this.start(user.clone(), window, cx),
                },
            );
        onboarding.focus_handle(cx).focus(window, cx);
        self.screen = Screen::Onboarding {
            view: onboarding,
            _subscription: subscription,
        };
    }

    fn start(&mut self, user: CoreUser, window: &mut Window, cx: &mut Context<Self>) {
        // Sign-up created the account's databases with the umask.
        self.restrict_files();
        let session = Session::start(user, cx);
        let workspace = cx.new(|cx| Workspace::new(Some(session), window, cx));
        self.screen = Screen::Ready(workspace);
        cx.notify();
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
    paths::ensure_private_dir(dir)
        .map_err(|err| format!("Couldn't create {}: {err}", dir.display()))?;
    paths::restrict_files(dir)
        .map_err(|err| format!("Couldn't secure {}: {err}", dir.display()))?;
    // The core builds `sqlite://{db_path}/…` URLs from it.
    dir.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{} is not valid UTF-8", dir.display()))
}

impl Render for AppRoot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let content: AnyElement = match &self.screen {
            Screen::Loading => div().into_any_element(),
            Screen::Onboarding { view, .. } => view.clone().into_any_element(),
            Screen::Ready(workspace) => workspace.clone().into_any_element(),
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
