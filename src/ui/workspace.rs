//! Root view: nav rail, chat list, conversation and the optional details pane.
//! Owns focus movement between panes and handles workspace-wide actions.

use gpui_kit::component::{ActiveTheme as _, h_flex, h_resizable, resizable_panel};
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, Entity, FocusHandle, Focusable as _, IntoElement, Render, Subscription, Window, px,
};

use crate::actions::{
    FocusChatList, FocusConversation, NextChat, NextUnreadChat, OpenChatAt, OpenPalette, PrevChat,
    PrevUnreadChat, ShowShortcuts, ToggleAppearance, ToggleDetails,
};
use crate::model::{ChatId, ChatSummary, mock};
use crate::settings::{Appearance, SettingsStore};
use crate::theme;
use crate::ui::chat_list::{ChatList, ChatListEvent};
use crate::ui::conversation::Conversation;
use crate::ui::details_pane::details_pane;
use crate::ui::nav_rail::nav_rail;
use crate::ui::palette;

const CHAT_LIST_MIN: f32 = 240.;
const CHAT_LIST_MAX: f32 = 480.;

pub struct Workspace {
    chat_list: Entity<ChatList>,
    conversation: Entity<Conversation>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let chat_list = cx.new(|cx| ChatList::new(mock::chats(), cx));
        let conversation = cx.new(Conversation::new);

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
                if SettingsStore::get(cx).appearance == Appearance::System {
                    theme::apply_appearance(window, cx);
                }
            }),
        ];
        theme::apply_appearance(window, cx);
        chat_list.focus_handle(cx).focus(window, cx);

        Self {
            chat_list,
            conversation,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    pub fn chats(&self, cx: &gpui_kit::App) -> Vec<ChatSummary> {
        self.chat_list.read(cx).chats().to_vec()
    }

    /// Opens a chat and moves focus to the conversation, as when picking a
    /// chat from the palette.
    pub fn jump_to_chat(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_list.update(cx, |list, cx| list.open(id, cx));
        self.focus_conversation(&FocusConversation, window, cx);
    }

    fn update_chat_list(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ChatList, &mut Context<ChatList>),
    ) {
        self.chat_list.update(cx, f);
    }

    fn focus_chat_list(&mut self, _: &FocusChatList, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_list.focus_handle(cx).focus(window, cx);
    }

    fn focus_conversation(
        &mut self,
        _: &FocusConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Without an open chat there is nothing to focus; stay put. The chat
        // list is the source of truth: the conversation learns about a newly
        // opened chat only once the `Opened` event is delivered.
        if self.chat_list.read(cx).selected_index().is_some() {
            self.conversation.focus_handle(cx).focus(window, cx);
        }
    }

    fn toggle_details(&mut self, _: &ToggleDetails, _: &mut Window, cx: &mut Context<Self>) {
        SettingsStore::update(cx, |settings| {
            settings.details_open = !settings.details_open
        });
        cx.notify();
    }

    fn toggle_appearance(
        &mut self,
        _: &ToggleAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        SettingsStore::update(cx, |settings| {
            settings.appearance = settings.appearance.next()
        });
        theme::apply_appearance(window, cx);
        cx.notify();
    }

    fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        palette::open_palette(self.chats(cx), cx.weak_entity(), window, cx);
    }

    fn show_shortcuts(&mut self, _: &ShowShortcuts, window: &mut Window, cx: &mut Context<Self>) {
        palette::show_shortcuts(window, cx);
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = SettingsStore::get(cx).clone();
        let open_chat = self.conversation.read(cx).chat().cloned();
        let theme = cx.theme();
        let chat_list_width = settings.chat_list_width.clamp(CHAT_LIST_MIN, CHAT_LIST_MAX);

        h_flex()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &NextChat, _, cx| {
                this.update_chat_list(cx, |list, cx| list.step(true, cx))
            }))
            .on_action(cx.listener(|this, _: &PrevChat, _, cx| {
                this.update_chat_list(cx, |list, cx| list.step(false, cx))
            }))
            .on_action(cx.listener(|this, _: &NextUnreadChat, _, cx| {
                this.update_chat_list(cx, |list, cx| list.step_unread(true, cx))
            }))
            .on_action(cx.listener(|this, _: &PrevUnreadChat, _, cx| {
                this.update_chat_list(cx, |list, cx| list.step_unread(false, cx))
            }))
            .on_action(cx.listener(|this, action: &OpenChatAt, _, cx| {
                let index = action.index;
                this.update_chat_list(cx, |list, cx| list.open_at(index, cx))
            }))
            .on_action(cx.listener(Self::focus_chat_list))
            .on_action(cx.listener(Self::focus_conversation))
            .on_action(cx.listener(Self::toggle_details))
            .on_action(cx.listener(Self::toggle_appearance))
            .on_action(cx.listener(Self::open_palette))
            .on_action(cx.listener(Self::show_shortcuts))
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .child(nav_rail(settings.appearance, cx))
            .child(
                h_resizable("workspace-panes")
                    .on_resize(|state, _, cx| {
                        let Some(width) = state.read(cx).sizes().first().copied() else {
                            return;
                        };
                        let width = f32::from(width);
                        SettingsStore::update(cx, |settings| settings.chat_list_width = width);
                    })
                    .child(
                        resizable_panel()
                            .size(px(chat_list_width))
                            .size_range(px(CHAT_LIST_MIN)..px(CHAT_LIST_MAX))
                            .child(self.chat_list.clone()),
                    )
                    .child(resizable_panel().child(self.conversation.clone()))
                    .child(
                        resizable_panel()
                            .size(px(300.))
                            .size_range(px(240.)..px(420.))
                            .visible(settings.details_open && open_chat.is_some())
                            .child(details_pane(open_chat.as_ref(), cx)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::keymap;
    use crate::settings::Settings;

    /// The platform's primary modifier, as `secondary-` resolves to.
    const MOD: &str = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    fn setup(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        let (window, workspace) = cx.update(|cx| {
            gpui_kit::init(cx);
            SettingsStore::init(None, Settings::default(), cx);
            theme::init(None, cx).unwrap();
            assert_eq!(keymap::load(None, cx), Vec::<String>::new());
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| Workspace::new(window, cx))
            })
            .unwrap()
        });
        let cx = VisualTestContext::from_window(window, cx).into_mut();
        cx.run_until_parked();
        (workspace, cx)
    }

    fn open_title(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<String> {
        workspace.read_with(cx, |workspace, cx| {
            Some(workspace.conversation.read(cx).chat()?.title.to_string())
        })
    }

    fn titles(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .chats(cx)
                .into_iter()
                .map(|c| c.title.to_string())
                .collect()
        })
    }

    #[gpui::test]
    fn alt_arrows_cycle_through_chats_and_wrap(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let titles = titles(&workspace, cx);
        assert_eq!(open_title(&workspace, cx), None);

        cx.simulate_keystrokes("alt-down");
        assert_eq!(open_title(&workspace, cx).as_ref(), Some(&titles[0]));
        cx.simulate_keystrokes("alt-down alt-down");
        assert_eq!(open_title(&workspace, cx).as_ref(), Some(&titles[2]));
        cx.simulate_keystrokes("alt-up alt-up alt-up");
        assert_eq!(open_title(&workspace, cx).as_ref(), titles.last());
    }

    #[gpui::test]
    fn number_shortcuts_and_unread_navigation(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let chats = workspace.read_with(cx, |workspace, cx| workspace.chats(cx));

        cx.simulate_keystrokes(&format!("{MOD}-3"));
        assert_eq!(
            open_title(&workspace, cx).as_deref(),
            Some(&*chats[2].title)
        );
        // Out of range is a no-op, not a panic.
        cx.simulate_keystrokes(&format!("{MOD}-9"));
        assert_eq!(
            open_title(&workspace, cx).as_deref(),
            Some(&*chats[2].title)
        );

        let first_unread = chats.iter().find(|chat| chat.unread > 0).unwrap();
        cx.simulate_keystrokes("alt-shift-down");
        assert_eq!(
            open_title(&workspace, cx).as_deref(),
            Some(&*first_unread.title)
        );
    }

    #[gpui::test]
    fn focus_moves_between_list_and_conversation(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let (list_focus, conversation_focus) = workspace.read_with(cx, |workspace, cx| {
            (
                workspace.chat_list.focus_handle(cx),
                workspace.conversation.focus_handle(cx),
            )
        });
        let focused = |cx: &mut VisualTestContext| {
            cx.update(|window, _| {
                (
                    list_focus.is_focused(window),
                    conversation_focus.is_focused(window),
                )
            })
        };
        assert_eq!(focused(cx), (true, false));

        // Nothing open yet: focusing the conversation is refused.
        cx.simulate_keystrokes("enter");
        assert_eq!(focused(cx), (true, false));

        // j/k work only while the list has focus.
        cx.simulate_keystrokes("j enter");
        assert_eq!(focused(cx), (false, true));
        let titles = titles(&workspace, cx);
        assert_eq!(open_title(&workspace, cx).as_ref(), Some(&titles[0]));
        cx.simulate_keystrokes("j");
        assert_eq!(open_title(&workspace, cx).as_ref(), Some(&titles[0]));

        cx.simulate_keystrokes("escape k");
        assert_eq!(focused(cx), (true, false));
        assert_eq!(open_title(&workspace, cx).as_ref(), titles.last());
    }

    #[gpui::test]
    fn toggles_update_persisted_settings(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        let settings =
            |cx: &mut VisualTestContext| cx.update(|_, cx| SettingsStore::get(cx).clone());
        assert!(settings(cx).details_open);

        cx.simulate_keystrokes(&format!("{MOD}-i"));
        assert!(!settings(cx).details_open);

        cx.simulate_keystrokes(&format!("{MOD}-shift-t"));
        assert_eq!(settings(cx).appearance, Appearance::Light);
        cx.simulate_keystrokes(&format!("{MOD}-shift-t {MOD}-shift-t"));
        assert_eq!(settings(cx).appearance, Appearance::System);
    }

    #[gpui::test]
    fn user_keymap_overrides_and_unbinds_defaults(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keymap.json");
        std::fs::write(
            &path,
            r#"[{ "context": "Workspace", "bindings": {
                "alt-down": null,
                "alt-n": "air::NextChat"
            }}]"#,
        )
        .unwrap();
        let (workspace, cx) = setup(cx);
        cx.update(|_, cx| {
            cx.clear_key_bindings();
            assert_eq!(keymap::load(Some(&path), cx), Vec::<String>::new());
        });

        cx.simulate_keystrokes("alt-down");
        assert_eq!(open_title(&workspace, cx), None);
        cx.simulate_keystrokes("alt-n");
        assert!(open_title(&workspace, cx).is_some());
    }

    #[gpui::test]
    fn palette_jumps_to_chat_and_focuses_conversation(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.simulate_keystrokes(&format!("{MOD}-k"));
        cx.run_until_parked();
        cx.simulate_input("Grace");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(open_title(&workspace, cx).as_deref(), Some("Grace Hopper"));
        let conversation_focus =
            workspace.read_with(cx, |workspace, cx| workspace.conversation.focus_handle(cx));
        assert!(cx.update(|window, _| conversation_focus.is_focused(window)));
    }

    #[gpui::test]
    fn palette_runs_commands_against_the_workspace(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        cx.simulate_keystrokes(&format!("{MOD}-k"));
        cx.run_until_parked();
        cx.simulate_input("Toggle details");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!cx.update(|_, cx| SettingsStore::get(cx).details_open));
    }
}
