//! Root view: nav rail, chat list, conversation and the optional details pane.

use gpui_kit::component::{
    ActiveTheme as _, Theme, ThemeMode, h_flex, h_resizable, resizable_panel,
};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, IntoElement, Render, Subscription, Window, px};

use crate::model::mock;
use crate::ui::chat_list::{ChatList, ChatListEvent};
use crate::ui::conversation::Conversation;
use crate::ui::details_pane::details_pane;
use crate::ui::nav_rail::nav_rail;

pub struct Workspace {
    chat_list: Entity<ChatList>,
    conversation: Entity<Conversation>,
    details_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat_list = cx.new(|_| ChatList::new(mock::chats()));
        let conversation = cx.new(|_| Conversation::default());

        let subscriptions = vec![
            cx.subscribe(&chat_list, |this, chat_list, event, cx| match event {
                ChatListEvent::Opened(id) => {
                    let Some(chat) = chat_list.read(cx).chat(id).cloned() else {
                        return;
                    };
                    let messages = mock::messages(id);
                    this.conversation
                        .update(cx, |conversation, cx| conversation.show(chat, messages, cx));
                    cx.notify();
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
            }),
        ];
        Theme::sync_system_appearance(Some(window), cx);

        Self {
            chat_list,
            conversation,
            details_open: true,
            _subscriptions: subscriptions,
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let open_chat = self.conversation.read(cx).chat().cloned();

        h_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .child(nav_rail(
                |_, window, cx| {
                    let mode = if cx.theme().is_dark() {
                        ThemeMode::Light
                    } else {
                        ThemeMode::Dark
                    };
                    Theme::change(mode, Some(window), cx);
                    window.refresh();
                },
                cx,
            ))
            .child(
                h_resizable("workspace-panes")
                    .child(
                        resizable_panel()
                            .size(px(320.))
                            .size_range(px(240.)..px(480.))
                            .child(self.chat_list.clone()),
                    )
                    .child(resizable_panel().child(self.conversation.clone()))
                    .child(
                        resizable_panel()
                            .size(px(300.))
                            .size_range(px(240.)..px(420.))
                            .visible(self.details_open && open_chat.is_some())
                            .child(details_pane(open_chat.as_ref(), cx)),
                    ),
            )
    }
}
