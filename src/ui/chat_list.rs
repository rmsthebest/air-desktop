//! Middle-left pane: the list of chats. Virtualized: only visible rows are
//! built, so large accounts cost the same as small ones.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    ScrollStrategy, SharedString, UniformListScrollHandle, Window, div, px, uniform_list,
};

use crate::actions::{SelectNext, SelectPrev};
use crate::model::{ChatId, ChatSummary, PreviewKind, format_activity, next_unread, step_index};
use crate::session::chats::LoadState;
use crate::ui::key_context;
use crate::ui::widgets::{avatar, badge};

pub enum ChatListEvent {
    Opened(ChatId),
    /// The open chat's summary changed (title, preview…).
    Updated(ChatSummary),
    /// The open chat is gone (deleted, or left on another device).
    Closed,
}

pub struct ChatList {
    chats: Vec<ChatSummary>,
    state: LoadState,
    /// The open chat. Selecting a chat opens it; there is no separate
    /// highlight, so keyboard and mouse always agree on what is shown.
    selected: Option<ChatId>,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
}

impl EventEmitter<ChatListEvent> for ChatList {}

impl Focusable for ChatList {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ChatList {
    pub fn new(chats: Vec<ChatSummary>, state: LoadState, cx: &mut Context<Self>) -> Self {
        Self {
            chats,
            state,
            selected: None,
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
        }
    }

    /// Replaces the chats, keeping the open chat open if it still exists.
    pub fn set_chats(&mut self, chats: Vec<ChatSummary>, state: LoadState, cx: &mut Context<Self>) {
        if let Some(selected) = &self.selected {
            let old = self.chats.iter().find(|chat| &chat.id == selected);
            match chats.iter().find(|chat| &chat.id == selected) {
                Some(new) if old != Some(new) => cx.emit(ChatListEvent::Updated(new.clone())),
                Some(_) => {}
                None => {
                    self.selected = None;
                    cx.emit(ChatListEvent::Closed);
                }
            }
        }
        self.chats = chats;
        self.state = state;
        cx.notify();
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
        self.scroll_handle
            .scroll_to_item(ix, ScrollStrategy::Nearest);
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

    fn render_row(&self, ix: usize, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let chat = &self.chats[ix];
        let selected = self.selected.as_ref() == Some(&chat.id);
        let id = chat.id.clone();
        let activity: SharedString = chat
            .last_activity
            .map(|at| format_activity(at, chrono::Utc::now()).into())
            .unwrap_or_default();

        let preview = chat.preview.as_ref().map(|preview| {
            let italic = preview.kind != PreviewKind::Message;
            h_flex()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(theme.muted_foreground)
                .when_some(preview.prefix.clone(), |line, prefix| {
                    line.child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::MEDIUM)
                            .when(preview.kind == PreviewKind::Draft, |prefix| prefix.italic())
                            .child(format!("{prefix}:\u{a0}")),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(italic, |text| text.italic())
                        .child(preview.text.clone()),
                )
        });

        let row = h_flex()
            .id(("chat-row", ix))
            .gap_3()
            .px_3()
            .py_2()
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
                            .when(chat.pending, |line| {
                                line.child(
                                    div()
                                        .flex_none()
                                        .px_1p5()
                                        .rounded(theme.radius)
                                        .border_1()
                                        .border_color(theme.border)
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Request"),
                                )
                            })
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(activity),
                            ),
                    )
                    .child(
                        // Fixed height, so every row is as tall as the one
                        // the list measures, with or without a preview.
                        h_flex()
                            .h(px(20.))
                            .gap_2()
                            .children(preview)
                            .when(chat.unread > 0, |line| {
                                line.child(div().flex_1()).child(badge(chat.unread, cx))
                            }),
                    ),
            );
        div().px_2().pb_0p5().child(row).into_any_element()
    }

    fn render_placeholder(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let message: SharedString = match &self.state {
            LoadState::Loading => "Loading chats…".into(),
            LoadState::Failed(err) => format!("Couldn't load your chats: {err}").into(),
            LoadState::Loaded if self.chats.is_empty() => "No chats yet".into(),
            LoadState::Loaded => return None,
        };
        Some(
            div()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(message)
                .into_any_element(),
        )
    }
}

impl Render for ChatList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let theme = cx.theme();
        let list = uniform_list(
            "chat-list",
            self.chats.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|ix| this.render_row(ix, focused, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .pb_2()
        .track_scroll(&self.scroll_handle);

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
            .children(self.render_placeholder(cx))
            .child(list)
    }
}
