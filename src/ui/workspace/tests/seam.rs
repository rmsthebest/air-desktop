//! Workspace tests on a real session: Air's core talking to its in-process
//! test server (`airserver_test_harness`), which needs Postgres and MinIO
//! from Air's compose file. Ignored by default, like the session's seam
//! tests: `cargo test seam -- --ignored`.

use std::time::{Duration, Instant};

use aircoreclient::clients::CoreUser;
use airserver_test_harness::utils::setup::TestBackend;

use super::*;
use crate::runtime::Tokio;

/// A backend and a workspace signed in as its first user.
struct Seam<'a> {
    /// The runtime the core and the backend run on: a test's own steps
    /// (the peer's side) are `block_on` it. Kept, so `drop` never needs
    /// the app (it may run while a panic unwinds out of an update).
    tokio: tokio::runtime::Handle,
    /// Taken in `drop`: the backend shuts down on tokio.
    backend: Option<TestBackend>,
    user: CoreUser,
    workspace: Entity<Workspace>,
    cx: &'a mut VisualTestContext,
}

fn start(cx: &mut TestAppContext) -> Seam<'_> {
    start_after(cx, async |_, _| {})
}

/// Like [`start`], with `prepare` run on the account before its session
/// starts.
fn start_after(
    cx: &mut TestAppContext,
    prepare: impl AsyncFnOnce(&mut TestBackend, &CoreUser),
) -> Seam<'_> {
    cx.executor().allow_parking();
    cx.update(|cx| crate::runtime::init(cx).unwrap());
    let tokio = cx.update(|cx| Tokio::handle(cx));
    let (backend, user) = tokio.block_on(async {
        let mut backend = TestBackend::single().await;
        let user = backend.add_user().await;
        prepare(&mut backend, &user).await;
        (backend, user)
    });
    let session_user = user.clone();
    let (window, workspace) = cx.update(|cx| {
        gpui_kit::init(cx);
        assert_eq!(SettingsStore::open(None, cx), None);
        theme::init(None, cx).unwrap();
        assert_eq!(keymap::load(None, cx), Vec::<String>::new());
        let session = Session::start(session_user, cx);
        gpui_kit::open_window(Default::default(), cx, |window, cx| {
            cx.new(|cx| Workspace::new(Some(session), window, cx))
        })
        .unwrap()
    });
    let cx = VisualTestContext::from_window(window, cx).into_mut();
    cx.run_until_parked();
    Seam {
        tokio,
        backend: Some(backend),
        user,
        workspace,
        cx,
    }
}

impl Drop for Seam<'_> {
    fn drop(&mut self) {
        let _entered = self.tokio.enter();
        drop(self.backend.take());
    }
}

impl Seam<'_> {
    fn backend(&mut self) -> &mut TestBackend {
        self.backend
            .as_mut()
            .expect("the backend lives as long as the test")
    }

    fn tokio(&self) -> tokio::runtime::Handle {
        self.tokio.clone()
    }

    /// Lets the app run until `done` holds (real time: the core works on
    /// tokio threads), failing after a while.
    fn until(
        &mut self,
        what: &str,
        done: impl Fn(&Entity<Workspace>, &mut VisualTestContext) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            self.cx.run_until_parked();
            if done(&self.workspace, self.cx) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A small WebP picture.
fn webp() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(40, 30, image::Rgba([200, 30, 30, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::WebP)
        .unwrap();
    bytes.into_inner()
}

impl Seam<'_> {
    /// A new contact of the user's, listed under `name`.
    fn contact(&mut self) -> (CoreUser, aircoreclient::ChatId, String) {
        let tokio = self.tokio();
        let user = self.user.clone();
        let backend = self.backend();
        let peer = tokio.block_on(backend.add_user());
        let chat = tokio.block_on(backend.connect_users(&user, &peer));
        let name = tokio
            .block_on(user.user_profile(peer.user_id()))
            .display_name
            .to_string();
        let listed = name.clone();
        self.until("the contact listed", move |workspace, cx| {
            titles(workspace, cx).contains(&listed)
        });
        (peer, chat, name)
    }

    /// `peer` sends the user a picture in `chat`.
    fn send_picture(&mut self, peer: &CoreUser, chat: aircoreclient::ChatId) {
        let tokio = self.tokio();
        let user = self.user.clone();
        tokio
            .block_on(
                self.backend()
                    .send_attachment(chat, peer, &[&user], &webp(), "red.webp"),
            )
            .unwrap();
    }

    /// Opens the chat listed as `title`, its messages focused.
    fn open(&mut self, title: &str) {
        let id = self
            .workspace
            .read_with(self.cx, |workspace, cx| {
                workspace
                    .chats(cx)
                    .into_iter()
                    .find(|chat| chat.title == title)
                    .map(|chat| chat.id)
            })
            .unwrap();
        self.workspace.update(self.cx, |workspace, cx| {
            workspace.chat_list.update(cx, |list, cx| list.open(id, cx))
        });
        self.cx.simulate_keystrokes(&format!("{MOD}-m"));
    }

    /// Waits until the open chat's last message is an image ready to show,
    /// and puts the cursor on it.
    fn until_picture_shown(&mut self) {
        self.until("the picture", |workspace, cx| {
            workspace.read_with(cx, |workspace, cx| {
                let conversation = workspace.conversation.read(cx);
                conversation.messages_entity().is_some_and(|messages| {
                    messages.read(cx).messages().last().is_some_and(|message| {
                        matches!(&message.body, crate::model::MessageBody::Attachment(a) if a.image.is_some())
                    })
                })
            })
        });
        self.cx.simulate_keystrokes("end");
    }

    fn has_dialog(&mut self) -> bool {
        self.cx.update(|window, cx| window.has_active_dialog(cx))
    }
}

/// An image still opening when the user opens another chat never shows
/// over it, and `v` there opens that chat's own (gates 21c and 21d).
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_an_image_opening_stays_with_its_chat(cx: &mut TestAppContext) {
    let mut seam = start(cx);
    let (bob, bob_chat, bob_name) = seam.contact();
    let (carol, carol_chat, carol_name) = seam.contact();
    seam.send_picture(&bob, bob_chat);
    seam.send_picture(&carol, carol_chat);

    seam.open(&bob_name);
    seam.until_picture_shown();
    // `v`, then another chat before the image arrives.
    seam.act_then_open(
        Box::new(crate::actions::ViewImage),
        |workspace, cx| workspace.conversation.read(cx).is_viewing(),
        &carol_name,
    );
    // Long enough for the load to have finished.
    seam.settle();
    assert!(!seam.has_dialog(), "Bob's image showed over Carol's chat");

    seam.cx.simulate_keystrokes(&format!("{MOD}-m"));
    seam.until_picture_shown();
    seam.cx.simulate_keystrokes("v");
    seam.until("Carol's image", |_, cx| {
        cx.update(|window, cx| window.has_active_dialog(cx))
    });
}

/// Everything the user was told until `done` holds.
fn told_until(seam: &mut Seam<'_>, what: &str, done: impl Fn(&[String]) -> bool) -> Vec<String> {
    let told = std::cell::RefCell::new(Vec::new());
    seam.until(what, |_, _| {
        told.borrow_mut().extend(crate::ui::take_told());
        done(&told.borrow())
    });
    told.into_inner()
}

/// A deletion aborted in the background (while the app was closed) is
/// told once, and recorded as told (gate 18).
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_a_background_deletion_abort_is_told_once(cx: &mut TestAppContext) {
    use airserver_test_harness::utils::core_user_ext::CoreUserExt as _;

    let mut seam = start_after(cx, async |backend, user| {
        // As upstream's test: with a sibling, a failing deletion is
        // retried in the background until it's given up.
        let _sibling = user.add_synced_device().await;
        let mut attempts = 0;
        while !user.account_deletion_is_aborted().await.unwrap() {
            attempts += 1;
            assert!(attempts <= 20, "the deletion must be given up");
            user.delete_account().await.unwrap();
            backend.listener_control_handle().set_fail_next_request();
            user.outbound_service().run_once().await;
        }
        assert!(user.has_unreported_account_deletion_abort().await.unwrap());
    });
    const TOLD: &str = "Your Air account couldn't be deleted. You can try again.";
    let told = told_until(&mut seam, "the abort told", |told| {
        told.iter().any(|told| told == TOLD)
    });
    assert_eq!(told.iter().filter(|told| *told == TOLD).count(), 1);
    let tokio = seam.tokio();
    let user = seam.user.clone();
    seam.until("the abort recorded as told", |_, _| {
        !tokio
            .block_on(user.has_unreported_account_deletion_abort())
            .unwrap()
    });

    // A later look (the own user changed) doesn't tell it again.
    tokio
        .block_on(user.set_own_user_profile(aircoreclient::UserProfile {
            display_name: "Renamed".parse().unwrap(),
            ..tokio.block_on(user.own_user_profile()).unwrap()
        }))
        .unwrap();
    seam.until("the new name", |workspace, cx| {
        workspace.read_with(cx, |workspace, cx| {
            workspace.session().unwrap().read(cx).display_name() == "Renamed"
        })
    });
    let settled = Instant::now() + Duration::from_millis(500);
    while Instant::now() < settled {
        seam.cx.run_until_parked();
        let told = crate::ui::take_told();
        assert!(
            !told.iter().any(|told| told == TOLD),
            "told again: {told:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Seam<'_> {
    fn listed(&mut self, title: &str) -> ChatId {
        self.workspace.read_with(self.cx, |workspace, cx| {
            workspace
                .chats(cx)
                .into_iter()
                .find(|chat| chat.title == title)
                .unwrap()
                .id
        })
    }

    /// Asks for `action` in the open chat, then opens `next` before any
    /// result can arrive: the action (deferred) runs as the first update
    /// ends, and the app's tasks run only once the test lets them.
    /// `started` tells that the action is under way (so a refusal or a
    /// missed dispatch can't pass for "dropped").
    fn act_then_open(
        &mut self,
        action: Box<dyn gpui::Action>,
        started: impl Fn(&Workspace, &App) -> bool,
        next: &str,
    ) {
        let next = self.listed(next);
        self.cx
            .update(|window, cx| window.dispatch_action(action, cx));
        assert!(
            self.workspace
                .read_with(self.cx, |workspace, cx| started(workspace, cx)),
            "the action didn't start"
        );
        self.workspace.update(self.cx, |workspace, cx| {
            workspace
                .chat_list
                .update(cx, |list, cx| list.open(next, cx))
        });
    }

    /// Runs the app for half a second (real time), for results to land.
    fn settle(&mut self) {
        let settled = Instant::now() + Duration::from_millis(500);
        while Instant::now() < settled {
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A safety code that arrives once another chat is open is dropped; the
/// open chat's own shows (gate 16).
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_a_late_safety_code_is_dropped(cx: &mut TestAppContext) {
    let mut seam = start(cx);
    let (_bob, _, bob_name) = seam.contact();
    let (_carol, _, carol_name) = seam.contact();
    seam.open(&bob_name);
    seam.act_then_open(
        Box::new(crate::actions::ShowSafetyCode),
        |workspace, _| workspace.safety_code.is_some(),
        &carol_name,
    );
    seam.settle();
    assert!(!seam.has_dialog(), "Bob's code showed in Carol's chat");

    seam.cx.simulate_keystrokes(&format!("{MOD}-shift-s"));
    seam.until("Carol's code", |_, cx| {
        cx.update(|window, cx| window.has_active_dialog(cx))
    });
}
