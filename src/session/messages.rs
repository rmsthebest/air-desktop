//! The open chat's messages: a window of at most [`MAX_WINDOW`] messages
//! around what the user is looking at, paged in both directions and kept
//! current from database changes; plus marking the chat as read. Port of
//! upstream `applogic`'s `message_list_cubit.rs` and `mark_as_read.rs`.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use aircommon::identifiers::{MimiId, UserId as CoreUserId};
use aircommon::utils::FibonacciBackoff;
use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::{DbEntityId, DbOperation};
use aircoreclient::{
    ChatId as CoreChatId, ChatMessage, EventMessage, Message as CoreMessage,
    MessageId as CoreMessageId, ReadReceiptsSetting,
};
use chrono::{DateTime, Utc};
use gpui::{EventEmitter, SharedString};
use gpui_kit::{App, AsyncApp, Context, Entity, Subscription, Task, WeakEntity};
use mimi_content::MessageStatus as CoreStatus;
use tokio::sync::mpsc;
use tracing::{error, warn};
use uuid::Uuid;

use crate::markdown;
use crate::model::{ChatId, Message, MessageBody, MessageId, MessageStatus, Quote, UserId};
use crate::runtime::{Tokio, TokioTask};
use crate::session::contacts::{Failure, on_tokio};
use crate::session::pending::{Changes, Pending};
use crate::session::preview::{Body, body_of, quote_line};
use crate::session::{Session, SessionEvent, display_name};

/// Messages loaded per page. As upstream.
const PAGE: usize = 64;
/// Most messages kept loaded; paging further drops them from the far end.
const MAX_WINDOW: usize = PAGE * 10;
/// How long the newest seen message has to stay put before the chat is
/// marked as read up to it. As upstream.
const MARK_AS_READ_DEBOUNCE: Duration = Duration::from_millis(300);
const COMMANDS_BUFFER: usize = 8;
const UPDATES_BUFFER: usize = 8;

pub enum MessagesEvent {
    /// The first messages arrived: show the unread divider, or the newest.
    Opened,
    /// The messages in `old` were replaced by `count` new ones. `paged`:
    /// they were paged in, not just sent, so the view shouldn't jump to
    /// them.
    Spliced {
        old: Range<usize>,
        count: usize,
        paged: bool,
    },
    /// The user asked for the newest messages: show them.
    ScrolledToBottom,
}

pub struct Messages {
    messages: Vec<Message>,
    has_older: bool,
    has_newer: bool,
    first_unread: Option<MessageId>,
    loaded: bool,
    error: Option<SharedString>,
    /// A page was asked for and hasn't arrived: don't ask again.
    loading_older: bool,
    loading_newer: bool,
    /// Seen up to here (and marked as read, or about to be, when live).
    read_until: Option<(DateTime<Utc>, MessageId)>,
    live: Option<Live>,
}

/// What a `Messages` backed by the account has on top of the window.
struct Live {
    user: CoreUser,
    chat: CoreChatId,
    commands: mpsc::Sender<Command>,
    pending: Arc<Pending>,
    /// Seen, but not yet marked (the debounce is running).
    unmarked: Option<CoreMessageId>,
    mark_task: Option<Task<()>>,
    tokio: tokio::runtime::Handle,
    _worker: TokioTask<()>,
    _receive: Task<()>,
    _subscription: Subscription,
}

impl Live {
    /// Marks what was seen as read now. Not cancelled once started:
    /// marking and its receipts belong together.
    fn flush(&mut self) {
        if let Some(until) = self.unmarked.take() {
            self.tokio
                .spawn(mark_as_read(self.user.clone(), self.chat, until));
        }
    }
}

impl Drop for Live {
    /// Closing the chat within the debounce still marks what was seen.
    fn drop(&mut self) {
        self.flush();
    }
}

impl EventEmitter<MessagesEvent> for Messages {}

impl Messages {
    /// Fixed messages, for mock data and tests.
    pub fn fixed(messages: Vec<Message>) -> Self {
        Self {
            messages,
            has_older: false,
            has_newer: false,
            first_unread: None,
            loaded: true,
            error: None,
            loading_older: false,
            loading_newer: false,
            read_until: None,
            live: None,
        }
    }

    /// The messages of `chat`, loaded from the account. Opens at the first
    /// unread message, or at the newest.
    pub fn open(session: &Entity<Session>, chat: &ChatId, cx: &mut Context<Self>) -> Self {
        let user = session.read(cx).user().clone();
        let chat = core_chat_id(chat);
        let pending = Arc::new(Pending::default());
        let (commands_tx, commands_rx) = mpsc::channel(COMMANDS_BUFFER);
        let (updates_tx, updates_rx) = mpsc::channel(UPDATES_BUFFER);
        // Subscribed before the worker loads anything, so no change is
        // missed (they queue in `pending`).
        let subscription = cx.subscribe(session, |this: &mut Self, _, event, _| match event {
            SessionEvent::Changed {
                notifications,
                overflowed,
            } => {
                if let Some(live) = &this.live {
                    live.pending.add(notifications, *overflowed);
                }
            }
        });
        let worker = Worker {
            user: user.clone(),
            chat,
            window: Window::default(),
            names: HashMap::new(),
            tx: updates_tx,
        };
        let live = Live {
            _worker: Tokio::spawn(cx, worker.run(commands_rx, pending.clone())),
            _receive: cx.spawn(async move |this, cx| receive(this, updates_rx, cx).await),
            user,
            chat,
            commands: commands_tx,
            pending,
            unmarked: None,
            mark_task: None,
            tokio: Tokio::handle(cx),
            _subscription: subscription,
        };
        Self {
            loaded: false,
            live: Some(live),
            ..Self::fixed(Vec::new())
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Where the "new messages" divider goes.
    pub fn first_unread(&self) -> Option<usize> {
        let id = self.first_unread.as_ref()?;
        self.messages.iter().position(|message| &message.id == id)
    }

    /// Fixed messages with an unread divider before `first_unread`.
    #[cfg(test)]
    pub fn with_first_unread(self, first_unread: MessageId) -> Self {
        Self {
            first_unread: Some(first_unread),
            ..self
        }
    }

    #[cfg(test)]
    pub fn read_until(&self) -> Option<&MessageId> {
        self.read_until.as_ref().map(|(_, id)| id)
    }

    /// Asks for the page before the oldest loaded message, if there is one.
    pub fn load_older(&mut self) {
        if self.has_older && !self.loading_older {
            self.loading_older = self.command(Command::LoadOlder);
        }
    }

    /// Asks for the page after the newest loaded message, if there is one.
    pub fn load_newer(&mut self) {
        if self.has_newer && !self.loading_newer {
            self.loading_newer = self.command(Command::LoadNewer);
        }
    }

    /// Loads the newest messages, then emits [`MessagesEvent::ScrolledToBottom`].
    pub fn jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        if self.live.is_some() && self.has_newer {
            self.command(Command::JumpToBottom);
        } else {
            cx.emit(MessagesEvent::ScrolledToBottom);
        }
    }

    /// Whether the command was queued. A full queue already has work that
    /// will update the window; the view asks again when it changes.
    fn command(&self, command: Command) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.commands.try_send(command).is_ok())
    }

    /// The user has seen the message at `ix` (the newest visible one, in an
    /// active window). Marks the chat as read up to it, debounced.
    pub fn seen(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(message) = self.messages.get(ix) else {
            return;
        };
        // In the order the core keeps messages: by time, then id.
        let covered = self.read_until.as_ref().is_some_and(|(at, id)| {
            (*at, id.0.as_ref()) >= (message.sent_at, message.id.0.as_ref())
        });
        if covered {
            return;
        }
        self.read_until = Some((message.sent_at, message.id.clone()));
        let (Some(live), Some(until)) = (&mut self.live, core_message_id(&message.id)) else {
            return;
        };
        live.unmarked = Some(until);
        // A newer message seen within the debounce replaces this one.
        live.mark_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(MARK_AS_READ_DEBOUNCE).await;
            _ = this.update(cx, |this, _| {
                if let Some(live) = &mut this.live {
                    live.flush();
                }
            });
        }));
    }

    /// Adds `emoji` to the message's reactions, or retracts it (`add`
    /// false): every reaction of the user's that labels the same, as the
    /// view shows them as one. The caller decides from what it shows: two
    /// presses before the first lands both add, the second a no-op in the
    /// core, leaving the reaction on; a retract the store no longer needs
    /// (another device took it back) does nothing. A failed retract can
    /// leave some variants; pressing again takes the rest. `None` for fixed messages (no account). Runs detached, as
    /// sends do.
    pub fn set_reaction(
        &self,
        id: &MessageId,
        emoji: &str,
        add: bool,
        cx: &App,
    ) -> Option<impl Future<Output = Result<(), String>> + use<>> {
        let live = self.live.as_ref()?;
        let (user, chat, id) = (live.user.clone(), live.chat, core_message_id(id));
        let emoji = emoji.to_string();
        Some(on_tokio(cx, "react", async move {
            let id =
                id.ok_or_else(|| Failure::Shown("That message can't take reactions".into()))?;
            set_reaction(&user, chat, id, emoji, add).await
        }))
    }

    /// Appends a message locally (the mock echo of a send).
    pub fn push(&mut self, message: Message, cx: &mut Context<Self>) {
        // The worker owns a live window: a local insert would desync it.
        debug_assert!(self.live.is_none(), "push is for fixed messages");
        let at = self.messages.len();
        self.messages.push(message);
        cx.emit(MessagesEvent::Spliced {
            old: at..at,
            count: 1,
            paged: false,
        });
        cx.notify();
    }

    fn update(&mut self, update: Update, cx: &mut Context<Self>) {
        match update {
            Update::Window(window) => {
                // While newer messages are still unloaded, anything that
                // arrives already existed.
                let paged = self.has_newer;
                for change in window.changes {
                    let (old, count) = change.apply(&mut self.messages);
                    cx.emit(MessagesEvent::Spliced { old, count, paged });
                }
                self.has_older = window.has_older;
                self.has_newer = window.has_newer;
                if let Some(first_unread) = window.first_unread {
                    self.first_unread = first_unread;
                }
                if !self.loaded {
                    self.loaded = true;
                    cx.emit(MessagesEvent::Opened);
                }
                self.error = None;
                if window.scroll_to_bottom {
                    cx.emit(MessagesEvent::ScrolledToBottom);
                }
            }
            Update::Failed(message) => self.error = Some(message.into()),
        }
        // Whatever was asked for has been answered.
        self.loading_older = false;
        self.loading_newer = false;
        cx.notify();
    }
}

/// Applies updates in batches, so a burst costs one redraw.
async fn receive(this: WeakEntity<Messages>, mut rx: mpsc::Receiver<Update>, cx: &mut AsyncApp) {
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

/// Marks the chat as read up to `until` and reports it: to the chat if the
/// user sends read receipts, else only to their own devices (through the
/// self chat), which follow the same report. As upstream.
pub(super) async fn mark_as_read(user: CoreUser, chat: CoreChatId, until: CoreMessageId) {
    let read = match user.mark_chat_as_read(chat, until).await {
        Ok((_, read)) => read,
        Err(err) => return error!("failed to mark chat as read: {err:#}"),
    };
    if read.is_empty() {
        return;
    }
    let receipts = user
        .user_setting::<ReadReceiptsSetting>()
        .await
        .is_none_or(|ReadReceiptsSetting(enabled)| enabled);
    let report_to = if receipts {
        Some(chat)
    } else {
        match user.self_chat_id().await {
            Ok(self_chat) => self_chat,
            Err(err) => return error!("failed to load the self chat: {err:#}"),
        }
    };
    if let Some(report_to) = report_to {
        let statuses = read
            .iter()
            .map(|(id, mimi_id)| (*id, mimi_id, CoreStatus::Read));
        if let Err(err) = user
            .outbound_service()
            .enqueue_receipts(report_to, statuses)
            .await
        {
            error!("failed to enqueue read receipts: {err:#}");
        }
    }
}

pub(super) fn core_chat_id(id: &ChatId) -> CoreChatId {
    // Model chat ids from the session are core uuids.
    CoreChatId {
        uuid: Uuid::parse_str(&id.0).unwrap_or_default(),
    }
}

pub(super) fn core_message_id(id: &MessageId) -> Option<CoreMessageId> {
    Uuid::parse_str(&id.0).ok().map(CoreMessageId::new)
}

fn model_message_id(id: CoreMessageId) -> MessageId {
    MessageId(id.uuid().to_string().into())
}

enum Command {
    LoadOlder,
    LoadNewer,
    JumpToBottom,
}

pub(super) enum Update {
    Window(WindowUpdate),
    Failed(String),
}

pub(super) struct WindowUpdate {
    pub(super) changes: Vec<Change<Message>>,
    has_older: bool,
    has_newer: bool,
    /// `Some` when the unread divider moved (`Some(None)`: it went away).
    first_unread: Option<Option<MessageId>>,
    scroll_to_bottom: bool,
}

/// One edit of the window, in oldest-first order.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Change<T> {
    Replace(Vec<T>),
    Insert { at: usize, items: Vec<T> },
    Remove { at: usize, count: usize },
    Patch { at: usize, item: T },
}

impl<T> Change<T> {
    /// Applies the edit; returns the replaced range and the new count, for
    /// the list view's splice.
    fn apply(self, items: &mut Vec<T>) -> (Range<usize>, usize) {
        match self {
            Change::Replace(new) => {
                let old = 0..items.len();
                let count = new.len();
                *items = new;
                (old, count)
            }
            Change::Insert { at, items: new } => {
                let count = new.len();
                items.splice(at..at, new);
                (at..at, count)
            }
            Change::Remove { at, count } => {
                items.drain(at..at + count);
                (at..at + count, 0)
            }
            Change::Patch { at, item } => {
                items[at] = item;
                (at..at + 1, 1)
            }
        }
    }
}

/// Identifies a loaded message and where it sorts.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    id: CoreMessageId,
    at: DateTime<Utc>,
    /// Whose name the message shows, to refresh it when they rename.
    sender: Option<CoreUserId>,
    /// Whose message it quotes, likewise.
    quoted: Option<CoreUserId>,
    /// The message it quotes, to refresh the quote when that changes.
    quotes: Option<CoreMessageId>,
    /// The quoted message's MIMI id while it isn't here, to pick it up
    /// when it arrives.
    awaits: Option<MimiId>,
}

/// The loaded window's bookkeeping: what the view shows, as keys. Each
/// method returns the edits that bring the view's list in line.
#[derive(Default)]
struct Window {
    keys: Vec<Key>,
    has_older: bool,
    has_newer: bool,
    /// Loaded at least once.
    loaded: bool,
}

impl Window {
    fn at_bottom(&self) -> bool {
        !self.has_newer
    }

    fn position(&self, id: &CoreMessageId) -> Option<usize> {
        self.keys.iter().position(|key| &key.id == id)
    }

    fn replace<T>(
        &mut self,
        items: Vec<(Key, T)>,
        has_older: bool,
        has_newer: bool,
    ) -> Vec<Change<T>> {
        let (keys, items) = items.into_iter().unzip();
        self.keys = keys;
        self.has_older = has_older;
        self.has_newer = has_newer;
        self.loaded = true;
        vec![Change::Replace(items)]
    }

    /// Older messages go in front; past [`MAX_WINDOW`], the newest drop.
    fn prepend<T>(&mut self, items: Vec<(Key, T)>, has_older: bool) -> Vec<Change<T>> {
        let (keys, items): (Vec<_>, Vec<_>) = items.into_iter().unzip();
        self.keys.splice(0..0, keys);
        self.has_older = has_older;
        let mut changes = vec![Change::Insert { at: 0, items }];
        let evict = self.keys.len().saturating_sub(MAX_WINDOW);
        if evict > 0 {
            self.keys.truncate(MAX_WINDOW);
            self.has_newer = true;
            changes.push(Change::Remove {
                at: MAX_WINDOW,
                count: evict,
            });
        }
        changes
    }

    /// Newer messages go at the end; past [`MAX_WINDOW`], the oldest drop.
    fn append<T>(&mut self, items: Vec<(Key, T)>, has_newer: bool) -> Vec<Change<T>> {
        let (keys, items): (Vec<_>, Vec<_>) = items.into_iter().unzip();
        let at = self.keys.len();
        self.keys.extend(keys);
        self.has_newer = has_newer;
        let mut changes = vec![Change::Insert { at, items }];
        let evict = self.keys.len().saturating_sub(MAX_WINDOW);
        if evict > 0 {
            self.keys.drain(..evict);
            self.has_older = true;
            changes.push(Change::Remove {
                at: 0,
                count: evict,
            });
        }
        changes
    }

    fn remove<T>(&mut self, id: &CoreMessageId) -> Option<Change<T>> {
        let at = self.position(id)?;
        self.keys.remove(at);
        Some(Change::Remove { at, count: 1 })
    }

    fn patch<T>(&mut self, key: Key, item: T) -> Option<Change<T>> {
        let at = self.position(&key.id)?;
        self.keys[at] = key;
        Some(Change::Patch { at, item })
    }
}

/// Owns the window on tokio and turns commands and database changes into
/// updates for the entity.
struct Worker {
    user: CoreUser,
    chat: CoreChatId,
    window: Window,
    /// Display names shown in the window; dropped when the user changes.
    names: HashMap<CoreUserId, SharedString>,
    tx: mpsc::Sender<Update>,
}

/// The entity is gone: stop.
struct Closed;

/// Runs a worker for `chat` on the current tokio runtime, outside GPUI.
#[cfg(test)]
pub(super) fn start(
    user: CoreUser,
    chat: CoreChatId,
    pending: Arc<Pending>,
) -> (tokio::task::JoinHandle<()>, mpsc::Receiver<Update>) {
    let (commands_tx, commands_rx) = mpsc::channel(COMMANDS_BUFFER);
    let (tx, rx) = mpsc::channel(UPDATES_BUFFER);
    let worker = Worker {
        user,
        chat,
        window: Window::default(),
        names: HashMap::new(),
        tx,
    };
    let task = tokio::spawn(async move {
        // Keeps the command channel open for the worker's lifetime.
        let _commands = commands_tx;
        worker.run(commands_rx, pending).await
    });
    (task, rx)
}

impl Worker {
    async fn run(mut self, mut commands: mpsc::Receiver<Command>, pending: Arc<Pending>) {
        let mut backoff = FibonacciBackoff::default();
        while !self.window.loaded {
            if self.initial_load().await.is_err() {
                return;
            }
            if !self.window.loaded {
                tokio::time::sleep(backoff.next_backoff()).await;
            }
        }
        loop {
            let result = tokio::select! {
                command = commands.recv() => match command {
                    Some(Command::LoadOlder) => self.load_older().await,
                    Some(Command::LoadNewer) => self.load_newer().await,
                    Some(Command::JumpToBottom) => self.load_bottom(true).await,
                    None => return,
                },
                _ = pending.wake.notified() => self.changed(pending.take()).await,
            };
            if result.is_err() {
                return;
            }
        }
    }

    async fn send(
        &self,
        changes: Vec<Change<Message>>,
        first_unread: Option<Option<MessageId>>,
        scroll_to_bottom: bool,
    ) -> Result<(), Closed> {
        let update = Update::Window(WindowUpdate {
            changes,
            has_older: self.window.has_older,
            has_newer: self.window.has_newer,
            first_unread,
            scroll_to_bottom,
        });
        self.tx.send(update).await.map_err(|_| Closed)
    }

    async fn fail(&self, what: &str, err: anyhow::Error) -> Result<(), Closed> {
        error!("failed to {what}: {err:#}");
        let message = format!("Couldn't {what}: {err:#}");
        self.tx
            .send(Update::Failed(message))
            .await
            .map_err(|_| Closed)
    }

    /// Opens at the first unread message, else at the newest.
    async fn initial_load(&mut self) -> Result<(), Closed> {
        let first_unread = match self.user.first_unread_message(self.chat).await {
            Ok(first_unread) => first_unread,
            Err(err) => {
                error!("failed to load the first unread message: {err:#}");
                None
            }
        };
        let Some(unread) = first_unread else {
            return self.load_bottom(false).await;
        };
        let around = self
            .user
            .messages_around(self.chat, unread.timestamp().into(), unread.id(), PAGE)
            .await;
        match around {
            Ok((messages, has_older, has_newer)) => {
                let items = self.convert_all(messages).await;
                let changes = self.window.replace(items, has_older, has_newer);
                let divider = Some(Some(model_message_id(unread.id())));
                self.send(changes, divider, false).await
            }
            Err(err) => self.fail("load messages", err).await,
        }
    }

    async fn load_bottom(&mut self, scroll_to_bottom: bool) -> Result<(), Closed> {
        // One more than a page tells whether there are older ones.
        match self.user.messages(self.chat, PAGE + 1).await {
            Ok(mut messages) => {
                let has_older = messages.len() > PAGE;
                if has_older {
                    messages.remove(0);
                }
                let items = self.convert_all(messages).await;
                let changes = self.window.replace(items, has_older, false);
                self.send(changes, None, scroll_to_bottom).await
            }
            Err(err) => self.fail("load messages", err).await,
        }
    }

    async fn load_older(&mut self) -> Result<(), Closed> {
        let Some(oldest) = self.window.keys.first().cloned() else {
            // Everything loaded was deleted: start over.
            return self.load_bottom(false).await;
        };
        let before = self
            .user
            .messages_before(self.chat, oldest.at.into(), oldest.id, PAGE)
            .await;
        match before {
            Ok((messages, has_older)) => {
                let items = self.convert_all(messages).await;
                let changes = self.window.prepend(items, has_older);
                self.send(changes, None, false).await
            }
            Err(err) => self.fail("load older messages", err).await,
        }
    }

    async fn load_newer(&mut self) -> Result<(), Closed> {
        let Some(newest) = self.window.keys.last().cloned() else {
            return self.load_bottom(false).await;
        };
        let after = self
            .user
            .messages_after(self.chat, newest.at.into(), newest.id, PAGE)
            .await;
        match after {
            Ok((messages, has_newer)) => {
                let items = self.convert_all(messages).await;
                let changes = self.window.append(items, has_newer);
                self.send(changes, None, false).await
            }
            Err(err) => self.fail("load newer messages", err).await,
        }
    }

    /// Follows database changes, as upstream: removed messages leave the
    /// window, changed ones are reloaded in place, and new ones are paged in
    /// while the window shows the newest messages.
    async fn changed(&mut self, changes: Changes) -> Result<(), Closed> {
        if changes.all {
            // Something was missed: start over where the user is.
            return if self.window.at_bottom() {
                self.load_bottom(false).await
            } else {
                self.reload_in_place().await
            };
        }
        let mut edits = Vec::new();
        let mut new_messages = false;
        let mut own_new_message = false;
        let mut renamed = Vec::new();
        let mut arrived = Vec::new();
        for (entity, ops) in &changes.merged.ops {
            match entity {
                DbEntityId::Message(id) if ops.contains(DbOperation::Remove) => {
                    edits.extend(self.window.remove(id));
                }
                // Loaded already (an Add merged with an Update, or paged in
                // since): reload it, whatever happened.
                DbEntityId::Message(id) if self.window.position(id).is_some() => {
                    match self.user.message(*id).await {
                        Ok(Some(message)) => {
                            let (key, item) = self.convert(message).await;
                            edits.extend(self.window.patch(key, item));
                        }
                        Ok(None) => edits.extend(self.window.remove(id)),
                        Err(err) => error!("failed to reload a message: {err:#}"),
                    }
                }
                DbEntityId::Message(id) if ops.contains(DbOperation::Add) => {
                    match self.user.message(*id).await {
                        Ok(Some(message)) if message.chat_id() == self.chat => {
                            new_messages = true;
                            arrived.extend(message.message().mimi_id().copied());
                            // Writing in the chat is engaging with it.
                            own_new_message |= !message.is_sent();
                        }
                        Ok(_) => {}
                        Err(err) => error!("failed to load a new message: {err:#}"),
                    }
                }
                DbEntityId::User(user) if self.names.remove(user).is_some() => {
                    renamed.push(user.clone());
                }
                _ => {}
            }
        }
        for user in renamed {
            edits.extend(self.reload_from(&user).await);
        }
        // Replies show their original: refresh them when it changes.
        let changed: Vec<_> = changes
            .merged
            .ops
            .keys()
            .filter_map(|entity| match entity {
                DbEntityId::Message(id) => Some(*id),
                _ => None,
            })
            .collect();
        edits.extend(self.reload_replies_to(&changed, &arrived).await);
        let divider = own_new_message.then_some(None);
        if !edits.is_empty() || divider.is_some() {
            self.send(edits, divider, false).await?;
        }
        if new_messages && self.window.at_bottom() {
            // Paged in without the `has_newer` check: the window was at the
            // bottom, so these are the only newer ones.
            self.window.has_newer = true;
            self.load_newer().await?;
        }
        Ok(())
    }

    /// Reloads the loaded replies to any of `originals`, or awaiting one
    /// of the `arrived`.
    async fn reload_replies_to(
        &mut self,
        originals: &[CoreMessageId],
        arrived: &[MimiId],
    ) -> Vec<Change<Message>> {
        let ids: Vec<_> = self
            .window
            .keys
            .iter()
            .filter(|key| {
                key.quotes.is_some_and(|id| originals.contains(&id))
                    || key.awaits.is_some_and(|id| arrived.contains(&id))
            })
            .map(|key| key.id)
            .collect();
        let mut edits = Vec::new();
        for id in ids {
            match self.user.message(id).await {
                Ok(Some(message)) => {
                    let (key, item) = self.convert(message).await;
                    edits.extend(self.window.patch(key, item));
                }
                Ok(None) => {}
                Err(err) => error!("failed to reload a reply: {err:#}"),
            }
        }
        edits
    }

    /// Reloads the messages of a sender whose name changed.
    async fn reload_from(&mut self, user: &CoreUserId) -> Vec<Change<Message>> {
        let ids: Vec<_> = self
            .window
            .keys
            .iter()
            .filter(|key| key.sender.as_ref() == Some(user) || key.quoted.as_ref() == Some(user))
            .map(|key| key.id)
            .collect();
        let mut edits = Vec::new();
        for id in ids {
            if let Ok(Some(message)) = self.user.message(id).await {
                let (key, item) = self.convert(message).await;
                edits.extend(self.window.patch(key, item));
            }
        }
        edits
    }

    /// Reloads the window from its oldest message on.
    async fn reload_in_place(&mut self) -> Result<(), Closed> {
        let Some(oldest) = self.window.keys.first().cloned() else {
            return self.load_bottom(false).await;
        };
        let from = self
            .user
            .messages_from(self.chat, oldest.at.into(), oldest.id, PAGE)
            .await;
        match from {
            Ok((messages, has_newer)) => {
                let items = self.convert_all(messages).await;
                let has_older = self.window.has_older;
                let changes = self.window.replace(items, has_older, has_newer);
                self.send(changes, None, false).await
            }
            Err(err) => self.fail("reload messages", err).await,
        }
    }

    async fn convert_all(&mut self, messages: Vec<ChatMessage>) -> Vec<(Key, Message)> {
        let mut items = Vec::with_capacity(messages.len());
        for message in messages {
            items.push(self.convert(message).await);
        }
        items
    }

    async fn name(&mut self, user: &CoreUserId) -> SharedString {
        if let Some(name) = self.names.get(user) {
            return name.clone();
        }
        let name = display_name(&self.user, user).await;
        self.names.insert(user.clone(), name.clone());
        name
    }

    /// Turns a stored message into what the view shows. Port of
    /// `UiChatMessage::from_message`, minus what isn't shown yet
    /// (attachments).
    async fn convert(&mut self, message: ChatMessage) -> (Key, Message) {
        let sender = message.message().actor().cloned();
        let sender_name = match &sender {
            Some(sender) => self.name(sender).await,
            None => SharedString::default(),
        };
        let own = sender.as_ref() == Some(self.user.user_id());
        // As upstream: deletion is read from the content, which peer
        // status reports can't overwrite.
        let deleted = message.message().is_deleted();
        let status = match message.status() {
            _ if deleted => MessageStatus::Sent,
            CoreStatus::Error => MessageStatus::Failed,
            CoreStatus::Read => MessageStatus::Read,
            CoreStatus::Delivered => MessageStatus::Delivered,
            _ if !message.is_sent() => MessageStatus::Sending,
            _ => MessageStatus::Sent,
        };
        let body = if deleted {
            MessageBody::Notice("Message deleted".into())
        } else if message.status() == CoreStatus::Hidden {
            MessageBody::Notice("Hidden message from a blocked contact".into())
        } else {
            match message.message() {
                CoreMessage::Content(content) => match body_of(content.content()) {
                    Body::Markdown(source) => MessageBody::Text {
                        markdown: Arc::new(markdown::parse(&source)),
                        source: source.into(),
                    },
                    Body::Attachment => {
                        MessageBody::Notice("Attachments aren't supported yet".into())
                    }
                    Body::Unsupported => MessageBody::Notice("Unsupported message".into()),
                },
                CoreMessage::Event(event @ EventMessage::System(_)) => {
                    // Names in system messages are peer-chosen too.
                    let text = event.string_representation(&self.user).await;
                    MessageBody::Event(crate::model::preview_line(&text))
                }
                CoreMessage::Event(EventMessage::Error(error)) => {
                    warn!("showing an error message in a chat");
                    MessageBody::Event(crate::model::preview_line(error.message()))
                }
            }
        };
        let original = match message.in_reply_to() {
            Some((_, Some(original))) => Some(original),
            _ => None,
        };
        // The core finds the original by its id alone: a reply must not
        // pull in a message from another chat.
        let in_chat = match original {
            Some(original) => matches!(
                self.user.message(original.message_id).await,
                Ok(Some(found)) if found.chat_id() == self.chat
            ),
            None => false,
        };
        let (quoted, quotes, reply_to) = match original {
            Some(original) if in_chat => (
                Some(original.sender.clone()),
                Some(original.message_id),
                Some(Quote {
                    sender: self.name(&original.sender).await,
                    text: quote_line(original.mimi_content.as_ref()),
                }),
            ),
            // Gone, not here (yet), or from elsewhere.
            _ if message.in_reply_to().is_some() => (
                None,
                original.map(|original| original.message_id),
                Some(Quote {
                    sender: SharedString::default(),
                    text: quote_line(None),
                }),
            ),
            _ => (None, None, None),
        };
        // A deleted or hidden message's reactions aren't shown either.
        let (reactions, more_reactions) = if matches!(body, MessageBody::Text { .. }) {
            crate::model::reactions(
                message
                    .reactions()
                    .iter()
                    .map(|(emoji, users)| (emoji.as_str(), users.as_slice())),
                self.user.user_id(),
            )
        } else {
            (Vec::new(), 0)
        };
        let edited = match message.message() {
            CoreMessage::Content(content) => content.edited_at().is_some(),
            CoreMessage::Event(_) => false,
        };
        let key = Key {
            id: message.id(),
            at: message.timestamp(),
            sender: sender.clone(),
            quoted,
            quotes,
            awaits: match message.in_reply_to() {
                Some((mimi_id, None)) => Some(*mimi_id),
                _ => None,
            },
        };
        let item = Message {
            id: model_message_id(message.id()),
            sender_id: sender.map(|sender| UserId(sender.uuid().to_string().into())),
            sender_name,
            own,
            body,
            sent_at: message.timestamp(),
            status,
            edited,
            reply_to,
            reactions,
            more_reactions,
        };
        (key, item)
    }
}

/// See [`Messages::set_reaction`].
pub(super) async fn set_reaction(
    user: &CoreUser,
    chat: CoreChatId,
    message: CoreMessageId,
    emoji: String,
    add: bool,
) -> Result<(), Failure> {
    // The core finds the message by id alone; only this chat's are ours.
    let found = user.message(message).await?;
    if found.as_ref().is_none_or(|found| found.chat_id() != chat) {
        return Err(Failure::Shown("That message is gone".into()));
    }
    if add {
        user.send_reaction(chat, message, emoji).await?;
        return Ok(());
    }
    let label = crate::model::reaction_label(&emoji);
    let stored = user.message_reactions(message).await?;
    let own = stored.into_iter().filter(|(raw, users)| {
        users.contains(user.user_id()) && crate::model::reaction_label(raw) == label
    });
    for (raw, _) in own {
        user.delete_reaction(chat, message, raw).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u128) -> Key {
        Key {
            id: CoreMessageId::new(Uuid::from_u128(n)),
            at: DateTime::from_timestamp(n as i64, 0).unwrap(),
            sender: None,
            quoted: None,
            quotes: None,
            awaits: None,
        }
    }

    fn items(range: Range<u128>) -> Vec<(Key, u128)> {
        range.map(|n| (key(n), n)).collect()
    }

    /// Applies the window's edits to a mirror, as the entity does, and
    /// checks the two agree.
    fn mirror(window: &Window, view: &mut Vec<u128>, changes: Vec<Change<u128>>) {
        for change in changes {
            change.apply(view);
        }
        let keys: Vec<_> = window
            .keys
            .iter()
            .map(|key| key.id.uuid().as_u128())
            .collect();
        assert_eq!(&keys, view);
    }

    #[test]
    fn paging_keeps_the_window_bounded_and_in_step_with_the_view() {
        let mut window = Window::default();
        let mut view = Vec::new();
        let page = PAGE as u128;
        let start = 100 * page;

        let changes = window.replace(items(start..start + page), true, false);
        mirror(&window, &mut view, changes);
        assert!(window.at_bottom());

        // Scrolling up past the cap drops the newest and leaves the bottom.
        for n in 1..=10 {
            let older = start - n * page;
            let changes = window.prepend(items(older..older + page), true);
            mirror(&window, &mut view, changes);
        }
        assert_eq!(window.keys.len(), MAX_WINDOW);
        assert!(window.has_newer && !window.at_bottom());
        assert_eq!(view.first(), Some(&(start - 10 * page)));

        // Scrolling back down drops the oldest and reaches the bottom again.
        let newest = *view.last().unwrap() + 1;
        let changes = window.append(items(newest..newest + page), false);
        mirror(&window, &mut view, changes);
        assert_eq!(window.keys.len(), MAX_WINDOW);
        assert!(window.has_older && window.at_bottom());
        assert_eq!(view.last(), Some(&(newest + page - 1)));
    }

    #[gpui::test]
    fn what_was_seen_never_moves_back(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        let at = Utc::now();
        let sender = (crate::model::UserId("u".into()), "Ada".into(), false);
        let message = |id: &str| Message::text(MessageId(id.into()), sender.clone(), "hi", at);
        // Same time: the core orders them by id.
        let messages = cx.new(|_| Messages::fixed(vec![message("a"), message("b")]));
        let read = |cx: &mut gpui::TestAppContext| {
            messages.read_with(cx, |m, _| m.read_until().map(|id| id.0.to_string()))
        };
        messages.update(cx, |m, cx| m.seen(1, cx));
        assert_eq!(read(cx).as_deref(), Some("b"));
        messages.update(cx, |m, cx| m.seen(0, cx));
        assert_eq!(read(cx).as_deref(), Some("b"));
    }

    #[test]
    fn edits_in_place_reach_only_loaded_messages() {
        let mut window = Window::default();
        let mut view = Vec::new();
        let changes = window.replace(items(0..5), false, false);
        mirror(&window, &mut view, changes);

        let changes: Vec<_> = window.remove(&key(2).id).into_iter().collect();
        mirror(&window, &mut view, changes);
        assert_eq!(view, [0, 1, 3, 4]);
        // Messages outside the window are none of its business.
        assert_eq!(window.remove::<u128>(&key(9).id), None);
        assert_eq!(window.patch(key(9), 9), None);

        let patched = window.patch(key(3), 33).unwrap();
        assert_eq!(patched, Change::Patch { at: 2, item: 33 });
        patched.apply(&mut view);
        assert_eq!(view, [0, 1, 33, 4]);
    }
}
