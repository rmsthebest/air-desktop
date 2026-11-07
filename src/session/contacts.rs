//! The user's usernames and contact requests. Port of upstream
//! `user_cubit/username.rs` (one listener per own username, kept in line
//! with the stored records) and of the contact parts of `UserCubit` and
//! `ChatsDataSource`.
//!
//! A username listener receives contact requests sent to that username.
//! Processing one stores a pending chat; the UI learns about it from the
//! database notifications like any other change.

use std::collections::{HashMap, HashSet};
use std::future::Future;

use aircommon::identifiers::{Username, UsernameValidationError};
use aircommon::time::TimeStamp;
use aircommon::utils::FibonacciBackoff;
use aircoreclient::UsernameRecord;
use aircoreclient::clients::process::process_as::keep_failed_username_message;
use aircoreclient::clients::{CoreUser, UsernameQueueMessage};
use aircoreclient::db::notification::DbEntityId;
use aircoreclient::{AcceptContactRequestError, AddUsernameContactError, ChatId as CoreChatId};
use gpui::SharedString;
use gpui_kit::App;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_stream::StreamExt as _;
use tracing::{error, info, warn};

use super::Session;
use super::alerts::Alerts;
use super::chats::model_id;
use super::messages::core_chat_id;
use super::queue::HEALTHY_UPTIME;
use crate::model::ChatId;
use crate::runtime::Tokio;

impl Session {
    /// The user's own usernames: what others can add them by.
    pub fn usernames(&self) -> &[SharedString] {
        &self.usernames
    }

    /// Sends a contact request to `input` (a username as typed). Resolves
    /// to the new chat.
    pub fn add_contact(
        &self,
        input: &str,
        cx: &App,
    ) -> impl Future<Output = Result<ChatId, String>> + use<> {
        let operation = add_contact(self.user.clone(), input.to_string());
        on_tokio(cx, "add contact", operation)
    }

    /// Accepts the incoming contact request shown in `chat`. Resolves to the
    /// chat of the new contact, which can differ from `chat`.
    pub fn accept_request(
        &self,
        chat: &ChatId,
        cx: &App,
    ) -> impl Future<Output = Result<ChatId, String>> + use<> {
        let operation = accept_request(self.user.clone(), core_chat_id(chat));
        on_tokio(cx, "accept contact request", operation)
    }

    /// Declines the incoming contact request shown in `chat`. Nothing is
    /// sent to its sender; the chat goes away.
    pub fn decline_request(
        &self,
        chat: &ChatId,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat) = (self.user.clone(), core_chat_id(chat));
        on_tokio(cx, "decline contact request", async move {
            Ok(user.decline_contact_request(chat).await?)
        })
    }

    /// Registers a username. Resolves to it as stored (normalized).
    pub fn add_username(
        &self,
        input: &str,
        cx: &App,
    ) -> impl Future<Output = Result<String, String>> + use<> {
        let operation = add_username(self.user.clone(), input.to_string());
        on_tokio(cx, "add username", operation)
    }

    pub fn remove_username(
        &self,
        input: &str,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let operation = remove_username(self.user.clone(), input.to_string());
        on_tokio(cx, "remove username", operation)
    }
}

pub(super) async fn add_contact(user: CoreUser, input: String) -> Result<ChatId, Failure> {
    let username = parse_username(&input)?;
    let shown = username.plaintext().to_string();
    let not_found = || Failure::Shown(format!("No one has the username {shown}"));
    let Some(hash) = user.check_username_exists(username.clone()).await? else {
        return Err(not_found());
    };
    match user.add_contact(username, hash, false).await? {
        Ok(chat) => Ok(model_id(&chat)),
        Err(err) => Err(match err {
            AddUsernameContactError::UsernameNotFound => not_found(),
            AddUsernameContactError::DuplicateRequest => {
                Failure::Shown(format!("You already sent {shown} a contact request"))
            }
            AddUsernameContactError::OwnUsername => {
                Failure::Shown("That is your own username".into())
            }
            AddUsernameContactError::RateLimited => {
                Failure::Shown("You've sent all the contact requests you can today".into())
            }
        }),
    }
}

pub(super) async fn accept_request(user: CoreUser, chat: CoreChatId) -> Result<ChatId, Failure> {
    match user.accept_contact_request(chat).await? {
        Ok(chat) => Ok(model_id(&chat)),
        Err(AcceptContactRequestError::Unavailable) => Err(Failure::Shown(
            "The request was withdrawn before you accepted it".into(),
        )),
        Err(AcceptContactRequestError::IncompatibleClient { reason }) => {
            warn!("contact request from an incompatible client: {reason}");
            Err(Failure::Shown(
                "The sender's app is too old or too new to connect with".into(),
            ))
        }
    }
}

pub(super) async fn add_username(user: CoreUser, input: String) -> Result<String, Failure> {
    let username = parse_username(&input)?;
    let shown = username.plaintext().to_string();
    if user.usernames().await?.contains(&username) {
        return Err(Failure::Shown(format!("{shown} is already yours")));
    }
    // The core also returns nothing when the server says the username
    // belongs to someone else.
    match user.add_username(username).await? {
        Some(_) => Ok(shown),
        None => Err(Failure::Shown(format!("{shown} is taken"))),
    }
}

pub(super) async fn remove_username(user: CoreUser, input: String) -> Result<(), Failure> {
    let username = parse_username(&input)?;
    if !user.usernames().await?.contains(&username) {
        let shown = username.plaintext();
        return Err(Failure::Shown(format!(
            "{shown} isn't one of your usernames"
        )));
    }
    user.remove_username(&username).await?;
    Ok(())
}

/// Why a contact operation failed: something to tell the user, or an
/// internal error that is logged and reported generically.
#[derive(Debug)]
pub(super) enum Failure {
    Shown(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(err: anyhow::Error) -> Self {
        Self::Internal(err)
    }
}

impl Failure {
    /// For an operation that never goes over the network: an internal
    /// error is logged and reported without blaming the connection.
    pub(super) fn local(self, what: &str) -> Self {
        match self {
            Failure::Internal(err) => {
                error!("failed to {what}: {err:#}");
                Failure::Shown(format!("Couldn't {what}"))
            }
            shown => shown,
        }
    }
}

pub(super) fn on_tokio<T: Send + 'static, F>(
    cx: &App,
    what: &'static str,
    future: F,
) -> impl Future<Output = Result<T, String>> + use<T, F>
where
    F: Future<Output = Result<T, Failure>> + Send + 'static,
{
    // Not aborted with the caller: stopping an operation midway (say, the
    // workspace going away) could skip the core's cleanup of a failed
    // request. Quitting the app still stops it.
    let task = Tokio::handle(cx).spawn(future);
    async move {
        match task.await.map_err(anyhow::Error::from) {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(Failure::Shown(message))) => Err(message),
            Ok(Err(Failure::Internal(err))) | Err(err) => {
                error!("failed to {what}: {err:#}");
                Err(format!(
                    "Couldn't {what}. Check your connection and try again."
                ))
            }
        }
    }
}

/// A username as the user typed it: case and surrounding space don't
/// matter, and a leading `@` is allowed.
fn parse_username(input: &str) -> Result<Username, Failure> {
    let input = input.trim();
    let input = input
        .strip_prefix('@')
        .unwrap_or(input)
        .to_ascii_lowercase();
    Username::new(input).map_err(|err| {
        Failure::Shown(
            match err {
                UsernameValidationError::TooShort => "Usernames have at least 5 characters",
                UsernameValidationError::TooLong => "Usernames have at most 63 characters",
                UsernameValidationError::InvalidCharacter => {
                    "Usernames have only letters, digits and dashes"
                }
                UsernameValidationError::ConsecutiveDashes => {
                    "Usernames don't have two dashes in a row"
                }
                UsernameValidationError::LeadingDigit => "Usernames don't start with a digit",
            }
            .into(),
        )
    })
}

/// Keeps one listener per stored username, restarting a username's when its
/// record changes, and reports the usernames. Runs until dropped (the
/// session aborts it); dropping it stops the listeners too.
pub(super) async fn run(user: CoreUser, alerts: Alerts, report: impl Fn(Vec<SharedString>) + Send) {
    // Subscribe before the first load, so no change is missed.
    let mut notifications = user.db_notifications();
    let mut listeners = Listeners {
        alerts,
        running: HashMap::new(),
    };
    listeners.reconcile(&user, &report, &HashSet::new()).await;
    while let Some(notification) = notifications.next().await {
        let changed: HashSet<Username> = notification
            .ops
            .keys()
            .filter_map(|entity| match entity {
                DbEntityId::Username(username) => Some(username.clone()),
                _ => None,
            })
            .collect();
        if !changed.is_empty() {
            listeners.reconcile(&user, &report, &changed).await;
        }
    }
}

struct Listeners {
    alerts: Alerts,
    running: HashMap<Username, Listener>,
}

/// Aborts the listener task when dropped.
struct Listener(JoinHandle<()>);

impl Drop for Listener {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl Listeners {
    async fn reconcile(
        &mut self,
        user: &CoreUser,
        report: &impl Fn(Vec<SharedString>),
        changed: &HashSet<Username>,
    ) {
        let records = match user.username_records().await {
            Ok(records) => records,
            Err(err) => {
                error!("failed to load username records; not listening to usernames: {err:#}");
                return;
            }
        };
        report(
            records
                .iter()
                .map(|record| record.username.plaintext().to_string().into())
                .collect(),
        );
        self.running
            .retain(|username, _| records.iter().any(|record| &record.username == username));
        for record in records {
            if changed.contains(&record.username) || !self.running.contains_key(&record.username) {
                let username = record.username.clone();
                let task = tokio::spawn(listen(user.clone(), self.alerts.clone(), record));
                // Replacing a listener drops (aborts) the old one.
                self.running.insert(username, Listener(task));
            }
        }
    }
}

/// Listens to one username's queue, reconnecting with backoff as upstream's
/// background stream task does.
async fn listen(user: CoreUser, alerts: Alerts, record: UsernameRecord) {
    let mut backoff = FibonacciBackoff::default();
    loop {
        let started = Instant::now();
        match user.listen_username(&record).await {
            Ok((stream, responder)) => {
                let mut stream = std::pin::pin!(stream);
                while let Some(message) = stream.next().await {
                    // `None` marks the end of the queued messages.
                    let Some(message) = message else { continue };
                    if let Some(id) = handle(&user, &alerts, &record.username, message).await {
                        responder.ack(id).await;
                    }
                }
            }
            // The core drops a username the server doesn't know; the
            // reconcile then stops this listener.
            Err(err) => warn!("could not listen to a username: {err}"),
        }
        if started.elapsed() >= HEALTHY_UPTIME {
            backoff.reset();
        } else {
            let delay = backoff.next_backoff();
            info!(retry_in = ?delay, "username stream ended");
            tokio::time::sleep(delay).await;
        }
    }
}

/// Processes a queued contact request. Returns the id to acknowledge, or
/// `None` to leave the message queued.
async fn handle(
    user: &CoreUser,
    alerts: &Alerts,
    username: &Username,
    message: UsernameQueueMessage,
) -> Option<uuid::Uuid> {
    let id = message.message_id.map(uuid::Uuid::from);
    let created_at = message.created_at.map(TimeStamp::from);
    match user
        .process_username_queue_message(username.clone(), message)
        .await
    {
        Ok(Some(stored)) => alerts.contact_request(user, stored).await,
        Ok(None) => {}
        Err(err) => {
            error!("failed to process a contact request: {err:#}");
            // As upstream: a request for key material that hasn't arrived
            // from a sibling device yet is kept for a while.
            if keep_failed_username_message(&err, created_at) {
                return None;
            }
        }
    }
    if id.is_none() {
        error!("contact request without a message id; can't acknowledge it");
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(input: &str) -> Result<String, String> {
        match parse_username(input) {
            Ok(username) => Ok(username.plaintext().to_string()),
            Err(Failure::Shown(message)) => Err(message),
            Err(Failure::Internal(err)) => panic!("unexpected internal error: {err}"),
        }
    }

    #[test]
    fn usernames_are_read_as_typed_but_never_loosened() {
        assert_eq!(shown("  @Ada-Lovelace \n"), Ok("ada-lovelace".into()));
        // Normalizing stops at case and decoration: anything else is the
        // core's validation, explained rather than silently fixed.
        assert!(shown("ada lovelace").is_err());
        assert!(shown("ada--l").is_err());
        assert!(shown("1adal").is_err());
        assert!(shown("@@adal").is_err());
        // Unicode case folding would turn look-alikes into real names
        // (KELVIN SIGN lowercases to "k").
        assert!(shown("\u{212A}ate-x").is_err());
    }
}
