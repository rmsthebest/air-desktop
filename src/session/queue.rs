//! Listens to the queue service (QS) for incoming messages. Port of
//! upstream `applogic`'s `user_cubit/qs.rs` + `util/background.rs` for a
//! desktop app that is always in the foreground.
//!
//! Processing a QS event stores its effects in the database; the UI learns
//! about them from `CoreUser::db_notifications`, not from here. Processing
//! the queue's Empty sentinel also starts the core's outbound service.
//! What processing stored is also handed to [`Alerts`], and the chat the
//! user is looking at is reported to their other devices.

use std::time::Duration;

use aircommon::messages::client_state::NotificationSuppression;
use aircommon::utils::FibonacciBackoff;
use aircoreclient::clients::process::qs_stream::QsProcessEventResult;
use aircoreclient::clients::{
    CoreUser, ListenResponse, QsListenResponder, SiblingClientStates, listen_response,
};
use chrono::{DateTime, Utc};
use tokio::time::Instant;
use tokio_stream::StreamExt as _;
use tracing::{error, info, warn};

use super::alerts::Alerts;

/// A stream that stayed up this long was healthy: reconnect at once and
/// reset the backoff. Same as upstream.
pub(super) const HEALTHY_UPTIME: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    /// Waiting before the next connection attempt.
    Offline,
}

/// The server's word on this client version. Kept apart from the connection,
/// which goes on changing while the version is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionStatus {
    Supported,
    /// It stops accepting this version then.
    ExpiresAt(DateTime<Utc>),
    /// It no longer accepts this version.
    Unsupported,
}

impl VersionStatus {
    /// What an announcement means. The server sent it on a stream it accepted,
    /// and announces only dates ahead of it: one that looks past here means
    /// this clock is wrong, not a refusal (upstream refuses, but that would
    /// replace the workspace of a session that works). When the date really
    /// comes, the next listen is refused.
    fn announced(expires: Option<DateTime<Utc>>) -> Self {
        expires.map_or(Self::Supported, Self::ExpiresAt)
    }
}

/// What the listener reports to the session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueueEvent {
    Status(ConnectionStatus),
    /// The server let us listen: this version isn't refused (any announced
    /// date stands until the status it sends first on every stream).
    VersionAccepted,
    Version(VersionStatus),
}

/// What the session knows of the listener.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct QueueState {
    pub(super) status: ConnectionStatus,
    pub(super) version: VersionStatus,
}

impl QueueState {
    pub(super) fn apply(&mut self, event: QueueEvent) {
        match event {
            QueueEvent::Status(status) => self.status = status,
            QueueEvent::VersionAccepted => {
                if self.version == VersionStatus::Unsupported {
                    self.version = VersionStatus::Supported;
                }
            }
            QueueEvent::Version(version) => self.version = version,
        }
    }
}

/// How a stream session ended.
#[derive(Debug, PartialEq)]
enum Ended {
    /// Closed, or stopped because processing left a gap in the queue.
    Stopped { uptime: Duration },
    /// Could not connect.
    Failed,
}

/// Decides how long to wait before reconnecting.
#[derive(Default)]
struct Reconnect {
    backoff: FibonacciBackoff,
}

impl Reconnect {
    fn delay(&mut self, ended: &Ended) -> Duration {
        match ended {
            Ended::Stopped { uptime } if *uptime >= HEALTHY_UPTIME => {
                self.backoff.reset();
                Duration::ZERO
            }
            _ => self.backoff.next_backoff(),
        }
    }
}

/// Runs until dropped (the session aborts it), reconnecting with backoff.
pub async fn run(user: CoreUser, mut alerts: Alerts, report: impl Fn(QueueEvent) + Send) {
    let mut reconnect = Reconnect::default();
    loop {
        report(QueueEvent::Status(ConnectionStatus::Connecting));
        let ended = listen_once(&user, &mut alerts, &report).await;
        let delay = reconnect.delay(&ended);
        if !delay.is_zero() {
            info!(retry_in = ?delay, "queue stream ended ({ended:?})");
            tokio::time::sleep(delay).await;
        }
    }
}

async fn listen_once(
    user: &CoreUser,
    alerts: &mut Alerts,
    report: &(impl Fn(QueueEvent) + Send),
) -> Ended {
    let status = |status| report(QueueEvent::Status(status));
    let (stream, responder) = match user.listen_queue().await {
        Ok(listening) => {
            report(QueueEvent::VersionAccepted);
            listening
        }
        Err(err) => {
            warn!("could not listen to queue: {err}");
            status(ConnectionStatus::Offline);
            if err.is_unsupported_version() {
                report(QueueEvent::Version(VersionStatus::Unsupported));
            } else {
                // As upstream: a deletion past the QS client can't listen
                // anymore, which is what would otherwise resume it.
                if let Err(err) = user.resume_account_deletion().await {
                    error!("failed to resume account deletion: {err:#}");
                }
            }
            return Ended::Failed;
        }
    };
    // As upstream: without the sibling state key the stream isn't set up.
    let mut siblings = match user.sibling_client_states() {
        Ok(siblings) => siblings,
        Err(err) => {
            error!("failed to set up sibling client states: {err:#}");
            status(ConnectionStatus::Offline);
            return Ended::Failed;
        }
    };
    let started = Instant::now();
    status(ConnectionStatus::Connected);

    // Report a client state on every stream, and again when it changes.
    // As upstream, also when nothing is suppressed, so the server can't
    // tell clients apart by it.
    let mut reported = alerts.suppression();
    report_client_state(user, &responder, reported).await;
    user.replace_qs_listen_responder(responder.clone()).await;

    let mut stream = std::pin::pin!(stream);
    loop {
        tokio::select! {
            event = stream.next() => {
                let event = match event {
                    Some(Ok(event)) => event,
                    Some(Err(err)) => {
                        warn!("queue stream closed: {err}");
                        break;
                    }
                    None => break,
                };
                if !handle_event(user, &mut siblings, alerts, report, event).await {
                    break;
                }
            }
            () = alerts.attention_changed() => {
                let suppression = alerts.suppression();
                if suppression != reported {
                    reported = suppression;
                    report_client_state(user, &responder, reported).await;
                }
            }
        }
    }
    status(ConnectionStatus::Offline);
    Ended::Stopped {
        uptime: started.elapsed(),
    }
}

async fn report_client_state(
    user: &CoreUser,
    responder: &QsListenResponder,
    suppression: NotificationSuppression,
) {
    match user.encrypt_client_state(suppression) {
        Ok(state) => responder.report_client_state(state).await,
        Err(err) => error!("failed to encrypt client state: {err:#}"),
    }
}

/// Returns whether to keep reading the stream.
async fn handle_event(
    user: &CoreUser,
    siblings: &mut SiblingClientStates,
    alerts: &Alerts,
    report: &(impl Fn(QueueEvent) + Send),
    event: ListenResponse,
) -> bool {
    match &event.event {
        Some(listen_response::Event::SiblingClientState(state)) => {
            if let Err(err) = siblings.try_apply(state.clone()) {
                error!("failed to apply sibling client state: {err:#}");
            }
            return true;
        }
        // Also processed by the core below (it stores max_devices).
        Some(listen_response::Event::VersionStatus(version)) => {
            let expires = version
                .expires_at
                .and_then(|at| DateTime::from_timestamp(at.seconds, 0));
            report(QueueEvent::Version(VersionStatus::announced(expires)));
        }
        _ => {}
    }
    match user.process_qs_event(event).await {
        Ok(QsProcessEventResult::FullyProcessed { processed }) => {
            alerts.processed(user, &processed, siblings).await;
            true
        }
        // A partially processed batch leaves a gap in the sequence: drop the
        // stream and resume from the stored position on reconnect.
        Ok(QsProcessEventResult::PartiallyProcessed { processed, .. }) => {
            alerts.processed(user, &processed, siblings).await;
            false
        }
        Ok(QsProcessEventResult::Accumulated | QsProcessEventResult::Ignored) => true,
        Err(err) => {
            error!("failed to process queue event: {err:#}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_announced_date_is_never_a_refusal() {
        let past = Utc::now() - chrono::Duration::days(1);
        assert_eq!(VersionStatus::announced(None), VersionStatus::Supported);
        assert_eq!(
            VersionStatus::announced(Some(past)),
            VersionStatus::ExpiresAt(past)
        );
    }

    /// The update screen hangs on this: losing the connection must not end
    /// a refusal, while the server accepting us again must.
    #[test]
    fn a_refused_version_stays_refused_until_the_server_accepts_it() {
        let at = Utc::now();
        let mut state = QueueState {
            status: ConnectionStatus::Connecting,
            version: VersionStatus::Unsupported,
        };
        for status in [ConnectionStatus::Offline, ConnectionStatus::Connecting] {
            state.apply(QueueEvent::Status(status));
            assert_eq!(state.version, VersionStatus::Unsupported);
        }
        state.apply(QueueEvent::VersionAccepted);
        assert_eq!(state.version, VersionStatus::Supported);

        // An announced date isn't dropped by a reconnect, only by the
        // server's next word, so the reminder doesn't blink.
        state.apply(QueueEvent::Version(VersionStatus::ExpiresAt(at)));
        state.apply(QueueEvent::Status(ConnectionStatus::Offline));
        state.apply(QueueEvent::VersionAccepted);
        assert_eq!(state.version, VersionStatus::ExpiresAt(at));
        state.apply(QueueEvent::Version(VersionStatus::Supported));
        assert_eq!(state.version, VersionStatus::Supported);
    }

    #[test]
    fn reconnect_backs_off_until_a_stream_was_healthy() {
        let mut reconnect = Reconnect::default();
        let failed = [
            reconnect.delay(&Ended::Failed),
            reconnect.delay(&Ended::Stopped {
                uptime: Duration::from_secs(5),
            }),
            reconnect.delay(&Ended::Failed),
        ];
        assert!(failed.iter().all(|delay| !delay.is_zero()));

        // A long-lived stream reconnects at once and starts over.
        assert_eq!(
            reconnect.delay(&Ended::Stopped {
                uptime: HEALTHY_UPTIME
            }),
            Duration::ZERO
        );
        assert_eq!(reconnect.delay(&Ended::Failed), failed[0]);
    }
}
