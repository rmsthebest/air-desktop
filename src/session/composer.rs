//! The open chat's composer: its draft (kept per chat, also across
//! restarts), sending, editing a sent message and replying to one. Port of
//! the draft and send parts of upstream `applogic`'s
//! `chat_details_cubit.rs` and the composer's debouncing in
//! `message_composer.dart`.
//!
//! As upstream, the composer's text is the view's; the draft is what was
//! last stored from it, plus what the message edits or replies to.

use std::time::Duration;

use aircommon::identifiers::MimiId;
use aircommon::{OpenMlsRand as _, RustCrypto};
use aircoreclient::clients::CoreUser;
use aircoreclient::{
    ChatId as CoreChatId, MarkChatAsRead, MessageDraft, MessageEditError,
    MessageId as CoreMessageId,
};
use anyhow::Context as _;
use chrono::{DateTime, Utc};
use gpui::{EventEmitter, SharedString};
use gpui_kit::{App, AsyncApp, Context, Entity, Task, WeakEntity};
use mimi_content::MimiContent;
use tokio::sync::{mpsc, oneshot};
use tracing::error;

use crate::model::{ChatId, MessageId, Quote};
use crate::runtime::{Tokio, TokioTask};
use crate::session::preview::{plain_body, quote_line};
use crate::session::{Session, display_name};

/// How long typing has to pause before the draft is stored. As upstream.
const STORE_DEBOUNCE: Duration = Duration::from_secs(1);

type Job = std::pin::Pin<Box<dyn Future<Output = ()> + Send>>;

/// Runs draft loads, stores and sends one at a time, in the order the UI
/// queued them, for all of the account's chats. So a chat reopened right
/// after it was left loads what leaving it stored, and a late store can't
/// overwrite a newer one.
#[derive(Clone)]
pub struct DraftQueue(mpsc::UnboundedSender<Job>);

impl DraftQueue {
    pub(super) fn start(user: CoreUser, cx: &App) -> (Self, TokioTask<()>) {
        let (queue, run) = Self::new(user);
        (queue, Tokio::spawn(cx, run))
    }

    /// The queue, and the future that runs it.
    pub(super) fn new(user: CoreUser) -> (Self, impl Future<Output = ()> + Send + 'static) {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let run = async move {
            // Drafts still being typed when the app last quit. As upstream,
            // at startup.
            if let Err(err) = user.commit_all_message_drafts().await {
                error!("failed to commit drafts: {err:#}");
            }
            while let Some(job) = rx.recv().await {
                job.await;
            }
        };
        (Self(tx), run)
    }

    pub(super) fn run<R: Send + 'static>(
        &self,
        job: impl Future<Output = R> + Send + 'static,
    ) -> oneshot::Receiver<R> {
        let (tx, rx) = oneshot::channel();
        _ = self.0.send(Box::pin(async move {
            _ = tx.send(job.await);
        }));
        rx
    }
}

pub enum ComposerEvent {
    /// Put this text in the composer: a stored draft (only into an empty
    /// composer), or the message an edit starts from.
    SetText { text: String, replace: bool },
    /// Sending failed. `text` is what was sent, to restore unless the user
    /// started a new message.
    SendFailed { text: String, error: SharedString },
}

/// The stored part of the composer.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Draft {
    pub(super) text: String,
    pub(super) editing: Option<CoreMessageId>,
    pub(super) reply_to: Option<(MimiId, Quote)>,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) committed: bool,
}

impl Draft {
    pub(super) fn empty() -> Self {
        Self {
            text: String::new(),
            editing: None,
            reply_to: None,
            updated_at: Utc::now(),
            committed: false,
        }
    }

    pub(super) fn to_core(&self) -> MessageDraft {
        MessageDraft {
            message: self.text.clone(),
            in_reply_to: self.reply_to.as_ref().map(|(id, _)| (*id, None)),
            editing_id: self.editing,
            updated_at: self.updated_at,
            is_committed: self.committed,
        }
    }
}

/// The draft's state changes, apart from storing them. Each returns whether
/// the draft changed (and so needs storing).
#[derive(Debug, Default)]
struct DraftState {
    draft: Option<Draft>,
    /// Text was typed since the last send: a new message, which a failed
    /// send's reply or edit must not attach to.
    typed_since_send: bool,
}

impl DraftState {
    /// The composer's text, as typed (`committed: false`) or on leaving.
    fn store_text(&mut self, text: &str, committed: bool) -> bool {
        match &mut self.draft {
            None if text.is_empty() => false,
            Some(draft) if draft.text != text => {
                draft.text = text.to_string();
                draft.updated_at = Utc::now();
                draft.committed = committed;
                true
            }
            Some(draft) if committed && !draft.committed => {
                draft.committed = true;
                true
            }
            Some(_) => false,
            None => {
                self.draft = Some(Draft {
                    text: text.to_string(),
                    committed,
                    ..Draft::empty()
                });
                true
            }
        }
    }

    /// Starts editing a sent message, from its text. Replaces whatever was
    /// drafted, unless it is that edit already.
    fn start_edit(&mut self, id: CoreMessageId, text: String) -> bool {
        if self.draft.as_ref().and_then(|draft| draft.editing) == Some(id) {
            return false;
        }
        self.draft = Some(Draft {
            text,
            editing: Some(id),
            ..Draft::empty()
        });
        true
    }

    /// Replies to a message. An edit in progress is dropped (a reply is a
    /// new message); a new message's text is kept.
    fn start_reply(&mut self, mimi_id: MimiId, quote: Quote) -> bool {
        let current = self
            .draft
            .as_ref()
            .and_then(|draft| draft.reply_to.as_ref());
        if current.is_some_and(|(id, _)| *id == mimi_id) {
            return false;
        }
        self.draft.take_if(|draft| draft.editing.is_some());
        let draft = self.draft.get_or_insert_with(Draft::empty);
        draft.reply_to = Some((mimi_id, quote));
        draft.updated_at = Utc::now();
        draft.committed = false;
        true
    }

    /// Escape: drops the reply first, else the edit (and its text).
    fn cancel(&mut self) -> Cancelled {
        let Some(draft) = &mut self.draft else {
            return Cancelled::Nothing;
        };
        if draft.reply_to.take().is_some() {
            draft.updated_at = Utc::now();
            Cancelled::Reply
        } else if draft.editing.is_some() {
            self.draft = None;
            Cancelled::Edit
        } else {
            Cancelled::Nothing
        }
    }

    /// Sending: the draft goes with the message, so nothing done while it
    /// is in flight (another send, leaving the chat) reuses its reply or
    /// edit.
    fn take_for_send(&mut self) -> Option<Draft> {
        self.typed_since_send = false;
        self.draft.take()
    }

    /// The composer's text changed (before it is stored).
    fn typed(&mut self, text: &str) {
        self.typed_since_send |= !text.trim().is_empty();
    }

    /// The send of `text` with `sent` failed: the draft comes back, unless
    /// the user started another one meanwhile. A refused edit comes back as
    /// a new message. Returns whether the draft changed.
    fn send_failed(&mut self, sent: Option<Draft>, text: String, edit_refused: bool) -> bool {
        if self.draft.is_some() || self.typed_since_send {
            return false;
        }
        let mut draft = sent.unwrap_or_else(Draft::empty);
        draft.text = text;
        if edit_refused {
            draft.editing = None;
        }
        draft.updated_at = Utc::now();
        self.draft = Some(draft);
        true
    }
}

#[derive(Debug, PartialEq)]
enum Cancelled {
    Nothing,
    Reply,
    Edit,
}

/// How a send went, as far as the composer cares.
#[derive(Debug)]
pub(super) enum Sent {
    Ok,
    /// The edit can't be made (window passed, limit reached, original
    /// gone); says why.
    EditRefused(&'static str),
    Failed(anyhow::Error),
}

pub struct Composer {
    state: DraftState,
    live: Option<Live>,
}

struct Live {
    user: CoreUser,
    chat: CoreChatId,
    queue: DraftQueue,
    /// The debounced store of what is being typed.
    store_task: Option<Task<()>>,
    /// The latest edit or reply lookup; a newer one replaces it.
    lookup: Option<Task<()>>,
    _load: Task<()>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    /// A composer that stores nothing, for mock data and tests.
    pub fn fixed() -> Self {
        Self {
            state: DraftState::default(),
            live: None,
        }
    }

    /// The composer of `chat`, starting from its stored draft.
    pub fn open(session: &Entity<Session>, chat: &ChatId, cx: &mut Context<Self>) -> Self {
        let user = session.read(cx).user().clone();
        let queue = session.read(cx).drafts().clone();
        let chat = super::messages::core_chat_id(chat);
        let load = queue.run(load_draft(user.clone(), chat));
        let task = cx.spawn(async move |this, cx| {
            let loaded = load.await.context("the draft queue stopped");
            let draft = match loaded.and_then(|draft| draft) {
                Ok(draft) => draft,
                Err(err) => return error!("failed to load the draft: {err:#}"),
            };
            _ = this.update(cx, |this, cx| {
                // Whatever the user did meanwhile is newer.
                if this.state.draft.is_some() {
                    return;
                }
                let Some(draft) = draft else { return };
                let text = draft.text.clone();
                this.state.draft = Some(draft);
                if !text.is_empty() {
                    cx.emit(ComposerEvent::SetText {
                        text,
                        replace: false,
                    });
                }
                cx.notify();
            });
        });
        Self {
            state: DraftState::default(),
            live: Some(Live {
                user,
                chat,
                queue,
                store_task: None,
                lookup: None,
                _load: task,
            }),
        }
    }

    /// Whether this composer stores and sends (else the caller echoes).
    pub fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// Whether the draft edits a sent message.
    pub fn is_editing(&self) -> bool {
        self.state
            .draft
            .as_ref()
            .is_some_and(|draft| draft.editing.is_some())
    }

    /// The message the draft replies to.
    pub fn reply_to(&self) -> Option<&Quote> {
        let draft = self.state.draft.as_ref()?;
        draft.reply_to.as_ref().map(|(_, quote)| quote)
    }

    /// The user typed: stores the text once typing pauses.
    pub fn typed(&mut self, text: String, cx: &mut Context<Self>) {
        self.state.typed(&text);
        let Some(live) = &mut self.live else { return };
        live.store_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STORE_DEBOUNCE).await;
            _ = this.update(cx, |this, _| {
                if this.state.store_text(&text, false) {
                    this.persist();
                }
            });
        }));
    }

    /// The chat is closing with `text` in the composer: commits the draft,
    /// so the chat list shows it and the chat reopens with it.
    pub fn leave(&mut self, text: &str) {
        if let Some(live) = &mut self.live {
            live.store_task = None;
        }
        if self.state.store_text(text, true) {
            self.persist();
        }
    }

    /// Stores the draft through the queue: it completes even if the chat
    /// closes meanwhile.
    fn persist(&self) {
        let Some(live) = &self.live else { return };
        let draft = self.state.draft.as_ref().map(Draft::to_core);
        let (user, chat) = (live.user.clone(), live.chat);
        drop(live.queue.run(async move {
            if let Err(err) = user.store_message_draft(chat, draft.as_ref()).await {
                error!("failed to store the draft: {err:#}");
            }
        }));
    }

    /// Sends `text` as a new message, or as the edit or reply the draft is.
    /// The view has cleared the composer; on failure the text comes back
    /// as [`ComposerEvent::SendFailed`].
    pub fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(live) = &mut self.live else { return };
        live.store_task = None;
        let draft = self.state.take_for_send();
        let (user, chat) = (live.user.clone(), live.chat);
        let task = live.queue.run({
            let (text, draft) = (text.clone(), draft.clone());
            async move { send(&user, chat, text, draft.as_ref()).await }
        });
        let restore = (live.user.clone(), live.chat, live.queue.clone());
        // Not tied to the entity: a send completes even if the chat closes.
        cx.spawn(async move |this, cx| {
            let sent = task
                .await
                .unwrap_or_else(|_| Sent::Failed(anyhow::anyhow!("the draft queue stopped")));
            let (refused, error) = match &sent {
                Sent::Ok => return,
                Sent::EditRefused(reason) => (true, SharedString::from(*reason)),
                Sent::Failed(err) => {
                    error!("failed to send a message: {err:#}");
                    let error = format!("Couldn't send the message: {err:#}");
                    (false, error.into())
                }
            };
            let shown = this.update(cx, |this, cx| {
                if this.state.send_failed(draft.clone(), text.clone(), refused) {
                    this.persist();
                }
                cx.emit(ComposerEvent::SendFailed {
                    text: text.clone(),
                    error: error.clone(),
                });
                cx.notify();
            });
            if shown.is_err() {
                // The chat was closed: keep the text as its draft, unless it
                // has a newer one.
                let mut state = DraftState::default();
                state.send_failed(draft, text, refused);
                let Some(draft) = state.draft.map(|draft| draft.to_core()) else {
                    return;
                };
                let (user, chat, queue) = restore;
                drop(queue.run(async move {
                    if let Ok(None) = user.message_draft(chat).await
                        && let Err(err) = user.store_message_draft(chat, Some(&draft)).await
                    {
                        error!("failed to keep an unsent message as a draft: {err:#}");
                    }
                }));
            }
        })
        .detach();
    }

    /// Edits `message` (an own message), or the last one that can still
    /// be edited.
    pub fn edit(&mut self, message: Option<&MessageId>, cx: &mut Context<Self>) {
        let Some(live) = &self.live else { return };
        let (user, chat) = (live.user.clone(), live.chat);
        let id = message.and_then(super::messages::core_message_id);
        if message.is_some() && id.is_none() {
            return;
        }
        let load = Tokio::spawn(cx, async move {
            let message = match id {
                Some(id) => user.message(id).await?,
                None => user.last_editable_message(chat, Utc::now()).await?,
            };
            let Some(message) = message.filter(|message| message.chat_id() == chat) else {
                return Ok(None);
            };
            if message.message().sender() != Some(user.user_id()) {
                return Ok(None);
            }
            let body = message.message().mimi_content().and_then(plain_body);
            anyhow::Ok(body.map(|body| (message.id(), body.to_string())))
        });
        self.run(cx, async move |this, cx| {
            let Some((id, body)) = load.await?? else {
                return Ok(());
            };
            this.update(cx, |this, cx| {
                if this.state.start_edit(id, body.clone()) {
                    this.persist();
                    cx.emit(ComposerEvent::SetText {
                        text: body,
                        replace: true,
                    });
                    cx.notify();
                }
            })
        });
    }

    /// Replies to `message`.
    pub fn reply(&mut self, message: &MessageId, cx: &mut Context<Self>) {
        let Some(live) = &self.live else { return };
        let Some(id) = super::messages::core_message_id(message) else {
            return;
        };
        let (user, chat) = (live.user.clone(), live.chat);
        let load = Tokio::spawn(cx, async move {
            let Some(message) = user.message(id).await? else {
                return Ok(None);
            };
            let (Some(sender), Some(mimi_id)) =
                (message.message().sender(), message.message().mimi_id())
            else {
                // Not sent yet, or a system message: nothing to refer to.
                return Ok(None);
            };
            if message.chat_id() != chat {
                return Ok(None);
            }
            let quote = Quote {
                sender: display_name(&user, sender).await,
                text: quote_line(message.message().mimi_content()),
            };
            anyhow::Ok(Some((*mimi_id, quote)))
        });
        self.run(cx, async move |this, cx| {
            let Some((mimi_id, quote)) = load.await?? else {
                return Ok(());
            };
            this.update(cx, |this, cx| {
                if this.state.start_reply(mimi_id, quote) {
                    this.persist();
                    cx.notify();
                }
            })
        });
    }

    /// Drops the reply, else the edit. Returns whether there was either.
    pub fn cancel(&mut self, cx: &mut Context<Self>) -> bool {
        let cancelled = self.state.cancel();
        match cancelled {
            Cancelled::Nothing => return false,
            Cancelled::Reply => {}
            Cancelled::Edit => cx.emit(ComposerEvent::SetText {
                text: String::new(),
                replace: true,
            }),
        }
        self.persist();
        cx.notify();
        true
    }

    fn run(
        &mut self,
        cx: &mut Context<Self>,
        f: impl AsyncFnOnce(WeakEntity<Self>, &mut AsyncApp) -> anyhow::Result<()> + 'static,
    ) {
        let task = cx.spawn(async move |this, cx| {
            if let Err(err) = f(this, cx).await {
                error!("failed to load the message to edit or reply to: {err:#}");
            }
        });
        if let Some(live) = &mut self.live {
            live.lookup = Some(task);
        }
    }
}

pub(super) async fn load_draft(user: CoreUser, chat: CoreChatId) -> anyhow::Result<Option<Draft>> {
    let Some(draft) = user.message_draft(chat).await? else {
        return Ok(None);
    };
    let reply_to = match draft.in_reply_to {
        Some((mimi_id, original)) => {
            let quote = match original {
                Some(original) => Quote {
                    sender: display_name(&user, &original.sender).await,
                    text: quote_line(original.mimi_content.as_ref()),
                },
                None => Quote {
                    sender: SharedString::default(),
                    text: quote_line(None),
                },
            };
            Some((mimi_id, quote))
        }
        None => None,
    };
    Ok(Some(Draft {
        text: draft.message,
        editing: draft.editing_id,
        reply_to,
        updated_at: draft.updated_at,
        committed: draft.is_committed,
    }))
}

/// The message being edited was deleted meanwhile.
#[derive(Debug)]
struct EditGone;

impl std::fmt::Display for EditGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the edited message is gone")
    }
}

impl std::error::Error for EditGone {}

/// Port of upstream's `try_send_message`: the draft says whether this edits
/// or replies; the draft itself is cleared only once the message is
/// stored, so its context survives a failed send.
pub(super) async fn send(
    user: &CoreUser,
    chat: CoreChatId,
    text: String,
    draft: Option<&Draft>,
) -> Sent {
    let result = async {
        let replaces = match draft.and_then(|draft| draft.editing) {
            Some(id) => {
                let Some(original) = user.message(id).await? else {
                    return Err(EditGone.into());
                };
                let body = original.message().mimi_content().and_then(plain_body);
                if body == Some(text.as_str()) {
                    // Unchanged: just leave edit mode.
                    return anyhow::Ok(());
                }
                Some(original)
            }
            None => None,
        };
        let salt: [u8; 16] = RustCrypto::default().random_array()?;
        let mut content = MimiContent::simple_markdown_message(text, salt);
        content.in_reply_to = draft
            .and_then(|draft| draft.reply_to.as_ref())
            .map(|(id, _)| (*id).into());
        Box::pin(user.send_message(chat, content, replaces, MarkChatAsRead::Yes)).await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => {
            // Keyed by version: a newer draft stored meanwhile survives.
            if let Some(draft) = draft
                && let Err(err) = user
                    .delete_message_draft_version(chat, draft.updated_at)
                    .await
            {
                error!("failed to clear the draft after sending: {err:#}");
            }
            Sent::Ok
        }
        Err(err) => {
            if let Some(refused) = err.downcast_ref::<MessageEditError>() {
                Sent::EditRefused(match refused {
                    MessageEditError::WindowPassed => {
                        "This message can't be edited anymore. Your text is kept as a new message."
                    }
                    MessageEditError::LimitReached => {
                        "This message was edited too often. Your text is kept as a new message."
                    }
                })
            } else if err.is::<EditGone>() {
                Sent::EditRefused(
                    "The message you were editing is gone. Your text is kept as a new message.",
                )
            } else {
                Sent::Failed(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(text: &str) -> Quote {
        Quote {
            sender: "Ada".into(),
            text: text.to_string().into(),
        }
    }

    fn mimi(n: u8) -> MimiId {
        MimiId::from_slice(&[n; 32]).unwrap()
    }

    #[test]
    fn typing_then_leaving_commits_the_draft() {
        let mut state = DraftState::default();
        assert!(state.store_text("hel", false));
        assert!(state.store_text("hello", false));
        assert!(!state.store_text("hello", false));
        // Leaving with the same text still commits it.
        assert!(state.store_text("hello", true));
        assert!(state.draft.as_ref().unwrap().committed);
    }

    #[test]
    fn a_reply_keeps_new_text_but_drops_an_edit() {
        let mut state = DraftState::default();
        state.store_text("hi there", false);
        assert!(state.start_reply(mimi(1), quote("question")));
        assert!(!state.start_reply(mimi(1), quote("question")));
        let draft = state.draft.clone().unwrap();
        assert_eq!(draft.text, "hi there");

        let id = CoreMessageId::new(uuid::Uuid::from_u128(7));
        assert!(state.start_edit(id, "sent text".into()));
        assert!(state.draft.as_ref().unwrap().reply_to.is_none());
        assert!(state.start_reply(mimi(2), quote("other")));
        let draft = state.draft.as_ref().unwrap();
        assert_eq!(draft.editing, None, "a reply is a new message");
        assert_eq!(draft.text, "");
    }

    #[test]
    fn escape_drops_the_reply_then_the_edit() {
        let mut state = DraftState::default();
        let id = CoreMessageId::new(uuid::Uuid::from_u128(7));
        state.store_text("new", false);
        assert_eq!(state.cancel(), Cancelled::Nothing);
        state.start_reply(mimi(1), quote("q"));
        assert_eq!(state.cancel(), Cancelled::Reply);
        assert_eq!(state.draft.as_ref().unwrap().text, "new");
        state.start_edit(id, "sent text".into());
        assert_eq!(state.cancel(), Cancelled::Edit);
        assert_eq!(state.draft, None);
    }

    #[test]
    fn a_send_takes_the_draft_and_a_failure_gives_it_back() {
        let mut state = DraftState::default();
        state.start_reply(mimi(1), quote("q"));
        let sent = state.take_for_send();
        // In flight: leaving the chat stores nothing of the reply.
        assert!(!state.store_text("", true));
        assert!(state.send_failed(sent.clone(), "my reply".into(), false));
        let draft = state.draft.clone().unwrap();
        assert_eq!(draft.text, "my reply");
        assert!(draft.reply_to.is_some(), "the reply context comes back");

        // A refused edit comes back as a new message.
        let mut state = DraftState::default();
        let id = CoreMessageId::new(uuid::Uuid::from_u128(7));
        state.start_edit(id, "old".into());
        let sent = state.take_for_send();
        assert!(state.send_failed(sent, "new text".into(), true));
        let draft = state.draft.as_ref().unwrap();
        assert_eq!((draft.editing, draft.text.as_str()), (None, "new text"));

        // Not onto a new message typed meanwhile (not yet stored).
        let mut state = DraftState::default();
        state.start_reply(mimi(1), quote("q"));
        let sent = state.take_for_send();
        state.typed("something else");
        assert!(!state.send_failed(sent, "my reply".into(), false));
        assert_eq!(state.draft, None);

        // Not over a draft started meanwhile.
        let mut state = DraftState::default();
        state.start_reply(mimi(1), quote("q"));
        let sent = state.take_for_send();
        state.start_reply(mimi(2), quote("next"));
        assert!(!state.send_failed(sent, "my reply".into(), false));
        assert_eq!(state.draft.unwrap().reply_to.unwrap().0, mimi(2));
    }
}
