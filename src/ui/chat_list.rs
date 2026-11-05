//! Middle-left pane: the list of chats.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, Render, ScrollHandle,
    SharedString, Window, div, px,
};

use crate::actions::{SelectNext, SelectPrev};
use crate::model::{ChatId, ChatSummary, format_activity, next_unread, step_index};
use crate::ui::key_context;
use crate::ui::widgets::{avatar, badge};

pub enum ChatListEvent {
    Opened(ChatId),
}

pub struct ChatList {
    chats: Vec<ChatSummary>,
    /// The open chat. Selecting a chat opens it; there is no separate
    /// highlight, so keyboard and mouse always agree on what is shown.
    selected: Option<ChatId>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
}

impl EventEmitter<ChatListEvent> for ChatList {}

impl Focusable for ChatList {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ChatList {
    pub fn new(chats: Vec<ChatSummary>, cx: &mut Context<Self>) -> Self {
        Self {
            chats,
            selected: None,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
        }
    }

    pub fn chats(&self) -> &[ChatSummary] {
        &self.chats
    }

    pub fn chat(&self, id: &ChatId) -> Option<&ChatSummary> {
        self.chats.iter().find(|chat| &chat.id == id)
    }

    pub fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.chats.iter().position(|chat| &chat.id == selected)
    }

    /// Opens `id`. Emits [`ChatListEvent::Opened`] only when the open chat
    /// actually changes.
    pub fn open(&mut self, id: ChatId, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&id) {
            return;
        }
        // The id may come from a stale snapshot (e.g. the palette's).
        let Some(ix) = self.chats.iter().position(|chat| chat.id == id) else {
            return;
        };
        self.scroll_handle.scroll_to_item(ix);
        self.selected = Some(id.clone());
        cx.emit(ChatListEvent::Opened(id));
        cx.notify();
    }

    pub fn open_at(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(chat) = self.chats.get(ix) {
            self.open(chat.id.clone(), cx);
        }
    }

    pub fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(ix) = step_index(self.chats.len(), self.selected_index(), forward) {
            self.open_at(ix, cx);
        }
    }

    pub fn step_unread(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(ix) = next_unread(&self.chats, self.selected_index(), forward) {
            self.open_at(ix, cx);
        }
    }

    fn render_row(
        &self,
        ix: usize,
        chat: &ChatSummary,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let selected = self.selected.as_ref() == Some(&chat.id);
        let id = chat.id.clone();
        let preview: SharedString = chat.last_message.clone().unwrap_or_default();

        h_flex()
            .id(("chat-row", ix))
            .gap_3()
            .px_3()
            .py_2()
            .mx_2()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .when(selected, |row| {
                row.bg(theme.list_active)
                    .border_color(if focused {
                        theme.ring
                    } else {
                        theme.list_active_border
                    })
                    .shadow_xs()
            })
            .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus_handle.focus(window, cx);
                this.open(id.clone(), cx);
            }))
            .child(avatar(&chat.title, px(40.), cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(chat.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format_activity(chat.last_activity, chrono::Utc::now())),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(preview),
                            )
                            .when(chat.unread > 0, |row| row.child(badge(chat.unread, cx))),
                    ),
            )
    }
}

impl Render for ChatList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let rows: Vec<_> = self
            .chats
            .iter()
            .enumerate()
            .map(|(ix, chat)| self.render_row(ix, chat, focused, cx).into_any_element())
            .collect();
        let theme = cx.theme();

        v_flex()
            .key_context(key_context("ChatList", cx))
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.step(false, cx)))
            .size_full()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .flex_none()
                    .h(px(52.))
                    .px_4()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Chats"),
            )
            .child(
                v_flex()
                    .id("chat-list-scroll")
                    .flex_1()
                    .gap_0p5()
                    .pb_2()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .children(rows),
            )
    }
}
