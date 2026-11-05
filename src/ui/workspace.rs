//! Root view: nav rail, chat list, conversation and the optional details pane.
//! Owns focus movement between panes, workspace-wide actions, and in Helix
//! mode the status line, which-key popup and `:` command line.

use gpui::{AsKeystroke as _, Keystroke, OperatingSystem, Subscription};
use gpui_kit::component::{ActiveTheme as _, h_flex, h_resizable, resizable_panel, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, FontWeight, IntoElement, Render,
    Window, div, px,
};

use crate::actions::{
    self, FindChat, FocusChatList, FocusComposer, FocusConversation, FocusMessages, FocusNextPane,
    JumpBack, JumpForward, NextChat, NextUnreadChat, OpenChatAt, OpenCommandLine, OpenPalette,
    PickTheme, PrevChat, PrevUnreadChat, SelectFirst, SelectLast, ShowShortcuts, ToggleAppearance,
    ToggleDetails, ToggleHelixMode,
};
use crate::command_line::{self, Command, NameMatch, matches_name};
use crate::model::{ChatId, ChatSummary, JumpList, Message, MessageId, UserId, mock};
use crate::session::Session;
use crate::session::chats::{Chats, LoadState};
use crate::settings::{Appearance, KeymapMode, SettingsStore};
use crate::theme;
use crate::ui::chat_list::{ChatList, ChatListEvent};
use crate::ui::command_line::{CommandLine, CommandLineEvent};
use crate::ui::conversation::{Conversation, ConversationEvent};
use crate::ui::details_pane::details_pane;
use crate::ui::nav_rail::{Account, nav_rail};
use crate::ui::palette::{self, Scope};
use crate::ui::{key_context, show_error, update_settings};

const CHAT_LIST_MIN: f32 = 240.;
const CHAT_LIST_MAX: f32 = 480.;

pub struct Workspace {
    /// `None` only in tests, which run on mock data without an account.
    session: Option<Entity<Session>>,
    /// The account's chats; the chat list shows a copy.
    _chats: Option<Entity<Chats>>,
    chat_list: Entity<ChatList>,
    conversation: Entity<Conversation>,
    focus_handle: FocusHandle,
    jumps: JumpList,
    command_line: Option<(Entity<CommandLine>, Subscription)>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(
        session: Option<Entity<Session>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let chats = session
            .as_ref()
            .map(|session| cx.new(|cx| Chats::new(session, cx)));
        let chat_list = cx.new(|cx| match &chats {
            Some(_) => ChatList::new(Vec::new(), LoadState::Loading, cx),
            None => ChatList::new(mock::chats(), LoadState::Loaded, cx),
        });
        let conversation = cx.new(|cx| Conversation::new(window, cx));

        let mut subscriptions = vec![
            cx.subscribe(&chat_list, |this, chat_list, event, cx| match event {
                ChatListEvent::Opened(id) => {
                    let Some(chat) = chat_list.read(cx).chat(id).cloned() else {
                        return;
                    };
                    // After a back/forward step this is the jumplist's current
                    // entry, which `push` ignores.
                    this.jumps.push(id.clone());
                    // Real messages load from milestone 5.
                    let messages = match this.session {
                        Some(_) => Vec::new(),
                        None => mock::messages(id),
                    };
                    this.conversation
                        .update(cx, |conversation, cx| conversation.show(chat, messages, cx));
                    cx.notify();
                }
                ChatListEvent::Updated(chat) => {
                    let chat = chat.clone();
                    this.conversation
                        .update(cx, |conversation, cx| conversation.update_chat(chat, cx));
                    cx.notify();
                }
                ChatListEvent::Closed => {
                    this.conversation
                        .update(cx, |conversation, cx| conversation.close(cx));
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &conversation,
                window,
                |this, _, event, window, cx| match event {
                    ConversationEvent::Send(text) => this.send(text.clone(), window, cx),
                },
            ),
            // Redraw the status line and which-key popup as keys are typed.
            cx.observe_pending_input(window, |_, _, cx| cx.notify()),
        ];
        if let Some(session) = &session {
            // Account name and connection status are shown in the rail.
            subscriptions.push(cx.observe(session, |_, _, cx| cx.notify()));
        }
        if let Some(chats) = &chats {
            subscriptions.push(cx.observe(chats, |this, chats, cx| {
                let (list, state) = {
                    let chats = chats.read(cx);
                    (chats.chats().to_vec(), chats.state().clone())
                };
                this.chat_list
                    .update(cx, |chat_list, cx| chat_list.set_chats(list, state, cx));
            }));
        }
        chat_list.focus_handle(cx).focus(window, cx);

        Self {
            session,
            _chats: chats,
            chat_list,
            conversation,
            focus_handle: cx.focus_handle(),
            jumps: JumpList::default(),
            command_line: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn chats(&self, cx: &App) -> Vec<ChatSummary> {
        self.chat_list.read(cx).chats().to_vec()
    }

    fn helix(cx: &App) -> bool {
        SettingsStore::get(cx).keymap_mode == KeymapMode::Helix
    }

    /// Mock data echoes the message locally; sending for real comes with
    /// milestone 6.
    fn send(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_some() {
            // Keep what the user wrote.
            self.conversation.update(cx, |conversation, cx| {
                conversation.set_composer_text(&text, window, cx)
            });
            show_error("Sending messages isn't supported yet.".into(), window, cx);
            return;
        }
        let message = Message {
            id: MessageId(format!("local-{}", chrono::Utc::now().timestamp_micros()).into()),
            sender_id: UserId("me".into()),
            sender_name: "You".into(),
            own: true,
            body: text.into(),
            sent_at: chrono::Utc::now(),
        };
        self.conversation.update(cx, |conversation, cx| {
            conversation.push_message(message, cx)
        });
    }

    /// Opens a chat and focuses it, as when picking one from the palette:
    /// the message list in Helix mode (normal mode), else the composer.
    pub fn jump_to_chat(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_list.update(cx, |list, cx| list.open(id, cx));
        if Self::helix(cx) {
            self.focus_messages(&FocusMessages, window, cx);
        } else {
            self.focus_composer(&FocusComposer, window, cx);
        }
    }

    fn update_chat_list(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut ChatList, &mut Context<ChatList>),
    ) {
        self.chat_list.update(cx, f);
    }

    /// Whether a chat is open. The chat list is the source of truth: the
    /// conversation learns about a newly opened chat only once the `Opened`
    /// event is delivered.
    fn has_open_chat(&self, cx: &App) -> bool {
        self.chat_list.read(cx).selected_index().is_some()
    }

    fn focus_chat_list(&mut self, _: &FocusChatList, window: &mut Window, cx: &mut Context<Self>) {
        self.chat_list.focus_handle(cx).focus(window, cx);
    }

    fn focus_messages(&mut self, _: &FocusMessages, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_open_chat(cx) {
            self.conversation.focus_handle(cx).focus(window, cx);
        }
    }

    fn focus_composer(&mut self, _: &FocusComposer, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_open_chat(cx) {
            self.conversation
                .read(cx)
                .composer_focus(cx)
                .focus(window, cx);
        }
    }

    fn focus_conversation(
        &mut self,
        _: &FocusConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_composer(&FocusComposer, window, cx);
    }

    fn focus_next_pane(&mut self, _: &FocusNextPane, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_list.focus_handle(cx).is_focused(window) {
            self.focus_messages(&FocusMessages, window, cx);
        } else {
            self.focus_chat_list(&FocusChatList, window, cx);
        }
    }

    fn jump(&mut self, back: bool, cx: &mut Context<Self>) {
        let list = self.chat_list.read(cx);
        self.jumps.retain(|id| list.chat(id).is_some());
        let target = if back {
            self.jumps.back()
        } else {
            self.jumps.forward()
        };
        if let Some(id) = target {
            self.chat_list.update(cx, |list, cx| list.open(id, cx));
        }
    }

    fn toggle_details(&mut self, _: &ToggleDetails, window: &mut Window, cx: &mut Context<Self>) {
        update_settings(window, cx, |settings| {
            settings.details_open = !settings.details_open
        });
        cx.notify();
    }

    fn set_appearance(
        &mut self,
        appearance: Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        update_settings(window, cx, |settings| settings.appearance = appearance);
        theme::apply_appearance(window, cx);
        cx.notify();
    }

    fn toggle_appearance(
        &mut self,
        _: &ToggleAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let next = SettingsStore::get(cx).appearance.next();
        self.set_appearance(next, window, cx);
    }

    fn toggle_helix_mode(
        &mut self,
        _: &ToggleHelixMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = match SettingsStore::get(cx).keymap_mode {
            KeymapMode::Standard => KeymapMode::Helix,
            KeymapMode::Helix => KeymapMode::Standard,
        };
        self.set_keymap_mode(mode, window, cx);
    }

    fn set_keymap_mode(&mut self, mode: KeymapMode, window: &mut Window, cx: &mut Context<Self>) {
        update_settings(window, cx, |settings| settings.keymap_mode = mode);
        window.refresh();
    }

    fn open_palette_scoped(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        palette::open_palette(scope, self.chats(cx), cx.weak_entity(), window, cx);
    }

    fn open_command_line(
        &mut self,
        _: &OpenCommandLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let themes = theme::available(cx);
        let chats = self
            .chats(cx)
            .into_iter()
            .map(|c| c.title.to_string())
            .collect();
        let previous_focus = window.focused(cx);
        let view = cx.new(|cx| CommandLine::new(themes, chats, window, cx));
        let subscription = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
            this.command_line = None;
            if let Some(focus) = &previous_focus {
                focus.focus(window, cx);
            }
            if let CommandLineEvent::Submit(line) = event
                && !line.trim().is_empty()
            {
                match command_line::parse(line) {
                    Ok(command) => this.run(command, window, cx),
                    Err(err) => show_error(err, window, cx),
                }
            }
            cx.notify();
        });
        self.command_line = Some((view, subscription));
        cx.notify();
    }

    fn run(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::Quit => cx.quit(),
            Command::Theme(query) => {
                let themes = theme::available(cx);
                match matches_name(&query, themes.iter().map(String::as_str)) {
                    NameMatch::One(name) => {
                        if let Err(err) = theme::select_theme(name, window, cx) {
                            show_error(format!("Couldn't switch theme: {err:#}"), window, cx);
                        }
                    }
                    NameMatch::None => {
                        show_error(format!("No theme matches {query:?}"), window, cx)
                    }
                    NameMatch::Ambiguous(n) => show_error(
                        format!("{n} themes match {query:?}; be more specific"),
                        window,
                        cx,
                    ),
                }
            }
            Command::Open(query) => {
                let chats = self.chats(cx);
                let titles = chats.iter().map(|chat| chat.title.as_ref());
                match matches_name(&query, titles) {
                    NameMatch::One(title) => {
                        if let Some(chat) = chats.iter().find(|chat| chat.title.as_ref() == title) {
                            self.jump_to_chat(chat.id.clone(), window, cx);
                        }
                    }
                    NameMatch::None => show_error(format!("No chat matches {query:?}"), window, cx),
                    // Never guess: names are peer-chosen and may imitate
                    // each other. Let the user pick from the list instead.
                    NameMatch::Ambiguous(_) => self.open_palette_scoped(Scope::Chats, window, cx),
                }
            }
            Command::Appearance(appearance) => self.set_appearance(appearance, window, cx),
            Command::ToggleDetails => self.toggle_details(&ToggleDetails, window, cx),
            Command::Keymap(mode) => self.set_keymap_mode(mode, window, cx),
            Command::Shortcuts => palette::show_shortcuts(window, cx),
        }
    }

    fn render_status_line(&self, window: &Window, cx: &App) -> AnyElement {
        let theme = cx.theme();
        if let Some((command_line, _)) = &self.command_line {
            return command_line.clone().into_any_element();
        }
        let composer_focused = self
            .conversation
            .read(cx)
            .composer_focus(cx)
            .contains_focused(window, cx);
        let (mode, bg, fg) = if composer_focused {
            ("INS", theme.success, theme.success_foreground)
        } else {
            ("NOR", theme.primary, theme.primary_foreground)
        };
        let pending = window
            .pending_input_keystrokes()
            .map(|keys| format_keystrokes(keys, cx.platform_info().os))
            .unwrap_or_default();
        let chat = self
            .conversation
            .read(cx)
            .chat()
            .map(|chat| chat.title.clone())
            .unwrap_or_default();
        h_flex()
            .flex_none()
            .h(px(24.))
            .gap_3()
            .pr_3()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.sidebar)
            .text_xs()
            .child(
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .px_2()
                    .bg(bg)
                    .text_color(fg)
                    .font_weight(FontWeight::BOLD)
                    .child(mode),
            )
            .child(div().flex_1().truncate().child(chat))
            .child(div().text_color(theme.muted_foreground).child(pending))
            .into_any_element()
    }

    /// Helix-style popup listing what each next key does after a prefix
    /// such as `space` or `g`.
    fn render_which_key(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        let pending = window.pending_input_keystrokes()?;
        let os = cx.platform_info().os;
        let rows = which_key_rows(window, os);
        if rows.is_empty() {
            return None;
        }
        let theme = cx.theme();
        Some(
            v_flex()
                .absolute()
                .bottom(px(32.))
                .right(px(12.))
                .min_w(px(220.))
                .p_2()
                .gap_0p5()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .text_sm()
                .child(
                    div()
                        .pb_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format_keystrokes(pending, os)),
                )
                .children(rows.into_iter().map(|(key, label)| {
                    h_flex()
                        .gap_3()
                        .child(
                            div()
                                .min_w(px(28.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.primary)
                                .child(key),
                        )
                        .child(label)
                }))
                .into_any_element(),
        )
    }
}

/// `(remaining keys, label)` for every binding that continues the pending
/// prefix, skipping unbound (`null`) entries.
fn which_key_rows(window: &Window, os: OperatingSystem) -> Vec<(String, String)> {
    let Some(pending) = window.pending_input_keystrokes() else {
        return Vec::new();
    };
    let mut rows: Vec<(String, String)> = window
        .possible_bindings_for_input(pending)
        .iter()
        .filter(|binding| !gpui::is_no_action(binding.action()))
        .filter_map(|binding| {
            let rest = binding.keystrokes().get(pending.len()..)?;
            let keys = rest
                .iter()
                .map(|k| k.as_keystroke().unparse(os))
                .collect::<Vec<_>>()
                .join(" ");
            Some((keys, actions::label(binding.action())))
        })
        .collect();
    rows.sort();
    rows.dedup();
    rows
}

fn format_keystrokes(keystrokes: &[Keystroke], os: OperatingSystem) -> String {
    keystrokes
        .iter()
        .map(|keystroke| keystroke.unparse(os))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = SettingsStore::get(cx).clone();
        let helix = settings.keymap_mode == KeymapMode::Helix;
        let open_chat = self.conversation.read(cx).chat().cloned();
        let chat_list_width = settings.chat_list_width.clamp(CHAT_LIST_MIN, CHAT_LIST_MAX);
        let status_line =
            (helix || self.command_line.is_some()).then(|| self.render_status_line(window, cx));
        let which_key = self.render_which_key(window, cx);
        let account = self.session.as_ref().map(|session| {
            let session = session.read(cx);
            Account {
                name: session.display_name().clone(),
                status: session.status(),
                version_expires: session.version_expires(),
            }
        });
        let theme = cx.theme();

        v_flex()
            .key_context(key_context("Workspace", cx))
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
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.update_chat_list(cx, |list, cx| list.open_at(0, cx))
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.update_chat_list(cx, |list, cx| {
                    if let Some(last) = list.chats().len().checked_sub(1) {
                        list.open_at(last, cx)
                    }
                })
            }))
            .on_action(cx.listener(|this, action: &OpenChatAt, _, cx| {
                let index = action.index;
                this.update_chat_list(cx, |list, cx| list.open_at(index, cx))
            }))
            .on_action(cx.listener(|this, _: &JumpBack, _, cx| this.jump(true, cx)))
            .on_action(cx.listener(|this, _: &JumpForward, _, cx| this.jump(false, cx)))
            .on_action(cx.listener(Self::focus_chat_list))
            .on_action(cx.listener(Self::focus_messages))
            .on_action(cx.listener(Self::focus_composer))
            .on_action(cx.listener(Self::focus_conversation))
            .on_action(cx.listener(Self::focus_next_pane))
            .on_action(cx.listener(Self::toggle_details))
            .on_action(cx.listener(Self::toggle_appearance))
            .on_action(cx.listener(Self::toggle_helix_mode))
            .on_action(cx.listener(Self::open_command_line))
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| {
                this.open_palette_scoped(Scope::All, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FindChat, window, cx| {
                this.open_palette_scoped(Scope::Chats, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PickTheme, window, cx| {
                this.open_palette_scoped(Scope::Themes, window, cx)
            }))
            .on_action(
                cx.listener(|_, _: &ShowShortcuts, window, cx| palette::show_shortcuts(window, cx)),
            )
            .relative()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.font_family.clone())
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(nav_rail(settings.appearance, account, cx))
                    .child(
                        h_resizable("workspace-panes")
                            .on_resize(|state, window, cx| {
                                let Some(width) = state.read(cx).sizes().first().copied() else {
                                    return;
                                };
                                let width = f32::from(width);
                                update_settings(window, cx, |settings| {
                                    settings.chat_list_width = width
                                });
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
                    ),
            )
            .children(status_line)
            .children(which_key)
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
        setup_with_settings(None, cx)
    }

    fn setup_with_settings(
        settings_path: Option<std::path::PathBuf>,
        cx: &mut TestAppContext,
    ) -> (Entity<Workspace>, &mut VisualTestContext) {
        let (window, workspace) = cx.update(|cx| {
            gpui_kit::init(cx);
            assert_eq!(SettingsStore::open(settings_path, cx), None);
            theme::init(None, cx).unwrap();
            assert_eq!(keymap::load(None, cx), Vec::<String>::new());
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| Workspace::new(None, window, cx))
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
    fn live_chat_updates_keep_the_open_chat_in_step(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let mut chats = workspace.read_with(cx, |workspace, cx| workspace.chats(cx));
        cx.simulate_keystrokes("alt-down alt-down");
        let open = chats[1].id.clone();
        let set_chats = |chats: Vec<ChatSummary>, cx: &mut VisualTestContext| {
            workspace.update(cx, |workspace, cx| {
                workspace
                    .chat_list
                    .update(cx, |list, cx| list.set_chats(chats, LoadState::Loaded, cx))
            });
        };
        let messages = message_count(&workspace, cx);
        assert!(messages > 0);

        // Renamed and moved to the top: the header follows, the messages
        // stay, and navigation continues from its new place.
        let mut renamed = chats.remove(1);
        renamed.title = "Renamed".into();
        chats.insert(0, renamed);
        set_chats(chats.clone(), cx);
        assert_eq!(open_title(&workspace, cx).as_deref(), Some("Renamed"));
        assert_eq!(message_count(&workspace, cx), messages);
        cx.simulate_keystrokes("alt-down");
        assert_eq!(
            open_title(&workspace, cx).as_deref(),
            Some(&*chats[1].title)
        );

        // Deleted elsewhere while open: the conversation closes.
        cx.simulate_keystrokes("alt-up");
        chats.retain(|chat| chat.id != open);
        set_chats(chats.clone(), cx);
        assert_eq!(open_title(&workspace, cx), None);
        cx.simulate_keystrokes("alt-down");
        assert_eq!(
            open_title(&workspace, cx).as_deref(),
            Some(&*chats[0].title)
        );
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
        let focused = focus_probe(&workspace, cx);
        assert_eq!(focused(cx), Focus::ChatList);

        // Nothing open yet: focusing the composer is refused.
        cx.simulate_keystrokes("enter");
        assert_eq!(focused(cx), Focus::ChatList);

        // j/k move only while the list has focus; in the composer they type.
        cx.simulate_keystrokes("j enter");
        assert_eq!(focused(cx), Focus::Composer);
        let titles = titles(&workspace, cx);
        assert_eq!(open_title(&workspace, cx).as_ref(), Some(&titles[0]));
        cx.simulate_input("j");
        assert_eq!(open_title(&workspace, cx).as_ref(), Some(&titles[0]));
        assert_eq!(composer_text(&workspace, cx), "j");

        // Escape leaves the composer (the input passes it on).
        cx.simulate_keystrokes("escape k");
        assert_eq!(focused(cx), Focus::ChatList);
        assert_eq!(open_title(&workspace, cx).as_ref(), titles.last());
    }

    #[derive(Debug, PartialEq)]
    enum Focus {
        ChatList,
        Messages,
        Composer,
        Other,
    }

    fn focus_probe(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> impl Fn(&mut VisualTestContext) -> Focus + use<> {
        let (list, messages, composer) = workspace.read_with(cx, |workspace, cx| {
            (
                workspace.chat_list.focus_handle(cx),
                workspace.conversation.focus_handle(cx),
                workspace.conversation.read(cx).composer_focus(cx),
            )
        });
        move |cx| {
            cx.update(|window, cx| {
                if list.is_focused(window) {
                    Focus::ChatList
                } else if messages.is_focused(window) {
                    Focus::Messages
                } else if composer.contains_focused(window, cx) {
                    Focus::Composer
                } else {
                    Focus::Other
                }
            })
        }
    }

    fn composer_text(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> String {
        workspace.read_with(cx, |workspace, cx| {
            workspace.conversation.read(cx).composer_text(cx)
        })
    }

    fn message_count(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> usize {
        workspace.read_with(cx, |workspace, cx| {
            workspace.conversation.read(cx).message_count()
        })
    }

    fn cursor(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Option<usize> {
        workspace.read_with(cx, |workspace, cx| workspace.conversation.read(cx).cursor())
    }

    fn enable_helix(cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            SettingsStore::update(cx, |settings| settings.keymap_mode = KeymapMode::Helix).unwrap();
            window.refresh();
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn toggles_are_saved_to_disk(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let (_, cx) = setup_with_settings(Some(path.clone()), cx);
        let on_disk = || Settings::load(&path).0;
        assert!(on_disk().details_open);

        cx.simulate_keystrokes(&format!("{MOD}-i"));
        assert!(!on_disk().details_open);

        cx.simulate_keystrokes(&format!("{MOD}-shift-t"));
        assert_eq!(on_disk().appearance, Appearance::Light);
        cx.simulate_keystrokes(&format!("{MOD}-shift-t {MOD}-shift-t"));
        assert_eq!(on_disk().appearance, Appearance::System);
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
    fn palette_jumps_to_chat_and_focuses_composer(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.simulate_keystrokes(&format!("{MOD}-k"));
        cx.run_until_parked();
        cx.simulate_input("Grace");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(open_title(&workspace, cx).as_deref(), Some("Grace Hopper"));
        assert_eq!(focus_probe(&workspace, cx)(cx), Focus::Composer);
    }

    #[gpui::test]
    fn palette_selects_themes(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        cx.simulate_keystrokes(&format!("{MOD}-k"));
        cx.run_until_parked();
        cx.simulate_input("Theme: Default Dark");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.update(|_, cx| {
            let theme = gpui_kit::component::Theme::global(cx);
            assert_eq!(theme.dark_theme.name, "Default Dark");
            assert!(theme.is_dark());
        });
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

    #[gpui::test]
    fn enter_sends_and_shift_enter_adds_a_line(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.simulate_keystrokes("j enter");
        let before = message_count(&workspace, cx);

        cx.simulate_input("hello");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("world");
        assert_eq!(message_count(&workspace, cx), before);
        cx.simulate_keystrokes("enter");
        assert_eq!(message_count(&workspace, cx), before + 1);
        assert_eq!(composer_text(&workspace, cx), "");

        // Blank messages are not sent.
        cx.simulate_input("   ");
        cx.simulate_keystrokes("enter");
        assert_eq!(message_count(&workspace, cx), before + 1);
    }

    #[gpui::test]
    fn helix_bindings_are_inert_in_standard_mode(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.simulate_keystrokes("g e space d");
        assert_eq!(open_title(&workspace, cx), None);
        assert!(cx.update(|_, cx| SettingsStore::get(cx).details_open));
    }

    #[gpui::test]
    fn helix_normal_and_insert_modes(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        enable_helix(cx);
        let focused = focus_probe(&workspace, cx);
        let titles = titles(&workspace, cx);

        cx.simulate_keystrokes("g e");
        assert_eq!(open_title(&workspace, cx).as_ref(), titles.last());
        cx.simulate_keystrokes("g g");
        assert_eq!(open_title(&workspace, cx).as_ref(), Some(&titles[0]));

        // l/h move between panes; j/k move the message cursor.
        cx.simulate_keystrokes("l");
        assert_eq!(focused(cx), Focus::Messages);
        let count = message_count(&workspace, cx);
        cx.simulate_keystrokes("k");
        assert_eq!(cursor(&workspace, cx), Some(count - 1));
        cx.simulate_keystrokes("k k j");
        assert_eq!(cursor(&workspace, cx), Some(count - 2));
        cx.simulate_keystrokes("g g");
        assert_eq!(cursor(&workspace, cx), Some(0));
        cx.simulate_keystrokes("escape");
        assert_eq!(cursor(&workspace, cx), None);

        // i enters insert mode, where letters type instead of moving.
        cx.simulate_keystrokes("i");
        assert_eq!(focused(cx), Focus::Composer);
        cx.simulate_input("hjkl");
        assert_eq!(composer_text(&workspace, cx), "hjkl");
        cx.simulate_keystrokes("enter");
        assert_eq!(message_count(&workspace, cx), count + 1);

        // Escape returns to normal mode in the same pane.
        cx.simulate_keystrokes("escape");
        assert_eq!(focused(cx), Focus::Messages);
        cx.simulate_keystrokes("h");
        assert_eq!(focused(cx), Focus::ChatList);
    }

    #[gpui::test]
    fn helix_space_menu_brackets_and_jumplist(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        enable_helix(cx);
        let chats = workspace.read_with(cx, |workspace, cx| workspace.chats(cx));

        cx.simulate_keystrokes("space d");
        assert!(!cx.update(|_, cx| SettingsStore::get(cx).details_open));

        let first_unread = chats.iter().find(|chat| chat.unread > 0).unwrap();
        cx.simulate_keystrokes("] u");
        assert_eq!(
            open_title(&workspace, cx).as_deref(),
            Some(&*first_unread.title)
        );

        cx.simulate_keystrokes("g e");
        let last = chats.last().unwrap();
        assert_eq!(open_title(&workspace, cx).as_deref(), Some(&*last.title));
        cx.simulate_keystrokes("ctrl-o");
        assert_eq!(
            open_title(&workspace, cx).as_deref(),
            Some(&*first_unread.title)
        );
        cx.simulate_keystrokes("ctrl-i");
        assert_eq!(open_title(&workspace, cx).as_deref(), Some(&*last.title));
    }

    #[gpui::test]
    fn which_key_lists_continuations_of_pending_prefix(cx: &mut TestAppContext) {
        let (_, cx) = setup(cx);
        enable_helix(cx);
        // A user unbinding must disappear from the popup.
        cx.update(|_, cx| {
            cx.bind_keys([gpui::KeyBinding::new(
                "space d",
                gpui::NoAction,
                Some("Workspace && helix && !Input"),
            )])
        });
        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        let rows = cx.update(|window, cx| which_key_rows(window, cx.platform_info().os));
        let row = |keys: &str, label: &str| (keys.to_string(), label.to_string());
        // Only the keys after the prefix are shown.
        assert!(rows.contains(&row("f", "find chat")), "{rows:?}");
        assert!(!rows.iter().any(|(keys, _)| keys == "d"), "{rows:?}");
    }

    #[gpui::test]
    fn command_line_runs_commands_and_reports_errors(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        enable_helix(cx);
        let run = |line: &str, cx: &mut VisualTestContext| {
            cx.simulate_keystrokes(":");
            cx.run_until_parked();
            cx.simulate_input(line);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
        };

        run("o grace", cx);
        assert_eq!(open_title(&workspace, cx).as_deref(), Some("Grace Hopper"));
        assert_eq!(focus_probe(&workspace, cx)(cx), Focus::Messages);

        run("theme default-dark", cx);
        cx.update(|_, cx| {
            assert_eq!(
                gpui_kit::component::Theme::global(cx).dark_theme.name,
                "Default Dark"
            )
        });

        // A bad command closes the line and returns focus; nothing panics.
        run("nope", cx);
        assert!(workspace.read_with(cx, |workspace, _| workspace.command_line.is_none()));
        assert_eq!(focus_probe(&workspace, cx)(cx), Focus::Messages);

        // Tab cycles through completions; escape cancels without running
        // anything and returns focus to where it was.
        cx.simulate_keystrokes("h :");
        cx.run_until_parked();
        cx.simulate_input("q");
        let line = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .command_line
                    .as_ref()
                    .unwrap()
                    .0
                    .read(cx)
                    .value(cx)
            })
        };
        cx.simulate_keystrokes("tab");
        assert_eq!(line(cx), "quit");
        cx.simulate_keystrokes("tab");
        assert_eq!(line(cx), "q");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(workspace.read_with(cx, |workspace, _| workspace.command_line.is_none()));
        assert_eq!(focus_probe(&workspace, cx)(cx), Focus::ChatList);

        // Leaving Helix mode takes effect immediately.
        run("keymap standard", cx);
        assert_eq!(
            cx.update(|_, cx| SettingsStore::get(cx).keymap_mode),
            KeymapMode::Standard
        );
        cx.simulate_keystrokes("space d");
        assert!(cx.update(|_, cx| SettingsStore::get(cx).details_open));
    }

    #[gpui::test]
    fn ambiguous_open_lets_the_user_pick(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        enable_helix(cx);
        let before = open_title(&workspace, cx);
        // "a" is in several titles: nothing is opened, the chat picker is.
        cx.simulate_keystrokes(":");
        cx.run_until_parked();
        cx.simulate_input("o a");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(open_title(&workspace, cx), before);
        cx.simulate_input("Grace");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(open_title(&workspace, cx).as_deref(), Some("Grace Hopper"));
    }

    #[gpui::test]
    fn yank_copies_only_the_message_under_the_cursor(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        enable_helix(cx);
        let clipboard = |cx: &mut VisualTestContext| {
            cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
        };
        cx.simulate_keystrokes("g g l y");
        assert_eq!(clipboard(cx), None);
        cx.simulate_keystrokes("k k y");
        let expected = workspace.read_with(cx, |workspace, cx| {
            let conversation = workspace.conversation.read(cx);
            conversation.message_body(conversation.cursor().unwrap())
        });
        assert_eq!(clipboard(cx), Some(expected));
    }
}
