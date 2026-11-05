//! Main pane: header, message history and composer for the open chat.
//!
//! Two focus targets: the message list (Helix normal mode; `j`/`k` move a
//! message cursor) and the composer (insert mode). Enter in the composer
//! sends; shift-enter inserts a newline.
//!
//! The history is virtualized and pages in more as it nears either end. It
//! marks the chat as read as messages come into view, but only while the
//! window is active, so read receipts never claim more than was seen.

use std::ops::Range;

use gpui::{
    ClipboardItem, EventEmitter, FollowMode, KeyContext, ListAlignment, ListState, SharedString,
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
    ClearCursor, CursorDown, CursorFirst, CursorLast, CursorUp, HalfPageDown, HalfPageUp,
    OpenLinks, ToggleDetails, YankMessage,
};
use crate::markdown::Block;
use crate::model::{
    ChatSummary, Message, MessageBody, MessageStatus, format_day, format_message_time, move_cursor,
    starts_day, starts_group,
};
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
    /// Message under the normal-mode cursor.
    cursor: Option<usize>,
    /// Focus of the message list (normal mode).
    focus_handle: FocusHandle,
    composer: Entity<TextareaState>,
    _messages_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
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
        let this = cx.weak_entity();
        list.set_scroll_handler(move |event, window, cx| {
            let range = event.visible_range.clone();
            _ = this.update(cx, |this, cx| this.scrolled(range, window, cx));
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { shift: false, .. } = event {
                        this.submit(window, cx);
                    }
                },
            ),
            // Coming back to the window is seeing what it shows.
            cx.observe_window_activation(window, |this, window, cx| this.check_seen(window, cx)),
        ];
        Self {
            chat: None,
            messages: None,
            list,
            cursor: None,
            focus_handle: cx.focus_handle(),
            composer,
            _messages_subscription: None,
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.list.reset(messages.read(cx).messages().len());
        self._messages_subscription =
            Some(
                cx.subscribe_in(&messages, window, |this, _, event, window, cx| {
                    match event {
                        MessagesEvent::Spliced { old, count } => {
                            this.list.splice(old.clone(), *count);
                            this.clamp_cursor(cx);
                        }
                        MessagesEvent::ScrolledToBottom => this.list.scroll_to_end(),
                    }
                    cx.notify();
                    // After layout, so the list knows what it shows.
                    cx.on_next_frame(window, |this, window, cx| this.check_seen(window, cx));
                }),
            );
        self.chat = Some(chat);
        self.messages = Some(messages);
        self.cursor = None;
        self.list.scroll_to_end();
        cx.on_next_frame(window, |this, window, cx| this.check_seen(window, cx));
        cx.notify();
    }

    /// The open chat's summary changed; its messages stay as they are.
    pub fn update_chat(&mut self, chat: ChatSummary, cx: &mut Context<Self>) {
        self.chat = Some(chat);
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.chat = None;
        self.messages = None;
        self._messages_subscription = None;
        self.list.reset(0);
        self.cursor = None;
        cx.notify();
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
        cx.emit(ConversationEvent::Send(text));
    }

    /// Pages in more near either end, and marks what is in view as seen.
    fn scrolled(&mut self, visible: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(messages) = self.messages.clone() else {
            return;
        };
        let len = messages.read(cx).messages().len();
        if visible.start < PAGE_AHEAD {
            messages.read(cx).load_older();
        }
        if visible.end + PAGE_AHEAD >= len {
            messages.read(cx).load_newer();
        }
        if window.is_window_active()
            && let Some(newest) = visible.end.checked_sub(1)
        {
            messages.update(cx, |messages, cx| messages.seen(newest, cx));
        }
    }

    /// Marks the newest message seen when the list shows it.
    fn check_seen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(messages) = self.messages.clone() else {
            return;
        };
        let len = messages.read(cx).messages().len();
        if window.is_window_active()
            && self.list.is_scrolled_to_end() == Some(true)
            && let Some(newest) = len.checked_sub(1)
        {
            messages.update(cx, |messages, cx| messages.seen(newest, cx));
        }
    }

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.messages(cx).len();
        self.set_cursor(move_cursor(len, self.cursor, delta), cx);
    }

    fn set_cursor(&mut self, cursor: Option<usize>, cx: &mut Context<Self>) {
        self.cursor = cursor;
        if let Some(ix) = cursor {
            self.list.scroll_to_reveal_item(ix);
            if let Some(messages) = &self.messages {
                // The cursor pages too, as scrolling does.
                let messages = messages.read(cx);
                if ix < PAGE_AHEAD {
                    messages.load_older();
                }
                if ix + PAGE_AHEAD >= messages.messages().len() {
                    messages.load_newer();
                }
            }
        }
        cx.notify();
    }

    /// Keeps the cursor on a message when the window shrinks under it.
    fn clamp_cursor(&mut self, cx: &App) {
        let len = self.messages(cx).len();
        self.cursor = self.cursor.and_then(|ix| move_cursor(len, Some(ix), 0));
    }

    fn cursor_message<'a>(&self, cx: &'a App) -> Option<&'a Message> {
        self.messages(cx).get(self.cursor?)
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
            .w_full()
            .items_center()
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
            .child(body)
            .child(div().text_xs().opacity(0.7).text_right().child(footer));

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
            .into_any_element()
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
        let message_list_context: KeyContext = key_context("MessageList", cx);
        let composer_context: KeyContext = key_context("Composer", cx);
        let status = self.render_status(cx);
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
                    .flex_1()
                    .min_h_0()
                    .children(status)
                    .child(history),
            )
            .child(
                div()
                    .key_context(composer_context)
                    .flex_none()
                    .px_6()
                    .pb_4()
                    .child(
                        div()
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
                            .child(Textarea::new(&self.composer).appearance(false)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::model::{MessageId, UserId};

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
                "https://a.example",
                "https://b.example",
                "mailto:c@example.com"
            ]
        );
    }
}
