//! Workspace tests on a real session: Air's core talking to its in-process
//! test server (`airserver_test_harness`), which needs Postgres and MinIO
//! from Air's compose file. Ignored by default, like the session's seam
//! tests: `just seam` (`cargo test --features seam seam -- --ignored`;
//! without the feature nothing runs).

use std::time::{Duration, Instant};

use aircoreclient::clients::CoreUser;
use airserver_test_harness::utils::setup::TestBackend;

use super::*;
use crate::runtime::Tokio;
use crate::session::SessionEvent;

/// The recipients a test's peer names when sending: none, so the session's
/// own listener receives it (the harness draining the same queue would race
/// with it).
const NOBODY: &[&CoreUser] = &[];

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
        tokio
            .block_on(
                self.backend()
                    .send_attachment(chat, peer, NOBODY, &webp(), "red.webp"),
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

/// A peer's GIF arrives as an animation (the core tells it moves, we decode
/// the original), plays, freezes past its loops, and `p` plays it again.
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_a_gif_plays_then_freezes_until_replayed(cx: &mut TestAppContext) {
    let mut seam = start(cx);
    let (bob, chat, bob_name) = seam.contact();
    let gif = small_gif();
    let tokio = seam.tokio();
    tokio
        .block_on(
            seam.backend()
                .send_attachment(chat, &bob, NOBODY, &gif, "moves.gif"),
        )
        .unwrap();
    seam.open(&bob_name);
    seam.until("the animation", |workspace, cx| {
        workspace.read_with(cx, |workspace, cx| {
            shown_attachment(workspace, cx).is_some_and(|a| a.animation.is_some())
        })
    });
    seam.cx.simulate_keystrokes("end");
    let plays = |seam: &mut Seam<'_>| {
        seam.workspace.read_with(seam.cx, |workspace, cx| {
            let attachment = shown_attachment(workspace, cx).unwrap();
            workspace.conversation.read(cx).plays(&attachment, cx)
        })
    };
    assert!(plays(&mut seam), "it starts playing");
    // Two 30 ms frames, three times.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!plays(&mut seam), "it froze past its loops");
    seam.cx.simulate_keystrokes("p");
    assert!(plays(&mut seam), "`p` plays it again");
}

/// The newest images get the animation slots, whatever their order of
/// arrival.
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_the_newest_gifs_get_the_animation_slots(cx: &mut TestAppContext) {
    let mut seam = start(cx);
    let (bob, chat, bob_name) = seam.contact();
    let gif = small_gif();
    let tokio = seam.tokio();
    for n in 0..8 {
        tokio
            .block_on(seam.backend().send_attachment(
                chat,
                &bob,
                NOBODY,
                &gif,
                &format!("moves-{n}.gif"),
            ))
            .unwrap();
    }
    seam.open(&bob_name);
    let moving = |workspace: &Entity<Workspace>, cx: &mut VisualTestContext| {
        workspace.read_with(cx, |workspace, cx| {
            let conversation = workspace.conversation.read(cx);
            let Some(messages) = conversation.messages_entity() else {
                return Vec::new();
            };
            messages
                .read(cx)
                .messages()
                .iter()
                .filter_map(|message| match &message.body {
                    crate::model::MessageBody::Attachment(a) => Some(a.animation.is_some()),
                    _ => None,
                })
                .collect::<Vec<bool>>()
        })
    };
    seam.until("the newest six moving", |workspace, cx| {
        moving(workspace, cx) == [false, false, true, true, true, true, true, true]
    });

    // A newer one takes the slot of the oldest that moved.
    tokio
        .block_on(
            seam.backend()
                .send_attachment(chat, &bob, NOBODY, &gif, "moves-8.gif"),
        )
        .unwrap();
    seam.until("the slot passed on", |workspace, cx| {
        moving(workspace, cx) == [false, false, false, true, true, true, true, true, true]
    });
}

/// An image we refuse to play holds a slot, as it was read and decoded: the
/// limit on work per window holds, with a stock client's files. So six of
/// them keep the rest of a chat still, and five don't.
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_refused_animations_hold_their_slots(cx: &mut TestAppContext) {
    let mut seam = start(cx);
    let (bob, bob_chat, bob_name) = seam.contact();
    let (carol, carol_chat, carol_name) = seam.contact();
    let small = small_gif();
    // Past the frame limit, each frame unlike the last, as the sender's core
    // merges frames that look alike: a frame's pixels spell out its number.
    let mut refused = Vec::new();
    let mut encoder = image::codecs::gif::GifEncoder::new(&mut refused);
    let frames = (0..300u32).map(|n| {
        let image = image::RgbaImage::from_fn(16, 16, |x, y| {
            let bit = (n >> ((x + y) % 9)) & 1;
            image::Rgba([(bit * 255) as u8, 0, 0, 255])
        });
        image::Frame::from_parts(image, 0, 0, image::Delay::from_numer_denom_ms(30, 1))
    });
    encoder.encode_frames(frames).unwrap();
    drop(encoder);
    let tokio = seam.tokio();
    let mut send = |peer: &CoreUser, chat, file: &Vec<u8>| {
        tokio
            .block_on(
                seam.backend()
                    .send_attachment(chat, peer, NOBODY, file, "moves.gif"),
            )
            .unwrap();
    };
    send(&bob, bob_chat, &small);
    send(&carol, carol_chat, &small);
    for _ in 0..5 {
        send(&bob, bob_chat, &refused);
        send(&carol, carol_chat, &refused);
    }
    send(&carol, carol_chat, &refused);

    let shown = |workspace: &Entity<Workspace>, cx: &mut VisualTestContext| {
        workspace.read_with(cx, |workspace, cx| {
            let conversation = workspace.conversation.read(cx);
            let Some(messages) = conversation.messages_entity() else {
                return (0, 0);
            };
            let attachments = || {
                messages
                    .read(cx)
                    .messages()
                    .iter()
                    .filter_map(|message| match &message.body {
                        crate::model::MessageBody::Attachment(a) => Some(a),
                        _ => None,
                    })
            };
            (
                attachments().filter(|a| a.image.is_some()).count(),
                attachments().filter(|a| a.animation.is_some()).count(),
            )
        })
    };
    seam.open(&bob_name);
    seam.until("the small one moving past five refused", |workspace, cx| {
        shown(workspace, cx) == (6, 1)
    });

    seam.open(&carol_name);
    seam.until("all seven previewed", |workspace, cx| {
        shown(workspace, cx).0 == 7
    });
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        seam.cx.run_until_parked();
        let moving = shown(&seam.workspace, seam.cx).1;
        assert_eq!(moving, 0, "six refused files leave no slot");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn small_gif() -> Vec<u8> {
    let mut gif = Vec::new();
    let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
    let frames = (0..2u8).map(|n| {
        image::Frame::from_parts(
            image::RgbaImage::from_pixel(8, 8, image::Rgba([n * 200, 20, 30, 255])),
            0,
            0,
            image::Delay::from_numer_denom_ms(30, 1),
        )
    });
    encoder.encode_frames(frames).unwrap();
    drop(encoder);
    gif
}

/// The attachment of the open chat's newest message.
fn shown_attachment(workspace: &Workspace, cx: &App) -> Option<crate::model::Attachment> {
    let conversation = workspace.conversation.read(cx);
    let messages = conversation.messages_entity()?.read(cx).messages();
    match &messages.last()?.body {
        crate::model::MessageBody::Attachment(attachment) => Some(attachment.clone()),
        _ => None,
    }
}

/// The server's word on this version: an announced end reminds until
/// dismissed (a new date reminds again), a refusal stops everything.
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_the_server_can_end_this_version(cx: &mut TestAppContext) {
    use crate::session::queue::VersionStatus;
    let mut seam = start(cx);
    let set = |seam: &mut Seam<'_>, version| {
        seam.workspace.update(seam.cx, |workspace, cx| {
            let session = workspace.session().cloned().unwrap();
            session.update(cx, |session, cx| session.set_version_state(version, cx));
        });
        seam.cx.run_until_parked();
    };
    let expires =
        |days| VersionStatus::ExpiresAt(chrono::Utc::now() + chrono::Duration::days(days));
    let reminds = |seam: &mut Seam<'_>| {
        seam.workspace.read_with(seam.cx, |workspace, cx| {
            workspace.update_reminder(cx).is_some()
        })
    };
    assert!(!reminds(&mut seam));
    set(&mut seam, expires(30));
    assert!(reminds(&mut seam));
    seam.cx.dispatch_action(DismissUpdateReminder);
    seam.cx.run_until_parked();
    assert!(!reminds(&mut seam), "dismissed");
    set(&mut seam, expires(60));
    assert!(reminds(&mut seam), "a new date reminds again");
}

/// A chat behind the settings or the update screen isn't being looked at,
/// so its alerts aren't held back; showing it again counts again.
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_what_hides_the_chat_ends_attention(cx: &mut TestAppContext) {
    use crate::session::queue::VersionStatus;
    let mut seam = start(cx);
    let (_peer, _, name) = seam.contact();
    seam.open(&name);
    let session = seam.workspace.read_with(seam.cx, |workspace, _| {
        workspace.session().cloned().unwrap()
    });
    let looked_at = |seam: &mut Seam<'_>| {
        seam.cx.run_until_parked();
        session.read_with(seam.cx, |session, _| {
            session.attention().open_chat.is_some()
        })
    };
    assert!(looked_at(&mut seam));
    let set = |seam: &mut Seam<'_>, version| {
        session.update(seam.cx, |session, cx| {
            session.set_version_state(version, cx)
        })
    };
    set(&mut seam, VersionStatus::Unsupported);
    assert!(!looked_at(&mut seam), "behind the update screen");
    set(&mut seam, VersionStatus::Supported);
    assert!(looked_at(&mut seam), "shown again");
    seam.workspace.update_in(seam.cx, |workspace, window, cx| {
        workspace.open_settings(&OpenSettings, window, cx)
    });
    assert!(!looked_at(&mut seam), "behind the settings");
}

/// Linking over an info page, then leaving the link, lands on the page: it
/// is what shows, and the rows under it would take the keys unseen.
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_leaving_a_link_returns_to_the_page_under_it(cx: &mut TestAppContext) {
    let seam = start(cx);
    seam.workspace.update_in(seam.cx, |workspace, window, cx| {
        workspace.show_info(crate::about::Page::About, window, cx);
        workspace.start_link("1234 5678 9", window, cx);
    });
    seam.cx.run_until_parked();
    let (link, info) = seam.workspace.read_with(seam.cx, |workspace, cx| {
        (
            workspace.link_pane().unwrap().clone(),
            workspace.info.as_ref().unwrap().0.focus_handle(cx),
        )
    });
    link.update(seam.cx, |_, cx| cx.emit(LinkViewEvent::Close));
    seam.cx.run_until_parked();
    seam.workspace.read_with(seam.cx, |workspace, _| {
        assert!(workspace.link_pane().is_none());
    });
    seam.cx
        .update(|window, _| assert!(info.is_focused(window), "the page has the keys"));
}

/// Changes made while the app is busy reach the views as one event, however
/// many there were (gates 4-7).
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_a_burst_of_changes_arrives_batched(cx: &mut TestAppContext) {
    const BURST: usize = 5;
    let mut seam = start(cx);
    let (_peer, chat, _) = seam.contact();
    seam.settle();
    let session = seam.workspace.read_with(seam.cx, |workspace, _| {
        workspace.session().cloned().unwrap()
    });
    let batches = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _subscription = seam.cx.update({
        let batches = batches.clone();
        |_, cx| {
            cx.subscribe(&session, move |_, event: &SessionEvent, _| {
                let SessionEvent::Changed { notifications, .. } = event;
                batches.borrow_mut().push(notifications.len());
            })
        }
    });
    // The app's tasks don't run during this.
    let user = seam.user.clone();
    seam.tokio().block_on(async move {
        for n in 0..BURST {
            let draft = aircoreclient::MessageDraft {
                message: format!("draft {n}"),
                in_reply_to: None,
                editing_id: None,
                updated_at: chrono::Utc::now(),
                is_committed: false,
            };
            user.store_message_draft(chat, Some(&draft)).await.unwrap();
        }
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(batches.borrow().is_empty());
    seam.cx.run_until_parked();
    let batches = batches.borrow();
    assert!(
        batches.first().is_some_and(|first| *first >= BURST),
        "{batches:?}"
    );
}

/// A stored draft fills an empty composer when the chat opens, and never
/// replaces what the user typed before it loaded (gate 7).
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_a_draft_fills_only_an_empty_composer(cx: &mut TestAppContext) {
    let mut seam = start(cx);
    let (_bob, bob_chat, bob) = seam.contact();
    let (_carol, carol_chat, carol) = seam.contact();
    let user = seam.user.clone();
    seam.tokio().block_on(async move {
        for chat in [bob_chat, carol_chat] {
            let draft = aircoreclient::MessageDraft {
                message: "stored".into(),
                in_reply_to: None,
                editing_id: None,
                updated_at: chrono::Utc::now(),
                is_committed: true,
            };
            user.store_message_draft(chat, Some(&draft)).await.unwrap();
        }
    });
    let text = |seam: &mut Seam<'_>| {
        seam.workspace.read_with(seam.cx, |workspace, cx| {
            workspace.conversation.read(cx).composer_text(cx)
        })
    };
    seam.open(&bob);
    seam.until("the draft", |workspace, cx| {
        workspace.read_with(cx, |workspace, cx| {
            workspace.conversation.read(cx).composer_text(cx) == "stored"
        })
    });

    let carol = seam.listed(&carol);
    // Separate updates: the switch happens as the first one ends, and the app's
    // tasks, which would deliver the draft, run only once the test lets them.
    seam.workspace.update(seam.cx, |workspace, cx| {
        workspace
            .chat_list
            .update(cx, |list, cx| list.open(carol, cx))
    });
    seam.workspace.update_in(seam.cx, |workspace, window, cx| {
        workspace.conversation.update(cx, |conversation, cx| {
            conversation.set_composer_text("typed", window, cx)
        });
    });
    seam.settle();
    assert_eq!(text(&mut seam), "typed");
}

/// A new safety code request replaces one still being looked up: one dialog,
/// not one for each (gate 16).
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_a_new_safety_code_request_replaces_a_pending_one(cx: &mut TestAppContext) {
    let mut seam = start(cx);
    let (_bob, _, bob) = seam.contact();
    seam.open(&bob);
    seam.workspace.update_in(seam.cx, |workspace, window, cx| {
        workspace.safety_code("", window, cx);
        assert!(workspace.safety_code.is_some(), "the first is under way");
        workspace.safety_code("", window, cx);
    });
    seam.until("the code", |_, cx| {
        cx.update(|window, cx| window.has_active_dialog(cx))
    });
    seam.settle();
    seam.workspace.read_with(seam.cx, |workspace, _| {
        assert_eq!(workspace.safety_codes_shown, [bob.as_str()]);
    });
}

/// While the self chat is unknown, a chat change looks it up. One arriving
/// during a lookup asks for one more after it, and doesn't replace it: busy
/// chats could otherwise starve the lookup (gate 15e).
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_a_change_during_a_self_chat_lookup_looks_again(cx: &mut TestAppContext) {
    use aircoreclient::db::notification::{DbEntityId, DbNotification};
    let mut seam = start(cx);
    let (session, devices) = seam.workspace.read_with(seam.cx, |workspace, _| {
        (
            workspace.session().cloned().unwrap(),
            workspace.devices.clone().unwrap(),
        )
    });
    seam.until("the devices", {
        let devices = devices.clone();
        move |_, cx| devices.read_with(cx, |devices, _| devices.knows_self_chat())
    });
    let chat_changed = |seam: &mut Seam<'_>| {
        session.update(seam.cx, |_, cx| {
            let id = aircoreclient::ChatId::new(uuid::Uuid::from_u128(1));
            cx.emit(SessionEvent::Changed {
                notifications: vec![std::sync::Arc::new(DbNotification {
                    ops: [(DbEntityId::Chat(id), Default::default())].into(),
                })],
                overflowed: false,
            })
        });
    };

    // With none running, a change starts one, and finding the chat reloads.
    devices.update(seam.cx, |devices, _| devices.forget_self_chat());
    chat_changed(&mut seam);
    seam.until("the self chat again", {
        let devices = devices.clone();
        move |_, cx| devices.read_with(cx, |devices, _| devices.knows_self_chat())
    });
    devices.read_with(seam.cx, |devices, _| assert!(!devices.looks_again()));

    devices.update(seam.cx, |devices, _| {
        devices.forget_self_chat();
        devices.hold_lookup();
    });
    chat_changed(&mut seam);
    seam.cx.run_until_parked();
    devices.read_with(seam.cx, |devices, _| {
        assert!(devices.looks_again());
        assert!(!devices.knows_self_chat(), "the running one isn't replaced");
    });
}

/// The window pages both ways: one page per request however often it's asked
/// for, in order, without gaps, up to the newest (gates 4-7).
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_the_message_window_pages_both_ways(cx: &mut TestAppContext) {
    use crate::session::messages::PAGE;
    const TOTAL: usize = 3 * PAGE;
    // Sent before the session starts: its outbound service, running beside
    // these sends, would stamp some of them later, and so reorder them.
    let mut seeded = None;
    let mut seam = start_after(cx, async |backend, user| {
        let bob = backend.add_user().await;
        let chat = backend.connect_users(user, &bob).await;
        let name = user.user_profile(bob.user_id()).await.display_name;
        let mut oldest = None;
        for n in 0..TOTAL {
            let content =
                mimi_content::MimiContent::simple_markdown_message(format!("m{n}"), [n as u8; 16]);
            let sent = Box::pin(user.send_message(
                chat,
                content,
                None,
                aircoreclient::clients::MarkChatAsRead::Yes,
            ))
            .await
            .unwrap();
            oldest.get_or_insert(sent.id());
        }
        seeded = Some((name.to_string(), oldest.unwrap()));
    });
    let (bob, oldest) = seeded.unwrap();
    seam.until("the chat", {
        let bob = bob.clone();
        move |workspace, cx| titles(workspace, cx).contains(&bob)
    });
    seam.open(&bob);
    // The numbers of the messages shown, checked to be in order without gaps.
    let shown = |seam: &mut Seam<'_>| -> std::ops::RangeInclusive<usize> {
        seam.workspace.read_with(seam.cx, |workspace, cx| {
            let conversation = workspace.conversation.read(cx);
            let numbers: Vec<usize> = conversation
                .messages_entity()
                .map(|messages| messages.read(cx).messages().to_vec())
                .unwrap_or_default()
                .iter()
                .filter_map(|message| message.plain_text().strip_prefix('m')?.parse().ok())
                .collect();
            let (Some(first), Some(last)) = (numbers.first(), numbers.last()) else {
                return 1..=0;
            };
            let expected: Vec<usize> = (*first..=*last).collect();
            assert_eq!(numbers, expected, "out of order or with a gap");
            *first..=*last
        })
    };
    let messages = |seam: &mut Seam<'_>| {
        seam.workspace.read_with(seam.cx, |workspace, cx| {
            workspace
                .conversation
                .read(cx)
                .messages_entity()
                .cloned()
                .unwrap()
        })
    };
    let wait = |seam: &mut Seam<'_>, what: &str, done: &dyn Fn(usize, usize) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            seam.cx.run_until_parked();
            let range = shown(seam);
            if !range.is_empty() && done(*range.start(), *range.end()) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out: {what}, at {range:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    wait(&mut seam, "the newest page", &|_, last| last == TOTAL - 1);
    let first = *shown(&mut seam).start();
    assert!(first > 0, "the oldest are left out");

    // Asked for twice, as scrolling does: one page.
    let list = messages(&mut seam);
    list.update(seam.cx, |list, _| {
        list.load_older();
        list.load_older();
    });
    wait(&mut seam, "the older page", &|now, _| now < first);
    seam.settle();
    assert_eq!(*shown(&mut seam).start(), first - PAGE, "a page, once");

    // Away from the newest, then back up a page at a time.
    let oldest = crate::model::MessageId(oldest.uuid().to_string().into());
    seam.workspace.update(seam.cx, |workspace, cx| {
        let list = workspace.conversation.read(cx).messages_entity().cloned();
        list.unwrap()
            .update(cx, |list, cx| list.jump_to_message(&oldest, cx));
    });
    wait(&mut seam, "the oldest", &|now, _| now == 0);
    let last = *shown(&mut seam).end();
    assert!(last < TOTAL - 1, "the newest are left out");
    let list = messages(&mut seam);
    list.update(seam.cx, |list, _| {
        list.load_newer();
        list.load_newer();
    });
    wait(&mut seam, "the newer page", &|_, now| now > last);
    seam.settle();
    assert_eq!(*shown(&mut seam).end(), last + PAGE, "a page, once");
    while *shown(&mut seam).end() < TOTAL - 1 {
        let before = *shown(&mut seam).end();
        list.update(seam.cx, |list, _| list.load_newer());
        wait(&mut seam, "another page", &|_, now| now > before);
    }
}

/// `:debug-info` shows this account's id, from the core.
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_debug_info_comes_from_the_core(cx: &mut TestAppContext) {
    let mut seam = start(cx);
    let id = seam.user.user_id().uuid().to_string();
    seam.workspace.update_in(seam.cx, |workspace, window, cx| {
        workspace.show_info(crate::about::Page::Debug, window, cx)
    });
    seam.until("the debug info", move |workspace, cx| {
        workspace.read_with(cx, |workspace, cx| {
            let (view, _) = workspace.info.as_ref().unwrap();
            view.read(cx)
                .shown_text()
                .is_some_and(|text| text.contains(&id))
        })
    });
}

/// `o` on a reply loads the window around its original when that isn't
/// loaded, and the cursor lands on it.
#[gpui::test]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
fn seam_a_reply_jumps_to_an_original_outside_the_window(cx: &mut TestAppContext) {
    let mut seam = start(cx);
    let (_bob, chat, bob_name) = seam.contact();
    let tokio = seam.tokio();
    let user = seam.user.clone();
    tokio.block_on(async move {
        let send = |text: String, reply_to: Option<aircommon::identifiers::MimiId>, n: u8| {
            let user = user.clone();
            async move {
                let mut content = mimi_content::MimiContent::simple_markdown_message(text, [n; 16]);
                content.in_reply_to = reply_to.map(Into::into);
                Box::pin(user.send_message(
                    chat,
                    content,
                    None,
                    aircoreclient::clients::MarkChatAsRead::Yes,
                ))
                .await
                .unwrap()
            }
        };
        // Older ones too: its place in the loaded window is then in the
        // middle, where a reveal needs the heights it doesn't have yet.
        for n in 100..170 {
            send(format!("older {n}"), None, n).await;
        }
        let original = send("the original".into(), None, 0).await.id();
        let mimi_id = loop {
            let stored = user.message(original).await.unwrap().unwrap();
            if let Some(id) = stored.message().mimi_id() {
                break *id;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        // More than a page: opening at the newest leaves the original out.
        for n in 1..=70 {
            send(format!("filler {n}"), None, n).await;
        }
        send("the reply".into(), Some(mimi_id), 71).await;
    });
    seam.open(&bob_name);
    seam.until("the reply", |workspace, cx| {
        workspace.read_with(cx, |workspace, cx| {
            let conversation = workspace.conversation.read(cx);
            conversation.messages_entity().is_some_and(|messages| {
                let messages = messages.read(cx).messages();
                messages
                    .last()
                    .is_some_and(|message| message.plain_text() == "the reply")
                    && !messages
                        .iter()
                        .any(|message| message.plain_text() == "the original")
            })
        })
    });
    seam.cx.simulate_keystrokes("end");
    seam.cx.simulate_keystrokes("o");
    seam.until("the original under the cursor", |workspace, cx| {
        workspace.read_with(cx, |workspace, cx| {
            let conversation = workspace.conversation.read(cx);
            let Some(messages) = conversation.messages_entity() else {
                return false;
            };
            let Some(cursor) = conversation.cursor() else {
                return false;
            };
            let on_it = messages
                .read(cx)
                .messages()
                .get(cursor)
                .is_some_and(|message| message.plain_text() == "the original");
            // On it, and on screen: the cursor alone proves neither.
            on_it && conversation.is_in_view(cursor)
        })
    });
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
