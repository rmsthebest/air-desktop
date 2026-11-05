//! End-to-end checks against a local Air server (see CLAUDE.md). Ignored by
//! default; run with an unused invitation code:
//!
//! ```sh
//! AIR_E2E_CODE=XXXXXXXX cargo test e2e -- --ignored
//! ```
//!
//! With `AIR_E2E_DATA_DIR` set, the account is kept there, so the app can
//! be started on it: `AIR_DESKTOP_DATA_DIR=<same dir> cargo run`.

use std::time::Duration;

use tokio::sync::mpsc;

use super::account::{self, SignUp, SignUpError};
use super::queue::{self, ConnectionStatus, QueueEvent};
use aircoreclient::clients::store::{ClientRecord, ClientRecordState};

fn sign_up(code: Option<&str>) -> SignUp {
    SignUp {
        domain: "localhost".into(),
        display_name: "Ada Lovelace".into(),
        invitation_code: code.map(Into::into),
    }
}

#[tokio::test]
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
    listener.abort();
    drop(user);

    // A restart finds the same account and its profile on disk.
    let reloaded = account::load_default(db_path).await.unwrap().unwrap();
    assert_eq!(reloaded.user_id(), &user_id);
    let profile = reloaded.own_user_profile().await.unwrap();
    assert_eq!(profile.display_name.to_string(), "Ada Lovelace");
}
