//! First run: sign up (server, display name and, when the server asks for
//! one, an invitation code), or link this device into an existing account
//! (`secondary-l`): it shows a code, as a QR code and as digits, that a
//! device already in the account enters. Keyboard first: Tab moves between
//! fields, Enter signs up or (in the server field, linking) asks for a code
//! there; Escape leaves linking.

use std::rc::Rc;
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

use crate::actions::{CancelLink, LinkExistingAccount};
use crate::runtime::Tokio;
use crate::session::account::{self, SignUp, SignUpError};
use crate::session::devices::{self, Provision, ProvisionEvent};
use crate::ui::qr::{QrModules, qr_code};

/// How long typing in the server field settles before the server is asked
/// what sign-up needs. As upstream.
const DOMAIN_SETTLE: Duration = Duration::from_millis(500);
/// Upstream's default server.
const DEFAULT_DOMAIN: &str = "air.ms";

pub enum OnboardingEvent {
    /// Signed up, or linked into an existing account.
    SignedUp(CoreUser),
}

/// Where linking this device stands.
enum Linking {
    /// Waiting for the user to pick the server (or after a failure).
    Idle,
    Connecting,
    /// Showing a code; an unanswered one is replaced after a minute.
    Code {
        code: SharedString,
        qr: Option<Rc<QrModules>>,
    },
    /// The other device answered; the user confirms there.
    Answered,
}

pub struct Onboarding {
    db_path: String,
    /// Why the user is here again (the last account was unlinked).
    notice: Option<SharedString>,
    /// `Some` while linking to an existing account instead of signing up.
    linking: Option<Linking>,
    /// The link in progress; dropping it cancels the link.
    provision: Option<(Provision, Task<()>)>,
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
    #[cfg(all(test, feature = "seam"))]
    pub fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    pub fn new(
        db_path: String,
        notice: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
                    InputEvent::PressEnter { .. } if this.linking.is_some() => {
                        this.start_linking(window, cx)
                    }
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
            notice,
            linking: None,
            provision: None,
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
        if self.linking.is_none() {
            self.lookup_registration_info(cx);
        }
        cx.notify();
    }

    fn link_existing_account(
        &mut self,
        _: &LinkExistingAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.linking.is_some() {
            return;
        }
        self.linking = Some(Linking::Idle);
        self.info_task = None;
        // The name field isn't shown while linking: focus stays in the card.
        self.domain.focus_handle(cx).focus(window, cx);
        self.start_linking(window, cx);
    }

    /// Asks the server in the server field for a linking code, ending any
    /// link already in progress (the core deletes what it made).
    fn start_linking(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.provision = None;
        self.error = None;
        let domain = match account::parse_domain(self.domain.read(cx).value().as_ref()) {
            Ok(domain) => domain,
            Err(err) => {
                self.linking = Some(Linking::Idle);
                return self.fail(err, window, cx);
            }
        };
        self.linking = Some(Linking::Connecting);
        let (provision, mut events) = devices::provision(self.db_path.clone(), domain.clone(), cx);
        let receive = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.recv().await {
                let done = matches!(event, ProvisionEvent::Linked(_) | ProvisionEvent::Failed);
                let shown = this.update_in(cx, |this, window, cx| {
                    this.linking = Some(match event {
                        ProvisionEvent::Code(code) => Linking::Code {
                            qr: QrModules::encode(&devices::linking_url(&domain, &code))
                                .map(Rc::new),
                            code: devices::spaced(&code).into(),
                        },
                        ProvisionEvent::Linking => Linking::Answered,
                        ProvisionEvent::Linked(user) => {
                            this.provision = None;
                            cx.emit(OnboardingEvent::SignedUp(user));
                            Linking::Answered
                        }
                        ProvisionEvent::Failed => {
                            this.provision = None;
                            this.error = Some(
                                "Linking failed. Check the server and your connection, \
                                 then press Enter to try again."
                                    .into(),
                            );
                            this.domain.focus_handle(cx).focus(window, cx);
                            Linking::Idle
                        }
                    });
                    cx.notify();
                });
                if done || shown.is_err() {
                    return;
                }
            }
        });
        self.provision = Some((provision, receive));
        cx.notify();
    }

    /// Back to signing up, cancelling any link in progress.
    fn cancel_link(&mut self, _: &CancelLink, window: &mut Window, cx: &mut Context<Self>) {
        if self.linking.take().is_none() {
            return;
        }
        self.provision = None;
        self.error = None;
        self.display_name.focus_handle(cx).focus(window, cx);
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
        if self.busy || self.linking.is_some() {
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

    fn render_linking(&self, linking: &Linking, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = |text: &'static str| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let body: AnyElement = match linking {
            Linking::Idle => {
                muted("Enter the account's server, then press Enter for a code.").into_any_element()
            }
            Linking::Connecting => muted("Asking the server for a code…").into_any_element(),
            Linking::Code { code, qr } => v_flex()
                .gap_3()
                .items_center()
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    "On a device already in your account, open Air, go to \
                             Devices and choose Link a device. Scan this code, or \
                             enter the digits.",
                ))
                .when_some(qr.clone(), |column, qr| column.child(qr_code(qr, px(232.))))
                .child(
                    div()
                        .text_2xl()
                        .font_family("monospace")
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(code.clone()),
                )
                .child(muted("A new code appears every minute until one is used."))
                .into_any_element(),
            Linking::Answered => muted("Confirm on your other device. Linking…").into_any_element(),
        };
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Link to an existing account"),
                    )
                    .child(muted("Use this device with an account you already have.")),
            )
            .child(field("Server", &self.domain, cx))
            .child(body)
            .when_some(self.error.clone(), |card, error| {
                card.child(div().text_sm().text_color(theme.danger).child(error))
            })
            .child(
                Button::new("back-to-sign-up")
                    .label("Back to sign-up (Esc)")
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.cancel_link(&CancelLink, window, cx)
                        }),
                    ),
            )
            .into_any_element()
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
        let content = match &self.linking {
            Some(linking) => self.render_linking(linking, cx),
            None => self.render_sign_up(cx),
        };
        let theme = cx.theme();
        let card = v_flex()
            .key_context("Onboarding")
            .on_action(cx.listener(Self::link_existing_account))
            .on_action(cx.listener(Self::cancel_link))
            .w(px(420.))
            .gap_4()
            .p_8()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .when_some(self.notice.clone(), |card, notice| {
                card.child(div().text_sm().text_color(theme.warning).child(notice))
            })
            .child(content);
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .child(card)
    }
}

impl Onboarding {
    fn render_sign_up(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .gap_4()
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
            )
            .child(
                Button::new("link-existing")
                    .label("Link to an existing account (Ctrl+L)")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.link_existing_account(&LinkExistingAccount, window, cx)
                    })),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;

    fn setup(cx: &mut TestAppContext) -> (Entity<Onboarding>, &mut VisualTestContext) {
        setup_in(String::new(), cx)
    }

    fn setup_in(
        db_path: String,
        cx: &mut TestAppContext,
    ) -> (Entity<Onboarding>, &mut VisualTestContext) {
        cx.executor().allow_parking();
        let (window, onboarding) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::runtime::init(cx).unwrap();
            crate::settings::SettingsStore::init(None, Default::default(), cx);
            assert_eq!(crate::keymap::load(None, cx), Vec::<String>::new());
            // Nothing is looked up until the server field is edited.
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| Onboarding::new(db_path, None, window, cx))
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

    #[gpui::test]
    fn linking_is_entered_and_left_by_keyboard(cx: &mut TestAppContext) {
        let (onboarding, cx) = setup(cx);
        let linking =
            |cx: &mut VisualTestContext| onboarding.read_with(cx, |this, _| this.linking.is_some());
        // A server that can't be one: asked for nothing, focus goes there.
        cx.simulate_keystrokes(&format!("tab {MOD}-a"));
        cx.simulate_input("not a server!");
        cx.simulate_keystrokes(&format!("shift-tab {MOD}-l"));
        assert!(linking(cx));
        assert_eq!(
            error(&onboarding, cx),
            Some(SignUpError::InvalidDomain.to_string())
        );
        assert!(cx.update(|window, cx| {
            onboarding
                .read(cx)
                .domain
                .focus_handle(cx)
                .is_focused(window)
        }));
        assert!(onboarding.read_with(cx, |this, _| this.provision.is_none()));
        // Enter there retries linking, never signs up.
        cx.simulate_keystrokes("enter");
        assert!(linking(cx) && !onboarding.read_with(cx, |this, _| this.busy));
        cx.simulate_keystrokes("escape");
        assert!(!linking(cx));
        assert_eq!(error(&onboarding, cx), None);
    }

    #[gpui::test]
    fn escape_leaves_a_link_that_is_under_way(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (onboarding, cx) = setup_in(dir.path().to_str().unwrap().into(), cx);
        // A valid server (on a reserved name, so it is never reached).
        cx.simulate_keystrokes(&format!("tab {MOD}-a"));
        cx.simulate_input("air.invalid");
        cx.simulate_keystrokes(&format!("shift-tab {MOD}-l"));
        assert!(onboarding.read_with(cx, |this, _| this.provision.is_some()));
        cx.simulate_keystrokes("escape");
        assert!(onboarding.read_with(cx, |this, _| this.linking.is_none()
            && this.provision.is_none()));
    }

    const MOD: &str = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
}
