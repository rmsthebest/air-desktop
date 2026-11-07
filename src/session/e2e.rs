//! End-to-end checks against a local Air server (see CLAUDE.md). Ignored by
//! default; run with an unused invitation code:
//!
//! ```sh
//! AIR_E2E_CODE=XXXXXXXX cargo test e2e -- --ignored
//! ```
//!
//! With `AIR_E2E_DATA_DIR` set, the account is kept there, so the app can
//! be started on it: `AIR_DESKTOP_DATA_DIR=<same dir> cargo run`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use super::account::{self, SignUp, SignUpError};
use super::alerts::{Alert, Alerts, Attention, Dismiss, Notice, NoticeQueue};
use super::chats::{self, Update};
use super::contacts::{self, Failure};
use super::pending::Pending;
use super::queue::{self, ConnectionStatus, QueueEvent};
use super::{attachments, composer, devices, groups, messages, safety};
use crate::model::{AttachmentState, Message, MessageBody, MessageStatus, Quote};
use crate::settings::Notifications;
use aircommon::identifiers::UserId as CoreUserId;
use aircoreclient::clients::CoreUser;
use aircoreclient::clients::store::{ClientRecord, ClientRecordState};
use aircoreclient::{ChatStatus, ChatType};
use gpui::SharedString;

fn sign_up(code: Option<&str>) -> SignUp {
    SignUp {
        domain: "localhost".into(),
        display_name: "Ada Lovelace".into(),
        invitation_code: code.map(Into::into),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a local Air server and AIR_E2E_CODE"]
async fn e2e_sign_up_connect_and_reload() {
    let code = std::env::var("AIR_E2E_CODE").expect("set AIR_E2E_CODE to an invitation code");
    let dir = tempfile::tempdir().unwrap();
    let data = std::env::var_os("AIR_E2E_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dir.path().join("data"));
    crate::paths::ensure_private_dir(&data).unwrap();
    let db_path = data.to_str().unwrap().to_string();

    // The local server is gated: a missing code is caught once the server
    // says it wants one, without registering; a wrong one is turned down.
    let err = account::sign_up(db_path.clone(), sign_up(None)).await.err();
    assert_eq!(err, Some(SignUpError::CodeRequired));
    let err = account::sign_up(db_path.clone(), sign_up(Some("WRONG000")))
        .await
        .err();
    assert_eq!(err, Some(SignUpError::CodeRejected));

    let user = account::sign_up(db_path.clone(), sign_up(Some(&code)))
        .await
        .unwrap();
    let user_id = user.user_id().clone();

    // The rejected attempt left an unfinished registration behind; the
    // successful sign-up discarded it.
    let records = ClientRecord::load_all_from_air_db(&db_path).await.unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].client_record_state, ClientRecordState::Finished);

    // Database files end up private once the app restricts them.
    crate::paths::restrict_files(&data).unwrap();
    #[cfg(unix)]
    for entry in std::fs::read_dir(&data).unwrap() {
        use std::os::unix::fs::PermissionsExt as _;
        let entry = entry.unwrap();
        let mode = entry.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{:?}", entry.file_name());
    }

    // The queue listener connects and reports it.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (alerts, _attention, _notices) = alerts();
    let listener = tokio::spawn(queue::run(user.clone(), alerts, move |status| {
        let _ = tx.send(status);
    }));
    let connected = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(status) = rx.recv().await {
            if status == QueueEvent::Status(ConnectionStatus::Connected) {
                return true;
            }
        }
        false
    })
    .await;
    assert_eq!(connected, Ok(true));

    // The chat list loads, then follows database changes: a new group
    // shows up, titled and in the order.
    let pending = Arc::new(Pending::default());
    let (deltas_tx, mut deltas) = mpsc::channel(8);
    let mut notifications = user.db_notifications();
    let worker = tokio::spawn(chats::run(user.clone(), pending.clone(), deltas_tx));
    let Some(Update::Delta(initial)) = deltas.recv().await else {
        panic!("the initial load failed");
    };
    assert!(initial.order.is_some());
    let message_changes = Arc::new(Pending::default());
    let forward = tokio::spawn({
        let pending = pending.clone();
        let message_changes = message_changes.clone();
        async move {
            use tokio_stream::StreamExt as _;
            while let Some(notification) = notifications.next().await {
                pending.add(std::slice::from_ref(&notification), false);
                message_changes.add(&[notification], false);
            }
        }
    });
    let group = user
        .create_chat("Lab notes".into(), None, false)
        .await
        .unwrap();
    let group_id = group;
    let group = chats::model_id(&group);
    let shown = loop {
        let delta = next_delta(&mut deltas).await;
        let chat = delta.upserted.iter().find(|chat| chat.id == group);
        if let (Some(chat), Some(order)) = (chat, &delta.order) {
            assert!(order.contains(&group));
            break chat.clone();
        }
    };
    assert_eq!(shown.title, "Lab notes");
    assert!(shown.is_group);

    // A rename reaches the list, and its notice becomes the preview.
    user.set_chat_title(group_id, "Field notes".into())
        .await
        .unwrap();
    let renamed = loop {
        let delta = next_delta(&mut deltas).await;
        let chat = delta.upserted.into_iter().find(|chat| chat.id == group);
        if let Some(chat) = chat.filter(|chat| chat.title == "Field notes") {
            break chat;
        }
    };
    let preview = renamed.preview.expect("the rename has a preview");
    assert!(preview.text.contains("Field notes"), "{preview:?}");

    // The group's history opens with its system messages, and follows
    // new ones live.
    let (messages_worker, mut updates) =
        messages::start(user.clone(), group_id, message_changes.clone());
    let initial = next_window(&mut updates).await;
    let [messages::Change::Replace(history)] = &initial.changes[..] else {
        panic!("the history opens with a replace: {:?}", initial.changes);
    };
    assert!(
        history
            .iter()
            .any(|m| m.is_event() && m.plain_text().contains("Field notes")),
        "{history:?}"
    );
    user.set_chat_title(group_id, "Bench notes".into())
        .await
        .unwrap();
    loop {
        let update = next_window(&mut updates).await;
        let inserted = update.changes.iter().any(|change| match change {
            messages::Change::Insert { items, .. } => {
                items.iter().any(|m| m.plain_text().contains("Bench notes"))
            }
            _ => false,
        });
        if inserted {
            break;
        }
    }

    // Sending: the message lands in the history as our own.
    let sent = composer::send(&user, group_id, "hello **lab**".into(), None).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    let mine = next_message(&mut updates, |m| m.plain_text() == "hello **lab**").await;
    assert!(mine.own && mine.reply_to.is_none());
    let mine_id = messages::core_message_id(&mine.id).unwrap();

    // Sent messages can be edited and replied to, once the outbound
    // service has sent it.
    sent_mimi_id(&user, mine_id).await;
    // Editing patches it in place.
    let edit = composer::Draft {
        editing: Some(mine_id),
        ..composer::Draft::empty()
    };
    let sent = composer::send(&user, group_id, "hello again".into(), Some(&edit)).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    let edited = next_message(&mut updates, |m| m.edited).await;
    assert_eq!(
        (edited.id.clone(), edited.plain_text()),
        (mine.id.clone(), "hello again".into())
    );

    // A reply quotes it as edited.
    let mimi_id = sent_mimi_id(&user, mine_id).await;
    let quote = Quote {
        sender: "You".into(),
        text: "hello again".into(),
    };
    let reply = composer::Draft {
        reply_to: Some((mimi_id, quote.clone())),
        ..composer::Draft::empty()
    };
    let sent = composer::send(&user, group_id, "a reply".into(), Some(&reply)).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    let replied = next_message(&mut updates, |m| m.plain_text() == "a reply").await;
    assert_eq!(replied.reply_to, Some(quote.clone()));

    // A stored draft comes back, with what it replies to.
    let draft = composer::Draft {
        text: "unsent".into(),
        committed: true,
        ..reply
    };
    user.store_message_draft(group_id, Some(&draft.to_core()))
        .await
        .unwrap();
    let loaded_draft = composer::load_draft(user.clone(), group_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            loaded_draft.text.as_str(),
            loaded_draft.reply_to.clone().map(|(_, q)| q)
        ),
        ("unsent", Some(quote))
    );
    // Drafts go through one queue: a load queued right after a store (a
    // chat reopened just after leaving it) reads what was stored.
    let (queue, run) = composer::DraftQueue::new(user.clone());
    let queue_task = tokio::spawn(run);
    let left = composer::Draft {
        text: "left behind".into(),
        committed: true,
        ..composer::Draft::empty()
    };
    let (store_user, stored) = (user.clone(), left.to_core());
    drop(queue.run(async move {
        store_user
            .store_message_draft(group_id, Some(&stored))
            .await
            .unwrap();
    }));
    let reopened = queue
        .run(composer::load_draft(user.clone(), group_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.map(|d| d.text).as_deref(), Some("left behind"));
    queue_task.abort();
    user.store_message_draft(group_id, Some(&loaded_draft.to_core()))
        .await
        .unwrap();

    // Sending it clears the stored draft (by the version stored).
    let sent = composer::send(&user, group_id, "sent at last".into(), Some(&loaded_draft)).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    assert!(user.message_draft(group_id).await.unwrap().is_none());

    // A reply quotes the version it answered, but not one deleted since.
    sent_mimi_id(&user, mine_id).await;
    user.delete_message_for_everyone(group_id, mine_id)
        .await
        .unwrap();
    let requoted = next_message(&mut updates, |m| m.id == replied.id).await;
    let quote = requoted.reply_to.expect("still a reply");
    assert_eq!(quote.text, "Deleted message");
    messages_worker.abort();

    // Changes the worker never hears about (dropped notifications) are
    // caught by a full reload: the deleted group leaves the list.
    forward.abort();
    user.delete_and_erase_chat(group_id).await.unwrap();
    pending.add(&[], true);
    let removed = loop {
        let delta = next_delta(&mut deltas).await;
        if delta.removed.contains(&group) {
            break delta;
        }
    };
    assert!(!removed.upserted.iter().any(|chat| chat.id == group));
    worker.abort();
    listener.abort();
    drop(user);

    // A restart finds the same account and its profile on disk.
    let reloaded = account::load_default(db_path).await.unwrap().unwrap();
    assert_eq!(reloaded.user_id(), &user_id);
    let profile = reloaded.own_user_profile().await.unwrap();
    assert_eq!(profile.display_name.to_string(), "Ada Lovelace");
}

async fn next_delta(deltas: &mut mpsc::Receiver<Update>) -> chats::Delta {
    let update = tokio::time::timeout(Duration::from_secs(10), deltas.recv()).await;
    match update.expect("no delta within 10s") {
        Some(Update::Delta(delta)) => delta,
        _ => panic!("the chat list worker failed"),
    }
}

async fn next_window(updates: &mut mpsc::Receiver<messages::Update>) -> messages::WindowUpdate {
    let update = tokio::time::timeout(Duration::from_secs(10), updates.recv()).await;
    match update.expect("no message update within 10s") {
        Some(messages::Update::Window(update)) => update,
        Some(messages::Update::Failed(err)) => panic!("the message worker failed: {err}"),
        None => panic!("the message worker stopped"),
    }
}

/// Waits for a message matching `wanted` to be inserted or patched.
async fn next_message(
    updates: &mut mpsc::Receiver<messages::Update>,
    wanted: impl Fn(&Message) -> bool,
) -> Message {
    loop {
        for change in next_window(updates).await.changes {
            let found = match change {
                messages::Change::Insert { items, .. } => items.into_iter().find(|m| wanted(m)),
                messages::Change::Patch { item, .. } => Some(item).filter(|m| wanted(m)),
                _ => None,
            };
            if let Some(message) = found {
                return message;
            }
        }
    }
}

/// The message's id for replies, once the outbound service has sent it
/// (messages can only be edited and replied to once sent).
async fn sent_mimi_id(
    user: &aircoreclient::clients::CoreUser,
    id: aircoreclient::MessageId,
) -> aircommon::identifiers::MimiId {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let message = user.message(id).await.unwrap().unwrap();
            if message.is_sent()
                && let Some(id) = message.message().mimi_id()
            {
                return *id;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the message was never sent")
}

/// Two accounts on the local server connect through a username and talk.
/// Needs two unused invitation codes:
/// `AIR_E2E_CODES=XXXXXXXX,YYYYYYYY cargo test e2e_two -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a local Air server and AIR_E2E_CODES"]
async fn e2e_two_users_connect_by_username_and_talk() {
    let codes = std::env::var("AIR_E2E_CODES").expect("set AIR_E2E_CODES to two codes");
    let [ada_code, bob_code] = codes.split(',').collect::<Vec<_>>()[..] else {
        panic!("AIR_E2E_CODES takes two comma-separated codes");
    };
    let dir = tempfile::tempdir().unwrap();
    let ada = new_account(dir.path(), "Ada Lovelace", ada_code).await;
    let bob = new_account(dir.path(), "Bob", bob_code).await;
    let (ada_alerts, _, ada_notices) = alerts();
    let (bob_alerts, bob_attention, bob_notices) = alerts();
    let _queues = [
        listen_queue(&ada, ada_alerts.clone()).await,
        listen_queue(&bob, bob_alerts).await,
    ];

    // Ada's username listener reports her usernames as they change.
    let (names_tx, mut names) = mpsc::unbounded_channel();
    let _ada_usernames = Aborting(tokio::spawn(contacts::run(
        ada.clone(),
        ada_alerts,
        move |usernames| {
            let _ = names_tx.send(usernames);
        },
    )));
    assert!(next(&mut names).await.is_empty());
    let name = format!("ada-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    // A new account's privacy pass tokens arrive in the background; until
    // then the server is asked again, as the user would.
    let typed = format!(" @{} ", name.to_uppercase());
    let added = eventually("Ada gets her username", async || {
        contacts::add_username(ada.clone(), typed.clone())
            .await
            .ok()
    })
    .await;
    assert_eq!(added, name, "stored as typed, normalized");
    while next(&mut names).await != [SharedString::from(name.clone())] {}

    // Refusals are explained, not internal errors.
    fn refused<T>(result: Result<T, Failure>) -> bool {
        matches!(result, Err(Failure::Shown(_)))
    }
    assert!(refused(
        contacts::add_username(ada.clone(), name.clone()).await
    ));
    // Someone else's username is refused as taken, not as Bob's own.
    let taken = eventually(
        "Bob's claim is answered",
        async || match contacts::add_username(bob.clone(), name.clone()).await {
            Err(Failure::Shown(message)) => Some(message),
            _ => None,
        },
    )
    .await;
    assert!(taken.ends_with("is taken"), "{taken}");
    assert!(refused(
        contacts::add_contact(ada.clone(), name.clone()).await
    ));
    assert!(refused(
        contacts::add_contact(bob.clone(), "nobody-e2e-0".into()).await
    ));

    // Bob asks; asking twice is refused.
    let bob_chat = eventually("Bob sends a request", async || {
        contacts::add_contact(bob.clone(), name.clone()).await.ok()
    })
    .await;
    assert!(refused(
        contacts::add_contact(bob.clone(), name.clone()).await
    ));
    let bob_chat = messages::core_chat_id(&bob_chat);

    // Ada's listener stores the request as a pending chat, which she
    // accepts; Bob's chat then becomes a connection.
    let request = eventually("Ada gets the request", async || {
        for id in ada.ordered_chat_ids().await.unwrap() {
            if let Some(chat) = ada.chat(&id).await
                && matches!(chat.chat_type, ChatType::PendingConnection(_))
            {
                return Some(id);
            }
        }
        None
    })
    .await;
    // The request alerted Ada, naming who asked.
    let alert = next_alert(&ada_notices, |alert| alert.chat == request).await;
    assert_eq!(alert.title, "New contact request from Bob");

    let ada_chat = contacts::accept_request(ada.clone(), request)
        .await
        .unwrap();
    let ada_chat = messages::core_chat_id(&ada_chat);
    eventually("Bob's request is accepted", async || {
        let chat = bob.chat(&bob_chat).await?;
        matches!(chat.chat_type, ChatType::Connection(_)).then_some(())
    })
    .await;

    // Messages go both ways, named by their sender.
    let (_ada_worker, mut ada_updates, _) = open_messages(&ada, ada_chat);
    let (_bob_worker, mut bob_updates, bob_pending) = open_messages(&bob, bob_chat);
    let sent = composer::send(&bob, bob_chat, "hi ada".into(), None).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    let got = next_message(&mut ada_updates, |m| m.plain_text() == "hi ada").await;
    assert_eq!((got.own, got.sender_name.as_ref()), (false, "Bob"));
    let got_hi_ada = got.clone();

    // Ada reading it shows as read on Bob's side.
    let read_id = messages::core_message_id(&got.id).unwrap();
    messages::mark_as_read(ada.clone(), ada_chat, read_id).await;
    next_message(&mut bob_updates, |m| {
        m.own && m.plain_text() == "hi ada" && m.status == MessageStatus::Read
    })
    .await;

    let sent = composer::send(&ada, ada_chat, "hi bob".into(), None).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    let got = next_message(&mut bob_updates, |m| m.plain_text() == "hi bob").await;
    assert_eq!(got.sender_name, "Ada Lovelace");
    let alert = next_alert(&bob_notices, |alert| alert.body == "hi bob").await;
    assert_eq!(
        (alert.chat, alert.title.as_str()),
        (bob_chat, "Ada Lovelace")
    );

    // The chat Bob is looking at doesn't alert; once he looks away, it does.
    let looking = |open_chat| Attention {
        window_active: true,
        open_chat,
        content: Notifications::Full,
    };
    bob_attention.send_replace(looking(Some(bob_chat)));
    // Alerts are built after a message is stored, so the attention can't
    // change right after "quiet" shows. Bob's listener handles one queue
    // event at a time: once "barrier" shows, "quiet" was either handled in
    // an earlier event, or shares barrier's, where only the newest message
    // of a chat is put into words. Either way "quiet" can't alert, though
    // "barrier" may.
    let mut loud = None;
    for text in ["quiet", "barrier", "loud"] {
        let sent = composer::send(&ada, ada_chat, text.into(), None).await;
        assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
        loud = Some(next_message(&mut bob_updates, |m| m.plain_text() == text).await);
        if text == "barrier" {
            bob_attention.send_replace(looking(None));
        }
    }
    let alert = next_alert(&bob_notices, |alert| {
        assert_ne!(alert.body, "quiet", "the open chat alerted");
        alert.body == "loud"
    })
    .await;
    assert_eq!(alert.chat, bob_chat);

    // A message Ada deletes leaves Bob's alert: it falls back to what is
    // still unread, so deleted text doesn't stay on screen. Once Bob has
    // read everything, a deletion takes the alert down.
    for (text, read_first) in [("secret", false), ("secret again", true)] {
        if read_first {
            let read = messages::core_message_id(&loud.as_ref().unwrap().id).unwrap();
            messages::mark_as_read(bob.clone(), bob_chat, read).await;
        }
        let sent = composer::send(&ada, ada_chat, text.into(), None).await;
        assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
        next_alert(&bob_notices, |alert| alert.body == text).await;
        let message = next_message(&mut ada_updates, |m| m.own && m.plain_text() == text).await;
        let id = messages::core_message_id(&message.id).unwrap();
        ada.delete_message_for_everyone(ada_chat, id).await.unwrap();
        let notice = next_notice(&bob_notices, |n| notice_chat(n) == Some(bob_chat)).await;
        if read_first {
            assert_eq!(notice, Notice::Dismiss(Dismiss::Chat(bob_chat)));
        } else {
            assert!(
                matches!(&notice, Notice::Refresh(alert) if alert.body == "loud"),
                "{notice:?}"
            );
        }
    }

    // Ada reacts to Bob's first message: Bob sees it on his message, and
    // is alerted. Toggling again retracts it.
    let hi = messages::core_message_id(&got_hi_ada.id).unwrap();
    let added = messages::set_reaction(&ada, ada_chat, hi, "👍".into(), true).await;
    assert!(added.is_ok(), "{added:?}");
    let reacted = next_message(&mut bob_updates, |m| {
        m.plain_text() == "hi ada" && !m.reactions.is_empty()
    })
    .await;
    assert_eq!(
        reacted.reactions,
        [crate::model::Reaction {
            label: "👍".into(),
            count: 1,
            own: false
        }]
    );
    let alert = next_alert(&bob_notices, |alert| alert.chat == bob_chat).await;
    assert_eq!(alert.body, "Ada Lovelace reacted 👍 to hi ada");
    // Bob dismisses it: nothing it stood for is alerted again, not even
    // in a rewrite after a change (as the retraction below).
    let until = alert.until.expect("the alert stands for the reaction");
    bob.set_chat_notified_until(bob_chat, until).await.unwrap();
    let chat = bob.chat(&bob_chat).await.unwrap();
    assert_eq!(super::alerts::current_alert(&bob, &chat).await, None);
    let removed = messages::set_reaction(&ada, ada_chat, hi, "👍".into(), false).await;
    assert!(removed.is_ok(), "{removed:?}");
    next_message(&mut bob_updates, |m| {
        m.plain_text() == "hi ada" && m.reactions.is_empty()
    })
    .await;
    // A variant stored under another raw string (say, from another of
    // Ada's clients) shows as the same reaction, and is retracted with it.
    ada.send_reaction(ada_chat, hi, "👍\u{200B}".into())
        .await
        .unwrap();
    next_message(&mut bob_updates, |m| {
        m.plain_text() == "hi ada" && !m.reactions.is_empty()
    })
    .await;
    messages::set_reaction(&ada, ada_chat, hi, "👍".into(), false)
        .await
        .unwrap();
    assert!(ada.message_reactions(hi).await.unwrap().is_empty());
    // Only messages of the chat named take reactions.
    let elsewhere = aircoreclient::MessageId::new(uuid::Uuid::new_v4());
    assert!(refused(
        messages::set_reaction(&ada, ada_chat, elsewhere, "👍".into(), true).await
    ));

    // Ada sends a file and an image. Her own image shows its preview at
    // once; Bob's device downloads both and makes the image's preview.
    let files = tempfile::tempdir().unwrap();
    let notes = files.path().join("notes.txt");
    std::fs::write(&notes, "attached").unwrap();
    let photo = files.path().join("photo.png");
    image::RgbaImage::from_pixel(40, 30, image::Rgba([200, 0, 0, 255]))
        .save(&photo)
        .unwrap();
    let attachment = |m: &Message| match &m.body {
        MessageBody::Attachment(attachment) => Some(attachment.clone()),
        _ => None,
    };
    attachments::send(ada.clone(), ada_chat, notes)
        .await
        .unwrap();
    attachments::send(ada.clone(), ada_chat, photo)
        .await
        .unwrap();
    assert!(refused(
        attachments::send(ada.clone(), ada_chat, files.path().to_path_buf()).await
    ));
    next_message(&mut ada_updates, |m| {
        attachment(m).is_some_and(|a| a.image.is_some())
    })
    .await;
    let _downloads = Aborting(tokio::spawn(attachments::download_all(
        bob.clone(),
        move |id| bob_pending.add(&[attachments::previewed(id)], false),
    )));
    // The uploads race: either may land first.
    let (mut file, mut picture) = (None, None);
    while file.is_none() || picture.is_none() {
        let message = next_message(&mut bob_updates, |m| {
            attachment(m).is_some_and(|a| a.state == AttachmentState::Ready)
        })
        .await;
        let got = attachment(&message).unwrap();
        match got.image {
            Some(image) => picture = Some(image),
            None if got.name == "notes.txt" => file = Some(message),
            None => {}
        }
    }
    let (file, picture) = (file.unwrap(), picture.unwrap());
    assert_eq!(picture.size(), (40, 30));
    // Saving copies the file, never over another; only this chat's files.
    let downloads = files.path().join("downloads");
    std::fs::create_dir(&downloads).unwrap();
    let file_id = messages::core_message_id(&file.id).unwrap();
    for expected in ["notes.txt", "notes (2).txt"] {
        let saved = attachments::save(
            bob.clone(),
            bob_chat,
            file_id,
            "notes.txt".into(),
            downloads.clone(),
        )
        .await
        .unwrap();
        assert_eq!(saved, downloads.join(expected));
        assert_eq!(std::fs::read(&saved).unwrap(), b"attached");
    }
    let elsewhere = aircoreclient::ChatId {
        uuid: uuid::Uuid::new_v4(),
    };
    assert!(refused(
        attachments::save(bob.clone(), elsewhere, file_id, "x".into(), downloads).await
    ));

    // Bob's rename reaches the messages Ada already has from him.
    assert!(refused(
        super::set_display_name(bob.clone(), " \t ".into()).await
    ));
    let renamed = super::set_display_name(bob.clone(), "  Robert ".into()).await;
    assert_eq!(renamed.unwrap(), "Robert");
    next_message(&mut ada_updates, |m| {
        m.plain_text() == "hi ada" && m.sender_name == "Robert"
    })
    .await;

    // Ada starts a group and invites Bob by (the start of) his name.
    let group = groups::create(&ada, "Engines").await.unwrap();
    assert!(refused(groups::create(&ada, "  ").await));
    let invited = eventually("Bob's features are known", async || {
        groups::invite(ada.clone(), group, "rob".into()).await.ok()
    })
    .await;
    assert_eq!(invited, "Robert");
    assert!(refused(
        groups::invite(ada.clone(), group, "robert".into()).await
    ));
    let bob_group = eventually("Bob joins the group", async || {
        for id in bob.ordered_chat_ids().await.unwrap() {
            if let Some(chat) = bob.chat(&id).await
                && matches!(chat.chat_type, ChatType::Group(_))
                && !bob.chat_is_self_group(&chat.group_id).await.unwrap()
            {
                return Some(id);
            }
        }
        None
    })
    .await;

    // Both see both members, themselves first. Groups are created with the
    // core's trusted policy: any member may remove another.
    let members = |user: CoreUser, chat| async move {
        let members = groups::load(&user, chat).await.unwrap_or_default();
        members
            .into_iter()
            .map(|(member, _)| (member.name.to_string(), member.own, member.can_remove))
            .collect::<Vec<_>>()
    };
    let ada_view = vec![
        ("You".to_string(), true, false),
        ("Robert".to_string(), false, true),
    ];
    assert_eq!(members(ada.clone(), group).await, ada_view);
    let bob_view = vec![
        ("You".to_string(), true, false),
        ("Ada Lovelace".to_string(), false, true),
    ];
    eventually("Bob sees Ada in the group", async || {
        let seen = members(bob.clone(), bob_group).await;
        (seen == bob_view).then_some(())
    })
    .await;

    // A rename reaches Bob.
    groups::rename(&ada, group, "Difference Engines")
        .await
        .unwrap();
    eventually("Bob sees the new name", async || {
        let chat = bob.chat(&bob_group).await?;
        (chat.attributes()?.title == "Difference Engines").then_some(())
    })
    .await;

    // Ada removes Bob: his group ends, and he can only delete it.
    let (_, bob_id) = groups::load(&ada, group)
        .await
        .unwrap()
        .into_iter()
        .find(|(member, _)| !member.own)
        .unwrap();
    ada.remove_users(group, vec![bob_id]).await.unwrap();
    eventually("Bob is removed", async || {
        let chat = bob.chat(&bob_group).await?;
        matches!(chat.status(), ChatStatus::Inactive(_)).then_some(())
    })
    .await;
    assert_eq!(
        members(ada.clone(), group).await,
        [("You".to_string(), true, false)]
    );
    assert!(refused(groups::leave(&bob, bob_group).await));
    groups::delete(&bob, bob_group).await.unwrap();
    assert!(bob.chat(&bob_group).await.is_none());
    // Groups-only operations refuse a one-to-one chat and the self chat.
    assert!(refused(groups::leave(&ada, ada_chat).await));
    let ada_self = eventually("Ada has a self chat", async || {
        for id in ada.ordered_chat_ids().await.unwrap() {
            let chat = ada.chat(&id).await?;
            if ada.chat_is_self_group(&chat.group_id).await.unwrap() {
                return Some(id);
            }
        }
        None
    })
    .await;
    assert!(refused(groups::leave(&ada, ada_self).await));
    // Nor is it listed as one.
    let listed = chats::summary(&ada, ada.chat(&ada_self).await.unwrap()).await;
    assert!(listed.is_self_chat && !listed.is_group, "{listed:?}");

    // Deleting a group that is still going ends it for everyone in it.
    let group = groups::create(&ada, "Doomed").await.unwrap();
    groups::invite(ada.clone(), group, "robert".into())
        .await
        .unwrap();
    let bob_group = eventually("Bob joins the second group", async || {
        for id in bob.ordered_chat_ids().await.unwrap() {
            let chat = bob.chat(&id).await?;
            if chat.attributes().is_some_and(|a| a.title == "Doomed") {
                return Some(id);
            }
        }
        None
    })
    .await;
    groups::delete(&ada, group).await.unwrap();
    assert!(ada.chat(&group).await.is_none());
    eventually("the group ends for Bob", async || {
        let chat = bob.chat(&bob_group).await?;
        matches!(chat.status(), ChatStatus::Inactive(_)).then_some(())
    })
    .await;

    // A removed username is no longer listened to or reported.
    contacts::remove_username(ada.clone(), name.clone())
        .await
        .unwrap();
    while !next(&mut names).await.is_empty() {}
}

/// Ada links a second device, which then hides alerts from her first for
/// the chat it shows (re-reported as it changes), is renamed, and is
/// unlinked, deleting its data.
/// `AIR_E2E_CODES=XXXXXXXX,YYYYYYYY cargo test e2e_linked -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a local Air server and AIR_E2E_CODES"]
async fn e2e_linked_devices_share_the_account_and_quiet_each_other() {
    let codes = std::env::var("AIR_E2E_CODES").expect("set AIR_E2E_CODES to two codes");
    let [ada_code, bob_code] = codes.split(',').collect::<Vec<_>>()[..] else {
        panic!("AIR_E2E_CODES takes two comma-separated codes");
    };
    let dir = tempfile::tempdir().unwrap();
    let ada = new_account(dir.path(), "Ada Lovelace", ada_code).await;
    let bob = new_account(dir.path(), "Bob", bob_code).await;
    let (ada_alerts, _, ada_notices) = alerts();
    let (bob_alerts, _, _) = alerts();
    let _queues = [
        listen_queue(&ada, ada_alerts.clone()).await,
        listen_queue(&bob, bob_alerts).await,
    ];

    // The new device shows a code; Ada's first device answers it and,
    // once the new device proved it has it, confirms with a name.
    let desk_dir = dir.path().join("Ada desk");
    crate::paths::ensure_private_dir(&desk_dir).unwrap();
    let handle = tokio::runtime::Handle::current();
    let (_provision, mut provisioning) = devices::provision_on(
        &handle,
        desk_dir.to_str().unwrap().into(),
        "localhost".parse().unwrap(),
    );
    let devices::ProvisionEvent::Code(code) = next(&mut provisioning).await else {
        panic!("no code first");
    };
    // A mistyped code is told, not linked.
    let wrong = format!("9{code}");
    let (_wrong, mut wrong_events) = devices::link_on(&handle, ada.clone(), wrong);
    assert!(matches!(
        next(&mut wrong_events).await,
        devices::LinkEvent::Failed(_)
    ));
    let (mut link, mut linking) = devices::link_on(&handle, ada.clone(), code);
    assert_eq!(
        next(&mut linking).await,
        devices::LinkEvent::AwaitingConfirmation
    );
    link.confirm(" Ada's desk ").unwrap();
    assert_eq!(next(&mut linking).await, devices::LinkEvent::Linked);
    let desk = loop {
        match next(&mut provisioning).await {
            devices::ProvisionEvent::Linked(user) => break user,
            devices::ProvisionEvent::Failed => panic!("the new device failed to link"),
            _ => {}
        }
    };
    assert_eq!(desk.user_id(), ada.user_id());
    // A finished record, so the next start loads it.
    let desk_path = desk_dir.to_str().unwrap().to_string();
    let records = ClientRecord::load_all_from_air_db(&desk_path)
        .await
        .unwrap();
    assert!(
        records
            .iter()
            .any(|record| record.client_record_id == desk.client_record_id()
                && record.client_record_state == ClientRecordState::Finished)
    );
    let (desk_alerts, desk_attention, _) = alerts();
    let _desk_queue = listen_queue(&desk, desk_alerts).await;

    // Both list both devices, named as confirmed, each itself first, with
    // the same tags.
    let listed = async |user: &CoreUser| {
        let devices = devices::load(user.clone()).await.unwrap().devices;
        devices
            .into_iter()
            .map(|(device, _)| {
                (
                    device.name.to_string(),
                    device.tag.to_string(),
                    device.this_device,
                )
            })
            .collect::<Vec<_>>()
    };
    let on_ada = eventually("Ada's device lists the desk", async || {
        let devices = listed(&ada).await;
        (devices.len() == 2 && devices[1].0 == "Ada's desk").then_some(devices)
    })
    .await;
    assert!(on_ada[0].2 && !on_ada[1].2);
    let on_desk = eventually("the desk lists Ada's device", async || {
        let devices = listed(&desk).await;
        (devices.len() == 2).then_some(devices)
    })
    .await;
    assert_eq!(on_desk[0], (on_ada[1].0.clone(), on_ada[1].1.clone(), true));
    assert_eq!(on_desk[1].1, on_ada[0].1);

    // Bob and Ada become contacts.
    let (names_tx, mut names) = mpsc::unbounded_channel();
    let _ada_usernames = Aborting(tokio::spawn(contacts::run(
        ada.clone(),
        ada_alerts,
        move |usernames| {
            let _ = names_tx.send(usernames);
        },
    )));
    assert!(next(&mut names).await.is_empty());
    let name = format!("ada-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    eventually("Ada gets her username", async || {
        contacts::add_username(ada.clone(), name.clone()).await.ok()
    })
    .await;
    eventually("Bob sends a request", async || {
        contacts::add_contact(bob.clone(), name.clone()).await.ok()
    })
    .await;
    let request = eventually("Ada gets the request", async || {
        for id in ada.ordered_chat_ids().await.unwrap() {
            if let Some(chat) = ada.chat(&id).await
                && matches!(chat.chat_type, ChatType::PendingConnection(_))
            {
                return Some(id);
            }
        }
        None
    })
    .await;
    let chat = contacts::accept_request(ada.clone(), request)
        .await
        .unwrap();
    let chat = messages::core_chat_id(&chat);
    let bob_chat = eventually("Bob's request is accepted", async || {
        for id in bob.ordered_chat_ids().await.unwrap() {
            if let Some(found) = bob.chat(&id).await
                && matches!(found.chat_type, ChatType::Connection(_))
            {
                return Some(id);
            }
        }
        None
    })
    .await;
    eventually("the desk has the chat", async || desk.chat(&chat).await).await;

    // Ada and Bob see the same safety code, and so does Ada's other
    // device (one credential per user).
    let ada_sees = safety::with(&ada, bob.user_id()).await.ok().unwrap();
    let bob_sees = safety::with(&bob, ada.user_id()).await.ok().unwrap();
    assert_eq!(ada_sees, bob_sees);
    let desk_sees = eventually("the desk has Bob's credential", async || {
        safety::with(&desk, bob.user_id()).await.ok()
    })
    .await;
    assert_eq!(desk_sees, ada_sees);

    // Bob writes until Ada's first device's next notice for the chat is a
    // `quiet` one (dismissed, the desk shows it) or not.
    let sent = std::cell::Cell::new(0);
    let until_quiet = async |quiet: bool| {
        eventually("the desk's attention reaches Ada's device", async || {
            sent.set(sent.get() + 1);
            let text = format!("message {}", sent.get());
            let result = composer::send(&bob, bob_chat, text, None).await;
            assert!(matches!(result, composer::Sent::Ok), "{result:?}");
            let notice = next_notice(&ada_notices, |n| notice_chat(n) == Some(chat)).await;
            (matches!(notice, Notice::Dismiss(_)) == quiet).then_some(())
        })
        .await
    };
    // The desk shows the chat: Ada's other device stays quiet about it.
    desk_attention.send_replace(Attention {
        window_active: true,
        open_chat: Some(chat),
        content: Notifications::Full,
    });
    until_quiet(true).await;
    // The desk looks away: the change is reported again, and Ada's device
    // alerts once more.
    desk_attention.send_replace(Attention {
        window_active: false,
        open_chat: Some(chat),
        content: Notifications::Full,
    });
    until_quiet(false).await;

    // Either device renames the other, for both.
    let desk_id = devices::load(ada.clone()).await.unwrap().devices[1].1;
    ada.rename_device(desk_id, "Desk".into()).await.unwrap();
    eventually("the desk learns its new name", async || {
        (listed(&desk).await[0].0 == "Desk").then_some(())
    })
    .await;

    // Unlinked, the desk learns it, and deleting its data leaves no account
    // to load there.
    let unlinked = tokio::spawn(devices::unlinked(desk.clone(), desk.db_notifications()));
    ada.unlink_device(desk_id).await.unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(30), unlinked)
        .await
        .expect("the desk didn't learn it was unlinked")
        .unwrap();
    assert_eq!(reason, Some(aircoreclient::UnlinkReason::Unlinked));
    drop(_desk_queue);
    let desk_record = desk.client_record_id();
    assert_eq!(
        devices::tear_down(desk, desk_path.clone()).await,
        devices::TornDown::Deleted
    );
    // Marked while deleting; nothing is left to remove, so no mark stays.
    assert!(!desk_dir.join(format!("{desk_record}.deleted")).exists());
    assert!(account::load_default(desk_path).await.unwrap().is_none());
    eventually("Ada's device lists itself alone", async || {
        (listed(&ada).await.len() == 1).then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a local Air server and AIR_E2E_CODES"]
async fn e2e_contacts_are_blocked_reported_retracted_and_deleted() {
    use super::contact_safety::run as contact_op;
    use super::message_ops::{delete as delete_message, history as message_history};
    use crate::model::{ContactOp, DeleteFor};
    let codes = std::env::var("AIR_E2E_CODES").expect("set AIR_E2E_CODES to three codes");
    let [ada_code, bob_code, carol_code] = codes.split(',').collect::<Vec<_>>()[..] else {
        panic!("AIR_E2E_CODES takes three comma-separated codes");
    };
    let dir = tempfile::tempdir().unwrap();
    let ada = new_account(dir.path(), "Ada Lovelace", ada_code).await;
    let bob = new_account(dir.path(), "Bob", bob_code).await;
    let carol = new_account(dir.path(), "Carol", carol_code).await;
    let (ada_alerts, _, _) = alerts();
    let (bob_alerts, _, _) = alerts();
    let _queues = [
        listen_queue(&ada, ada_alerts.clone()).await,
        listen_queue(&bob, bob_alerts.clone()).await,
    ];
    let _usernames = [
        Aborting(tokio::spawn(contacts::run(ada.clone(), ada_alerts, |_| {}))),
        Aborting(tokio::spawn(contacts::run(bob.clone(), bob_alerts, |_| {}))),
    ];
    let username = async |user: &CoreUser, prefix: &str| {
        let name = format!(
            "{prefix}-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        );
        eventually("a username", async || {
            contacts::add_username(user.clone(), name.clone())
                .await
                .ok()
        })
        .await
    };
    // The open request `user` has from `from`, if any yet.
    let request_from = async |user: &CoreUser, from: &CoreUserId| {
        for id in user.ordered_chat_ids().await.unwrap() {
            if let Some(chat) = user.chat(&id).await
                && chat.chat_type == ChatType::PendingConnection(from.clone())
                && matches!(chat.status(), ChatStatus::Active)
            {
                return Some(id);
            }
        }
        None
    };
    let pending_request = async |user: &CoreUser, from: &CoreUser| {
        eventually("the request arrives", async || {
            request_from(user, from.user_id()).await
        })
        .await
    };
    let refused = |result: Result<(), Failure>| matches!(result, Err(Failure::Shown(_)));
    let (ada_name, bob_name) = (username(&ada, "ada").await, username(&bob, "bob").await);

    // Ada asks Bob, then retracts once Bob has the request: he can no
    // longer accept it, and her chat is gone.
    let asked = eventually("Ada sends a request", async || {
        contacts::add_contact(ada.clone(), bob_name.clone())
            .await
            .ok()
    })
    .await;
    let asked = messages::core_chat_id(&asked);
    let bobs_request = pending_request(&bob, &ada).await;
    assert!(refused(
        contact_op(ada.clone(), ContactOp::Block, asked).await
    ));
    assert!(refused(
        contact_op(ada.clone(), ContactOp::Delete, asked).await
    ));
    eventually("Ada retracts", async || {
        contact_op(ada.clone(), ContactOp::Retract, asked)
            .await
            .ok()
    })
    .await;
    assert!(ada.chat(&asked).await.is_none());
    assert!(matches!(
        contacts::accept_request(bob.clone(), bobs_request).await,
        Err(Failure::Shown(_))
    ));

    // Bob asks Ada; she accepts.
    eventually("Bob sends a request", async || {
        contacts::add_contact(bob.clone(), ada_name.clone())
            .await
            .ok()
    })
    .await;
    let request = pending_request(&ada, &bob).await;
    let chat = contacts::accept_request(ada.clone(), request)
        .await
        .unwrap();
    let chat = messages::core_chat_id(&chat);
    let bob_chat = eventually("Bob's request is accepted", async || {
        for id in bob.ordered_chat_ids().await.unwrap() {
            if let Some(found) = bob.chat(&id).await
                && matches!(found.chat_type, ChatType::Connection(_))
            {
                return Some(id);
            }
        }
        None
    })
    .await;
    let status = async |user: &CoreUser, chat| user.chat(&chat).await.unwrap().status().clone();

    // Blocked, Bob's messages are dropped; unblocked, they arrive again.
    let (_ada_worker, mut ada_updates, _) = open_messages(&ada, chat);
    contact_op(ada.clone(), ContactOp::Block, chat)
        .await
        .unwrap();
    assert_eq!(status(&ada, chat).await, ChatStatus::Blocked);
    assert!(refused(
        contact_op(ada.clone(), ContactOp::Block, chat).await
    ));
    let sent = composer::send(&bob, bob_chat, "while blocked".into(), None).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    // Nothing tells when Ada's queue dropped it: give it time. Too little
    // fails the test (the message arrives unblocked), never passes it.
    tokio::time::sleep(Duration::from_secs(3)).await;
    contact_op(ada.clone(), ContactOp::Unblock, chat)
        .await
        .unwrap();
    assert_eq!(status(&ada, chat).await, ChatStatus::Active);
    assert!(refused(
        contact_op(ada.clone(), ContactOp::Unblock, chat).await
    ));
    let sent = composer::send(&bob, bob_chat, "unblocked".into(), None).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    let got = next_message(&mut ada_updates, |m| {
        assert_ne!(m.plain_text(), "while blocked", "a blocked message arrived");
        m.plain_text() == "unblocked"
    })
    .await;
    assert!(!got.own);

    // Deleting messages. The core would delete a peer's message for
    // everyone; the session refuses. Ada deletes his for herself.
    let theirs = messages::core_message_id(&got.id);
    assert!(refused(
        delete_message(ada.clone(), chat, theirs, DeleteFor::Everyone).await
    ));
    let (_bob_worker, mut bob_updates, _) = open_messages(&bob, bob_chat);
    let sent = composer::send(&ada, chat, "first".into(), None).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    let mine = next_message(&mut ada_updates, |m| m.plain_text() == "first").await;
    let mine_id = messages::core_message_id(&mine.id).unwrap();
    sent_mimi_id(&ada, mine_id).await;
    let edit = composer::Draft {
        editing: Some(mine_id),
        ..composer::Draft::empty()
    };
    let sent = composer::send(&ada, chat, "second".into(), Some(&edit)).await;
    assert!(matches!(sent, composer::Sent::Ok), "{sent:?}");
    // Bob gets the edit, with its history.
    let edited = next_message(&mut bob_updates, |m| m.edited).await;
    assert_eq!(edited.plain_text(), "second");
    let bobs_copy = messages::core_message_id(&edited.id);
    let history = message_history(bob.clone(), bob_chat, bobs_copy)
        .await
        .unwrap();
    let texts: Vec<_> = history
        .versions
        .iter()
        .map(|version| version.markdown.is_some())
        .collect();
    assert_eq!(texts, [true, true]);
    // Not another chat's message, even by its id.
    assert!(refused(
        message_history(bob.clone(), bob_chat, theirs)
            .await
            .map(drop)
    ));
    // Ada deletes it for everyone once it is sent: Bob is left a
    // placeholder, and no history.
    eventually("the edit is sent", async || {
        let message = ada.message(mine_id).await.unwrap()?;
        message.is_sent().then_some(())
    })
    .await;
    delete_message(ada.clone(), chat, Some(mine_id), DeleteFor::Everyone)
        .await
        .unwrap();
    let placeholder = next_message(&mut bob_updates, |m| m.id == edited.id && m.deleted).await;
    assert!(!placeholder.plain_text().contains("second"));
    let history = message_history(bob.clone(), bob_chat, bobs_copy)
        .await
        .unwrap();
    assert!(history.versions.is_empty());
    // A deleted message isn't deleted for everyone again.
    assert!(refused(
        delete_message(ada.clone(), chat, Some(mine_id), DeleteFor::Everyone).await
    ));
    let theirs = theirs.unwrap();
    delete_message(ada.clone(), chat, Some(theirs), DeleteFor::Me)
        .await
        .unwrap();
    assert!(ada.message(theirs).await.unwrap().is_none());

    // Muting is the chat's, as the list shows it.
    ada.set_chat_muted_until(chat, Some(aircoreclient::ChatMuted::Forever))
        .await
        .unwrap();
    let summary = chats::summary(&ada, ada.chat(&chat).await.unwrap()).await;
    assert_eq!(summary.muted, Some(crate::model::Mute::Forever));

    // Reporting tells the server; the chat stays.
    contact_op(ada.clone(), ContactOp::Report, chat)
        .await
        .unwrap();
    assert!(ada.chat(&chat).await.is_some());

    // Deleting erases Ada's chat and ends Bob's.
    contact_op(ada.clone(), ContactOp::Delete, chat)
        .await
        .unwrap();
    assert!(ada.chat(&chat).await.is_none());
    eventually("Bob's chat ends", async || {
        matches!(status(&bob, bob_chat).await, ChatStatus::Inactive(_)).then_some(())
    })
    .await;

    // Bob asks again; Ada blocks the request. The core declines it and
    // erases its chat, and Bob's next request never reaches her.
    let asked = eventually("Bob asks again", async || {
        contacts::add_contact(bob.clone(), ada_name.clone())
            .await
            .ok()
    })
    .await;
    let asked = messages::core_chat_id(&asked);
    let request = pending_request(&ada, &bob).await;
    contact_op(ada.clone(), ContactOp::Block, request)
        .await
        .unwrap();
    assert!(ada.chat(&request).await.is_none());
    eventually("Bob retracts the declined request", async || {
        contact_op(bob.clone(), ContactOp::Retract, asked)
            .await
            .ok()
    })
    .await;
    eventually("Bob asks once more", async || {
        contacts::add_contact(bob.clone(), ada_name.clone())
            .await
            .ok()
    })
    .await;
    // Carol asks after him, on the same username: once Ada has Carol's
    // request, her listener has been past Bob's.
    eventually("Carol asks", async || {
        contacts::add_contact(carol.clone(), ada_name.clone())
            .await
            .ok()
    })
    .await;
    pending_request(&ada, &carol).await;
    // His was erased with the block: any chat of his request, in any
    // status, means it got through.
    for id in ada.ordered_chat_ids().await.unwrap() {
        let chat = ada.chat(&id).await.unwrap();
        assert_ne!(
            chat.chat_type,
            ChatType::PendingConnection(bob.user_id().clone()),
            "a blocked sender's request reached Ada"
        );
    }
}

async fn new_account(dir: &std::path::Path, name: &str, code: &str) -> CoreUser {
    let data = dir.join(name);
    crate::paths::ensure_private_dir(&data).unwrap();
    let sign_up = SignUp {
        display_name: name.into(),
        ..sign_up(Some(code))
    };
    account::sign_up(data.to_str().unwrap().into(), sign_up)
        .await
        .unwrap()
}

/// Runs the queue listener until dropped, once it is connected.
async fn listen_queue(user: &CoreUser, alerts: Alerts) -> Aborting {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let listener = Aborting(tokio::spawn(queue::run(
        user.clone(),
        alerts,
        move |event| {
            let _ = tx.send(event);
        },
    )));
    while next(&mut rx).await != QueueEvent::Status(ConnectionStatus::Connected) {}
    listener
}

/// Alerts as the session sets them up, minus the desktop notifier.
fn alerts() -> (Alerts, watch::Sender<Attention>, Arc<NoticeQueue>) {
    let (attention, attention_rx) = watch::channel(Attention::default());
    let notices = Arc::new(NoticeQueue::default());
    (
        Alerts::new(attention_rx, notices.clone()),
        attention,
        notices,
    )
}

/// The next notice that matches, skipping others.
async fn next_notice(notices: &NoticeQueue, matches: impl Fn(&Notice) -> bool) -> Notice {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let notice = notices.next().await;
            if matches(&notice) {
                return notice;
            }
        }
    })
    .await
    .expect("no matching notice within 20s")
}

/// The next alert shown that matches, skipping other notices.
async fn next_alert(notices: &NoticeQueue, matches: impl Fn(&Alert) -> bool) -> Alert {
    let shown = |notice: &Notice| matches!(notice, Notice::Show(alert) if matches(alert));
    match next_notice(notices, shown).await {
        Notice::Show(alert) => alert,
        _ => unreachable!("only shows match"),
    }
}

fn notice_chat(notice: &Notice) -> Option<aircoreclient::ChatId> {
    match notice {
        Notice::Show(alert) | Notice::Refresh(alert) => Some(alert.chat),
        Notice::Dismiss(Dismiss::Chat(chat)) => Some(*chat),
        Notice::Dismiss(Dismiss::All) => None,
    }
}

/// A message window on `chat` that follows the user's changes.
fn open_messages(
    user: &CoreUser,
    chat: aircoreclient::ChatId,
) -> (
    [Aborting; 2],
    mpsc::Receiver<messages::Update>,
    Arc<Pending>,
) {
    let pending = Arc::new(Pending::default());
    let mut notifications = user.db_notifications();
    let forward = tokio::spawn({
        let pending = pending.clone();
        async move {
            use tokio_stream::StreamExt as _;
            while let Some(notification) = notifications.next().await {
                pending.add(&[notification], false);
            }
        }
    });
    let (worker, updates) = messages::start(user.clone(), chat, pending.clone());
    ([Aborting(forward), Aborting(worker)], updates, pending)
}

struct Aborting(tokio::task::JoinHandle<()>);

impl Drop for Aborting {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn next<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> T {
    let next = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await;
    next.expect("nothing within 20s")
        .expect("the sender stopped")
}

/// Polls `check` until it yields a value.
async fn eventually<T>(what: &str, check: impl AsyncFn() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let Some(value) = check().await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"))
}
