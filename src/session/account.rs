//! Loading and creating the account. Port of upstream `applogic`'s
//! `api/user.rs` and `api/registration.rs`, minus push tokens (desktop has
//! none, so admission sessions are not available; invitation codes are).
//!
//! Everything here touches `aircoreclient` and must run on tokio.

use std::cmp::Reverse;
use std::time::Duration;

use aircommon::identifiers::{Fqdn, UserId};
use aircommon::registration::{ChallengeKind, RegistrationChallenge, RegistrationInfo};
use aircoreclient::clients::CoreUser;
use aircoreclient::clients::registration::RegistrationError;
use aircoreclient::clients::store::{ClientRecord, ClientRecordState};
use aircoreclient::{DisplayName, UserProfile};
use tracing::{error, info};
use uuid::Uuid;

/// How long the server gets to say whether sign-up needs a challenge. An
/// unreachable server counts as open; sign-up itself then reports the truth.
const REGISTRATION_INFO_TIMEOUT: Duration = Duration::from_secs(3);

/// Loads the account to use at startup: the default finished one, else the
/// newest finished one.
///
/// Unlike upstream, an unfinished registration is not resumed here: that
/// would finish it with a generated display name (the chosen one is set
/// only after registration) and contact the server before the window shows
/// anything. Sign-up resumes it instead, see [`sign_up`].
pub async fn load_default(db_path: String) -> anyhow::Result<Option<CoreUser>> {
    let mut records = ClientRecord::load_all_from_air_db(&db_path).await?;
    let known: std::collections::HashSet<Uuid> = records
        .iter()
        .map(|record| record.client_record_id)
        .collect();
    let dir = std::path::PathBuf::from(&db_path);
    tokio::task::spawn_blocking(move || remove_marked(&dir, &known)).await?;
    records.retain(|record| record.client_record_state == ClientRecordState::Finished);
    records.sort_unstable_by_key(|record| Reverse((record.is_default, record.created_at)));
    for record in records {
        match CoreUser::load(&db_path, record.client_record_id).await {
            Ok(user) => return Ok(Some(user)),
            // A broken record must not hide the others.
            Err(err) => error!(
                client_record_id = %record.client_record_id,
                "failed to load account: {err:#}"
            ),
        }
    }
    Ok(None)
}

/// Marks `id`'s leftover database files for removal: a teardown that
/// failed partway (the core drops the record even when a file stays) left
/// them, nothing can load them, and they hold keys and history.
pub fn mark_deleted(dir: &std::path::Path, id: Uuid) -> std::io::Result<()> {
    std::fs::write(dir.join(format!("{id}.deleted")), b"")
}

/// Drops `id`'s mark once its files are gone.
pub fn unmark_deleted(dir: &std::path::Path, id: Uuid) {
    match std::fs::remove_file(dir.join(format!("{id}.deleted"))) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => error!("failed to remove a deletion mark: {err}"),
    }
}

/// Removes the database files (`<id>.db`, `-shm`, `-wal`) of accounts
/// marked by [`mark_deleted`], then the mark. Only marked ones: a database
/// without a record may be a sign-up or link still under way in another
/// instance, and the core writes the record last. A marked id that has a
/// record again is left alone.
pub fn remove_marked(dir: &std::path::Path, known: &std::collections::HashSet<Uuid>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".deleted"))
            .and_then(|stem| Uuid::parse_str(stem).ok())
        else {
            continue;
        };
        if known.contains(&id) {
            continue;
        }
        let mut removed = true;
        for extension in ["db", "db-shm", "db-wal"] {
            match std::fs::remove_file(dir.join(format!("{id}.{extension}"))) {
                Ok(()) => info!("removed the data of a deleted account"),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    error!("failed to remove a deleted account's data: {err}");
                    removed = false;
                }
            }
        }
        if removed {
            unmark_deleted(dir, id);
        }
    }
}

#[derive(Debug, Clone)]
pub struct SignUp {
    pub domain: String,
    pub display_name: String,
    pub invitation_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SignUpError {
    InvalidDomain,
    EmptyDisplayName,
    /// The server wants an invitation code and none was given.
    CodeRequired,
    /// The server turned the invitation code down.
    CodeRejected,
    /// The server wants a challenge this client can't answer.
    ChallengeUnsupported,
    Other(String),
}

impl std::fmt::Display for SignUpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidDomain => "That doesn't look like a server address",
            Self::EmptyDisplayName => "Enter a display name",
            Self::CodeRequired => "This server needs an invitation code",
            Self::CodeRejected => "That invitation code isn't valid",
            Self::ChallengeUnsupported => {
                "This server only accepts sign-ups from the mobile app for now"
            }
            Self::Other(message) => message,
        })
    }
}

pub fn parse_domain(domain: &str) -> Result<Fqdn, SignUpError> {
    domain
        .trim()
        .parse()
        .map_err(|_| SignUpError::InvalidDomain)
}

/// Asks the server what signing up needs. `None` if it can't be reached in
/// time.
pub async fn registration_info(domain: Fqdn) -> Option<RegistrationInfo> {
    match tokio::time::timeout(
        REGISTRATION_INFO_TIMEOUT,
        CoreUser::get_registration_info(domain),
    )
    .await
    {
        Ok(Ok(info)) => Some(info),
        Ok(Err(err)) => {
            info!("registration info unavailable: {err:#}");
            None
        }
        Err(_) => None,
    }
}

/// Whether the form should ask for an invitation code.
pub fn needs_code(info: Option<&RegistrationInfo>) -> bool {
    info.is_some_and(|info| {
        info.challenge_required
            && info
                .accepted_challenges
                .contains(&ChallengeKind::InvitationCode)
    })
}

/// The challenge to send. Only when the server asked for one: an unasked
/// invitation code would be spent for nothing.
fn challenge(
    info: Option<&RegistrationInfo>,
    code: Option<&str>,
) -> Result<Option<RegistrationChallenge>, SignUpError> {
    match info {
        Some(info) if info.challenge_required => {
            challenge_for(&info.accepted_challenges, code).map(Some)
        }
        // Open, or unknown: the server answers ChallengeRequired if needed.
        _ => Ok(None),
    }
}

fn challenge_for(
    accepted: &[ChallengeKind],
    code: Option<&str>,
) -> Result<RegistrationChallenge, SignUpError> {
    if !accepted.contains(&ChallengeKind::InvitationCode) {
        return Err(SignUpError::ChallengeUnsupported);
    }
    match code.map(str::trim).filter(|code| !code.is_empty()) {
        Some(code) => Ok(RegistrationChallenge::InvitationCode(code.to_string())),
        None => Err(SignUpError::CodeRequired),
    }
}

fn from_core_error(err: anyhow::Error) -> SignUpError {
    match err.downcast::<RegistrationError>() {
        Ok(RegistrationError::ChallengeRequired(accepted)) => {
            // Asked for a challenge after all (it can change between the
            // info request and the sign-up): say which one is missing.
            challenge_for(&accepted, None)
                .err()
                .unwrap_or(SignUpError::CodeRequired)
        }
        Ok(RegistrationError::ChallengeRejected) => SignUpError::CodeRejected,
        Err(err) => SignUpError::Other(format!("{err:#}")),
    }
}

/// Registers a new account at `sign_up.domain` and sets its display name.
///
/// An earlier attempt that got past the server's challenge (so its
/// invitation code is spent) but failed later is resumed rather than
/// started over; attempts that didn't get that far are discarded.
pub async fn sign_up(db_path: String, sign_up: SignUp) -> Result<CoreUser, SignUpError> {
    let domain = parse_domain(&sign_up.domain)?;
    let display_name: DisplayName = sign_up
        .display_name
        .parse()
        .map_err(|_| SignUpError::EmptyDisplayName)?;

    let user = match resume_unfinished(&db_path, &domain).await? {
        Some(user) => user,
        None => {
            let info = registration_info(domain.clone()).await;
            let challenge = challenge(info.as_ref(), sign_up.invitation_code.as_deref())?;
            let user_id = UserId::new(Uuid::new_v4(), domain);
            CoreUser::new(user_id, &db_path, None, challenge)
                .await
                .map_err(from_core_error)?
        }
    };
    let profile = UserProfile {
        user_id: user.user_id().clone(),
        display_name,
        profile_picture: None,
    };
    // As upstream: the account exists now; a missing name is not fatal.
    if let Err(err) = user.set_own_user_profile(profile).await {
        error!("could not set own profile: {err:#}");
    }
    Ok(user)
}

/// Finishes an interrupted registration at `domain`, if there is one that
/// needs no new challenge. Removes interrupted registrations that can't be
/// finished that way, and those for other servers (the user moved on).
async fn resume_unfinished(db_path: &str, domain: &Fqdn) -> Result<Option<CoreUser>, SignUpError> {
    fn other(err: impl Into<anyhow::Error>) -> SignUpError {
        SignUpError::Other(format!("{:#}", err.into()))
    }
    let records = ClientRecord::load_all_from_air_db(db_path)
        .await
        .map_err(other)?;
    for record in records {
        if record.client_record_state == ClientRecordState::Finished {
            continue;
        }
        let id = record.client_record_id;
        if record.user_id.domain() == domain {
            match CoreUser::load(db_path, id).await {
                Ok(user) => return Ok(Some(user)),
                // Stuck before the challenge: nothing was spent on it.
                Err(err) if err.is::<RegistrationError>() => {}
                // E.g. offline: keep it, it may still be resumable.
                Err(err) => return Err(other(err)),
            }
        }
        info!("discarding an unfinished registration");
        aircoreclient::delete_client_database(db_path, id)
            .await
            .map_err(other)?;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invitation code a challenge carries (the type has no `PartialEq`).
    fn code(
        result: Result<Option<RegistrationChallenge>, SignUpError>,
    ) -> Result<Option<String>, SignUpError> {
        result.map(|challenge| {
            challenge.map(|challenge| match challenge {
                RegistrationChallenge::InvitationCode(code) => code,
                other => panic!("unexpected challenge {other:?}"),
            })
        })
    }

    fn info(required: bool, accepted: &[ChallengeKind]) -> RegistrationInfo {
        RegistrationInfo {
            challenge_required: required,
            accepted_challenges: accepted.to_vec(),
        }
    }

    #[test]
    fn only_marked_databases_without_a_record_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (deleted, linking, recorded) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let files = [
            format!("{deleted}.db"),
            format!("{deleted}.db-shm"),
            format!("{deleted}.db-wal"),
            // No record yet, no mark: a link under way elsewhere.
            format!("{linking}.db"),
            format!("{linking}.db-wal"),
            // Marked, but its record is back: left alone.
            format!("{recorded}.db"),
            "air.db".into(),
        ];
        for file in &files {
            std::fs::write(dir.path().join(file), b"").unwrap();
        }
        mark_deleted(dir.path(), deleted).unwrap();
        mark_deleted(dir.path(), recorded).unwrap();
        remove_marked(dir.path(), &[recorded].into());
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut expected = vec![
            format!("{linking}.db"),
            format!("{linking}.db-wal"),
            format!("{recorded}.db"),
            format!("{recorded}.deleted"),
            "air.db".into(),
        ];
        expected.sort();
        assert_eq!(left, expected);

        // A file that can't be removed keeps the mark, so a later load
        // tries again; the rest goes.
        let stuck = Uuid::new_v4();
        std::fs::write(dir.path().join(format!("{stuck}.db")), b"").unwrap();
        std::fs::create_dir(dir.path().join(format!("{stuck}.db-wal"))).unwrap();
        mark_deleted(dir.path(), stuck).unwrap();
        remove_marked(dir.path(), &[recorded].into());
        assert!(!dir.path().join(format!("{stuck}.db")).exists());
        assert!(dir.path().join(format!("{stuck}.deleted")).exists());
    }

    #[test]
    fn invitation_code_is_sent_only_when_asked_for() {
        let open = info(false, &[ChallengeKind::InvitationCode]);
        assert_eq!(code(challenge(Some(&open), Some("ABC"))), Ok(None));
        assert_eq!(code(challenge(None, Some("ABC"))), Ok(None));

        let gated = info(true, &[ChallengeKind::InvitationCode]);
        assert_eq!(
            code(challenge(Some(&gated), Some(" ABC "))),
            Ok(Some("ABC".into()))
        );
        assert_eq!(
            code(challenge(Some(&gated), Some("  "))),
            Err(SignUpError::CodeRequired)
        );
        assert!(needs_code(Some(&gated)));
        assert!(!needs_code(Some(&open)) && !needs_code(None));

        // A server that only takes push-based admission can't be answered.
        let push_only = info(true, &[ChallengeKind::AdmissionSession]);
        assert_eq!(
            code(challenge(Some(&push_only), Some("ABC"))),
            Err(SignUpError::ChallengeUnsupported)
        );
        assert!(!needs_code(Some(&push_only)));
    }

    #[test]
    fn core_registration_errors_map_to_form_errors() {
        let required = RegistrationError::ChallengeRequired(vec![ChallengeKind::InvitationCode]);
        assert_eq!(from_core_error(required.into()), SignUpError::CodeRequired);
        let push_only = RegistrationError::ChallengeRequired(vec![ChallengeKind::AdmissionSession]);
        assert_eq!(
            from_core_error(push_only.into()),
            SignUpError::ChallengeUnsupported
        );
        assert_eq!(
            from_core_error(RegistrationError::ChallengeRejected.into()),
            SignUpError::CodeRejected
        );
    }
}
