//! Middle-left pane: the list of chats.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, EventEmitter, FontWeight, IntoElement, Render, SharedString, Window, div, px,
};

use crate::model::{ChatId, ChatSummary};
use crate::ui::widgets::{avatar, badge};

pub enum ChatListEvent {
    Opened(ChatId),
}

pub struct ChatList {
    chats: Vec<ChatSummary>,
    selected: Option<ChatId>,
}

impl EventEmitter<ChatListEvent> for ChatList {}

impl ChatList {
    pub fn new(chats: Vec<ChatSummary>) -> Self {
        Self {
            chats,
            selected: None,
        }
    }

    pub fn chat(&self, id: &ChatId) -> Option<&ChatSummary> {
        self.chats.iter().find(|chat| &chat.id == id)
    }

    pub fn open(&mut self, id: ChatId, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&id) {
            return;
        }
        self.selected = Some(id.clone());
        cx.emit(ChatListEvent::Opened(id));
        cx.notify();
    }

    fn render_row(
        &self,
        ix: usize,
        chat: &ChatSummary,
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
                    .border_color(theme.list_active_border)
                    .shadow_xs()
            })
            .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.open(id.clone(), cx)))
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
                                    .child(chat.last_activity.clone()),
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = self
            .chats
            .iter()
            .enumerate()
            .map(|(ix, chat)| self.render_row(ix, chat, cx).into_any_element())
            .collect();
        let theme = cx.theme();

        v_flex()
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
                    .children(rows),
            )
    }
}
