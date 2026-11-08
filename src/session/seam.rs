//! Account seam tests: the session layer against a real core, talking to
//! Air's in-process test server (`airserver_test_harness`). Unlike the e2e
//! tests they need no invitation codes and hit no rate limits, but the
//! server needs Postgres and MinIO, from Air's compose file (see
//! CLAUDE.md). Ignored by default; run with
//!
//! ```sh
//! just seam   # cargo test --features seam seam -- --ignored
//! ```

use aircommon::identifiers::UserId as CoreUserId;
use aircoreclient::clients::CoreUser;
use aircoreclient::{AttachmentStatus, ChatType};
use airserver_test_harness::utils::setup::TestBackend;

use super::attachments;
use super::contacts::Failure;
use super::groups::{self, Picked};

/// The user's contact-request chat with `peer`, if any.
async fn request_chat(user: &CoreUser, peer: &CoreUserId) -> Option<aircoreclient::ChatId> {
    for id in user.ordered_chat_ids().await.unwrap() {
        if let Some(chat) = user.chat(&id).await
            && chat.chat_type == ChatType::TargetedMessageConnection(peer.clone())
        {
            return Some(id);
        }
    }
    None
}

fn picked(id: &CoreUserId) -> Picked {
    Picked {
        name: "Bob".into(),
        tag: "#b".into(),
        id: id.clone(),
    }
}

fn refusal<T: std::fmt::Debug>(result: Result<T, Failure>) -> String {
    match result {
        Err(Failure::Shown(message)) => message,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// A contact request through a group is refused while one is open, and
/// can be sent again once it expired: the core keeps the chat but drops
/// its record (gate 21b's over-refusal).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
async fn seam_contact_from_group_again_after_expiry() {
    let mut backend = TestBackend::single().await;
    let [alice, bob, carol] = backend.add_users::<3>().await;
    // Alice and Bob meet in Carol's group, not as contacts.
    backend.connect_users(&carol, &alice).await;
    backend.connect_users(&carol, &bob).await;
    let group = backend.create_group(&carol).await;
    backend
        .invite_to_group(group, &carol, &[&alice, &bob])
        .await;
    // A contact can't be asked again.
    let carol_id = carol.user_id().clone();
    let err = refusal(groups::add_member_contact(&alice, group, picked(&carol_id)).await);
    assert!(err.contains("already a contact"), "{err}");

    let bob_id = bob.user_id().clone();
    groups::add_member_contact(&alice, group, picked(&bob_id))
        .await
        .unwrap();
    let err = refusal(groups::add_member_contact(&alice, group, picked(&bob_id)).await);
    assert!(err.contains("already sent"), "{err}");

    let request = request_chat(&alice, &bob_id).await.unwrap();
    // The chat list's countdown: the core's 30 days, while it is open.
    let open = alice.chat(&request).await.unwrap();
    let left = super::chats::request_expires(&alice, &open, Some(crate::model::Request::Outgoing))
        .await
        .unwrap()
        - chrono::Utc::now();
    assert!(left > chrono::Duration::days(29) && left <= chrono::Duration::days(30));
    alice
        .backdate_contact_request(request, chrono::Duration::days(31))
        .await
        .unwrap();
    alice
        .outbound_service()
        .schedule_connection_request_expiry(chrono::Utc::now())
        .await
        .unwrap();
    alice.outbound_service().run_once().await;
    // Expired: the chat stays, the request can be made again.
    let expired = alice.chat(&request).await.unwrap();
    assert!(
        matches!(expired.status, aircoreclient::ChatStatus::Inactive(_)),
        "{:?}",
        expired.status
    );
    groups::add_member_contact(&alice, group, picked(&bob_id))
        .await
        .unwrap();
}

/// A small WebP picture.
fn webp(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::WebP)
        .unwrap();
    bytes.into_inner()
}

/// Bob sends Alice a file in their chat; she downloads it (making its
/// thumbnail, as the session does). Returns her message.
async fn received_file(
    backend: &TestBackend,
    alice: &CoreUser,
    bob: &CoreUser,
    chat: aircoreclient::ChatId,
    bytes: &[u8],
    name: &str,
) -> aircoreclient::MessageId {
    backend
        .send_attachment(chat, bob, &[alice], bytes, name)
        .await
        .unwrap();
    let message = alice.last_message(chat).await.unwrap().unwrap().id();
    attachments::retry(alice.clone(), chat, message)
        .await
        .unwrap();
    message
}

/// The viewer opens a downloaded image, and refuses a file the core made
/// no thumbnail for without decoding it (gate 21). (A declared image whose
/// thumbnail failed takes the same branch; an honest sender's core can't
/// send one: it decodes images while uploading.)
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
async fn seam_the_viewer_opens_only_images_the_core_read() {
    let mut backend = TestBackend::single().await;
    let [alice, bob] = backend.add_users::<2>().await;
    let chat = backend.connect_users(&alice, &bob).await;

    let picture = received_file(&backend, &alice, &bob, chat, &webp(40, 30), "red.webp").await;
    let shown = attachments::full_picture(alice.clone(), chat, picture)
        .await
        .unwrap();
    assert_eq!(shown.size(), (40, 30));

    // Downloaded, but not an image: not decoded, refused as such.
    let broken = received_file(&backend, &alice, &bob, chat, b"not a picture", "notes.bin").await;
    let id = alice.attachment_infos_for_message(broken).await[0].attachment_id;
    assert!(matches!(
        alice.attachment_status(id).await.unwrap(),
        Some(aircoreclient::AttachmentStatus::Ready)
    ));
    let err = refusal(attachments::full_picture(alice.clone(), chat, broken).await);
    assert_eq!(err, "That file isn't an image Air can show");
}

/// Polls `check` (on the core's state) until it yields, for at most 20 s.
async fn eventually<T>(what: &str, check: impl AsyncFn() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// `retry` fetches a file left "downloading" by a quit mid-download, as
/// nothing is fetching it, unless this process's own transfer holds it
/// (gate 12e).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
async fn seam_a_stale_download_is_fetched_unless_claimed() {
    let mut backend = TestBackend::single().await;
    let [alice, bob] = backend.add_users::<2>().await;
    let chat = backend.connect_users(&alice, &bob).await;
    // Large, so that the download is still running when it's stopped.
    backend
        .send_attachment(chat, &bob, &[&alice], &vec![7; 8 << 20], "big.bin")
        .await
        .unwrap();
    let message = alice.last_message(chat).await.unwrap().unwrap().id();
    let id = alice.attachment_infos_for_message(message).await[0].attachment_id;
    let status = async || alice.attachment_status(id).await.unwrap();
    assert!(matches!(status().await, Some(AttachmentStatus::Pending)));
    {
        // Stopped right after the core marked it, like a quit.
        let (_progress, download) = alice.download_attachment(id);
        let marked = async {
            while !matches!(status().await, Some(AttachmentStatus::Downloading)) {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        };
        tokio::select! {
            biased;
            () = marked => {}
            result = download => panic!("the download ended first: {result:?}"),
        }
    }

    let held = attachments::hold(id).unwrap();
    let err = refusal(attachments::retry(alice.clone(), chat, message).await);
    assert_eq!(err, "Already downloading");
    assert!(matches!(
        status().await,
        Some(AttachmentStatus::Downloading)
    ));
    drop(held);

    attachments::retry(alice.clone(), chat, message)
        .await
        .unwrap();
    assert!(matches!(status().await, Some(AttachmentStatus::Ready)));
}

/// A failed upload is sent again only once at a time and only when it
/// failed; sent again, it uploads (gates 12d and 21).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Postgres and MinIO (Air's compose file)"]
async fn seam_a_failed_upload_is_sent_again() {
    let mut backend = TestBackend::single().await;
    let [alice, bob] = backend.add_users::<2>().await;
    let chat = backend.connect_users(&alice, &bob).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, "the file").unwrap();

    // The server is unreachable for the upload: the message stays, failed.
    backend
        .listener_control_handle()
        .set_drop_next_request_to("ProvisionAttachment");
    attachments::send(alice.clone(), chat, path).await.unwrap();
    let message = alice.last_message(chat).await.unwrap().unwrap().id();
    let id = alice.attachment_infos_for_message(message).await[0].attachment_id;
    let status = async || alice.attachment_status(id).await.unwrap();
    eventually("the upload to fail", async || {
        matches!(status().await, Some(AttachmentStatus::UploadFailed)).then_some(())
    })
    .await;

    // Not while another transfer holds it.
    let held = attachments::hold(id).unwrap();
    let err = refusal(attachments::retry_upload(alice.clone(), chat, message).await);
    assert_eq!(err, "Already sending");
    drop(held);

    attachments::retry_upload(alice.clone(), chat, message)
        .await
        .unwrap();
    eventually("the upload", async || {
        matches!(status().await, Some(AttachmentStatus::Ready)).then_some(())
    })
    .await;
    // Uploaded: nothing to send again (the outbound service sends it).
    attachments::retry_upload(alice.clone(), chat, message)
        .await
        .unwrap();
    assert!(matches!(status().await, Some(AttachmentStatus::Ready)));

    // A message without a file has nothing to send.
    backend.send_message(chat, &alice, &[&bob]).await;
    let text = alice.last_message(chat).await.unwrap().unwrap().id();
    let err = refusal(attachments::retry_upload(alice.clone(), chat, text).await);
    assert_eq!(err, "That message has no file");
}
