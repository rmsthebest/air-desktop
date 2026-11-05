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

use tokio::sync::mpsc;

use super::account::{self, SignUp, SignUpError};
use super::chats::{self, Update};
use super::pending::Pending;
use super::queue::{self, ConnectionStatus, QueueEvent};
use super::{composer, messages};
use crate::model::{Message, Quote};
use aircoreclient::clients::store::{ClientRecord, ClientRecordState};

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
    let listener = tokio::spawn(queue::run(user.clone(), move |status| {
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
