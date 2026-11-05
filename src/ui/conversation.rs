//! Main pane: header, message history and composer for the open chat.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, FontWeight, IntoElement, Render, Window, div, px};

use crate::model::{ChatSummary, Message};
use crate::ui::widgets::avatar;

/// Messages never stretch wider than this, so lines stay readable on wide
/// monitors.
const MAX_CONTENT_WIDTH: f32 = 760.;

#[derive(Default)]
pub struct Conversation {
    chat: Option<ChatSummary>,
    messages: Vec<Message>,
}

impl Conversation {
    pub fn chat(&self) -> Option<&ChatSummary> {
        self.chat.as_ref()
    }

    pub fn show(&mut self, chat: ChatSummary, messages: Vec<Message>, cx: &mut Context<Self>) {
        self.chat = Some(chat);
        self.messages = messages;
        cx.notify();
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

    fn render_message(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let message = &self.messages[ix];
        // Consecutive messages from one sender form a group: only the first
        // shows the sender name, and the gap between them is tighter.
        let starts_group = ix == 0 || self.messages[ix - 1].sender != message.sender;

        let bubble = v_flex()
            .max_w(gpui_kit::relative(0.75))
            .px_3()
            .py_2()
            .rounded(theme.radius_lg)
            .when(message.own, |bubble| {
                bubble
                    .bg(theme.primary)
                    .text_color(theme.primary_foreground)
            })
            .when(!message.own, |bubble| {
                bubble
                    .bg(theme.secondary)
                    .text_color(theme.secondary_foreground)
                    .border_1()
                    .border_color(theme.border)
            })
            .shadow_xs()
            .child(div().child(message.body.clone()))
            .child(
                div()
                    .text_xs()
                    .opacity(0.7)
                    .text_right()
                    .child(message.time.clone()),
            );

        v_flex()
            .w_full()
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
                        .child(message.sender.clone()),
                )
            })
            .child(bubble)
            .into_any_element()
    }
}

impl Render for Conversation {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let Some(chat) = &self.chat else {
            return v_flex()
                .size_full()
                .bg(theme.background)
                .child(self.render_empty(cx));
        };

        let messages: Vec<_> = (0..self.messages.len())
            .map(|ix| self.render_message(ix, cx))
            .collect();

        v_flex()
            .size_full()
            .bg(theme.background)
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
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(chat.title.clone()),
                    ),
            )
            .child(
                div()
                    .id("message-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .mx_auto()
                            .w_full()
                            .max_w(px(MAX_CONTENT_WIDTH))
                            .px_6()
                            .pb_4()
                            .children(messages),
                    ),
            )
            .child(
                div().flex_none().px_6().pb_4().child(
                    div()
                        .mx_auto()
                        .w_full()
                        .max_w(px(MAX_CONTENT_WIDTH))
                        .px_4()
                        .py_3()
                        .rounded(theme.radius_lg)
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!("Message {}", chat.title)),
                ),
            )
    }
}
