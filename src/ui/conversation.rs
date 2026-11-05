//! Main pane: header, message history and composer for the open chat.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, IconName, Selectable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    ScrollHandle, Window, div, px,
};

use crate::actions::ToggleDetails;
use crate::model::{ChatSummary, Message, format_message_time, starts_group};
use crate::ui::widgets::avatar;

/// Messages never stretch wider than this, so lines stay readable on wide
/// monitors.
const MAX_CONTENT_WIDTH: f32 = 760.;

pub struct Conversation {
    chat: Option<ChatSummary>,
    messages: Vec<Message>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
}

impl Focusable for Conversation {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Conversation {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            chat: None,
            messages: Vec::new(),
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
        }
    }

    pub fn chat(&self) -> Option<&ChatSummary> {
        self.chat.as_ref()
    }

    pub fn show(&mut self, chat: ChatSummary, messages: Vec<Message>, cx: &mut Context<Self>) {
        self.chat = Some(chat);
        self.messages = messages;
        self.scroll_handle.scroll_to_bottom();
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
        let starts_group = starts_group(&self.messages, ix);

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
                    .child(format_message_time(message.sent_at)),
            );

        v_flex()
            .id(message.id.0.clone())
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
                        .child(message.sender_name.clone()),
                )
            })
            .child(bubble)
            .into_any_element()
    }
}

impl Render for Conversation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let details_open = crate::settings::SettingsStore::get(cx).details_open;
        let theme = cx.theme();
        let root = v_flex()
            .key_context("Conversation")
            .track_focus(&self.focus_handle)
            .size_full();
        let Some(chat) = &self.chat else {
            return root.bg(theme.background).child(self.render_empty(cx));
        };

        let messages: Vec<_> = (0..self.messages.len())
            .map(|ix| self.render_message(ix, cx))
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
                div()
                    .id("message-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
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
                        .border_color(if focused { theme.ring } else { theme.border })
                        .shadow_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!("Message {}", chat.title)),
                ),
            )
    }
}
