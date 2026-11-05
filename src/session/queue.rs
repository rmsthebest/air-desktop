//! Listens to the queue service (QS) for incoming messages. Port of
//! upstream `applogic`'s `user_cubit/qs.rs` + `util/background.rs` for a
//! desktop app that is always in the foreground.
//!
//! Processing a QS event stores its effects in the database; the UI learns
//! about them from `CoreUser::db_notifications`, not from here. Processing
//! the queue's Empty sentinel also starts the core's outbound service.

use std::time::Duration;

use aircommon::messages::client_state::NotificationSuppression;
use aircommon::utils::FibonacciBackoff;
use aircoreclient::clients::process::qs_stream::QsProcessEventResult;
use aircoreclient::clients::{CoreUser, ListenResponse, SiblingClientStates, listen_response};
use chrono::{DateTime, Utc};
use tokio::time::Instant;
use tokio_stream::StreamExt as _;
use tracing::{error, info, warn};

/// A stream that stayed up this long was healthy: reconnect at once and
/// reset the backoff. Same as upstream.
const HEALTHY_UPTIME: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    /// Waiting before the next connection attempt.
    Offline,
    /// The server no longer accepts this client version.
    UnsupportedVersion,
}

/// What the listener reports to the session.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueueEvent {
    Status(ConnectionStatus),
    /// The server announced when it stops accepting this client version
    /// (`None`: no expiry announced, or the announcement was withdrawn).
    VersionExpires(Option<DateTime<Utc>>),
}

/// How a stream session ended.
#[derive(Debug, PartialEq)]
enum Ended {
    /// Closed, or stopped because processing left a gap in the queue.
    Stopped { uptime: Duration },
    /// Could not connect.
    Failed,
    /// The server rejects this client version.
    Unsupported,
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
pub async fn run(user: CoreUser, report: impl Fn(QueueEvent) + Send) {
    let mut reconnect = Reconnect::default();
    let mut unsupported = false;
    loop {
        // Keep "unsupported" on screen while retrying: it only changes when
        // the server accepts us again.
        if !unsupported {
            report(QueueEvent::Status(ConnectionStatus::Connecting));
        }
        let ended = listen_once(&user, &report).await;
        unsupported = ended == Ended::Unsupported;
        let delay = reconnect.delay(&ended);
        if !delay.is_zero() {
            info!(retry_in = ?delay, "queue stream ended ({ended:?})");
            tokio::time::sleep(delay).await;
        }
    }
}

async fn listen_once(user: &CoreUser, report: &(impl Fn(QueueEvent) + Send)) -> Ended {
    let status = |status| report(QueueEvent::Status(status));
    let (stream, responder) = match user.listen_queue().await {
        Ok(listening) => listening,
        Err(err) => {
            warn!("could not listen to queue: {err}");
            if err.is_unsupported_version() {
                status(ConnectionStatus::UnsupportedVersion);
                return Ended::Unsupported;
            } else {
                status(ConnectionStatus::Offline);
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

    // Report a client state on every stream, as upstream does even when
    // nothing is suppressed, so the server can't tell clients apart by it.
    // Desktop notifications don't suppress siblings' yet (milestone 7).
    match user.encrypt_client_state(NotificationSuppression::None) {
        Ok(state) => responder.report_client_state(state).await,
        Err(err) => error!("failed to encrypt client state: {err:#}"),
    }
    user.replace_qs_listen_responder(responder).await;

    let mut stream = std::pin::pin!(stream);
    while let Some(event) = stream.next().await {
        let event = match event {
            Ok(event) => event,
            Err(err) => {
                warn!("queue stream closed: {err}");
                break;
            }
        };
        if !handle_event(user, &mut siblings, report, event).await {
            break;
        }
    }
    status(ConnectionStatus::Offline);
    Ended::Stopped {
        uptime: started.elapsed(),
    }
}

/// Returns whether to keep reading the stream.
async fn handle_event(
    user: &CoreUser,
    siblings: &mut SiblingClientStates,
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
            report(match expires {
                Some(at) if at <= Utc::now() => {
                    QueueEvent::Status(ConnectionStatus::UnsupportedVersion)
                }
                expires => QueueEvent::VersionExpires(expires),
            });
        }
        _ => {}
    }
    match user.process_qs_event(event).await {
        // A partially processed batch leaves a gap in the sequence: drop the
        // stream and resume from the stored position on reconnect.
        Ok(result) => !matches!(result, QsProcessEventResult::PartiallyProcessed { .. }),
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
