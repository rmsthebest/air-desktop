//! The open chat's composer: its draft (kept per chat, also across
//! restarts), sending, editing a sent message and replying to one. Port of
//! the draft and send parts of upstream `applogic`'s
//! `chat_details_cubit.rs` and the composer's debouncing in
//! `message_composer.dart`.
//!
//! As upstream, the composer's text is the view's; the draft is what was
//! last stored from it, plus what the message edits or replies to.

use std::sync::Arc;
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
use gpui_kit::{AsyncApp, Context, Entity, Task, WeakEntity};
use mimi_content::MimiContent;
use tracing::error;

use crate::model::{ChatId, MessageId, Quote};
use crate::runtime::Tokio;
use crate::session::preview::{plain_body, quote_line};
use crate::session::{Session, display_name};

/// How long typing has to pause before the draft is stored. As upstream.
const STORE_DEBOUNCE: Duration = Duration::from_secs(1);
/// Leaving a chat commits its draft after this long, so the chat list
/// doesn't reorder under the user's next pick. As upstream.
const COMMIT_DELAY: Duration = Duration::from_millis(300);

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

/// What identifies a draft version: a draft that changed since a send
/// belongs to the next message and must survive the send's cleanup.
type Version = (DateTime<Utc>, Option<CoreMessageId>, Option<MimiId>);

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

    fn version(&self) -> Version {
        (
            self.updated_at,
            self.editing,
            self.reply_to.as_ref().map(|(id, _)| *id),
        )
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
}

impl DraftState {
    /// The composer's text, as typed (`committed: false`) or on leaving.
    fn store_text(&mut self, text: &str, committed: bool) -> bool {
        match &mut self.draft {
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

    /// An edit the core refused: the text becomes a new message.
    fn keep_as_new(&mut self, text: String) -> bool {
        let Some(draft) = &mut self.draft else {
            return false;
        };
        draft.editing = None;
        draft.text = text;
        draft.updated_at = Utc::now();
        true
    }

    /// The draft sent as `sent` was delivered to the core: clears it,
    /// unless it changed since.
    fn sent(&mut self, sent: Option<Version>) {
        if sent.is_some() && self.draft.as_ref().map(Draft::version) == sent {
            self.draft = None;
        }
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
    /// The edit window passed or the edit limit was reached.
    EditRefused(MessageEditError),
    Failed(anyhow::Error),
}

pub struct Composer {
    state: DraftState,
    live: Option<Live>,
}

struct Live {
    user: CoreUser,
    chat: CoreChatId,
    /// Stores and sends one at a time, in order, so an older draft can't
    /// overwrite a newer one.
    lock: Arc<tokio::sync::Mutex<()>>,
    tokio: tokio::runtime::Handle,
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
        let chat = super::messages::core_chat_id(chat);
        let load = Tokio::spawn(cx, load_draft(user.clone(), chat));
        let task = cx.spawn(async move |this, cx| {
            let draft = match load.await.and_then(|draft| draft) {
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
                lock: Arc::default(),
                tokio: Tokio::handle(cx),
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
        let Some(live) = &mut self.live else { return };
        live.store_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(STORE_DEBOUNCE).await;
            _ = this.update(cx, |this, _| {
                if this.state.store_text(&text, false) {
                    this.persist(Duration::ZERO);
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
            self.persist(COMMIT_DELAY);
        }
    }

    /// Stores the draft on tokio, detached: it completes even if the chat
    /// closes meanwhile.
    fn persist(&self, delay: Duration) {
        let Some(live) = &self.live else { return };
        let draft = self.state.draft.as_ref().map(Draft::to_core);
        let (user, chat, lock) = (live.user.clone(), live.chat, live.lock.clone());
        live.tokio.spawn(async move {
            tokio::time::sleep(delay).await;
            let _guard = lock.lock().await;
            if let Err(err) = user.store_message_draft(chat, draft.as_ref()).await {
                error!("failed to store the draft: {err:#}");
            }
        });
    }

    /// Sends `text` as a new message, or as the edit or reply the draft is.
    /// The view has cleared the composer; on failure the text comes back
    /// as [`ComposerEvent::SendFailed`].
    pub fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(live) = &mut self.live else { return };
        live.store_task = None;
        let draft = self.state.draft.clone();
        let (user, chat, lock) = (live.user.clone(), live.chat, live.lock.clone());
        let task = Tokio::spawn(cx, {
            let text = text.clone();
            async move {
                let _guard = lock.lock().await;
                send(&user, chat, text, draft.as_ref()).await
            }
        });
        let sent_version = self.state.draft.as_ref().map(Draft::version);
        // Not tied to the entity: a send completes even if the chat closes.
        cx.spawn(async move |this, cx| {
            let sent = task
                .await
                .unwrap_or_else(|err| Sent::Failed(err.context("the send was cancelled")));
            _ = this.update(cx, |this, cx| {
                match sent {
                    Sent::Ok => this.state.sent(sent_version),
                    Sent::EditRefused(refused) => {
                        if this.state.keep_as_new(text.clone()) {
                            this.persist(Duration::ZERO);
                        }
                        let error = match refused {
                            MessageEditError::WindowPassed => {
                                "This message can't be edited anymore. Your text is kept as a new message."
                            }
                            MessageEditError::LimitReached => {
                                "This message was edited too often. Your text is kept as a new message."
                            }
                        };
                        cx.emit(ComposerEvent::SendFailed {
                            text,
                            error: error.into(),
                        });
                    }
                    Sent::Failed(err) => {
                        error!("failed to send a message: {err:#}");
                        cx.emit(ComposerEvent::SendFailed {
                            text,
                            error: format!("Couldn't send the message: {err:#}").into(),
                        });
                    }
                }
                cx.notify();
            });
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
                    this.persist(Duration::ZERO);
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
                    this.persist(Duration::ZERO);
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
        self.persist(Duration::ZERO);
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
                let original = user
                    .message(id)
                    .await?
                    .context("the edited message is gone")?;
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
        Err(err) => match err.downcast_ref::<MessageEditError>() {
            Some(refused) => Sent::EditRefused(*refused),
            None => Sent::Failed(err),
        },
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
    fn a_send_clears_only_the_draft_it_sent() {
        let mut state = DraftState::default();
        state.start_reply(mimi(1), quote("q"));
        let sent = state.draft.as_ref().map(Draft::version);
        state.sent(sent);
        assert_eq!(state.draft, None);

        // A reply staged while the send was in flight belongs to the next
        // message.
        state.start_reply(mimi(1), quote("q"));
        let sent = state.draft.as_ref().map(Draft::version);
        state.start_reply(mimi(2), quote("next"));
        state.sent(sent);
        assert!(state.draft.is_some());
    }

    #[test]
    fn a_refused_edit_becomes_a_new_message() {
        let mut state = DraftState::default();
        let id = CoreMessageId::new(uuid::Uuid::from_u128(7));
        state.start_edit(id, "old".into());
        assert!(state.keep_as_new("new text".into()));
        let draft = state.draft.as_ref().unwrap();
        assert_eq!((draft.editing, draft.text.as_str()), (None, "new text"));
    }
}
