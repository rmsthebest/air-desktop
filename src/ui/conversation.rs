//! Main pane: header, message history and composer for the open chat.
//!
//! Two focus targets: the message list (Helix normal mode; `j`/`k` move a
//! message cursor) and the composer (insert mode). Enter in the composer
//! sends; shift-enter inserts a newline.
//!
//! The history is virtualized and pages in more as it nears either end. It
//! marks the chat as read as messages come into view, but only while the
//! window is active, so read receipts never claim more than was seen. What
//! is in view is measured from the drawn items themselves (see `Probes`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    Bounds, ClipboardItem, EventEmitter, FollowMode, KeyContext, ListAlignment, ListOffset,
    ListState, Pixels, SharedString, canvas,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    Subscription, Window, div, list, px,
};

use crate::actions::{
    AcceptRequest, CancelCompose, ClearCursor, CursorDown, CursorFirst, CursorLast, CursorUp,
    DeclineRequest, EditLastMessage, EditMessage, HalfPageDown, HalfPageUp, OpenLinks,
    PickReaction, React, ReplyToMessage, ToggleDetails, YankMessage,
};
use crate::markdown::Block;
use crate::model::{
    ChatSummary, Message, MessageBody, MessageStatus, QUICK_REACTIONS, Quote, Reaction, Request,
    format_day, format_message_time, move_cursor, starts_day, starts_group,
};
use crate::session::composer::{Composer, ComposerEvent};
use crate::session::messages::{Messages, MessagesEvent};
use crate::ui::key_context;
use crate::ui::markdown_view;
use crate::ui::widgets::avatar;

/// Messages never stretch wider than this, so lines stay readable on wide
/// monitors.
const MAX_CONTENT_WIDTH: f32 = 760.;
const HALF_PAGE: isize = 10;
/// Within this many messages of a loaded end, the next page is requested.
const PAGE_AHEAD: usize = 10;

pub enum ConversationEvent {
    /// The user submitted a non-empty message in the composer.
    Send(String),
}

pub struct Conversation {
    chat: Option<ChatSummary>,
    messages: Option<Entity<Messages>>,
    list: ListState,
    probes: Rc<RefCell<Probes>>,
    /// The messages in view, as of the last drawn frame.
    shown: Option<Shown>,
    /// Message under the normal-mode cursor.
    cursor: Option<usize>,
    /// The reaction picker is open, for the message under the cursor.
    reacting: bool,
    /// Focus of the message list (normal mode).
    focus_handle: FocusHandle,
    composer: Entity<TextareaState>,
    /// The open chat's draft, sending, edits and replies.
    compose: Option<Entity<Composer>>,
    _messages_subscription: Option<Subscription>,
    _compose_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

/// The part of the history in view.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    /// Messages at least partly in view (what paging looks at).
    range: Range<usize>,
    /// How far the first one starts above the top of the list.
    first_offset: Pixels,
    /// The newest message seen in full (or filling the view): what may be
    /// marked as read.
    seen: Option<usize>,
}

/// Where each message was drawn this frame, collected while the list draws
/// and turned into a [`Shown`] right after. The list itself only reports
/// what it shows on wheel scrolls.
#[derive(Default)]
struct Probes {
    drawn: Vec<(usize, Bounds<Pixels>)>,
    shown: Option<Shown>,
}

impl Probes {
    fn shown(drawn: &[(usize, Bounds<Pixels>)], viewport: Bounds<Pixels>) -> Option<Shown> {
        // The list may lay out more than once per frame; its last pass is
        // what is drawn.
        let drawn: BTreeMap<usize, Bounds<Pixels>> = drawn.iter().copied().collect();
        let mut visible = drawn.iter().filter(|(_, bounds)| {
            bounds.bottom() > viewport.top() && bounds.top() < viewport.bottom()
        });
        let (first, first_bounds) = visible.next()?;
        let last = visible.clone().next_back().map_or(*first, |(ix, _)| *ix);
        let seen = std::iter::once((first, first_bounds))
            .chain(visible)
            .filter(|(_, bounds)| {
                let in_full = bounds.bottom() <= viewport.bottom() + px(1.);
                let fills_view =
                    bounds.top() <= viewport.top() + px(1.) && bounds.bottom() >= viewport.bottom();
                in_full || fills_view
            })
            .map(|(ix, _)| *ix)
            .max();
        Some(Shown {
            range: *first..last + 1,
            first_offset: viewport.top() - first_bounds.top(),
            seen,
        })
    }
}

impl EventEmitter<ConversationEvent> for Conversation {}

impl Focusable for Conversation {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Conversation {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder("Write a message…")
        });
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        // Stays at the newest message as new ones arrive, unless the user
        // scrolled up.
        list.set_follow_mode(FollowMode::Tail);
        let subscriptions = vec![
            cx.subscribe_in(
                &composer,
                window,
                |this, composer, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { shift: false, .. } => this.submit(window, cx),
                    InputEvent::Change => {
                        let text = composer.read(cx).value().to_string();
                        if let Some(compose) = &this.compose {
                            compose.update(cx, |compose, cx| compose.typed(text, cx));
                        }
                    }
                    _ => {}
                },
            ),
            // Coming back to the window is seeing what it shows.
            cx.observe_window_activation(window, |this, window, cx| {
                let shown = this.shown.clone();
                this.shown_changed(shown, window, cx)
            }),
        ];
        Self {
            chat: None,
            messages: None,
            list,
            probes: Rc::default(),
            shown: None,
            cursor: None,
            reacting: false,
            focus_handle: cx.focus_handle(),
            composer,
            compose: None,
            _messages_subscription: None,
            _compose_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn chat(&self) -> Option<&ChatSummary> {
        self.chat.as_ref()
    }

    pub fn show(
        &mut self,
        chat: ChatSummary,
        messages: Entity<Messages>,
        compose: Entity<Composer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_chat(window, cx);
        self._compose_subscription =
            Some(
                cx.subscribe_in(&compose, window, |this, _, event, window, cx| match event {
                    ComposerEvent::SetText { text, replace } => {
                        if *replace || this.composer.read(cx).value().is_empty() {
                            this.set_composer_text(text, window, cx);
                        }
                    }
                    ComposerEvent::SendFailed { text, error } => {
                        // Unless the user started a new message meanwhile.
                        if this.composer.read(cx).value().trim().is_empty() {
                            this.set_composer_text(text, window, cx);
                        }
                        crate::ui::show_error(error.to_string(), window, cx);
                    }
                }),
            );
        self.compose = Some(compose);
        self.list.reset(messages.read(cx).messages().len());
        self._messages_subscription =
            Some(cx.subscribe_in(&messages, window, |this, _, event, _, cx| {
                match event {
                    MessagesEvent::Opened => this.scroll_to_start(cx),
                    MessagesEvent::Spliced { old, count, paged } => {
                        this.splice(old.clone(), *count, *paged, cx)
                    }
                    MessagesEvent::ScrolledToBottom => this.list.scroll_to_end(),
                }
                cx.notify();
            }));
        let loaded = messages.read(cx).is_loaded();
        self.chat = Some(chat);
        self.messages = Some(messages);
        self.cursor = None;
        self.reacting = false;
        self.shown = None;
        self.probes.borrow_mut().shown = None;
        if loaded {
            self.scroll_to_start(cx);
        }
        cx.notify();
    }

    /// Where a chat opens: at the unread divider, else at the newest.
    fn scroll_to_start(&mut self, cx: &App) {
        let first_unread = self
            .messages
            .as_ref()
            .and_then(|messages| messages.read(cx).first_unread());
        match first_unread {
            Some(ix) => self.list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(0.),
            }),
            None => self.list.scroll_to_end(),
        }
    }

    fn splice(&mut self, old: Range<usize>, count: usize, paged: bool, cx: &App) {
        let append = old.is_empty() && old.start == self.list.item_count();
        if append && paged && self.list.is_following_tail() {
            // Paged in below the bottom of the view: stay on what is shown
            // rather than following the tail to the end of the new page.
            if let Some(shown) = &self.shown {
                self.list.scroll_to(ListOffset {
                    item_ix: shown.range.start,
                    offset_in_item: shown.first_offset,
                });
            }
        }
        self.list.splice(old.clone(), count);
        self.cursor = self.cursor.and_then(|ix| shift(ix, &old, count));
        // Its message gone (or deleted), the picker goes too.
        self.reacting &= self.reactable(cx).is_some();
    }

    /// The open chat's summary changed; its messages stay as they are.
    pub fn update_chat(&mut self, chat: ChatSummary, cx: &mut Context<Self>) {
        self.chat = Some(chat);
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_chat(window, cx);
        self.compose = None;
        self._compose_subscription = None;
        self.chat = None;
        self.messages = None;
        self.shown = None;
        self._messages_subscription = None;
        self.list.reset(0);
        self.cursor = None;
        self.reacting = false;
        cx.notify();
    }

    /// Hands what is in the composer to the chat being left, as its draft,
    /// and empties it for the next chat.
    fn leave_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(compose) = self.compose.take() else {
            return;
        };
        let text = self.composer.read(cx).value().to_string();
        compose.update(cx, |compose, _| compose.leave(&text));
        self.set_composer_text("", window, cx);
    }

    pub fn set_composer_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |composer, cx| {
            composer.set_value(text.to_string(), window, cx)
        });
    }

    /// Appends a message to the open chat, e.g. the local echo of a send.
    pub fn push_message(&mut self, message: Message, cx: &mut Context<Self>) {
        if let Some(messages) = &self.messages {
            messages.update(cx, |messages, cx| messages.push(message, cx));
        }
        self.list.scroll_to_end();
    }

    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }

    fn messages<'a>(&self, cx: &'a App) -> &'a [Message] {
        match &self.messages {
            Some(messages) => messages.read(cx).messages(),
            None => &[],
        }
    }

    #[cfg(test)]
    pub fn messages_entity(&self) -> Option<&Entity<Messages>> {
        self.messages.as_ref()
    }

    #[cfg(test)]
    pub fn is_reacting(&self) -> bool {
        self.reacting
    }

    #[cfg(test)]
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    #[cfg(test)]
    pub fn message_body(&self, ix: usize, cx: &App) -> String {
        self.messages(cx)[ix].plain_text().to_string()
    }

    #[cfg(test)]
    pub fn message_count(&self, cx: &App) -> usize {
        self.messages(cx).len()
    }

    #[cfg(test)]
    pub fn composer_text(&self, cx: &App) -> String {
        self.composer.read(cx).value().to_string()
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Keep leading whitespace (indented code); only drop the trailing
        // newlines/spaces left by typing.
        let text = self.composer.read(cx).value().trim_end().to_string();
        if text.trim().is_empty() || self.chat.is_none() {
            return;
        }
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        match self.compose.clone().filter(|c| c.read(cx).is_live()) {
            Some(compose) => {
                compose.update(cx, |compose, cx| compose.send(text, cx));
                // Your own message is the one to see.
                if let Some(messages) = self.messages.clone() {
                    messages.update(cx, |messages, cx| messages.jump_to_bottom(cx));
                }
            }
            None => cx.emit(ConversationEvent::Send(text)),
        }
    }

    /// Up in an empty composer edits the last message; otherwise the key
    /// moves the cursor as usual.
    fn edit_last(&mut self, _: &EditLastMessage, _: &mut Window, cx: &mut Context<Self>) {
        let empty = self.composer.read(cx).value().trim().is_empty();
        match &self.compose {
            Some(compose) if empty && !compose.read(cx).is_editing() => {
                compose.update(cx, |compose, cx| compose.edit(None, cx))
            }
            _ => cx.propagate(),
        }
    }

    /// Escape drops a reply or edit; otherwise it does what it would.
    fn cancel_compose(&mut self, _: &CancelCompose, _: &mut Window, cx: &mut Context<Self>) {
        let cancelled = self
            .compose
            .clone()
            .is_some_and(|compose| compose.update(cx, |compose, cx| compose.cancel(cx)));
        if !cancelled {
            cx.propagate();
        }
    }

    fn edit_message(&mut self, _: &EditMessage, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .cursor_message(cx)
            .filter(|m| m.own)
            .map(|m| m.id.clone())
        else {
            return;
        };
        // An edit starts from the message's text: don't drop what's typed
        // (switching from one edit to another is fine).
        let editing = self
            .compose
            .as_ref()
            .is_some_and(|c| c.read(cx).is_editing());
        if !editing && !self.composer.read(cx).value().trim().is_empty() {
            crate::ui::show_error(
                "Send or clear the message you're writing before editing another.".into(),
                window,
                cx,
            );
            return;
        }
        if let Some(compose) = self.compose.clone() {
            compose.update(cx, |compose, cx| compose.edit(Some(&id), cx));
            self.composer_focus(cx).focus(window, cx);
        }
    }

    fn reply(&mut self, _: &ReplyToMessage, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .cursor_message(cx)
            .filter(|m| !m.is_event())
            .map(|m| m.id.clone())
        else {
            return;
        };
        if let Some(compose) = self.compose.clone() {
            compose.update(cx, |compose, cx| compose.reply(&id, cx));
            self.composer_focus(cx).focus(window, cx);
        }
    }

    /// What is in view changed (or the window was activated): pages in
    /// more near either end, and marks the newest message in view as seen.
    fn shown_changed(&mut self, shown: Option<Shown>, window: &mut Window, cx: &mut Context<Self>) {
        self.shown = shown.clone();
        let (Some(shown), Some(messages)) = (shown, self.messages.clone()) else {
            return;
        };
        let active = window.is_window_active();
        messages.update(cx, |messages, cx| {
            if shown.range.start < PAGE_AHEAD {
                messages.load_older();
            }
            if shown.range.end + PAGE_AHEAD >= messages.messages().len() {
                messages.load_newer();
            }
            if active && let Some(seen) = shown.seen {
                messages.seen(seen, cx);
            }
        });
    }

    /// Notes where message `ix` is drawn; see [`Probes`].
    fn probe(&self, ix: usize) -> impl IntoElement {
        let probes = self.probes.clone();
        canvas(
            move |bounds, _, _| probes.borrow_mut().drawn.push((ix, bounds)),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    /// Runs after the list has drawn: works out what is in view and
    /// reports a change once the frame is drawn.
    fn probe_results(&self, cx: &Context<Self>) -> impl IntoElement {
        let probes = self.probes.clone();
        let list = self.list.clone();
        let this = cx.weak_entity();
        canvas(
            move |_, window, cx| {
                let mut probes = probes.borrow_mut();
                let drawn = std::mem::take(&mut probes.drawn);
                let shown = Probes::shown(&drawn, list.viewport_bounds());
                if shown != probes.shown {
                    probes.shown = shown.clone();
                    window.defer(cx, move |window, cx| {
                        _ = this.update(cx, |this, cx| this.shown_changed(shown, window, cx));
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_0()
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.messages(cx).len();
        self.set_cursor(move_cursor(len, self.cursor, delta), cx);
    }

    fn set_cursor(&mut self, cursor: Option<usize>, cx: &mut Context<Self>) {
        self.cursor = cursor;
        // Follows the cursor, while there is something to react to.
        self.reacting &= self.reactable(cx).is_some();
        if let Some(ix) = cursor {
            // Revealing it pages in more, as scrolling does.
            self.list.scroll_to_reveal_item(ix);
        }
        cx.notify();
    }

    fn cursor_message<'a>(&self, cx: &'a App) -> Option<&'a Message> {
        self.messages(cx).get(self.cursor?)
    }

    /// The message under the cursor, if it can take reactions.
    fn reactable<'a>(&self, cx: &'a App) -> Option<&'a Message> {
        self.cursor_message(cx)
            .filter(|message| matches!(message.body, MessageBody::Text { .. }))
    }

    fn pick_reaction(&mut self, _: &PickReaction, _: &mut Window, cx: &mut Context<Self>) {
        self.reacting = !self.reacting && self.reactable(cx).is_some();
        cx.notify();
    }

    fn react(&mut self, action: &React, window: &mut Window, cx: &mut Context<Self>) {
        self.reacting = false;
        cx.notify();
        let (Some(emoji), Some(messages)) = (QUICK_REACTIONS.get(action.index), &self.messages)
        else {
            return;
        };
        let Some(message) = self.reactable(cx) else {
            return;
        };
        let id = message.id.clone();
        let given = crate::model::gave(&message.reactions, emoji);
        let Some(toggle) = messages.read(cx).set_reaction(&id, emoji, !given, cx) else {
            crate::ui::show_refusal("Reactions need an account".into(), window, cx);
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            if let Err(refusal) = toggle.await {
                _ = cx.update(|window, cx| crate::ui::show_refusal(refusal, window, cx));
            }
        })
        .detach();
    }

    /// The quick reactions on offer, the ones the user gave highlighted.
    fn render_picker(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.reacting {
            return None;
        }
        let message = self.reactable(cx)?;
        let theme = cx.theme();
        let chips = QUICK_REACTIONS.iter().enumerate().map(|(ix, emoji)| {
            let own = message
                .reactions
                .iter()
                .any(|reaction| reaction.own && reaction.label.as_ref() == *emoji);
            h_flex()
                .gap_1()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(if own { theme.primary } else { theme.border })
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child((ix + 1).to_string()),
                )
                .child(*emoji)
        });
        Some(
            h_flex()
                .flex_none()
                .px_6()
                .py_2()
                .gap_2()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("React"),
                )
                .children(chips)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("esc to cancel"),
                )
                .into_any_element(),
        )
    }

    fn yank(&mut self, _: &YankMessage, _: &mut Window, cx: &mut Context<Self>) {
        // Only on an explicit command, and only the body under the cursor.
        if let Some(message) = self.cursor_message(cx) {
            cx.write_to_clipboard(ClipboardItem::new_string(message.plain_text().to_string()));
        }
    }

    fn open_links(&mut self, _: &OpenLinks, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.cursor_message(cx).map(links).unwrap_or_default();
        markdown_view::confirm_open(targets, window, cx);
    }

    fn render_empty(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .text_color(theme.muted_foreground)
            .child(div().text_lg().child("No chat selected"))
            .child(div().text_sm().child("Pick a conversation from the list"))
            .into_any_element()
    }

    fn render_item(&self, ix: usize, list_focused: bool, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(entity) = &self.messages else {
            return div().into_any_element();
        };
        let entity = entity.read(cx);
        let messages = entity.messages();
        let Some(message) = messages.get(ix) else {
            return div().into_any_element();
        };
        let divider = |label: String, accent: bool| {
            h_flex()
                .w_full()
                .max_w(px(MAX_CONTENT_WIDTH))
                .px_6()
                .pt_4()
                .gap_3()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(if accent {
                    theme.primary
                } else {
                    theme.muted_foreground
                })
                .child(div().flex_1().h(px(1.)).bg(theme.border))
                .child(label)
                .child(div().flex_1().h(px(1.)).bg(theme.border))
        };
        v_flex()
            .relative()
            .w_full()
            .items_center()
            .child(self.probe(ix))
            .when(starts_day(messages, ix), |item| {
                item.child(divider(
                    format_day(message.sent_at, chrono::Utc::now()),
                    false,
                ))
            })
            .when(entity.first_unread() == Some(ix), |item| {
                item.child(divider("New messages".into(), true))
            })
            .child(if message.is_event() {
                self.render_event(message, ix, list_focused, cx)
            } else {
                self.render_message(messages, ix, list_focused, cx)
            })
            .when(ix + 1 == messages.len(), |item| item.pb_4())
            .into_any_element()
    }

    fn render_event(
        &self,
        message: &Message,
        ix: usize,
        list_focused: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let under_cursor = self.cursor == Some(ix);
        div()
            .mt_3()
            .px_3()
            .py_1()
            .max_w(px(MAX_CONTENT_WIDTH * 0.8))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(if under_cursor {
                cursor_color(list_focused, cx)
            } else {
                gpui_kit::transparent_black()
            })
            .text_xs()
            .text_center()
            .text_color(theme.muted_foreground)
            .child(message.plain_text())
            .into_any_element()
    }

    fn render_message(
        &self,
        messages: &[Message],
        ix: usize,
        list_focused: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let message = &messages[ix];
        // Consecutive messages from one sender form a group: only the first
        // shows the sender name, and the gap between them is tighter.
        let starts_group = starts_group(messages, ix);
        let under_cursor = self.cursor == Some(ix);

        let body = match &message.body {
            MessageBody::Text { markdown, .. } => {
                markdown_view::render(markdown, message.id.0.clone(), cx)
            }
            MessageBody::Notice(text) | MessageBody::Event(text) => div()
                .italic()
                .opacity(0.8)
                .child(text.clone())
                .into_any_element(),
        };
        let mut footer = format_message_time(message.sent_at);
        if message.edited {
            footer.push_str(" · edited");
        }
        if message.own {
            footer.push_str(match message.status {
                MessageStatus::Sending => " · sending",
                MessageStatus::Sent => " · sent",
                MessageStatus::Delivered => " · delivered",
                MessageStatus::Read => " · read",
                MessageStatus::Failed => " · not sent",
            });
        }

        let bubble = v_flex()
            .max_w(gpui_kit::relative(0.75))
            .px_3()
            .py_2()
            .gap_1()
            .rounded(theme.radius_lg)
            .border_1()
            .when(message.own, |bubble| {
                bubble
                    .bg(theme.primary)
                    .text_color(theme.primary_foreground)
                    .border_color(theme.primary)
            })
            .when(!message.own, |bubble| {
                bubble
                    .bg(theme.secondary)
                    .text_color(theme.secondary_foreground)
                    .border_color(theme.border)
            })
            .when(message.status == MessageStatus::Failed, |bubble| {
                bubble.border_color(theme.danger)
            })
            .when(under_cursor, |bubble| {
                bubble
                    .border_2()
                    .border_color(cursor_color(list_focused, cx))
            })
            .shadow_xs()
            .children(
                message
                    .reply_to
                    .as_ref()
                    .map(|quote| render_quote(quote, cx)),
            )
            .child(body)
            .child(div().text_xs().opacity(0.7).text_right().child(footer));
        let reactions = (!message.reactions.is_empty())
            .then(|| render_reactions(&message.reactions, message.more_reactions, cx));

        v_flex()
            .w_full()
            .max_w(px(MAX_CONTENT_WIDTH))
            .px_6()
            .when(starts_group, |row| row.mt_3())
            .when(!starts_group, |row| row.mt_1())
            .when(message.own, |row| row.items_end())
            .when(!message.own, |row| row.items_start())
            .when(starts_group && !message.own, |row| {
                row.child(
                    div()
                        .px_1()
                        .pb_1()
                        .max_w_full()
                        .truncate()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(message.sender_name.clone()),
                )
            })
            .child(bubble)
            .children(reactions)
            .into_any_element()
    }

    /// What the composer's message will be, when it isn't a plain new one.
    fn render_compose_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let compose = self.compose.as_ref()?.read(cx);
        let theme = cx.theme();
        let (label, quote) = if compose.is_editing() {
            ("Editing message", None)
        } else {
            ("Replying", Some(compose.reply_to()?))
        };
        Some(
            h_flex()
                .px_3()
                .pt_2()
                .gap_2()
                .items_start()
                .text_xs()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().italic().text_color(theme.primary).child(label))
                        .children(quote.map(|quote| render_quote(quote, cx))),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .child("esc to cancel"),
                )
                .into_any_element(),
        )
    }

    fn render_status(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let messages = self.messages.as_ref()?.read(cx);
        let text = if let Some(error) = messages.error() {
            error.clone()
        } else if !messages.is_loaded() {
            "Loading messages…".into()
        } else if messages.messages().is_empty() {
            "No messages yet".into()
        } else {
            return None;
        };
        Some(
            div()
                .p_4()
                .w_full()
                .text_center()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text)
                .into_any_element(),
        )
    }
}

/// Where the message at `ix` is after the messages in `old` were replaced
/// by `count` others; `None` if it was one of them.
fn shift(ix: usize, old: &Range<usize>, count: usize) -> Option<usize> {
    if old.end <= ix {
        Some(ix - old.len() + count)
    } else if ix < old.start {
        Some(ix)
    } else if old.len() == count {
        // Edited in place.
        Some(ix)
    } else {
        None
    }
}

/// Every link in a message, in order of appearance, without repeats.
fn links(message: &Message) -> Vec<SharedString> {
    fn collect(blocks: &[Block], out: &mut Vec<SharedString>) {
        for block in blocks {
            match block {
                Block::Paragraph(inline) | Block::Heading(inline) => {
                    for (_, target) in &inline.links {
                        if !out.contains(target) {
                            out.push(target.clone());
                        }
                    }
                }
                Block::Quote(blocks) => collect(blocks, out),
                Block::List { items, .. } => items.iter().for_each(|item| collect(item, out)),
                Block::Code(_) | Block::Rule => {}
            }
        }
    }
    let mut out = Vec::new();
    if let MessageBody::Text { markdown, .. } = &message.body {
        collect(&markdown.blocks, &mut out);
    }
    out
}

/// The replied-to message, above a reply or the composer.
fn render_quote(quote: &Quote, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .pl_2()
        .border_l_2()
        .border_color(theme.border)
        .opacity(0.85)
        .text_xs()
        .min_w_0()
        .when(!quote.sender.is_empty(), |quote_box| {
            quote_box.child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(quote.sender.clone()),
            )
        })
        .child(div().truncate().child(quote.text.clone()))
        .into_any_element()
}

fn cursor_color(list_focused: bool, cx: &App) -> gpui::Hsla {
    let theme = cx.theme();
    if list_focused {
        theme.ring
    } else {
        theme.muted_foreground
    }
}

impl Render for Conversation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list_focused = self.focus_handle.is_focused(window);
        let composer_focused = self.composer_focus(cx).contains_focused(window, cx);
        let details_open = crate::settings::SettingsStore::get(cx).details_open;
        let mut message_list_context: KeyContext = key_context("MessageList", cx);
        if self.reacting {
            message_list_context.add("reacting");
        }
        let picker = self.render_picker(cx);
        let composer_context: KeyContext = key_context("Composer", cx);
        let status = self.render_status(cx);
        let compose_bar = self.render_compose_bar(cx);
        let theme = cx.theme();
        let root = v_flex().key_context("Conversation").size_full();
        let Some(chat) = &self.chat else {
            return root
                .track_focus(&self.focus_handle)
                .bg(theme.background)
                .child(self.render_empty(cx));
        };

        let history = list(
            self.list.clone(),
            cx.processor(move |this, ix, _, cx| this.render_item(ix, list_focused, cx)),
        )
        .size_full();

        root.bg(theme.background)
            .child(
                h_flex()
                    .flex_none()
                    .h(px(52.))
                    .px_4()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(avatar(&chat.title, px(30.), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(chat.title.clone()),
                    )
                    .child(
                        Button::new("toggle-details")
                            .icon(IconName::PanelRight)
                            .ghost()
                            .selected(details_open)
                            .tooltip_with_action("Details", &ToggleDetails, Some("Workspace"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleDetails), cx)
                            }),
                    ),
            )
            .child(
                v_flex()
                    .id("message-list")
                    .key_context(message_list_context)
                    .track_focus(&self.focus_handle)
                    .on_action(cx.listener(|this, _: &CursorDown, _, cx| this.move_cursor(1, cx)))
                    .on_action(cx.listener(|this, _: &CursorUp, _, cx| this.move_cursor(-1, cx)))
                    .on_action(
                        cx.listener(|this, _: &HalfPageDown, _, cx| {
                            this.move_cursor(HALF_PAGE, cx)
                        }),
                    )
                    .on_action(
                        cx.listener(|this, _: &HalfPageUp, _, cx| this.move_cursor(-HALF_PAGE, cx)),
                    )
                    .on_action(cx.listener(|this, _: &CursorFirst, _, cx| {
                        let first = (!this.messages(cx).is_empty()).then_some(0);
                        this.set_cursor(first, cx)
                    }))
                    .on_action(cx.listener(|this, _: &CursorLast, _, cx| {
                        if let Some(messages) = this.messages.clone() {
                            messages.update(cx, |messages, cx| messages.jump_to_bottom(cx));
                        }
                        let last = this.messages(cx).len().checked_sub(1);
                        this.set_cursor(last, cx)
                    }))
                    .on_action(
                        cx.listener(|this, _: &ClearCursor, _, cx| this.set_cursor(None, cx)),
                    )
                    .on_action(cx.listener(Self::yank))
                    .on_action(cx.listener(Self::open_links))
                    .on_action(cx.listener(Self::edit_message))
                    .on_action(cx.listener(Self::reply))
                    .on_action(cx.listener(Self::pick_reaction))
                    .on_action(cx.listener(Self::react))
                    .flex_1()
                    .min_h_0()
                    .children(status)
                    .child(history)
                    .children(picker)
                    .child(self.probe_results(cx)),
            )
            .child(match chat.request {
                None => self
                    .render_composer(composer_context, composer_focused, compose_bar, cx)
                    .into_any_element(),
                Some(request) => render_request(request, &chat.title, cx),
            })
    }
}

impl Conversation {
    fn render_composer(
        &self,
        composer_context: KeyContext,
        composer_focused: bool,
        compose_bar: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .key_context(composer_context)
            .on_action(cx.listener(Self::edit_last))
            .on_action(cx.listener(Self::cancel_compose))
            .flex_none()
            .px_6()
            .pb_4()
            .child(
                v_flex()
                    .mx_auto()
                    .w_full()
                    .max_w(px(MAX_CONTENT_WIDTH))
                    .rounded(theme.radius_lg)
                    .bg(theme.popover)
                    .border_1()
                    .border_color(if composer_focused {
                        theme.ring
                    } else {
                        theme.border
                    })
                    .shadow_sm()
                    .children(compose_bar)
                    .child(Textarea::new(&self.composer).appearance(false)),
            )
    }
}

/// Takes the composer's place in a chat whose contact request is open:
/// nothing can be sent before both sides are connected.
fn render_request(request: Request, title: &SharedString, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let card = h_flex()
        .mx_auto()
        .w_full()
        .max_w(px(MAX_CONTENT_WIDTH))
        .p_3()
        .gap_3()
        .rounded(theme.radius_lg)
        .bg(theme.popover)
        .border_1()
        .border_color(theme.border)
        .shadow_sm();
    let text = |text: String| div().flex_1().min_w_0().text_sm().child(text);
    let card = match request {
        Request::Incoming => card
            .child(text(format!("{title} wants to connect with you.")))
            .child(
                Button::new("decline-request")
                    .label("Decline")
                    .ghost()
                    .tooltip_with_action(
                        "Decline: nothing is sent to them",
                        &DeclineRequest,
                        Some("Workspace"),
                    )
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(DeclineRequest), cx)),
            )
            .child(
                Button::new("accept-request")
                    .label("Accept")
                    .primary()
                    .tooltip_with_action("Accept", &AcceptRequest, Some("Workspace"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(AcceptRequest), cx)),
            ),
        Request::Outgoing => card.child(
            text(format!(
                "Waiting for {title} to accept your contact request."
            ))
            .text_color(theme.muted_foreground),
        ),
        Request::Closed => card.child(
            text("This contact request ended without a connection.".into())
                .text_color(theme.muted_foreground),
        ),
    };
    div()
        .flex_none()
        .px_6()
        .pb_4()
        .child(card)
        .into_any_element()
}

/// Reactions under a message: each kind once, with its count; the user's
/// own outlined.
fn render_reactions(reactions: &[Reaction], more: usize, cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .flex_wrap()
        .max_w(gpui_kit::relative(0.75))
        .pt_1()
        .gap_1()
        .children(reactions.iter().map(|reaction| {
            h_flex()
                .gap_1()
                .px_2()
                .rounded_full()
                .border_1()
                .border_color(if reaction.own {
                    theme.primary
                } else {
                    theme.border
                })
                .bg(theme.secondary)
                .text_xs()
                .child(reaction.label.clone())
                .child(reaction.count.to_string())
        }))
        .when(more > 0, |row| {
            row.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("+{more}")),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::model::{MessageId, UserId};

    #[gpui::test]
    fn a_chat_opens_at_its_unread_divider(cx: &mut gpui::TestAppContext) {
        let (window, conversation) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::settings::SettingsStore::open(None, cx);
            crate::theme::init(None, cx).unwrap();
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| Conversation::new(window, cx))
            })
            .unwrap()
        });
        let cx = gpui::VisualTestContext::from_window(window, cx).into_mut();
        cx.update(|window, _| window.activate_window());
        let start = Utc::now() - chrono::Duration::hours(1);
        let history: Vec<_> = (0..200)
            .map(|n| {
                let sender = (UserId("u".into()), "Ada".into(), false);
                let id = MessageId(format!("m{n}").into());
                let at = start + chrono::Duration::seconds(n);
                Message::text(id, sender, &format!("message {n}"), at)
            })
            .collect();
        let messages =
            cx.new(|_| Messages::fixed(history).with_first_unread(MessageId("m120".into())));
        let chat = crate::model::mock::chats().remove(0);
        conversation.update_in(cx, |conversation, window, cx| {
            conversation.show(
                chat,
                messages.clone(),
                cx.new(|_| Composer::fixed()),
                window,
                cx,
            )
        });
        cx.run_until_parked();

        let shown = conversation.read_with(cx, |c, _| c.shown.clone()).unwrap();
        assert_eq!(shown.range.start, 120, "the divider tops the view");
        // Only what is in view counts as read, not the rest below it.
        let read = messages.read_with(cx, |m, _| m.read_until().cloned());
        let expected = format!("m{}", shown.seen.unwrap());
        assert_eq!(read.map(|id| id.0.to_string()), Some(expected));
        assert!(shown.range.end < 200);
    }

    #[test]
    fn only_messages_in_view_in_full_count_as_seen() {
        let rect = |top: f32, height: f32| Bounds {
            origin: gpui::point(px(0.), px(top)),
            size: gpui::size(px(100.), px(height)),
        };
        let viewport = rect(0., 100.);
        // Message 2 only peeks in at the bottom; 3 is below the view.
        let drawn = [
            (0, rect(-30., 40.)),
            (1, rect(10., 50.)),
            (2, rect(60., 80.)),
            (3, rect(140., 20.)),
        ];
        let shown = Probes::shown(&drawn, viewport).unwrap();
        assert_eq!(shown.range, 0..3);
        assert_eq!(shown.seen, Some(1));
        assert_eq!(shown.first_offset, px(30.));
        // A retried layout pass: only its (later) bounds count.
        let retried = [(3, rect(50., 20.)), (3, rect(140., 20.))];
        assert_eq!(Probes::shown(&retried, viewport), None);
        // A message taller than the view is seen while it fills it.
        let tall = Probes::shown(&[(0, rect(-50., 300.))], viewport).unwrap();
        assert_eq!(tall.seen, Some(0));
    }

    #[test]
    fn the_cursor_stays_on_its_message_across_splices() {
        // Older messages paged in above it.
        assert_eq!(shift(5, &(0..0), 64), Some(69));
        // The oldest dropped from the window.
        assert_eq!(shift(69, &(0..64), 0), Some(5));
        // Newer ones appended, or a message below it removed.
        assert_eq!(shift(5, &(10..10), 64), Some(5));
        assert_eq!(shift(5, &(7..8), 0), Some(5));
        // Its own message: patched stays, removed goes.
        assert_eq!(shift(5, &(5..6), 1), Some(5));
        assert_eq!(shift(5, &(5..6), 0), None);
        assert_eq!(shift(5, &(0..10), 3), None);
    }

    #[test]
    fn open_links_offers_every_link_once_including_nested_ones() {
        let source = "[a](https://a.example) and [again](https://a.example)\n\n\
                      > quoted [b](https://b.example)\n\n\
                      - item [c](mailto:c@example.com)\n\n\
                      `[not](https://code.example)`";
        let sender = (UserId("u".into()), "Ada".into(), false);
        let message = Message::text(MessageId("m".into()), sender, source, Utc::now());
        assert_eq!(
            links(&message),
            [
                "https://a.example/",
                "https://b.example/",
                "mailto:c@example.com"
            ]
        );
    }
}
