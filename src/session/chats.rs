//! The account's chats, in order, kept current from database changes. Port
//! of upstream `applogic`'s `chats_data_source.rs`: a tokio worker loads all
//! chats once, then reloads only the chats each change touches and sends the
//! UI deltas.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use aircommon::identifiers::UserId;
use aircommon::utils::FibonacciBackoff;
use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::DbEntityId;
use aircoreclient::{Chat, ChatId as CoreChatId, ChatMuted, ChatStatus, ChatType, MessageId};
use chrono::Datelike as _;
use gpui_kit::{AsyncApp, Context, Entity, Subscription, Task, WeakEntity};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::error;

use crate::model::{ChatId, ChatSummary, Mute, OneToOne, Side, Stage, name_line};
use crate::runtime::{Tokio, TokioTask};
use crate::session::pending::Pending;
use crate::session::preview;
use crate::session::{Session, SessionEvent};

/// Chats loaded per delta on startup: about a screenful, so the list shows
/// up before a large account has loaded. As upstream.
const CHUNK: usize = 32;
/// Deltas in flight to the UI; the worker waits beyond this.
const DELTAS_BUFFER: usize = 8;

#[derive(Debug, Clone, PartialEq)]
pub enum LoadState {
    Loading,
    Loaded,
    Failed(String),
}

pub struct Chats {
    chats: Vec<ChatSummary>,
    state: LoadState,
    pending: Arc<Pending>,
    _worker: TokioTask<()>,
    _receive: Task<()>,
    _subscription: Subscription,
}

impl Chats {
    pub fn new(session: &Entity<Session>, cx: &mut Context<Self>) -> Self {
        let user = session.read(cx).user().clone();
        let pending = Arc::new(Pending::default());
        let (tx, rx) = mpsc::channel(DELTAS_BUFFER);
        // The session subscribed to database changes before anything ran;
        // changes from now on queue up in `pending` until the initial load
        // is done, so none is missed.
        let subscription = cx.subscribe(session, |this: &mut Self, _, event, _| match event {
            SessionEvent::Changed {
                notifications,
                overflowed,
            } => this.pending.add(notifications, *overflowed),
        });
        Self {
            chats: Vec::new(),
            state: LoadState::Loading,
            _worker: Tokio::spawn(cx, run(user, pending.clone(), tx)),
            _receive: cx.spawn(async move |this, cx| receive(this, rx, cx).await),
            pending,
            _subscription: subscription,
        }
    }

    pub fn chats(&self) -> &[ChatSummary] {
        &self.chats
    }

    pub fn state(&self) -> &LoadState {
        &self.state
    }

    fn update(&mut self, update: Update, cx: &mut Context<Self>) {
        match update {
            Update::Delta(delta) => {
                apply(&mut self.chats, delta);
                self.state = LoadState::Loaded;
            }
            Update::Failed(message) => self.state = LoadState::Failed(message),
        }
        cx.notify();
    }
}

/// Applies deltas in batches, so a burst costs one redraw.
async fn receive(this: WeakEntity<Chats>, mut rx: mpsc::Receiver<Update>, cx: &mut AsyncApp) {
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while let Ok(update) = rx.try_recv() {
            batch.push(update);
        }
        let applied = this.update(cx, |this, cx| {
            for update in batch {
                this.update(update, cx);
            }
        });
        if applied.is_err() {
            return;
        }
    }
}

pub(super) enum Update {
    Delta(Delta),
    Failed(String),
}

#[derive(Debug, Default)]
pub(super) struct Delta {
    pub(super) upserted: Vec<ChatSummary>,
    pub(super) removed: HashSet<ChatId>,
    /// Only when the order changed.
    pub(super) order: Option<Vec<ChatId>>,
}

impl Delta {
    fn is_empty(&self) -> bool {
        self.upserted.is_empty() && self.removed.is_empty() && self.order.is_none()
    }
}

/// Replaces upserted chats, drops removed ones and applies the new order.
fn apply(chats: &mut Vec<ChatSummary>, delta: Delta) {
    let order = delta
        .order
        .unwrap_or_else(|| chats.iter().map(|chat| chat.id.clone()).collect());
    let mut by_id: HashMap<ChatId, ChatSummary> = chats
        .drain(..)
        .map(|chat| (chat.id.clone(), chat))
        .collect();
    for chat in delta.upserted {
        by_id.insert(chat.id.clone(), chat);
    }
    *chats = order
        .into_iter()
        .filter(|id| !delta.removed.contains(id))
        .filter_map(|id| by_id.remove(&id))
        .collect();
}

pub(super) async fn run(user: CoreUser, pending: Arc<Pending>, tx: mpsc::Sender<Update>) {
    let mut state = LocalState::default();
    let mut backoff = FibonacciBackoff::default();
    loop {
        match initial_load(&user, &mut state, &tx).await {
            Ok(()) => break,
            Err(Stop::Closed) => return,
            Err(Stop::Failed) => tokio::time::sleep(backoff.next_backoff()).await,
        }
    }
    // Set while a chat new to the list waits for an order to place it in.
    let mut order_backoff = FibonacciBackoff::default();
    let mut order_retry: Option<Instant> = None;
    loop {
        let retry = tokio::select! {
            _ = pending.wake.notified() => false,
            _ = tokio::time::sleep_until(order_retry.unwrap_or_else(Instant::now)),
                if order_retry.is_some() => true,
        };
        let changes = pending.take();
        let affected = if changes.all {
            state.published.keys().copied().collect()
        } else {
            state.affected(&changes.entities().cloned().collect())
        };
        // A full reload or an order retry can find chats nothing pointed
        // to, so they run even with nothing affected.
        if affected.is_empty() && !changes.all && !retry {
            continue;
        }
        let (delta, held_back) = reload(&user, affected, &mut state).await;
        if !delta.is_empty() && tx.send(Update::Delta(delta)).await.is_err() {
            return;
        }
        order_retry = if held_back {
            Some(Instant::now() + order_backoff.next_backoff())
        } else {
            order_backoff.reset();
            None
        };
    }
}

enum Stop {
    /// The UI is gone.
    Closed,
    Failed,
}

/// Publishes every chat, in chunks.
async fn initial_load(
    user: &CoreUser,
    state: &mut LocalState,
    tx: &mpsc::Sender<Update>,
) -> Result<(), Stop> {
    let all = match user.ordered_chat_ids().await {
        Ok(all) => all,
        Err(err) => {
            error!("failed to load chats: {err:#}");
            let failed = Update::Failed(format!("{err:#}"));
            tx.send(failed).await.map_err(|_| Stop::Closed)?;
            return Err(Stop::Failed);
        }
    };
    if all.is_empty() {
        let empty = Delta {
            order: Some(Vec::new()),
            ..Delta::default()
        };
        return tx
            .send(Update::Delta(empty))
            .await
            .map_err(|_| Stop::Closed);
    }
    for chunk in all.chunks(CHUNK) {
        let mut upserted = Vec::with_capacity(chunk.len());
        for id in chunk {
            if let Some(chat) = user.chat(id).await {
                let (summary, keys) = load_chat(user, chat).await;
                state.record(*id, keys);
                state.order.push(*id);
                upserted.push(summary);
            }
        }
        let delta = Delta {
            upserted,
            order: Some(state.order.iter().map(model_id).collect()),
            ..Delta::default()
        };
        tx.send(Update::Delta(delta))
            .await
            .map_err(|_| Stop::Closed)?;
    }
    Ok(())
}

/// Also returns whether a chat new to the list was held back for want of
/// an order.
async fn reload(
    user: &CoreUser,
    affected: HashSet<CoreChatId>,
    state: &mut LocalState,
) -> (Delta, bool) {
    let mut delta = Delta::default();
    let mut gone = Vec::new();
    for id in affected {
        match user.chat(&id).await {
            Some(chat) => {
                let (summary, keys) = load_chat(user, chat).await;
                state.record(id, keys);
                delta.upserted.push(summary);
            }
            None if state.contains(&id) => gone.push(id),
            None => {}
        }
    }
    let order = user
        .ordered_chat_ids()
        .await
        .inspect_err(|err| error!("failed to load chat order: {err:#}"))
        .ok();
    for id in order.iter().flatten() {
        if !state.contains(id)
            && let Some(chat) = user.chat(id).await
        {
            // A chat no notification told us about.
            let (summary, keys) = load_chat(user, chat).await;
            state.record(*id, keys);
            delta.upserted.push(summary);
        }
    }
    let held_back = state.reconcile(&mut delta, gone, order);
    (delta, held_back)
}

/// How `chat` is listed.
#[cfg(test)]
pub(super) async fn summary(user: &CoreUser, chat: Chat) -> ChatSummary {
    load_chat(user, chat).await.0
}

impl Session {
    /// Mutes the chat's alerts (`None` unmutes), on this device only, as
    /// upstream, and dismisses the one shown.
    pub fn set_muted(
        &self,
        chat: &ChatId,
        mute: Option<Mute>,
        cx: &gpui_kit::App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat) = (self.user.clone(), super::messages::core_chat_id(chat));
        let what = if mute.is_some() {
            "mute the chat"
        } else {
            "unmute the chat"
        };
        let muted = mute.map(|mute| match mute {
            Mute::Until(until) => ChatMuted::Until(until),
            Mute::Forever => ChatMuted::Forever,
        });
        let alerts = self.alerts.clone();
        super::contacts::on_tokio(cx, what, async move {
            user.set_chat_muted_until(chat, muted)
                .await
                .map_err(|err| super::contacts::Failure::from(err).local(what))?;
            // Its alert on screen goes with it; new ones are skipped.
            if muted.is_some() {
                alerts.dismiss(super::alerts::Dismiss::Chat(chat));
            }
            Ok(())
        })
    }
}

/// A group the user is no longer in: the core keeps it inactive. Any
/// status but active counts (the core gives groups no other today), so
/// nothing is sent to a group that isn't plainly live.
pub(super) fn group_ended(chat: &Chat) -> bool {
    matches!(chat.chat_type, ChatType::Group(_)) && !matches!(chat.status(), ChatStatus::Active)
}

/// The core stores "forever" as the start of 9999 but reads back only
/// later years as forever: a mute that long is forever.
fn mute(muted: &ChatMuted) -> Mute {
    match muted {
        ChatMuted::Until(until) if until.year() < 9999 => Mute::Until(*until),
        ChatMuted::Until(_) | ChatMuted::Forever => Mute::Forever,
    }
}

/// Loads what the list shows for a chat. Port of `load_chat_details`, minus
/// what the list doesn't show yet.
async fn load_chat(user: &CoreUser, chat: Chat) -> (ChatSummary, ChatKeys) {
    let unread = user.unread_messages_count(chat.id).await;
    let last = user
        .last_message(chat.id)
        .await
        .inspect_err(|err| error!("failed to load last message: {err:#}"))
        .ok()
        .flatten();
    let draft = user
        .message_draft(chat.id)
        .await
        .inspect_err(|err| error!("failed to load draft: {err:#}"))
        .ok()
        .flatten();
    let is_self_chat = user
        .chat_is_self_group(&chat.group_id)
        .await
        .inspect_err(|err| error!("failed to check for the self chat: {err:#}"))
        .unwrap_or(false);

    let mut users = Vec::new();
    let (title, is_group) = match &chat.chat_type {
        ChatType::HandleConnection(username) => (username.plaintext().to_string(), false),
        ChatType::Connection(peer)
        | ChatType::TargetedMessageConnection(peer)
        | ChatType::PendingConnection(peer) => {
            users.push(peer.clone());
            let name = user.user_profile(peer).await.display_name.to_string();
            (name, false)
        }
        // The self chat is a group of the user's devices, not one to manage.
        ChatType::Group(attributes) => (attributes.title.clone(), !is_self_chat),
    };
    let one_to_one = one_to_one(&chat.chat_type, chat.status());
    let title = if is_self_chat {
        "Notes to self".to_string()
    } else {
        title
    };

    let last_message = match &last {
        Some(message) => {
            users.extend(message.message().actor().cloned());
            preview::describe(user, message).await
        }
        None => None,
    };
    let summary = ChatSummary {
        id: model_id(&chat.id),
        // Peer-controlled: titles and names arrive unvalidated.
        title: name_line(&title),
        is_group,
        request: one_to_one.and_then(OneToOne::request),
        is_self_chat,
        one_to_one,
        preview: preview::preview(
            draft.as_ref().map(|draft| draft.message.as_str()),
            last_message,
            is_group,
        ),
        last_activity: last
            .as_ref()
            .map(|message| message.timestamp())
            .or(chat.last_message_at),
        unread: u32::try_from(unread).unwrap_or(u32::MAX),
        muted: chat.muted_until.as_ref().map(mute),
        group_ended: group_ended(&chat),
    };
    let keys = ChatKeys {
        last_message: last.map(|message| message.id()),
        users,
    };
    (summary, keys)
}

/// A one-to-one chat's facts, for contact operations and its request
/// state; `None` for groups (the self chat is one). The core ends a
/// request that expired or was withdrawn by making the chat inactive (as
/// upstream reads it).
pub(super) fn one_to_one(chat_type: &ChatType, status: &ChatStatus) -> Option<OneToOne> {
    let (side, peer_known) = match chat_type {
        ChatType::Group(_) => return None,
        ChatType::Connection(_) => (Side::Contact, true),
        ChatType::PendingConnection(_) => (Side::Incoming, true),
        ChatType::TargetedMessageConnection(_) => (Side::Outgoing, true),
        ChatType::HandleConnection(_) => (Side::Outgoing, false),
    };
    let stage = match status {
        ChatStatus::Pending => Stage::Sending,
        ChatStatus::Active => Stage::Open,
        ChatStatus::Inactive(_) => Stage::Ended,
        ChatStatus::Blocked => Stage::Blocked,
    };
    Some(OneToOne {
        side,
        stage,
        peer_known,
    })
}

pub(super) fn model_id(id: &CoreChatId) -> ChatId {
    ChatId(id.uuid.to_string().into())
}

/// What the worker published: enough to tell which chats a change touches.
#[derive(Default)]
struct LocalState {
    published: HashMap<CoreChatId, ChatKeys>,
    order: Vec<CoreChatId>,
}

#[derive(Debug)]
struct ChatKeys {
    last_message: Option<MessageId>,
    /// Users whose names the summary shows: the peer, the last message's
    /// sender. (Upstream's app looks names up live; ours are baked in.)
    users: Vec<UserId>,
}

impl LocalState {
    /// Settles the order after a reload and completes `delta`. `gone`: chats
    /// that no longer exist. `order`: `None` if it failed to load, in which
    /// case the current order is kept (upstream empties the list) and chats
    /// new to the list are held back, since `apply` drops unordered chats.
    /// Returns whether any were.
    fn reconcile(
        &mut self,
        delta: &mut Delta,
        mut gone: Vec<CoreChatId>,
        order: Option<Vec<CoreChatId>>,
    ) -> bool {
        let held_back = match order {
            Some(ids) => {
                let order: Vec<_> = ids.into_iter().filter(|id| self.contains(id)).collect();
                // Chats that left the order without a notification.
                let retained: HashSet<_> = order.iter().copied().collect();
                gone.extend(self.order.iter().filter(|id| !retained.contains(id)));
                if self.order != order {
                    delta.order = Some(order.iter().map(model_id).collect());
                    self.order = order;
                }
                false
            }
            None => {
                let placed: HashSet<_> = self.order.iter().copied().collect();
                let published = self.published.len();
                self.published.retain(|id, _| placed.contains(id));
                let placed: HashSet<_> = placed.iter().map(model_id).collect();
                delta.upserted.retain(|chat| placed.contains(&chat.id));
                self.published.len() < published
            }
        };
        for id in gone {
            self.published.remove(&id);
            self.order.retain(|ordered| ordered != &id);
            delta.removed.insert(model_id(&id));
        }
        held_back
    }

    fn contains(&self, id: &CoreChatId) -> bool {
        self.published.contains_key(id)
    }

    fn record(&mut self, id: CoreChatId, keys: ChatKeys) {
        self.published.insert(id, keys);
    }

    /// Chats a change touches: changed chats, and published chats whose last
    /// message or named users changed.
    fn affected(&self, entities: &BTreeSet<DbEntityId>) -> HashSet<CoreChatId> {
        let mut affected = HashSet::new();
        let mut messages = HashSet::new();
        let mut users = HashSet::new();
        for entity in entities {
            match entity {
                DbEntityId::Chat(id) => {
                    affected.insert(*id);
                }
                DbEntityId::Message(id) => {
                    messages.insert(id);
                }
                DbEntityId::User(id) => {
                    users.insert(id);
                }
                _ => {}
            }
        }
        if messages.is_empty() && users.is_empty() {
            return affected;
        }
        for (id, keys) in &self.published {
            let touched = keys
                .last_message
                .as_ref()
                .is_some_and(|message| messages.contains(message))
                || keys.users.iter().any(|user| users.contains(user));
            if touched {
                affected.insert(*id);
            }
        }
        affected
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_mute_until_the_cores_forever_is_forever() {
        // As the core stores and reads back `ChatMuted::Forever`.
        let stored = ChatMuted::from(ChatMuted::Forever.into_date_time());
        assert_eq!(mute(&stored), Mute::Forever);
        let until = chrono::Utc::now() + chrono::Duration::hours(1);
        assert_eq!(mute(&ChatMuted::Until(until)), Mute::Until(until));
    }

    #[test]
    fn the_cores_chats_map_to_one_to_one_facts() {
        use aircoreclient::ChatStatus;
        let user = aircommon::identifiers::UserId::random("localhost".parse().unwrap());
        let facts = |chat_type: &ChatType, status: &ChatStatus| {
            one_to_one(chat_type, status).map(|chat| (chat.side, chat.stage, chat.peer_known))
        };
        let ended = ChatStatus::inactive(vec![user.clone()]);
        let contact = ChatType::Connection(user.clone());
        let incoming = ChatType::PendingConnection(user.clone());
        let outgoing = ChatType::TargetedMessageConnection(user.clone());
        let by_name = ChatType::HandleConnection(
            aircommon::identifiers::Username::new("ada-lovelace".into()).unwrap(),
        );
        let group = ChatType::Group(aircoreclient::ChatAttributes {
            title: "Engines".into(),
            picture: None,
        });
        assert_eq!(
            facts(&contact, &ChatStatus::Active),
            Some((Side::Contact, Stage::Open, true))
        );
        // The peer deleted the contact: ended, no longer written to.
        assert_eq!(
            facts(&contact, &ended),
            Some((Side::Contact, Stage::Ended, true))
        );
        assert_eq!(
            facts(&contact, &ChatStatus::Blocked),
            Some((Side::Contact, Stage::Blocked, true))
        );
        assert_eq!(
            facts(&incoming, &ChatStatus::Active),
            Some((Side::Incoming, Stage::Open, true))
        );
        assert_eq!(
            facts(&incoming, &ChatStatus::Blocked),
            Some((Side::Incoming, Stage::Blocked, true))
        );
        assert_eq!(
            facts(&outgoing, &ChatStatus::Pending),
            Some((Side::Outgoing, Stage::Sending, true))
        );
        // A request to someone blocked before (a group member, say).
        assert_eq!(
            facts(&outgoing, &ChatStatus::Blocked),
            Some((Side::Outgoing, Stage::Blocked, true))
        );
        // Sent to a username: whom isn't known until they answer.
        assert_eq!(
            facts(&by_name, &ended),
            Some((Side::Outgoing, Stage::Ended, false))
        );
        // Groups, and so the self chat, have none.
        assert_eq!(facts(&group, &ChatStatus::Active), None);
    }

    use uuid::Uuid;

    use super::*;

    fn summary(id: &str, title: &str) -> ChatSummary {
        ChatSummary {
            id: ChatId(id.to_string().into()),
            title: title.to_string().into(),
            is_group: false,
            request: None,
            is_self_chat: false,
            one_to_one: None,
            preview: None,
            last_activity: None,
            unread: 0,
            muted: None,
            group_ended: false,
        }
    }

    fn ids(chats: &[ChatSummary]) -> Vec<String> {
        chats.iter().map(|chat| chat.id.0.to_string()).collect()
    }

    fn id(s: &str) -> ChatId {
        ChatId(s.to_string().into())
    }

    #[test]
    fn deltas_upsert_remove_and_reorder() {
        let mut chats = vec![summary("a", "A"), summary("b", "B"), summary("c", "C")];

        // An update in place keeps the order.
        apply(
            &mut chats,
            Delta {
                upserted: vec![summary("b", "B2")],
                ..Delta::default()
            },
        );
        assert_eq!(ids(&chats), ["a", "b", "c"]);
        assert_eq!(chats[1].title, "B2");

        // A new chat comes with an order; a removed one drops out of it.
        apply(
            &mut chats,
            Delta {
                upserted: vec![summary("d", "D")],
                removed: HashSet::from([id("a")]),
                order: Some(vec![id("d"), id("c"), id("a"), id("b")]),
            },
        );
        assert_eq!(ids(&chats), ["d", "c", "b"]);

        apply(
            &mut chats,
            Delta {
                removed: HashSet::from([id("c")]),
                ..Delta::default()
            },
        );
        assert_eq!(ids(&chats), ["d", "b"]);
    }

    fn user() -> UserId {
        UserId::new(Uuid::new_v4(), "localhost".parse().unwrap())
    }

    fn chat_id() -> CoreChatId {
        CoreChatId {
            uuid: Uuid::new_v4(),
        }
    }

    #[test]
    fn changes_reach_chats_through_their_last_message_and_named_users() {
        let (alice, bob) = (user(), user());
        let message = MessageId::new(Uuid::new_v4());
        let (with_alice, group, quiet) = (chat_id(), chat_id(), chat_id());
        let mut state = LocalState::default();
        state.record(
            with_alice,
            ChatKeys {
                last_message: None,
                users: vec![alice.clone()],
            },
        );
        state.record(
            group,
            ChatKeys {
                last_message: Some(message),
                users: vec![bob.clone()],
            },
        );
        state.record(
            quiet,
            ChatKeys {
                last_message: None,
                users: Vec::new(),
            },
        );
        let affected = |entities: &[DbEntityId]| {
            state.affected(&entities.iter().cloned().collect::<BTreeSet<_>>())
        };

        assert_eq!(
            affected(&[DbEntityId::User(alice)]),
            HashSet::from([with_alice])
        );
        assert_eq!(
            affected(&[DbEntityId::Message(message)]),
            HashSet::from([group])
        );
        // The sender of the last message renamed themselves.
        assert_eq!(affected(&[DbEntityId::User(bob)]), HashSet::from([group]));
        // A chat change reaches the chat even before it was published.
        let new = chat_id();
        assert_eq!(affected(&[DbEntityId::Chat(new)]), HashSet::from([new]));
        // Other users' and other messages' changes reach nothing.
        assert!(
            affected(&[
                DbEntityId::User(user()),
                DbEntityId::Message(MessageId::new(Uuid::new_v4()))
            ])
            .is_empty()
        );
    }

    fn keys() -> ChatKeys {
        ChatKeys {
            last_message: None,
            users: Vec::new(),
        }
    }

    /// What `reload` does for a chat it loaded.
    fn load(state: &mut LocalState, delta: &mut Delta, id: CoreChatId) {
        state.record(id, keys());
        delta.upserted.push(summary(&id.uuid.to_string(), "chat"));
    }

    #[test]
    fn a_failed_order_holds_new_chats_back_until_one_loads() {
        let (old, new) = (chat_id(), chat_id());
        let mut chats = Vec::new();
        let mut state = LocalState::default();
        let mut delta = Delta::default();
        load(&mut state, &mut delta, old);
        assert!(!state.reconcile(&mut delta, Vec::new(), Some(vec![old])));
        apply(&mut chats, delta);

        // The order fails while a new chat and a change to the old one load:
        // the change goes out, the new chat is held back, not lost.
        let mut delta = Delta::default();
        load(&mut state, &mut delta, old);
        load(&mut state, &mut delta, new);
        assert!(state.reconcile(&mut delta, Vec::new(), None));
        assert_eq!(delta.order, None);
        assert!(!state.contains(&new));
        apply(&mut chats, delta);
        assert_eq!(ids(&chats), [old.uuid.to_string()]);

        // The retry loads it as a chat nothing told us about, and places it.
        let mut delta = Delta::default();
        load(&mut state, &mut delta, new);
        assert!(!state.reconcile(&mut delta, Vec::new(), Some(vec![new, old])));
        apply(&mut chats, delta);
        assert_eq!(ids(&chats), [new.uuid.to_string(), old.uuid.to_string()]);
    }

    #[test]
    fn chats_leave_when_gone_or_out_of_the_order() {
        let (deleted, left, kept) = (chat_id(), chat_id(), chat_id());
        let mut chats = Vec::new();
        let mut state = LocalState::default();
        let mut delta = Delta::default();
        for id in [deleted, left, kept] {
            load(&mut state, &mut delta, id);
        }
        state.reconcile(&mut delta, Vec::new(), Some(vec![deleted, left, kept]));
        apply(&mut chats, delta);

        // `deleted` no longer loads; `left` still exists but dropped out of
        // the order without any notification.
        let mut delta = Delta::default();
        assert!(!state.reconcile(&mut delta, vec![deleted], Some(vec![kept])));
        assert_eq!(
            delta.removed,
            HashSet::from([model_id(&deleted), model_id(&left)])
        );
        assert!(!state.contains(&deleted) && !state.contains(&left));
        apply(&mut chats, delta);
        assert_eq!(ids(&chats), [kept.uuid.to_string()]);
    }
}
