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
use super::{composer, groups, messages};
use crate::model::{Message, MessageStatus, Quote};
use crate::settings::Notifications;
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
    let (_ada_worker, mut ada_updates) = open_messages(&ada, ada_chat);
    let (_bob_worker, mut bob_updates) = open_messages(&bob, bob_chat);
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
    let removed = messages::set_reaction(&ada, ada_chat, hi, "👍".into(), false).await;
    assert!(removed.is_ok(), "{removed:?}");
    next_message(&mut bob_updates, |m| {
        m.plain_text() == "hi ada" && m.reactions.is_empty()
    })
    .await;
    // Only messages of the chat named take reactions.
    let elsewhere = aircoreclient::MessageId::new(uuid::Uuid::new_v4());
    assert!(refused(
        messages::set_reaction(&ada, ada_chat, elsewhere, "👍".into(), true).await
    ));

    // Bob's rename reaches the messages Ada already has from him.
    let mut profile = bob.own_user_profile().await.unwrap();
    profile.display_name = "Robert".parse().unwrap();
    bob.set_own_user_profile(profile).await.unwrap();
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
) -> ([Aborting; 2], mpsc::Receiver<messages::Update>) {
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
    let (worker, updates) = messages::start(user.clone(), chat, pending);
    ([Aborting(forward), Aborting(worker)], updates)
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
