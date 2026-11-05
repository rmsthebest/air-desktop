//! First-run sign-up: server, display name and (when the server asks for
//! one) an invitation code. One screen, keyboard first: Tab moves between
//! fields, Enter in any field signs up.

use std::time::Duration;

use aircoreclient::clients::CoreUser;
use gpui::{EventEmitter, SharedString};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Subscription, Task,
    Window, div, px,
};

use crate::runtime::Tokio;
use crate::session::account::{self, SignUp, SignUpError};

/// How long typing in the server field settles before the server is asked
/// what sign-up needs. As upstream.
const DOMAIN_SETTLE: Duration = Duration::from_millis(500);
/// Upstream's default server.
const DEFAULT_DOMAIN: &str = "air.ms";

pub enum OnboardingEvent {
    SignedUp(CoreUser),
}

pub struct Onboarding {
    db_path: String,
    domain: Entity<InputState>,
    display_name: Entity<InputState>,
    code: Entity<InputState>,
    show_code: bool,
    busy: bool,
    error: Option<SharedString>,
    /// Pending registration-info lookup; replaced (and so cancelled) on
    /// every edit of the server field.
    info_task: Option<Task<()>>,
    sign_up_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OnboardingEvent> for Onboarding {}

impl Focusable for Onboarding {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.display_name.focus_handle(cx)
    }
}

impl Onboarding {
    pub fn new(db_path: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let domain = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(DEFAULT_DOMAIN)
                .placeholder(DEFAULT_DOMAIN)
        });
        let display_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("How others see you"));
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("Invitation code"));

        let submit_on_enter = |this: &mut Self,
                               _: &Entity<InputState>,
                               event: &InputEvent,
                               window: &mut Window,
                               cx: &mut Context<Self>| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(window, cx);
            }
        };
        let subscriptions = vec![
            cx.subscribe_in(
                &domain,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => this.domain_changed(cx),
                    InputEvent::PressEnter { .. } => this.submit(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(&display_name, window, submit_on_enter),
            cx.subscribe_in(&code, window, submit_on_enter),
        ];
        // The server is first contacted when the user edits the field or
        // signs up, not merely because the app was opened.
        Self {
            db_path,
            domain,
            display_name,
            code,
            show_code: false,
            busy: false,
            error: None,
            info_task: None,
            sign_up_task: None,
            _subscriptions: subscriptions,
        }
    }

    fn domain_changed(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.lookup_registration_info(cx);
        cx.notify();
    }

    /// Asks the server whether sign-up needs an invitation code, to show the
    /// field up front. Best effort: sign-up reports it anyway.
    fn lookup_registration_info(&mut self, cx: &mut Context<Self>) {
        let domain = self.domain.read(cx).value().to_string();
        let Ok(domain) = account::parse_domain(&domain) else {
            self.info_task = None;
            return;
        };
        self.info_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DOMAIN_SETTLE).await;
            let info = cx.update(|cx| Tokio::spawn(cx, account::registration_info(domain)));
            let Ok(info) = info.await else {
                return;
            };
            _ = this.update(cx, |this, cx| {
                // Only ever reveal the field: once asked for, keep it.
                if account::needs_code(info.as_ref()) {
                    this.show_code = true;
                    cx.notify();
                }
            });
        }));
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let sign_up = SignUp {
            domain: self.domain.read(cx).value().to_string(),
            display_name: self.display_name.read(cx).value().to_string(),
            invitation_code: self
                .show_code
                .then(|| self.code.read(cx).value().to_string()),
        };
        // Cheap checks first, without a round trip.
        if let Err(err) = account::parse_domain(&sign_up.domain) {
            return self.fail(err, window, cx);
        }
        if sign_up.display_name.trim().is_empty() {
            return self.fail(SignUpError::EmptyDisplayName, window, cx);
        }

        self.busy = true;
        self.error = None;
        cx.notify();
        let task = Tokio::spawn(cx, account::sign_up(self.db_path.clone(), sign_up));
        self.sign_up_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = match task.await {
                Ok(result) => result,
                Err(err) => Err(SignUpError::Other(format!("{err:#}"))),
            };
            _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(user) => cx.emit(OnboardingEvent::SignedUp(user)),
                    Err(err) => this.fail(err, window, cx),
                }
                cx.notify();
            });
        }));
    }

    /// Shows the error and puts focus on the field that can fix it.
    fn fail(&mut self, err: SignUpError, window: &mut Window, cx: &mut Context<Self>) {
        let field = match err {
            SignUpError::InvalidDomain | SignUpError::ChallengeUnsupported => &self.domain,
            SignUpError::EmptyDisplayName => &self.display_name,
            SignUpError::CodeRequired | SignUpError::CodeRejected => {
                self.show_code = true;
                &self.code
            }
            SignUpError::Other(_) => &self.display_name,
        };
        field.focus_handle(cx).focus(window, cx);
        self.error = Some(err.to_string().into());
        cx.notify();
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    #[cfg(test)]
    pub fn shows_code(&self) -> bool {
        self.show_code
    }
}

fn field(label: &'static str, input: &Entity<InputState>, cx: &gpui::App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(Input::new(input))
        .into_any_element()
}

impl Render for Onboarding {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let card = v_flex()
            .key_context("Onboarding")
            .w(px(420.))
            .gap_4()
            .p_8()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Welcome to Air"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Create an account on a server to get started."),
                    ),
            )
            .child(field("Display name", &self.display_name, cx))
            .child(field("Server", &self.domain, cx))
            .when(self.show_code, |card| {
                card.child(field("Invitation code", &self.code, cx))
            })
            .when_some(self.error.clone(), |card, error| {
                card.child(div().text_sm().text_color(theme.danger).child(error))
            })
            .child(
                Button::new("sign-up")
                    .primary()
                    .label(if self.busy {
                        "Signing up…"
                    } else {
                        "Sign up"
                    })
                    .loading(self.busy)
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            );
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .child(card)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;

    fn setup(cx: &mut TestAppContext) -> (Entity<Onboarding>, &mut VisualTestContext) {
        cx.executor().allow_parking();
        let (window, onboarding) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::runtime::init(cx).unwrap();
            // Nothing is looked up until the server field is edited.
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| Onboarding::new(String::new(), window, cx))
            })
            .unwrap()
        });
        let cx = VisualTestContext::from_window(window, cx).into_mut();
        cx.update(|window, cx| onboarding.focus_handle(cx).focus(window, cx));
        cx.run_until_parked();
        (onboarding, cx)
    }

    fn error(onboarding: &Entity<Onboarding>, cx: &mut VisualTestContext) -> Option<String> {
        onboarding.read_with(cx, |this, _| this.error().map(|e| e.to_string()))
    }

    #[gpui::test]
    fn form_is_filled_and_validated_by_keyboard(cx: &mut TestAppContext) {
        let (onboarding, cx) = setup(cx);

        // Enter with no name: caught locally, focus stays on the name.
        cx.simulate_keystrokes("enter");
        assert_eq!(
            error(&onboarding, cx),
            Some(SignUpError::EmptyDisplayName.to_string())
        );
        // Tab to the server field and make it invalid (nothing is looked up
        // for an invalid server), then Shift+Tab back and fill in the name.
        cx.simulate_keystrokes(&format!("tab {MOD}-a"));
        cx.simulate_input("not a server!");
        cx.simulate_keystrokes("shift-tab");
        cx.simulate_input("Ada");
        cx.simulate_keystrokes("enter");
        assert_eq!(
            error(&onboarding, cx),
            Some(SignUpError::InvalidDomain.to_string())
        );
        assert!(!onboarding.read_with(cx, |this, _| this.busy));
    }

    #[gpui::test]
    fn server_answers_route_focus_to_the_field_that_fixes_them(cx: &mut TestAppContext) {
        let (onboarding, cx) = setup(cx);
        let fail = |err: SignUpError, cx: &mut VisualTestContext| {
            onboarding.update_in(cx, |this, window, cx| this.fail(err, window, cx));
        };
        let focused = |input: fn(&Onboarding) -> &Entity<InputState>,
                       cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                input(onboarding.read(cx))
                    .focus_handle(cx)
                    .is_focused(window)
            })
        };

        assert!(!onboarding.read_with(cx, |this, _| this.shows_code()));
        for err in [SignUpError::CodeRequired, SignUpError::CodeRejected] {
            fail(err, cx);
            assert!(onboarding.read_with(cx, |this, _| this.shows_code()));
            assert!(focused(|this| &this.code, cx));
        }
        fail(SignUpError::ChallengeUnsupported, cx);
        assert!(focused(|this| &this.domain, cx));
        // Typing a new name doesn't hide the code field again.
        assert!(onboarding.read_with(cx, |this, _| this.shows_code()));
    }

    const MOD: &str = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
}
