//! Main pane: header, message history and composer for the open chat.
//!
//! Two focus targets: the message list (Helix normal mode; `j`/`k` move a
//! message cursor) and the composer (insert mode). Enter in the composer
//! sends; shift-enter inserts a newline.

use gpui::{ClipboardItem, EventEmitter, KeyContext};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    ScrollHandle, Subscription, Window, div, px,
};

use crate::actions::{
    ClearCursor, CursorDown, CursorFirst, CursorLast, CursorUp, HalfPageDown, HalfPageUp,
    ToggleDetails, YankMessage,
};
use crate::model::{ChatSummary, Message, format_message_time, move_cursor, starts_group};
use crate::ui::key_context;
use crate::ui::widgets::avatar;

/// Messages never stretch wider than this, so lines stay readable on wide
/// monitors.
const MAX_CONTENT_WIDTH: f32 = 760.;
const HALF_PAGE: isize = 10;

pub enum ConversationEvent {
    /// The user submitted a non-empty message in the composer.
    Send(String),
}

pub struct Conversation {
    chat: Option<ChatSummary>,
    messages: Vec<Message>,
    /// Message under the normal-mode cursor.
    cursor: Option<usize>,
    /// Focus of the message list (normal mode).
    focus_handle: FocusHandle,
    composer: Entity<TextareaState>,
    scroll_handle: ScrollHandle,
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
        let subscriptions = vec![cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    this.submit(window, cx);
                }
            },
        )];
        Self {
            chat: None,
            messages: Vec::new(),
            cursor: None,
            focus_handle: cx.focus_handle(),
            composer,
            scroll_handle: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    pub fn chat(&self) -> Option<&ChatSummary> {
        self.chat.as_ref()
    }

    pub fn show(&mut self, chat: ChatSummary, messages: Vec<Message>, cx: &mut Context<Self>) {
        self.chat = Some(chat);
        self.messages = messages;
        self.cursor = None;
        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    /// Appends a message to the open chat, e.g. the local echo of a send.
    pub fn push_message(&mut self, message: Message, cx: &mut Context<Self>) {
        self.messages.push(message);
        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    pub fn composer_focus(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }

    #[cfg(test)]
    pub fn cursor(&self) -> Option<usize> {
        self.cursor
    }

    #[cfg(test)]
    pub fn message_body(&self, ix: usize) -> String {
        self.messages[ix].body.to_string()
    }

    #[cfg(test)]
    pub fn message_count(&self) -> usize {
        self.messages.len()
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

    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.set_cursor(move_cursor(self.messages.len(), self.cursor, delta), cx);
    }

    fn set_cursor(&mut self, cursor: Option<usize>, cx: &mut Context<Self>) {
        self.cursor = cursor;
        if let Some(ix) = cursor {
            self.scroll_handle.scroll_to_item(ix);
        }
        cx.notify();
    }

    fn yank(&mut self, _: &YankMessage, _: &mut Window, cx: &mut Context<Self>) {
        // Only on an explicit command, and only the body under the cursor.
        if let Some(message) = self.cursor.and_then(|ix| self.messages.get(ix)) {
            cx.write_to_clipboard(ClipboardItem::new_string(message.body.to_string()));
        }
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

    fn render_message(&self, ix: usize, list_focused: bool, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let message = &self.messages[ix];
        // Consecutive messages from one sender form a group: only the first
        // shows the sender name, and the gap between them is tighter.
        let starts_group = starts_group(&self.messages, ix);
        let under_cursor = self.cursor == Some(ix);

        let bubble = v_flex()
            .max_w(gpui_kit::relative(0.75))
            .px_3()
            .py_2()
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
            .when(under_cursor, |bubble| {
                bubble.border_2().border_color(if list_focused {
                    theme.ring
                } else {
                    theme.muted_foreground
                })
            })
            .shadow_xs()
            .child(div().child(message.body.clone()))
            .child(
                div()
                    .text_xs()
                    .opacity(0.7)
                    .text_right()
                    .child(format_message_time(message.sent_at)),
            );

        v_flex()
            .id(message.id.0.clone())
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
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(message.sender_name.clone()),
                )
            })
            .child(bubble)
            .into_any_element()
    }
}

impl Render for Conversation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list_focused = self.focus_handle.is_focused(window);
        let composer_focused = self.composer_focus(cx).contains_focused(window, cx);
        let details_open = crate::settings::SettingsStore::get(cx).details_open;
        let message_list_context: KeyContext = key_context("MessageList", cx);
        let composer_context: KeyContext = key_context("Composer", cx);
        let theme = cx.theme();
        let root = v_flex().key_context("Conversation").size_full();
        let Some(chat) = &self.chat else {
            return root
                .track_focus(&self.focus_handle)
                .bg(theme.background)
                .child(self.render_empty(cx));
        };

        let messages: Vec<_> = (0..self.messages.len())
            .map(|ix| self.render_message(ix, list_focused, cx))
            .collect();

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
                        let first = (!this.messages.is_empty()).then_some(0);
                        this.set_cursor(first, cx)
                    }))
                    .on_action(cx.listener(|this, _: &CursorLast, _, cx| {
                        let last = this.messages.len().checked_sub(1);
                        this.set_cursor(last, cx)
                    }))
                    .on_action(
                        cx.listener(|this, _: &ClearCursor, _, cx| this.set_cursor(None, cx)),
                    )
                    .on_action(cx.listener(Self::yank))
                    .flex_1()
                    .items_center()
                    .pb_4()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .children(messages),
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
