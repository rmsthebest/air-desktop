//! Root view: nav rail, chat list, conversation and the optional details pane.
//! Owns focus movement between panes, workspace-wide actions, and in Helix
//! mode the status line, which-key popup and `:` command line.

use gpui::{AsKeystroke as _, Keystroke, OperatingSystem, Subscription};
use gpui_kit::component::{ActiveTheme as _, h_flex, h_resizable, resizable_panel, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, FontWeight, IntoElement, Render,
    Task, Window, div, px,
};

use std::path::PathBuf;

use crate::actions::{
    self, AcceptRequest, AddContact, AttachFile, BlockContact, DeclineRequest, DeleteChat,
    FindChat, FocusChatList, FocusComposer, FocusConversation, FocusMessages, FocusNextPane,
    InsertEmoji, InviteToGroup, JumpBack, JumpForward, LinkDevice, NewGroup, NextChat,
    NextUnreadChat, OpenChatAt, OpenCommandLine, OpenPalette, OpenSettings, PickTheme, PrevChat,
    PrevUnreadChat, ReportSpam, RetractRequest, SelectFirst, SelectLast, ShowSafetyCode,
    ShowShortcuts, ToggleAppearance, ToggleDetails, ToggleHelixMode, ToggleMute, UnblockContact,
};
use crate::command_line::{self, Candidates, Command, NameMatch, matches_name};
use crate::model::{
    AccountDeletion, ChatId, ChatSummary, ContactOp, JumpList, Message, MessageId, Mute, Request,
    Side, Stage, UserId, mock,
};
use crate::session::account_settings::AccountSettings;
use crate::session::chats::{Chats, LoadState};
use crate::session::composer::Composer;
use crate::session::devices::{self, Devices, Link, LinkEvent};
use crate::session::groups::Members;
use crate::session::invitations::Invitations;
use crate::session::messages::Messages;
use crate::session::safety::SafetyLines;
use crate::session::{DeletionAborted, OpenFromAlert, Session};
use crate::settings::{Appearance, KeymapMode, Notifications, SettingsStore};
use crate::theme;
use crate::ui::chat_list::{ChatList, ChatListEvent};
use crate::ui::command_line::{CommandLine, CommandLineEvent};
use crate::ui::conversation::{Conversation, ConversationEvent};
use crate::ui::details_pane::details_pane;
use crate::ui::link_view::{LinkPhase, LinkView, LinkViewEvent};
use crate::ui::nav_rail::{Account, nav_rail};
use crate::ui::palette::{self, Scope};
use crate::ui::settings_view::{AccountEntities, SettingsEvent, SettingsView};
use crate::ui::{key_context, show_error, show_info, show_refusal, update_settings};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::ButtonVariant;

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
    /// A contact operation in flight; another waits for it to finish, so a
    /// repeated key press doesn't send a request twice.
    contact_task: Option<Task<()>>,
    /// The open group's members.
    members: Option<(Entity<Members>, Subscription)>,
    settings_view: Entity<SettingsView>,
    /// The settings screen is shown in place of the conversation.
    settings_open: bool,
    /// A chat opened before it was listed, and how to focus it once it is.
    focus_when_opened: Option<(ChatId, FocusChat)>,
    /// The user's devices (with an account).
    devices: Option<Entity<Devices>>,
    /// The account's synced settings (with an account).
    account_settings: Option<Entity<AccountSettings>>,
    /// Invite codes to give out (with an account).
    invitations: Option<Entity<Invitations>>,
    /// Linking a new device from this one.
    linking: Option<Linking>,
    /// The safety code being looked up; a new request replaces it.
    safety_code: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// A link to a new device in progress (or just ended, while its pane shows).
struct Linking {
    /// Until the user confirms: dropping it turns the link down.
    link: Option<Link>,
    /// Linked or failed.
    finished: bool,
    /// Shown in place of the settings rows.
    pane: Option<(Entity<LinkView>, Subscription)>,
    _receive: Task<()>,
}

/// Focuses the open chat one way or another.
type FocusChat = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);

/// What reporting spam tells, as upstream.
const REPORT_SPAM: &str = "Air will be notified that this person may be sending spam. \
                           Air is never able to see the contents of any chats.";

/// The word typed to confirm deleting the account, as upstream.
const DELETE_ACCOUNT_WORD: &str = "delete";

/// A synced account setting to change.
enum Synced {
    ReadReceipts(bool),
    DeleteExpiredRequests(bool),
}

/// What `:mute`, `:unmute` and the toggle ask for.
enum MuteChange<'a> {
    /// Unmute if muted, else mute until unmuted.
    Toggle,
    /// For how long, as typed.
    Mute(&'a str),
    Unmute,
}

/// The confirmation `op` asks for in the one-to-one `chat` (title,
/// description, button), or why it doesn't apply there
/// ([`ContactOp::check`], which the session asks again). Upstream's
/// wording.
fn contact_confirmation(
    op: ContactOp,
    chat: &ChatSummary,
) -> Result<(String, String, &'static str), &'static str> {
    let facts = chat.one_to_one.ok_or("That's for a contact's chat")?;
    op.check(facts)?;
    let name = &chat.title;
    Ok(match op {
        ContactOp::Block if facts.side == Side::Incoming => (
            format!("Block {name}?"),
            format!(
                "You'll decline {name}'s contact request and won't receive contact \
                 requests or messages from them anymore. They won't be able to see \
                 that you blocked them. The request goes away, and with it the only \
                 place to unblock them."
            ),
            "Block",
        ),
        ContactOp::Block => (
            format!("Block {name}?"),
            format!(
                "You won't receive messages from {name} and they won't be able to view \
                 any updates to your profile. They won't be able to see that you \
                 blocked them. You can unblock them anytime."
            ),
            "Block",
        ),
        ContactOp::Unblock => (
            format!("Unblock {name}?"),
            format!(
                "You'll be able to send and receive messages from {name} and they'll be \
                 able to view your current profile."
            ),
            "Unblock",
        ),
        ContactOp::Report => (
            format!("Report {name} for spam?"),
            REPORT_SPAM.into(),
            "Report spam",
        ),
        ContactOp::Retract => (
            "Retract contact request?".into(),
            format!(
                "If you retract your contact request, {name} will no longer be able to \
                 accept it."
            ),
            "Retract",
        ),
        ContactOp::Delete if facts.side == Side::Contact && facts.stage == Stage::Ended => (
            format!("Delete {name}?"),
            "They ended this chat. It and its history are erased from all your devices.".into(),
            "Delete",
        ),
        ContactOp::Delete if facts.side == Side::Contact => (
            format!("Delete {name}?"),
            format!(
                "{name} is removed from your contacts and their chat with you ends. \
                 The chat and its history are erased from all your devices. To talk \
                 again, one of you sends a new contact request."
            ),
            "Delete",
        ),
        // A request that ended, or one to or from someone blocked (which
        // may still be open on their side: it is erased all the same).
        ContactOp::Delete => (
            "Delete this request?".into(),
            "The request and its chat are erased from all your devices.".into(),
            "Delete",
        ),
    })
}

/// Whose safety code `:safety-code <query>` asks for in the open `chat`.
#[derive(Debug, PartialEq)]
enum SafetyTarget {
    /// The one-to-one chat's contact (accepted or not: the session checks).
    Contact,
    /// The group member `query` names.
    Member,
    /// A group, but no member named: ask on the command line.
    AskMember,
}

fn safety_target(chat: Option<&ChatSummary>, query: &str) -> Result<SafetyTarget, &'static str> {
    let Some(chat) = chat else {
        return Err("Open a chat first");
    };
    let named = !query.trim().is_empty();
    if chat.is_self_chat {
        Err("That's your own devices: there's nobody to compare with")
    } else if chat.is_group {
        Ok(if named {
            SafetyTarget::Member
        } else {
            SafetyTarget::AskMember
        })
    } else if named {
        // Never silently show the contact's code for another name.
        Err("Only a group's members are named: here it's the contact's code")
    } else {
        Ok(SafetyTarget::Contact)
    }
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
        let devices = session
            .as_ref()
            .map(|session| cx.new(|cx| Devices::new(session, cx)));
        let account_settings = session
            .as_ref()
            .map(|session| cx.new(|cx| AccountSettings::new(session, cx)));
        let invitations = session.as_ref().map(|session| {
            let user = session.read(cx).user().clone();
            cx.new(|cx| Invitations::new(user, cx))
        });
        let account = match (&devices, &account_settings, &invitations) {
            (Some(devices), Some(settings), Some(invitations)) => Some(AccountEntities {
                devices: devices.clone(),
                settings: settings.clone(),
                invitations: invitations.clone(),
            }),
            _ => None,
        };
        let settings_view = cx.new(|cx| SettingsView::new(session.clone(), account, cx));

        let mut subscriptions = vec![
            cx.subscribe_in(
                &chat_list,
                window,
                |this, chat_list, event, window, cx| match event {
                    ChatListEvent::Opened(id) => {
                        let Some(chat) = chat_list.read(cx).chat(id).cloned() else {
                            return;
                        };
                        // After a back/forward step this is the jumplist's current
                        // entry, which `push` ignores.
                        this.jumps.push(id.clone());
                        // Reported below, once the new chat is shown: not
                        // the old one first.
                        this.unhide_settings(cx);
                        let (messages, compose) = match &this.session {
                            Some(session) => (
                                cx.new(|cx| Messages::open(session, id, cx)),
                                cx.new(|cx| Composer::open(session, id, cx)),
                            ),
                            None => (
                                cx.new(|_| Messages::fixed(mock::messages(id))),
                                cx.new(|_| Composer::fixed()),
                            ),
                        };
                        this.members = match &this.session {
                            Some(session) if chat.is_group => {
                                let members = cx.new(|cx| Members::open(session, id, cx));
                                let redraw = cx.observe(&members, |_, _, cx| cx.notify());
                                Some((members, redraw))
                            }
                            _ => None,
                        };
                        this.conversation.update(cx, |conversation, cx| {
                            conversation.show(chat, messages, compose, window, cx)
                        });
                        this.report_attention(window, cx);
                        cx.notify();
                    }
                    ChatListEvent::AwaitedOpened(id) => {
                        // Takes focus if it is still where the wait left
                        // it: the user may have gone on to type elsewhere.
                        if let Some((awaited, focus)) = this.focus_when_opened.take()
                            && &awaited == id
                            && this.chat_list.focus_handle(cx).is_focused(window)
                        {
                            focus(this, window, cx);
                        }
                    }
                    ChatListEvent::Updated(chat) => {
                        let chat = chat.clone();
                        this.conversation.update(cx, |conversation, cx| {
                            conversation.update_chat(chat, window, cx)
                        });
                        cx.notify();
                    }
                    ChatListEvent::Closed => {
                        this.members = None;
                        this.conversation
                            .update(cx, |conversation, cx| conversation.close(window, cx));
                        this.report_attention(window, cx);
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &conversation,
                window,
                |this, _, event, window, cx| match event {
                    ConversationEvent::Send(text) => this.send(text.clone(), window, cx),
                    ConversationEvent::Attach(paths) => {
                        this.attach_dropped(paths.clone(), window, cx)
                    }
                },
            ),
            cx.subscribe_in(&settings_view, window, |this, _, event, window, cx| {
                this.settings_changed(event.clone(), window, cx)
            }),
            // Redraw the status line and which-key popup as keys are typed.
            cx.observe_pending_input(window, |_, _, cx| cx.notify()),
            cx.observe_window_activation(window, |this, window, cx| {
                this.report_attention(window, cx)
            }),
        ];
        if let Some(session) = &session {
            // Account name and connection status are shown in the rail.
            subscriptions.push(cx.observe(session, |_, _, cx| cx.notify()));
            subscriptions.push(cx.subscribe_in(
                session,
                window,
                |this, _, OpenFromAlert(chat), window, cx| {
                    this.open_from_alert(chat.clone(), window, cx)
                },
            ));
            subscriptions.push(cx.subscribe_in(
                session,
                window,
                |_, _, DeletionAborted, window, cx| {
                    show_refusal(
                        "Your Air account couldn't be deleted. You can try again.".into(),
                        window,
                        cx,
                    )
                },
            ));
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

        let this = Self {
            session,
            _chats: chats,
            chat_list,
            conversation,
            focus_handle: cx.focus_handle(),
            jumps: JumpList::default(),
            command_line: None,
            contact_task: None,
            members: None,
            settings_view,
            settings_open: false,
            focus_when_opened: None,
            devices,
            account_settings,
            invitations,
            linking: None,
            safety_code: None,
            _subscriptions: subscriptions,
        };
        this.report_attention(window, cx);
        this
    }

    /// Lets the session know what is on screen, for desktop notifications.
    fn report_attention(&self, window: &Window, cx: &mut App) {
        let Some(session) = &self.session else { return };
        let active = window.is_window_active();
        // Behind the settings screen, the chat isn't being looked at.
        let open = self
            .conversation
            .read(cx)
            .chat()
            .filter(|_| !self.settings_open)
            .map(|chat| chat.id.clone());
        session.update(cx, |session, _| {
            session.set_attention(active, open.as_ref())
        });
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
        let message = Message::text(
            MessageId(format!("local-{}", chrono::Utc::now().timestamp_micros()).into()),
            (UserId("me".into()), "You".into(), true),
            &text,
            chrono::Utc::now(),
        );
        self.conversation.update(cx, |conversation, cx| {
            conversation.push_message(message, cx)
        });
    }

    /// Opens a chat and focuses it, as when picking one from the palette:
    /// the message list in Helix mode (normal mode), else the composer.
    pub fn jump_to_chat(&mut self, id: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_when_opened = None;
        self.chat_list.update(cx, |list, cx| list.open(id, cx));
        self.focus_jumped(window, cx);
    }

    /// A click on a chat's desktop notification: the window comes up
    /// showing the chat, as after picking it from the palette.
    fn open_from_alert(&mut self, chat: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        // On Wayland the compositor may only mark the window as wanting
        // attention: gpui can't pass on the server's activation token.
        window.activate_window();
        let (already_open, listed) = {
            let list = self.chat_list.read(cx);
            (list.selected() == Some(&chat), list.chat(&chat).is_some())
        };
        let was_hidden = self.unhide_settings(cx);
        // A chat just added may not be listed yet: it opens once it is.
        self.open_then_focus(chat, Self::focus_jumped, window, cx);
        // When the chat opens now, `Opened` reports it; reporting before
        // would name the chat that was open. Otherwise what the settings
        // hid is on screen again, and is reported here.
        if was_hidden && (already_open || !listed) {
            self.report_attention(window, cx);
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
            self.hide_settings(window, cx);
            self.conversation.focus_handle(cx).focus(window, cx);
        }
    }

    fn focus_composer(&mut self, _: &FocusComposer, window: &mut Window, cx: &mut Context<Self>) {
        let list = self.chat_list.read(cx);
        let Some(ix) = list.selected_index() else {
            return;
        };
        let composes = list.chats()[ix].composes();
        self.hide_settings(window, cx);
        // A contact request's chat has no composer until it is accepted,
        // nor has a blocked contact's.
        if !composes {
            self.conversation.focus_handle(cx).focus(window, cx);
        } else {
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

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = true;
        // Tokens for codes arrive in the background, untold.
        if let Some(invitations) = &self.invitations {
            invitations.update(cx, |invitations, cx| invitations.reload(cx));
        }
        self.conversation
            .update(cx, |conversation, _| conversation.set_hidden(true));
        self.report_attention(window, cx);
        match self.link_pane() {
            Some(view) => view.focus_handle(cx).focus(window, cx),
            None => self.settings_view.focus_handle(cx).focus(window, cx),
        }
        cx.notify();
    }

    fn hide_settings(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.unhide_settings(cx) {
            self.report_attention(window, cx);
        }
    }

    /// Hides the settings without reporting attention, for a caller that
    /// reports once it has shown what replaces them. Whether they were shown.
    fn unhide_settings(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.settings_open {
            return false;
        }
        self.settings_open = false;
        self.conversation
            .update(cx, |conversation, _| conversation.set_hidden(false));
        // Hidden with the settings: an unconfirmed link is turned down.
        self.close_link_pane(cx);
        cx.notify();
        true
    }

    /// Starts linking the new device showing `code`, in a pane in place of
    /// the settings rows.
    fn start_link(&mut self, code: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            show_refusal("That needs an account".into(), window, cx);
            return;
        };
        if self
            .linking
            .as_ref()
            .is_some_and(|linking| linking.link.is_none() && !linking.finished)
        {
            show_refusal("Still linking the previous device".into(), window, cx);
            return;
        }
        let code = match devices::parse_code(code) {
            Ok(code) => code,
            Err(refusal) => return show_refusal(refusal, window, cx),
        };
        // Replacing an unconfirmed link turns it down.
        self.linking = None;
        let (link, events) = session.read(cx).link_device(code, cx);
        self.show_link(link, events, window, cx);
    }

    fn show_link(
        &mut self,
        link: Link,
        mut events: tokio::sync::mpsc::UnboundedReceiver<LinkEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| LinkView::new(window, cx));
        let view_events =
            cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
                LinkViewEvent::Confirm(name) => this.confirm_link(name.clone(), window, cx),
                LinkViewEvent::Close => {
                    this.close_link_pane(cx);
                    this.settings_view.focus_handle(cx).focus(window, cx);
                }
            });
        let receive = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.recv().await {
                let finished = !matches!(event, LinkEvent::AwaitingConfirmation);
                let shown = this.update_in(cx, |this, window, cx| {
                    this.link_progressed(event, window, cx)
                });
                if finished || shown.is_err() {
                    return;
                }
            }
        });
        self.linking = Some(Linking {
            link: Some(link),
            finished: false,
            pane: Some((view.clone(), view_events)),
            _receive: receive,
        });
        self.open_settings(&OpenSettings, window, cx);
        view.focus_handle(cx).focus(window, cx);
    }

    fn link_progressed(&mut self, event: LinkEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(linking) = &mut self.linking else {
            return;
        };
        let phase = match event {
            LinkEvent::AwaitingConfirmation => LinkPhase::Confirm,
            LinkEvent::Linked => {
                if let Some(devices) = &self.devices {
                    devices.update(cx, |devices, cx| devices.reload(cx));
                }
                LinkPhase::Linked
            }
            LinkEvent::Failed(message) => LinkPhase::Failed(message.into()),
        };
        if phase != LinkPhase::Confirm {
            linking.finished = true;
            linking.link = None;
        }
        match &linking.pane {
            Some((view, _)) => {
                view.update(cx, |view, cx| view.set_phase(phase, window, cx));
            }
            // The pane was closed after the user confirmed: say how it went.
            None => {
                self.linking = None;
                match phase {
                    LinkPhase::Linked => show_info("Device linked".into(), window, cx),
                    LinkPhase::Failed(message) => show_refusal(message.to_string(), window, cx),
                    _ => {}
                }
            }
        }
        cx.notify();
    }

    /// Asks before giving the new device the account, then lets it link.
    fn confirm_link(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = match name.trim() {
            "" => "Link this device?".to_string(),
            name => format!("Link {}?", crate::model::name_line(name)),
        };
        self.confirm(
            title,
            "It gets full access to your Air account: your chats, contacts and \
             devices. Only link a device you own."
                .into(),
            "Link",
            move |this, window, cx| {
                let Some(linking) = &mut this.linking else {
                    return;
                };
                let Some(link) = &mut linking.link else {
                    return;
                };
                if let Err(refusal) = link.confirm(&name) {
                    return show_refusal(refusal, window, cx);
                }
                linking.link = None;
                if let Some((view, _)) = &linking.pane {
                    view.update(cx, |view, cx| {
                        view.set_phase(LinkPhase::Linking, window, cx)
                    });
                }
            },
            window,
            cx,
        );
    }

    /// Hides the link pane. A link not confirmed yet (or ended) goes with
    /// it; a confirmed one runs on and reports how it went.
    fn close_link_pane(&mut self, cx: &mut Context<Self>) {
        let Some(linking) = &mut self.linking else {
            return;
        };
        if linking.link.is_some() || linking.finished {
            self.linking = None;
        } else {
            linking.pane = None;
        }
        cx.notify();
    }

    fn link_pane(&self) -> Option<&Entity<LinkView>> {
        self.linking.as_ref()?.pane.as_ref().map(|(view, _)| view)
    }

    fn rename_device(&mut self, input: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(devices) = self.devices.clone() else {
            return show_refusal("That needs an account".into(), window, cx);
        };
        let (tag, name) = devices::split_rename(input);
        if name.is_empty() {
            return show_refusal(":device-name needs a name".into(), window, cx);
        }
        let target = match devices.read(cx).resolve(tag.unwrap_or(""), false) {
            Ok(target) => target,
            Err(refusal) => return show_refusal(refusal, window, cx),
        };
        let name = name.to_string();
        self.contact_op(
            window,
            cx,
            move |_, cx| devices.read(cx).rename(&target, &name, cx),
            |(), _, _, _| {},
        );
    }

    fn unlink_device(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(devices) = self.devices.clone() else {
            return show_refusal("That needs an account".into(), window, cx);
        };
        let target = match devices.read(cx).resolve(query, true) {
            Ok(target) => target,
            Err(refusal) => return show_refusal(refusal, window, cx),
        };
        // Names are alike more often than not: the tag says which it is.
        let title = format!("Unlink {} (#{})?", target.name, target.tag);
        self.confirm(
            title,
            "It loses access to your account, and deletes its data the next \
             time it connects. To use it again, link it again."
                .into(),
            "Unlink",
            move |this, window, cx| {
                let (devices, target) = (devices.clone(), target.clone());
                let name = target.name.clone();
                this.contact_op(
                    window,
                    cx,
                    move |_, cx| devices.read(cx).unlink(&target, cx),
                    move |(), _, window, cx| show_info(format!("Unlinked {name}"), window, cx),
                );
            },
            window,
            cx,
        );
    }

    /// Closes the settings, back to the open chat if there is one.
    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_settings(window, cx);
        if self.has_open_chat(cx) {
            self.focus_messages(&FocusMessages, window, cx);
        } else {
            self.focus_chat_list(&FocusChatList, window, cx);
        }
    }

    /// Does what the settings screen asked for, the way the matching
    /// command does it.
    fn settings_changed(
        &mut self,
        event: SettingsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsEvent::Appearance(appearance) => self.set_appearance(appearance, window, cx),
            SettingsEvent::Theme(name) => {
                if let Err(err) = theme::set_theme(&name, window, cx) {
                    show_error(format!("Couldn't switch theme: {err:#}"), window, cx);
                }
            }
            SettingsEvent::Keymap(mode) => self.set_keymap_mode(mode, window, cx),
            SettingsEvent::Notifications(level) => {
                update_settings(window, cx, |settings| settings.notifications = level)
            }
            SettingsEvent::EditDisplayName(name) => {
                self.prompt(&format!("name {name}"), window, cx)
            }
            SettingsEvent::AddUsername => self.prompt("username ", window, cx),
            SettingsEvent::SetPicture => self.pick_picture(PictureOf::Own, window, cx),
            SettingsEvent::RemovePicture => self.remove_picture(PictureOf::Own, window, cx),
            SettingsEvent::RemoveUsername(username) => {
                let name = username.to_string();
                self.confirm(
                    format!("Remove the username {name}?"),
                    "Nobody can send you a new contact request through it. \
                     Your contacts stay."
                        .into(),
                    "Remove",
                    move |this, window, cx| {
                        let name = name.clone();
                        this.contact_op(
                            window,
                            cx,
                            move |session, cx| session.remove_username(&name, cx),
                            |(), _, window, cx| show_info("Username removed".into(), window, cx),
                        );
                    },
                    window,
                    cx,
                );
            }
            SettingsEvent::EditDeviceName { tag, name } => {
                self.prompt(&format!("device-name #{tag} {name}"), window, cx)
            }
            SettingsEvent::UnlinkDevice(tag) => self.unlink_device(&format!("#{tag}"), window, cx),
            SettingsEvent::LinkDevice => self.prompt("link ", window, cx),
            SettingsEvent::ReadReceipts(on) => {
                self.set_synced(Synced::ReadReceipts(on), window, cx)
            }
            SettingsEvent::DeleteExpiredRequests(on) => {
                self.set_synced(Synced::DeleteExpiredRequests(on), window, cx)
            }
            SettingsEvent::CopyInviteCodes(codes) => self.copy_invite_codes(codes, window, cx),
            SettingsEvent::GetInviteCode(id) => self.get_invite_code(id, window, cx),
            // Asked first: a code given out but not used yet is gone with it.
            SettingsEvent::RemoveUsedCodes => self.confirm(
                "Remove the codes you gave out?".into(),
                "They're gone from this list for good, also any not used yet. \
                 They still work for whoever has them."
                    .into(),
                "Remove",
                |this, window, cx| {
                    this.invitation_op(|invitations, cx| invitations.clear_copied(cx), window, cx)
                },
                window,
                cx,
            ),
            SettingsEvent::DeleteAccount => self.delete_account("", window, cx),
            SettingsEvent::Close => self.close_settings(window, cx),
        }
    }

    /// Changes a synced account setting.
    fn set_synced(&mut self, change: Synced, window: &mut Window, cx: &mut Context<Self>) {
        let Some(settings) = &self.account_settings else {
            return show_refusal("That needs an account".into(), window, cx);
        };
        let settings = settings.read(cx);
        let done = match change {
            Synced::ReadReceipts(on) => {
                futures_util::FutureExt::boxed_local(settings.set_read_receipts(on, cx))
            }
            Synced::DeleteExpiredRequests(on) => {
                futures_util::FutureExt::boxed_local(settings.set_delete_expired_requests(on, cx))
            }
        };
        cx.spawn_in(window, async move |_, cx| {
            if let Err(refusal) = done.await {
                _ = cx.update(|window, cx| show_refusal(refusal, window, cx));
            }
        })
        .detach();
    }

    /// Runs an invite-code operation, then shows the codes as they are.
    fn invitation_op<F>(
        &mut self,
        op: impl FnOnce(&Invitations, &App) -> F,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        F: Future<Output = Result<(), String>> + 'static,
    {
        let Some(invitations) = self.invitations.clone() else {
            return show_refusal("That needs an account".into(), window, cx);
        };
        let done = op(invitations.read(cx), cx);
        cx.spawn_in(window, async move |_, cx| {
            let result = done.await;
            _ = cx.update(|window, cx| {
                invitations.update(cx, |invitations, cx| invitations.reload(cx));
                if let Err(refusal) = result {
                    show_refusal(refusal, window, cx);
                }
            });
        })
        .detach();
    }

    /// Copies invite codes to give out, and marks them given out.
    fn copy_invite_codes(
        &mut self,
        codes: Vec<gpui::SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
            crate::model::invite_message(&codes),
        ));
        show_info("Copied to clipboard".into(), window, cx);
        self.invitation_op(
            move |invitations, cx| invitations.mark_copied(codes, cx),
            window,
            cx,
        );
    }

    /// Trades a token for an invite code.
    fn get_invite_code(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(invitations) = self.invitations.clone() else {
            return show_refusal("That needs an account".into(), window, cx);
        };
        let code = invitations.read(cx).request(id, cx);
        cx.spawn_in(window, async move |_, cx| {
            let code = code.await;
            _ = cx.update(|window, cx| {
                invitations.update(cx, |invitations, cx| invitations.reload(cx));
                match code {
                    Ok(code) => show_info(format!("New invite code {code}"), window, cx),
                    Err(refusal) => show_refusal(refusal, window, cx),
                }
            });
        })
        .detach();
    }

    /// Deletes the account once the user typed the word and confirmed:
    /// without it, asks for it on the command line, as upstream asks to
    /// type it.
    fn delete_account(&mut self, typed: &str, window: &mut Window, cx: &mut Context<Self>) {
        match typed.trim() {
            "" => return self.prompt("delete-account ", window, cx),
            DELETE_ACCOUNT_WORD => {}
            _ => {
                return show_refusal(
                    format!(
                        "Type {DELETE_ACCOUNT_WORD} to confirm: :delete-account {DELETE_ACCOUNT_WORD}"
                    ),
                    window,
                    cx,
                );
            }
        }
        self.confirm(
            "Delete your account?".into(),
            "This deletes your Air account and resets this app, and your linked \
             devices reset too. You can't recover the account."
                .into(),
            "Delete account",
            move |this, window, cx| {
                let Some(session) = this.session.clone() else {
                    return show_refusal("That needs an account".into(), window, cx);
                };
                show_info("Deleting your account…".into(), window, cx);
                let deletion = session.update(cx, |session, cx| session.delete_account(cx));
                cx.spawn_in(window, async move |_, cx| {
                    let outcome = deletion.await;
                    _ = cx.update(|window, cx| match outcome {
                        // The session reports it; this device resets.
                        Ok(AccountDeletion::Completed) => {}
                        Ok(AccountDeletion::Pending) => show_info(
                            "Your Air account is still being deleted. This device resets \
                             once the deletion is done."
                                .into(),
                            window,
                            cx,
                        ),
                        Err(refusal) => show_refusal(refusal, window, cx),
                    });
                })
                .detach();
            },
            window,
            cx,
        );
    }

    fn focus_next_pane(&mut self, _: &FocusNextPane, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat_list.focus_handle(cx).is_focused(window) && self.settings_open {
            self.settings_view.focus_handle(cx).focus(window, cx);
        } else if self.chat_list.focus_handle(cx).is_focused(window) {
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
        self.prompt("", window, cx);
    }

    /// Asks for a username to send a contact request to, on the command
    /// line, prefilled with the command.
    fn add_contact(&mut self, _: &AddContact, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt("contact ", window, cx);
    }

    /// Shows the safety code shared with the open chat's contact, or with
    /// the open group's member `query` names (asked for if empty).
    fn safety_code(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let list = self.chat_list.read(cx);
        let chat = list.selected_index().map(|ix| list.chats()[ix].clone());
        let (chat, target) = match safety_target(chat.as_ref(), query) {
            Ok(SafetyTarget::AskMember) => return self.prompt("safety-code ", window, cx),
            Ok(target) => (chat.expect("a target needs a chat"), target),
            Err(refusal) => return show_refusal(refusal.into(), window, cx),
        };
        let Some(session) = &self.session else {
            return show_refusal("That needs an account".into(), window, cx);
        };
        let (name, code) = match target {
            SafetyTarget::Member => {
                let Some((members, _)) = &self.members else {
                    return show_refusal("Still loading the members".into(), window, cx);
                };
                match members.read(cx).safety_code(query, cx) {
                    Ok((name, code)) => (name, futures_util::FutureExt::boxed_local(code)),
                    Err(refusal) => return show_refusal(refusal, window, cx),
                }
            }
            _ => {
                let code = session.read(cx).contact_safety_code(&chat.id, cx);
                (
                    chat.title.clone(),
                    futures_util::FutureExt::boxed_local(code),
                )
            }
        };
        self.safety_code = Some(cx.spawn_in(window, async move |this, cx| {
            let code = code.await;
            _ = this.update_in(cx, |this, window, cx| {
                this.safety_code = None;
                // The user moved on: a code for another chat would mislead.
                if this.chat_list.read(cx).selected() != Some(&chat.id) {
                    return;
                }
                match code {
                    Ok(lines) => this.show_safety_code(name, lines, window, cx),
                    Err(refusal) => show_refusal(refusal, window, cx),
                }
            });
        }));
    }

    fn show_safety_code(
        &self,
        name: gpui::SharedString,
        lines: SafetyLines,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous_focus = window.focused(cx);
        let text = lines.text();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let theme = cx.theme();
            let (text, previous_focus) = (text.clone(), previous_focus.clone());
            let code = v_flex()
                .gap_1()
                .py_2()
                .font_family(theme.mono_font_family.clone())
                .text_lg()
                .children(lines.0.iter().map(|line| div().child(line.clone())));
            alert
                .confirm()
                .title(format!("Safety code with {name}"))
                .description(
                    v_flex().gap_2().child(code).child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "Compare these digits with what {name} sees in Air, in \
                             person or on a call. If they match, your messages are \
                             end-to-end encrypted with them and nobody in between."
                            )),
                    ),
                )
                .ok_text("Copy")
                .ok_variant(ButtonVariant::Primary)
                .cancel_text("Close")
                .on_ok(move |_, window, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                    // Closed by hand, as in `confirm`: focus goes back.
                    window.close_dialog(cx);
                    if let Some(focus) = &previous_focus {
                        focus.focus(window, cx);
                    }
                    show_info("Safety code copied".into(), window, cx);
                    false
                })
        });
    }

    fn link_device_prompt(&mut self, _: &LinkDevice, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt("link ", window, cx);
    }

    fn new_group(&mut self, _: &NewGroup, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt("group ", window, cx);
    }

    fn invite_to_group(&mut self, _: &InviteToGroup, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_group(window, cx).is_some() {
            self.prompt("invite ", window, cx);
        }
    }

    /// Picks files to send to the open chat with the system's file dialog.
    fn attach_file(&mut self, _: &AttachFile, window: &mut Window, cx: &mut Context<Self>) {
        // The files go to the chat open when the dialog was: the dialog
        // isn't modal, and the open chat can change meanwhile.
        let Some(chat) = self.sendable_chat(window, cx) else {
            return;
        };
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Send".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let paths = match picked.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(_)) => {
                    _ = cx.update(|window, cx| {
                        show_refusal(
                            "Couldn't open a file dialog; use :attach".into(),
                            window,
                            cx,
                        )
                    });
                    return;
                }
            };
            _ = this.update_in(cx, |this, window, cx| {
                let list = this.chat_list.read(cx);
                let open = list.selected_index().map(|ix| list.chats()[ix].id.clone());
                let changed = match open {
                    Some(open) if open == chat => None,
                    Some(_) => Some("Another chat was opened meanwhile; nothing was sent"),
                    None => Some("The chat was closed; nothing was sent"),
                };
                if let Some(refusal) = changed {
                    show_refusal(refusal.into(), window, cx);
                    return;
                }
                // Still that chat; `sendable_chat` tells why if it can't
                // take files any more.
                if this.sendable_chat(window, cx).is_some() {
                    this.attach(chat, paths, window, cx)
                }
            });
        })
        .detach();
    }

    /// Picks an emoji by name and inserts it into the open chat's composer.
    fn insert_emoji(&mut self, _: &InsertEmoji, window: &mut Window, cx: &mut Context<Self>) {
        if self.sendable_chat(window, cx).is_none() {
            return;
        }
        let conversation = self.conversation.downgrade();
        let on_pick: crate::ui::emoji_picker::OnPick =
            std::rc::Rc::new(move |emoji, window, cx| {
                _ = conversation.update(cx, |conversation, cx| {
                    conversation.insert_emoji(emoji, window, cx)
                });
            });
        crate::ui::emoji_picker::open("Insert", on_pick, window, cx);
    }

    /// The open chat, if files and messages can go to it.
    fn sendable_chat(&self, window: &mut Window, cx: &mut App) -> Option<ChatId> {
        let list = self.chat_list.read(cx);
        let chat = list.selected_index().map(|ix| &list.chats()[ix]);
        match chat {
            Some(chat) if chat.composes() => Some(chat.id.clone()),
            Some(chat) if chat.blocked() => {
                show_refusal("Unblock them first: :unblock".into(), window, cx);
                None
            }
            Some(chat) if chat.ended() => {
                show_refusal("They ended this chat".into(), window, cx);
                None
            }
            Some(chat) if chat.group_ended => {
                show_refusal("You're no longer in this group".into(), window, cx);
                None
            }
            Some(_) => {
                show_refusal("Accept the contact request first".into(), window, cx);
                None
            }
            None => {
                show_refusal("Open a chat first".into(), window, cx);
                None
            }
        }
    }

    /// Sends each file to the open chat.
    fn attach(
        &mut self,
        chat: ChatId,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = &self.session else {
            show_refusal("That needs an account".into(), window, cx);
            return;
        };
        let session = session.clone();
        // One after another, so they arrive in the order given.
        cx.spawn_in(window, async move |_, cx| {
            for path in paths {
                let Ok(send) = cx.update(|_, cx| session.read(cx).send_attachment(&chat, path, cx))
                else {
                    return;
                };
                if let Err(refusal) = send.await {
                    _ = cx.update(|window, cx| show_refusal(refusal, window, cx));
                }
            }
        })
        .detach();
    }

    /// Files dropped on the chat: a drag released over the window by
    /// mistake mustn't send them, so ask first.
    fn attach_dropped(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        // The chat asked about is the one they go to, whatever is open
        // by the time the user answers.
        let Some(chat) = self.sendable_chat(window, cx) else {
            return;
        };
        const LISTED: usize = 5;
        let mut names: Vec<String> = paths
            .iter()
            .take(LISTED)
            .map(|path| {
                let name = path.file_name().unwrap_or(path.as_os_str());
                crate::model::preview_line(&name.to_string_lossy()).to_string()
            })
            .collect();
        if paths.len() > LISTED {
            names.push(format!("and {} more", paths.len() - LISTED));
        }
        let what = match paths.len() {
            1 => "1 file".to_string(),
            n => format!("{n} files"),
        };
        let title = format!("Send {what} to {}?", self.open_title(cx));
        let paths = std::cell::RefCell::new(Some(paths));
        self.confirm(
            title,
            names.join(", "),
            "Send",
            move |this, window, cx| {
                if let Some(paths) = paths.borrow_mut().take() {
                    this.attach(chat.clone(), paths, window, cx);
                }
            },
            window,
            cx,
        );
    }

    /// Opens the command line with `initial` typed.
    fn prompt(&mut self, initial: &str, window: &mut Window, cx: &mut Context<Self>) {
        let chats = self.chats(cx);
        let candidates = Candidates {
            themes: theme::available(cx),
            chats: chats.iter().map(|chat| chat.title.to_string()).collect(),
            contacts: chats
                .iter()
                .filter(|chat| !chat.is_group && !chat.is_self_chat && chat.composes())
                .map(|chat| chat.title.to_string())
                .collect(),
            members: self
                .members
                .as_ref()
                .map(|(members, _)| members.read(cx).candidates())
                .unwrap_or_default(),
            devices: self
                .devices
                .as_ref()
                .map(|devices| {
                    devices
                        .read(cx)
                        .devices()
                        .iter()
                        .filter(|device| !device.this_device)
                        .map(|device| {
                            (
                                format!("#{}", device.tag),
                                format!("{} #{}", device.name, device.tag),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        let previous_focus = window.focused(cx);
        let view = cx.new(|cx| CommandLine::new(candidates, initial, window, cx));
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
            Command::Notifications(level) => {
                update_settings(window, cx, |settings| settings.notifications = level);
                let what = match level {
                    Notifications::Full => "Notifications show who wrote and what",
                    Notifications::Private => "Notifications no longer show names or text",
                    Notifications::Off => "Notifications are off",
                };
                show_info(what.into(), window, cx);
            }
            Command::Shortcuts => palette::show_shortcuts(window, cx),
            Command::DisplayName(name) => self.contact_op(
                window,
                cx,
                |session, cx| session.set_display_name(&name, cx),
                |name, this, window, cx| {
                    if let Some(session) = &this.session {
                        session.update(cx, |session, cx| session.reload_profile(cx));
                    }
                    show_info(format!("Your name is now {name}"), window, cx)
                },
            ),
            Command::AddContact(username) => self.send_contact_request(username, window, cx),
            Command::Accept => self.accept_request(&AcceptRequest, window, cx),
            Command::Decline => self.decline_request(&DeclineRequest, window, cx),
            Command::AddUsername(username) => self.contact_op(
                window,
                cx,
                |session, cx| session.add_username(&username, cx),
                |username, _, window, cx| {
                    show_info(format!("Others can now add you as {username}"), window, cx)
                },
            ),
            Command::RemoveUsername(username) => self.contact_op(
                window,
                cx,
                |session, cx| session.remove_username(&username, cx),
                |(), _, window, cx| show_info("Username removed".into(), window, cx),
            ),
            Command::Group(title) => self.contact_op(
                window,
                cx,
                |session, cx| session.create_group(&title, cx),
                |chat, this, window, cx| this.open_when_listed(chat, window, cx),
            ),
            Command::Invite(query) => {
                let Some(chat) = self.open_group(window, cx) else {
                    return;
                };
                self.contact_op(
                    window,
                    cx,
                    |session, cx| session.invite(&chat, &query, cx),
                    |name, _, window, cx| show_info(format!("Invited {name}"), window, cx),
                );
            }
            Command::Kick(query) => {
                if self.open_group(window, cx).is_none() {
                    return;
                }
                let Some((members, _)) = &self.members else {
                    show_refusal("That needs an account".into(), window, cx);
                    return;
                };
                let members = members.clone();
                let removal = match members.read(cx).resolve(&query) {
                    Ok(removal) => removal,
                    Err(refusal) => return show_refusal(refusal, window, cx),
                };
                // Names are peer-chosen: the tag shows who it really is.
                let title = format!("Remove {} (#{})?", removal.name, removal.tag);
                let removal = std::cell::RefCell::new(Some(removal));
                self.confirm(
                    title,
                    "They stop getting the group's messages.".into(),
                    "Remove",
                    move |this, window, cx| {
                        let Some(removal) = removal.borrow_mut().take() else {
                            return;
                        };
                        let members = members.clone();
                        // Started only once `contact_op` lets it.
                        this.contact_op(
                            window,
                            cx,
                            move |_, cx| members.read(cx).remove(removal, cx),
                            |name, _, window, cx| show_info(format!("Removing {name}"), window, cx),
                        );
                    },
                    window,
                    cx,
                );
            }
            Command::ContactMember(query) => {
                if self.open_group(window, cx).is_none() {
                    return;
                }
                let Some((members, _)) = &self.members else {
                    show_refusal("That needs an account".into(), window, cx);
                    return;
                };
                let members = members.clone();
                let picked = match members.read(cx).pick(&query) {
                    Ok(picked) => picked,
                    Err(refusal) => return show_refusal(refusal, window, cx),
                };
                // Names are peer-chosen: the tag shows who it really is.
                let title = format!("Send {} (#{}) a contact request?", picked.name, picked.tag);
                let group = self.open_title(cx);
                let picked = std::cell::RefCell::new(Some(picked));
                self.confirm(
                    title,
                    format!("Through {group}. They can accept or decline it."),
                    "Send",
                    move |this, window, cx| {
                        let Some(picked) = picked.borrow_mut().take() else {
                            return;
                        };
                        let members = members.clone();
                        this.contact_op(
                            window,
                            cx,
                            move |_, cx| members.read(cx).add_contact(picked, cx),
                            |chat, this, window, cx| this.open_when_listed(chat, window, cx),
                        );
                    },
                    window,
                    cx,
                );
            }
            Command::Attach(path) => {
                let path = expand_home(&path);
                if let Some(chat) = self.sendable_chat(window, cx) {
                    self.attach(chat, vec![path], window, cx);
                }
            }
            Command::Picture(path) if path.is_empty() => {
                self.pick_picture(PictureOf::Own, window, cx)
            }
            Command::Picture(path) => {
                self.change_picture(PictureOf::Own, Some(expand_home(&path)), window, cx)
            }
            Command::RemovePicture => self.remove_picture(PictureOf::Own, window, cx),
            Command::GroupPicture(path) => {
                let Some(chat) = self.open_group(window, cx) else {
                    return;
                };
                if path.is_empty() {
                    self.pick_picture(PictureOf::Group(chat), window, cx)
                } else {
                    let path = Some(expand_home(&path));
                    self.change_picture(PictureOf::Group(chat), path, window, cx)
                }
            }
            Command::RemoveGroupPicture => {
                let Some(chat) = self.open_group(window, cx) else {
                    return;
                };
                self.remove_picture(PictureOf::Group(chat), window, cx)
            }
            Command::Rename(title) => {
                let Some(chat) = self.open_group(window, cx) else {
                    return;
                };
                self.contact_op(
                    window,
                    cx,
                    |session, cx| session.rename_group(&chat, &title, cx),
                    |(), _, _, _| {},
                );
            }
            Command::Leave => {
                let Some(chat) = self.open_group(window, cx) else {
                    return;
                };
                let name = self.open_title(cx);
                self.confirm(
                    format!("Leave {name}?"),
                    "You stop getting its messages. The chat stays in your list, \
                     ended, until you delete it."
                        .into(),
                    "Leave",
                    move |this, window, cx| {
                        let chat = chat.clone();
                        this.contact_op(
                            window,
                            cx,
                            |session, cx| session.leave_group(&chat, cx),
                            |(), _, _, _| {},
                        );
                    },
                    window,
                    cx,
                );
            }
            Command::Link(code) => self.start_link(&code, window, cx),
            Command::DeviceName(input) => self.rename_device(&input, window, cx),
            Command::Unlink(query) => self.unlink_device(&query, window, cx),
            Command::SafetyCode(query) => self.safety_code(&query, window, cx),
            Command::DeleteChat => self.contact_safety(ContactOp::Delete, "", window, cx),
            Command::Block => self.contact_safety(ContactOp::Block, "", window, cx),
            Command::Unblock => self.contact_safety(ContactOp::Unblock, "", window, cx),
            Command::Report(query) => self.contact_safety(ContactOp::Report, &query, window, cx),
            Command::Retract => self.contact_safety(ContactOp::Retract, "", window, cx),
            Command::DeleteAccount(typed) => self.delete_account(&typed, window, cx),
            Command::ReadReceipts(on) => self.set_synced(Synced::ReadReceipts(on), window, cx),
            Command::DeleteExpiredRequests(on) => {
                self.set_synced(Synced::DeleteExpiredRequests(on), window, cx)
            }
            Command::Mute(arg) => self.mute(MuteChange::Mute(&arg), window, cx),
            Command::Unmute => self.mute(MuteChange::Unmute, window, cx),
        }
    }

    /// Mutes or unmutes the open chat's alerts, and says what it did.
    fn mute(&mut self, change: MuteChange, window: &mut Window, cx: &mut Context<Self>) {
        let list = self.chat_list.read(cx);
        let Some(chat) = list.selected_index().map(|ix| list.chats()[ix].clone()) else {
            return show_refusal("Open a chat first".into(), window, cx);
        };
        let now = chrono::Utc::now();
        let mute = match change {
            MuteChange::Toggle if chat.is_muted(now) => None,
            MuteChange::Toggle => Some(Mute::Forever),
            MuteChange::Unmute if !chat.is_muted(now) => {
                return show_refusal("It isn't muted".into(), window, cx);
            }
            MuteChange::Unmute => None,
            MuteChange::Mute(arg) => match Mute::parse(arg, chrono::Local::now()) {
                Ok(mute) => Some(mute),
                Err(refusal) => return show_refusal(refusal.into(), window, cx),
            },
        };
        let Some(session) = &self.session else {
            return show_refusal("That needs an account".into(), window, cx);
        };
        let done = session.read(cx).set_muted(&chat.id, mute, cx);
        cx.spawn_in(window, async move |_, cx| {
            let shown = done.await.map(|()| match mute {
                Some(mute) => mute.label(chrono::Utc::now()),
                None => "Unmuted".to_string(),
            });
            _ = cx.update(|window, cx| match shown {
                Ok(info) => show_info(info, window, cx),
                Err(refusal) => show_refusal(refusal, window, cx),
            });
        })
        .detach();
    }

    /// Runs `op` in the open chat once the user confirms: for a contact (or
    /// request), or in a group deleting it or reporting the member `query`
    /// names.
    fn contact_safety(
        &mut self,
        op: ContactOp,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let list = self.chat_list.read(cx);
        let Some(chat) = list.selected_index().map(|ix| list.chats()[ix].clone()) else {
            return show_refusal("Open a chat first".into(), window, cx);
        };
        if chat.is_self_chat {
            return show_refusal("That's your own devices".into(), window, cx);
        }
        if chat.is_group {
            return match op {
                ContactOp::Delete => self.delete_group(chat, window, cx),
                ContactOp::Report => self.report_member(query, window, cx),
                _ => show_refusal("That's for a contact's chat".into(), window, cx),
            };
        }
        if !query.trim().is_empty() {
            return show_refusal(
                "Only a group's members are named: here it's the contact".into(),
                window,
                cx,
            );
        }
        let (title, description, ok) = match contact_confirmation(op, &chat) {
            Ok(confirmation) => confirmation,
            Err(refusal) => return show_refusal(refusal.into(), window, cx),
        };
        let variant = match op {
            ContactOp::Unblock => ButtonVariant::Primary,
            _ => ButtonVariant::Danger,
        };
        let id = chat.id;
        self.confirm_as(
            title,
            description,
            ok,
            variant,
            move |this, window, cx| {
                let id = id.clone();
                this.contact_op(
                    window,
                    cx,
                    move |session, cx| session.contact_op(op, &id, cx),
                    // A block's lost composer is handled with the
                    // chat's update (`Conversation::update_chat`).
                    move |(), _, window, cx| {
                        if op == ContactOp::Report {
                            show_info("Spam reported".into(), window, cx)
                        }
                    },
                );
            },
            window,
            cx,
        );
    }

    fn delete_group(&mut self, chat: ChatSummary, window: &mut Window, cx: &mut Context<Self>) {
        let id = chat.id;
        self.confirm(
            format!("Delete {}?", chat.title),
            "If the group is still going, this ends it for everyone in it. \
             It is then erased from all your devices. This can't be undone."
                .into(),
            "Delete",
            move |this, window, cx| {
                let id = id.clone();
                this.contact_op(
                    window,
                    cx,
                    |session, cx| session.delete_group(&id, cx),
                    |(), _, _, _| {},
                );
            },
            window,
            cx,
        );
    }

    /// Reports the open group's member `query` names (asked for if empty).
    fn report_member(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        if query.trim().is_empty() {
            return self.prompt("report ", window, cx);
        }
        let Some((members, _)) = &self.members else {
            return show_refusal("That needs an account".into(), window, cx);
        };
        let members = members.clone();
        let suspect = match members.read(cx).suspect(query) {
            Ok(suspect) => suspect,
            Err(refusal) => return show_refusal(refusal, window, cx),
        };
        // Names are peer-chosen: the tag shows who it really is.
        let title = format!("Report {} for spam?", suspect.label);
        let suspect = std::cell::RefCell::new(Some(suspect));
        self.confirm(
            title,
            REPORT_SPAM.into(),
            "Report spam",
            move |this, window, cx| {
                let Some(suspect) = suspect.borrow_mut().take() else {
                    return;
                };
                let members = members.clone();
                this.contact_op(
                    window,
                    cx,
                    move |_, cx| members.read(cx).report_spam(suspect, cx),
                    |(), _, window, cx| show_info("Spam reported".into(), window, cx),
                );
            },
            window,
            cx,
        );
    }

    /// The open chat's title (peer-chosen, one bounded line).
    fn open_title(&self, cx: &App) -> gpui::SharedString {
        let list = self.chat_list.read(cx);
        list.selected_index()
            .map(|ix| list.chats()[ix].title.clone())
            .unwrap_or_default()
    }

    /// The open chat, if it is a group; tells the user otherwise.
    fn open_group(&self, window: &mut Window, cx: &mut App) -> Option<ChatId> {
        let list = self.chat_list.read(cx);
        let chat = list.selected_index().map(|ix| &list.chats()[ix]);
        match chat {
            Some(chat) if chat.is_group => Some(chat.id.clone()),
            _ => {
                show_refusal("Open a group first".into(), window, cx);
                None
            }
        }
    }

    /// Asks before something that can't be undone. Dialogs render outside
    /// the workspace, so `run` reaches it through a weak handle.
    fn confirm(
        &self,
        title: String,
        description: String,
        ok: &'static str,
        run: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.confirm_as(
            title,
            description,
            ok,
            ButtonVariant::Danger,
            run,
            window,
            cx,
        );
    }

    /// [`Self::confirm`], with the confirming button's look.
    #[allow(clippy::too_many_arguments)]
    fn confirm_as(
        &self,
        title: String,
        description: String,
        ok: &'static str,
        variant: ButtonVariant,
        run: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.weak_entity();
        crate::ui::confirm(
            title,
            description,
            ok,
            variant,
            move |window, cx| _ = this.update(cx, |this, cx| run(this, window, cx)),
            window,
            cx,
        );
    }

    fn send_contact_request(
        &mut self,
        username: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.contact_op(
            window,
            cx,
            |session, cx| session.add_contact(&username, cx),
            |chat, this, window, cx| this.open_when_listed(chat, window, cx),
        );
    }

    /// The open chat, if it shows an incoming contact request.
    fn open_request(&self, cx: &App) -> Option<ChatId> {
        let list = self.chat_list.read(cx);
        let chat = &list.chats()[list.selected_index()?];
        (chat.request == Some(Request::Incoming)).then(|| chat.id.clone())
    }

    fn accept_request(&mut self, _: &AcceptRequest, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.open_request(cx) else {
            cx.propagate();
            return;
        };
        self.contact_op(
            window,
            cx,
            |session, cx| session.accept_request(&chat, cx),
            // The contact's chat can be another than the request's.
            |chat, this, window, cx| this.open_when_listed(chat, window, cx),
        );
    }

    fn decline_request(&mut self, _: &DeclineRequest, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chat) = self.open_request(cx) else {
            cx.propagate();
            return;
        };
        self.contact_op(
            window,
            cx,
            |session, cx| session.decline_request(&chat, cx),
            |(), _, _, _| {},
        );
    }

    /// Runs one contact operation at a time, telling the user why it failed.
    fn contact_op<T: 'static, F>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        start: impl FnOnce(&Session, &App) -> F,
        done: impl FnOnce(T, &mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) where
        F: Future<Output = Result<T, String>> + 'static,
    {
        let Some(session) = &self.session else {
            show_refusal("That needs an account".into(), window, cx);
            return;
        };
        if self.contact_task.is_some() {
            show_refusal("Still working on the previous request".into(), window, cx);
            return;
        }
        let operation = start(session.read(cx), cx);
        self.contact_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = operation.await;
            _ = this.update_in(cx, |this, window, cx| {
                this.contact_task = None;
                match result {
                    Ok(value) => done(value, this, window, cx),
                    Err(message) => show_refusal(message, window, cx),
                }
            });
        }));
    }

    /// Sets (`Some` path) or removes a picture: the user's or a group's.
    fn change_picture(
        &mut self,
        of: PictureOf,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let done = match (&of, path.is_some()) {
            (PictureOf::Own, true) => "Your picture is changed",
            (PictureOf::Own, false) => "Your picture is removed",
            (PictureOf::Group(_), true) => "The group's picture is changed",
            (PictureOf::Group(_), false) => "The group's picture is removed",
        };
        match of {
            PictureOf::Own => self.contact_op(
                window,
                cx,
                |session, cx| session.set_picture(path, cx),
                move |(), this, window, cx| {
                    // The core may not report its own change.
                    if let Some(session) = &this.session {
                        session.update(cx, |session, cx| session.reload_profile(cx));
                    }
                    show_info(done.into(), window, cx)
                },
            ),
            PictureOf::Group(chat) => self.contact_op(
                window,
                cx,
                |session, cx| session.set_group_picture(&chat, path, cx),
                move |(), _, window, cx| show_info(done.into(), window, cx),
            ),
        }
    }

    /// Removes a picture once confirmed, however asked (command or row).
    fn remove_picture(&mut self, of: PictureOf, window: &mut Window, cx: &mut Context<Self>) {
        let (title, description) = match of {
            PictureOf::Own => (
                "Remove your picture?",
                "Your contacts see your initials instead.",
            ),
            PictureOf::Group(_) => (
                "Remove the group's picture?",
                "It goes for everyone in the group.",
            ),
        };
        self.confirm(
            title.into(),
            description.into(),
            "Remove",
            move |this, window, cx| this.change_picture(of.clone(), None, window, cx),
            window,
            cx,
        )
    }

    /// Picks a picture with the system's file dialog, then sets it. A
    /// group's goes to the group open when the dialog was, if still open.
    fn pick_picture(&mut self, of: PictureOf, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(_)) => {
                    _ = cx.update(|window, cx| {
                        show_refusal(
                            "Couldn't open a file dialog; give a path: :picture <path>".into(),
                            window,
                            cx,
                        )
                    });
                    None
                }
            };
            let Some(path) = path else {
                return;
            };
            _ = this.update_in(cx, |this, window, cx| {
                if let PictureOf::Group(chat) = &of {
                    let list = this.chat_list.read(cx);
                    let open = list.selected_index().map(|ix| &list.chats()[ix].id);
                    if open != Some(chat) {
                        let refusal = "Another chat was opened meanwhile; nothing was changed";
                        show_refusal(refusal.into(), window, cx);
                        return;
                    }
                }
                this.change_picture(of, Some(path), window, cx)
            });
        })
        .detach();
    }

    /// Opens `chat` now, or once the chat list has loaded it.
    fn open_when_listed(&mut self, chat: ChatId, window: &mut Window, cx: &mut Context<Self>) {
        self.open_then_focus(chat, Self::focus_composer_now, window, cx);
    }

    /// Opens `chat` and focuses it with `focus`, now or once it is listed.
    /// Until then the chat list has focus: typing mustn't land in the
    /// composer of the chat open before.
    fn open_then_focus(
        &mut self,
        chat: ChatId,
        focus: FocusChat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let listed = self.chat_list.read(cx).chat(&chat).is_some();
        if listed {
            self.focus_when_opened = None;
        } else {
            self.focus_when_opened = Some((chat.clone(), focus));
        }
        self.chat_list
            .update(cx, |list, cx| list.open_when_listed(chat, cx));
        if listed {
            focus(self, window, cx);
        } else {
            self.focus_chat_list(&FocusChatList, window, cx);
        }
    }

    fn focus_composer_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_composer(&FocusComposer, window, cx);
    }

    /// As after a jump: the message list in Helix mode, else the composer.
    fn focus_jumped(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if Self::helix(cx) {
            self.focus_messages(&FocusMessages, window, cx);
        } else {
            self.focus_composer(&FocusComposer, window, cx);
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

/// Whose picture a command changes.
#[derive(Clone)]
enum PictureOf {
    Own,
    Group(ChatId),
}

/// A typed path with a leading `~` meaning the home directory.
fn expand_home(path: &str) -> PathBuf {
    let home = || directories::UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => match home() {
            Some(home) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(path),
        },
        _ => PathBuf::from(path),
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
                picture: session.picture().cloned(),
                status: session.status(),
                version_expires: session.version_expires(),
                usernames: session.usernames().to_vec(),
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
            .on_action(cx.listener(Self::add_contact))
            .on_action(cx.listener(Self::new_group))
            .on_action(cx.listener(Self::link_device_prompt))
            .on_action(
                cx.listener(|this, _: &ShowSafetyCode, window, cx| {
                    this.safety_code("", window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &BlockContact, window, cx| {
                this.contact_safety(ContactOp::Block, "", window, cx)
            }))
            .on_action(cx.listener(|this, _: &UnblockContact, window, cx| {
                this.contact_safety(ContactOp::Unblock, "", window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReportSpam, window, cx| {
                this.contact_safety(ContactOp::Report, "", window, cx)
            }))
            .on_action(cx.listener(|this, _: &RetractRequest, window, cx| {
                this.contact_safety(ContactOp::Retract, "", window, cx)
            }))
            .on_action(cx.listener(|this, _: &DeleteChat, window, cx| {
                this.contact_safety(ContactOp::Delete, "", window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleMute, window, cx| {
                this.mute(MuteChange::Toggle, window, cx)
            }))
            .on_action(cx.listener(Self::attach_file))
            .on_action(cx.listener(Self::insert_emoji))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::invite_to_group))
            .on_action(cx.listener(Self::accept_request))
            .on_action(cx.listener(Self::decline_request))
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
                            .child(resizable_panel().child(if self.settings_open {
                                match self.link_pane() {
                                    Some(view) => view.clone().into_any_element(),
                                    None => self.settings_view.clone().into_any_element(),
                                }
                            } else {
                                self.conversation.clone().into_any_element()
                            }))
                            .child(
                                resizable_panel()
                                    .size(px(300.))
                                    .size_range(px(240.)..px(420.))
                                    .visible(
                                        settings.details_open
                                            && open_chat.is_some()
                                            && !self.settings_open,
                                    )
                                    .child(details_pane(
                                        open_chat.as_ref(),
                                        self.members
                                            .as_ref()
                                            .map(|(members, _)| members.read(cx))
                                            .filter(|members| members.is_loaded())
                                            .map(Members::members),
                                        cx,
                                    )),
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
            workspace.conversation.read(cx).message_count(cx)
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
    fn composer_keys_fall_through_when_nothing_is_drafted(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let focused = focus_probe(&workspace, cx);
        cx.simulate_keystrokes("j enter");
        // Up moves within a message being written, rather than editing.
        cx.simulate_input("ab");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("c");
        cx.simulate_keystrokes("up");
        cx.simulate_input("X");
        assert_eq!(composer_text(&workspace, cx), "aXb\nc");
        // Without a reply or edit to drop, escape leaves the composer.
        cx.simulate_keystrokes("escape");
        assert_eq!(focused(cx), Focus::ChatList);

        // Each chat has its own composer text.
        cx.simulate_keystrokes("j");
        assert_eq!(composer_text(&workspace, cx), "");
    }

    #[gpui::test]
    fn a_contact_request_takes_no_typing_until_accepted(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let focused = focus_probe(&workspace, cx);
        let chat_list = workspace.read_with(cx, |workspace, _| workspace.chat_list.clone());
        chat_list.update(cx, |list, cx| {
            let mut chats = list.chats().to_vec();
            chats[0].request = Some(Request::Incoming);
            list.set_chats(chats, LoadState::Loaded, cx);
        });
        cx.simulate_keystrokes("j enter");
        // Nothing can be sent to a stranger: the conversation keeps focus
        // and typing goes nowhere.
        assert_eq!(focused(cx), Focus::Messages);
        cx.simulate_input("hi");
        assert_eq!(composer_text(&workspace, cx), "");

        // Once accepted, the composer is back.
        chat_list.update(cx, |list, cx| {
            let mut chats = list.chats().to_vec();
            chats[0].request = None;
            list.set_chats(chats, LoadState::Loaded, cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape enter");
        cx.simulate_input("hi");
        assert_eq!(composer_text(&workspace, cx), "hi");
    }

    #[gpui::test]
    fn a_blocked_contact_takes_no_typing_and_nothing_runs_unasked(cx: &mut TestAppContext) {
        use crate::model::{OneToOne, Stage};
        let (workspace, cx) = setup(cx);
        let focused = focus_probe(&workspace, cx);
        let chat_list = workspace.read_with(cx, |workspace, _| workspace.chat_list.clone());
        let set_first = |request, facts, cx: &mut VisualTestContext| {
            chat_list.update(cx, |list, cx| {
                let mut chats = list.chats().to_vec();
                assert!(!chats[0].is_group);
                chats[0].request = request;
                chats[0].one_to_one = Some(facts);
                list.set_chats(chats, LoadState::Loaded, cx);
            });
            cx.run_until_parked();
        };
        let blocked = OneToOne {
            stage: Stage::Blocked,
            ..OneToOne::CONTACT
        };
        // Blocked (say from another device) while writing: the keyboard
        // moves to the messages, not into a composer that is gone.
        cx.simulate_keystrokes("j enter");
        assert_eq!(focused(cx), Focus::Composer);
        set_first(None, blocked, cx);
        assert_eq!(focused(cx), Focus::Messages);
        // Nothing reaches a blocked contact: no typing, replying or editing.
        cx.simulate_input("hi");
        cx.simulate_keystrokes("up r");
        assert_eq!(focused(cx), Focus::Messages);
        assert_eq!(composer_text(&workspace, cx), "");

        let asking = |cx: &mut VisualTestContext| cx.update(|w, cx| w.has_active_dialog(cx));
        let told = |cx: &mut VisualTestContext| cx.update(|w, cx| w.notifications(cx).len());
        let ask = |op, query: &'static str, cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.contact_safety(op, query, window, cx)
            });
            cx.run_until_parked();
        };
        // What can't apply is refused at once, without a dialog.
        let before = told(cx);
        ask(ContactOp::Block, "", cx);
        ask(ContactOp::Retract, "", cx);
        ask(ContactOp::Report, "grace", cx);
        assert_eq!(told(cx), before + 3);
        assert!(!asking(cx));
        // Unblocking asks first; Escape runs nothing.
        ask(ContactOp::Unblock, "", cx);
        assert!(asking(cx));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!asking(cx));
        assert_eq!(told(cx), before + 3);
        assert_eq!(focused(cx), Focus::Messages);

        // Should a blocked request's chat be left (the core erases it when
        // blocking), its sender can be unblocked there too.
        let blocked_request = OneToOne {
            side: crate::model::Side::Incoming,
            ..blocked
        };
        set_first(Some(Request::Closed), blocked_request, cx);
        ask(ContactOp::Unblock, "", cx);
        assert!(asking(cx));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(told(cx), before + 3);
        // A contact who deleted the chat can't be written to either.
        set_first(None, OneToOne::CONTACT, cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.focus_composer(&FocusComposer, window, cx)
        });
        assert_eq!(focused(cx), Focus::Composer);
        let ended = OneToOne {
            stage: Stage::Ended,
            ..OneToOne::CONTACT
        };
        set_first(None, ended, cx);
        assert_eq!(focused(cx), Focus::Messages);

        // Likewise a group the user is out of: no composer, and nothing
        // edited or replied to from the messages.
        let group = workspace.read_with(cx, |workspace, cx| {
            let chats = workspace.chat_list.read(cx).chats();
            chats.iter().position(|chat| chat.is_group).unwrap()
        });
        let set_group_ended = |ended, cx: &mut VisualTestContext| {
            chat_list.update(cx, |list, cx| {
                let mut chats = list.chats().to_vec();
                chats[group].group_ended = ended;
                let id = chats[group].id.clone();
                list.set_chats(chats, LoadState::Loaded, cx);
                list.open(id, cx);
            });
            cx.run_until_parked();
        };
        set_group_ended(false, cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.focus_composer(&FocusComposer, window, cx)
        });
        assert_eq!(focused(cx), Focus::Composer);
        set_group_ended(true, cx);
        assert_eq!(focused(cx), Focus::Messages);
        cx.simulate_input("hi");
        cx.simulate_keystrokes("up e r");
        assert_eq!(focused(cx), Focus::Messages);
        assert_eq!(composer_text(&workspace, cx), "");
    }

    #[gpui::test]
    fn deleting_a_message_asks_and_refuses_what_cannot_apply(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let asking = |cx: &mut VisualTestContext| cx.update(|w, cx| w.has_active_dialog(cx));
        let told = |cx: &mut VisualTestContext| cx.update(|w, cx| w.notifications(cx).len());
        let escape = |cx: &mut VisualTestContext| {
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            assert!(!asking(cx));
        };
        cx.simulate_keystrokes(&format!("j enter {MOD}-m home"));
        assert_eq!(cursor(&workspace, cx), Some(0));
        let count = message_count(&workspace, cx);
        let before = told(cx);
        // A peer's message: only the sender deletes it for everyone.
        cx.simulate_keystrokes("shift-d");
        cx.run_until_parked();
        assert!(!asking(cx));
        assert_eq!(told(cx), before + 1);
        // For the user it asks first; Escape deletes nothing.
        cx.simulate_keystrokes("d");
        cx.run_until_parked();
        assert!(asking(cx));
        escape(cx);
        assert_eq!(told(cx), before + 1);
        // The user's own message, for everyone: asked, then run (the
        // fixed sample has no account to run it on, which it says).
        cx.simulate_keystrokes("down shift-d");
        cx.run_until_parked();
        assert!(asking(cx));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!asking(cx));
        assert_eq!(told(cx), before + 2);
        assert_eq!(message_count(&workspace, cx), count);
    }

    #[gpui::test]
    fn deleting_the_account_takes_the_typed_word_then_a_confirmation(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let asking = |cx: &mut VisualTestContext| cx.update(|w, cx| w.has_active_dialog(cx));
        let told = |cx: &mut VisualTestContext| cx.update(|w, cx| w.notifications(cx).len());
        let run = |command: &str, cx: &mut VisualTestContext| {
            let command = command_line::parse(command).unwrap();
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.run(command, window, cx)
            });
            cx.run_until_parked();
        };
        let before = told(cx);
        // Nothing typed: it asks for the word on the command line.
        run(":delete-account", cx);
        assert!(!asking(cx));
        assert!(workspace.read_with(cx, |workspace, _| workspace.command_line.is_some()));
        // The wrong word is refused at once.
        run(":delete-account yes", cx);
        assert!(!asking(cx));
        assert_eq!(told(cx), before + 1);
        // The word asks to confirm; Escape deletes nothing.
        run(":delete-account delete", cx);
        assert!(asking(cx));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!asking(cx));
        assert_eq!(told(cx), before + 1);
    }

    #[gpui::test]
    fn removals_from_the_settings_ask_first(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let asking = |cx: &mut VisualTestContext| cx.update(|w, cx| w.has_active_dialog(cx));
        let told = |cx: &mut VisualTestContext| cx.update(|w, cx| w.notifications(cx).len());
        let remove = |event: SettingsEvent, cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.settings_changed(event, window, cx)
            });
            cx.run_until_parked();
        };
        // Given-out codes, the user's picture.
        for event in [SettingsEvent::RemoveUsedCodes, SettingsEvent::RemovePicture] {
            let before = told(cx);
            // Escape removes nothing. (Without an account, reaching the
            // removal is told as a refusal: that's what shows it ran.)
            remove(event.clone(), cx);
            assert!(asking(cx), "{event:?}");
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            assert!(!asking(cx));
            assert_eq!(told(cx), before, "{event:?}");
            // Confirmed, it runs.
            remove(event.clone(), cx);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            assert!(!asking(cx));
            assert_eq!(told(cx), before + 1, "{event:?}");
        }
        // Typed, the same: it asks (the group's too, for everyone in it).
        let run = |command: &str, cx: &mut VisualTestContext| {
            let command = command_line::parse(command).unwrap();
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.run(command, window, cx)
            });
            cx.run_until_parked();
        };
        run(":remove-picture", cx);
        assert!(asking(cx));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let chat_list = workspace.read_with(cx, |workspace, _| workspace.chat_list.clone());
        chat_list.update(cx, |list, cx| {
            let group = list.chats().iter().find(|chat| chat.is_group).unwrap();
            list.open(group.id.clone(), cx);
        });
        cx.run_until_parked();
        let before = told(cx);
        run(":remove-group-picture", cx);
        assert!(asking(cx));
        assert_eq!(told(cx), before);
    }

    #[gpui::test]
    fn emoji_come_from_shortcodes_and_the_picker(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.simulate_keystrokes("j enter");
        // A closed shortcode turns into its emoji, and typing goes on
        // after it.
        cx.simulate_input("hi :smile:");
        cx.simulate_input("!");
        assert_eq!(composer_text(&workspace, cx), "hi 😄!");
        // Not after a space: left as typed.
        cx.simulate_input(" 10:30:");
        assert_eq!(composer_text(&workspace, cx), "hi 😄! 10:30:");
        // The picker finds one by shortcode and inserts it at the cursor.
        cx.simulate_keystrokes(&format!("{MOD}-;"));
        cx.run_until_parked();
        assert!(cx.update(|w, cx| w.has_active_dialog(cx)));
        cx.simulate_input("+1");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!cx.update(|w, cx| w.has_active_dialog(cx)));
        assert_eq!(composer_text(&workspace, cx), "hi 😄! 10:30:👍");
        // Back in the composer, typing goes on.
        cx.simulate_input("x");
        assert_eq!(composer_text(&workspace, cx), "hi 😄! 10:30:👍x");
    }

    #[gpui::test]
    fn shortcodes_expand_only_as_typed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.simulate_keystrokes("j enter");
        // Text set without typing (a restored draft, an edit) still
        // expands when its closing colon is typed.
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.conversation.update(cx, |conversation, cx| {
                conversation.set_composer_text("see you :wave", window, cx)
            })
        });
        cx.simulate_keystrokes("end");
        cx.simulate_input(":");
        assert_eq!(composer_text(&workspace, cx), "see you 👋");
        // Undo brings the shortcode back, and it stays as it is.
        cx.simulate_keystrokes(&format!("{MOD}-z"));
        cx.run_until_parked();
        assert_eq!(composer_text(&workspace, cx), "see you :wave:");
    }

    #[gpui::test]
    fn retry_and_view_refuse_what_they_cannot_apply_to(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.simulate_keystrokes(&format!("j enter {MOD}-m home"));
        assert!(cursor(&workspace, cx).is_some());
        // A text message: nothing to send again, nothing to view.
        cx.simulate_keystrokes("shift-r");
        cx.run_until_parked();
        assert_eq!(
            crate::ui::last_told().as_deref(),
            Some("Only a file that failed to send can be sent again")
        );
        cx.simulate_keystrokes("v");
        cx.run_until_parked();
        assert_eq!(
            crate::ui::last_told().as_deref(),
            Some("That message has no image")
        );
        assert!(!cx.update(|w, cx| w.has_active_dialog(cx)));
    }

    #[gpui::test]
    fn any_emoji_reacts_from_the_reaction_bar(cx: &mut TestAppContext) {
        for helix in [false, true] {
            let (workspace, cx) = setup(cx);
            if helix {
                enable_helix(cx);
            }
            cx.simulate_keystrokes(&format!("j enter {MOD}-m home"));
            assert!(cursor(&workspace, cx).is_some());
            // `/` in the reaction bar finds any emoji (in Helix mode too,
            // where `/` elsewhere finds a chat); picking one reacts.
            cx.simulate_keystrokes("+ /");
            cx.run_until_parked();
            assert!(cx.update(|w, cx| w.has_active_dialog(cx)), "helix: {helix}");
            cx.simulate_input("+1");
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            assert!(!cx.update(|w, cx| w.has_active_dialog(cx)));
            // Mock messages have no account: the reaction was asked for.
            assert_eq!(
                crate::ui::last_told().as_deref(),
                Some("Reactions need an account"),
                "helix: {helix}"
            );
        }
    }

    #[gpui::test]
    fn helix_i_still_inserts_from_the_messages(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let focused = focus_probe(&workspace, cx);
        enable_helix(cx);
        cx.simulate_keystrokes(&format!("j enter {MOD}-m k"));
        assert_eq!(focused(cx), Focus::Messages);
        assert!(cursor(&workspace, cx).is_some());
        cx.simulate_keystrokes("i");
        cx.run_until_parked();
        assert_eq!(focused(cx), Focus::Composer);
        assert!(!cx.update(|w, cx| w.has_active_dialog(cx)));
    }

    #[gpui::test]
    fn request_keys_leave_a_normal_chat_alone(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let focused = focus_probe(&workspace, cx);
        cx.simulate_keystrokes("j enter");
        let title = open_title(&workspace, cx);
        let count = message_count(&workspace, cx);
        // Without a request to act on, ctrl-enter reaches the composer's
        // own binding and sends; declining does nothing.
        cx.simulate_input("ab");
        cx.simulate_keystrokes("secondary-enter");
        assert_eq!(message_count(&workspace, cx), count + 1);
        cx.simulate_input("cd");
        cx.simulate_keystrokes("secondary-shift-backspace");
        assert_eq!(composer_text(&workspace, cx), "cd");
        assert_eq!(focused(cx), Focus::Composer);
        assert_eq!(open_title(&workspace, cx), title);
    }

    #[gpui::test]
    fn a_new_contact_chat_opens_once_it_is_listed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let chat_list = workspace.read_with(cx, |workspace, _| workspace.chat_list.clone());
        let mut chats = chat_list.read_with(cx, |list, _| list.chats().to_vec());
        let mut new = chats[0].clone();
        new.id = ChatId("new".into());
        new.title = "New contact".into();
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_when_listed(new.id.clone(), window, cx)
        });
        cx.run_until_parked();
        assert_ne!(open_title(&workspace, cx).as_deref(), Some("New contact"));

        // The list loads it later, with other changes in between.
        chat_list.update(cx, |list, cx| {
            list.set_chats(chats.clone(), LoadState::Loaded, cx)
        });
        chats.insert(0, new.clone());
        chat_list.update(cx, |list, cx| {
            list.set_chats(chats.clone(), LoadState::Loaded, cx)
        });
        cx.run_until_parked();
        assert_eq!(open_title(&workspace, cx).as_deref(), Some("New contact"));

        // Moving on before it is listed means it no longer takes over.
        let mut later = new;
        later.id = ChatId("later".into());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_when_listed(later.id.clone(), window, cx)
        });
        cx.simulate_keystrokes("alt-down");
        let moved_to = open_title(&workspace, cx);
        chats.push(later);
        chat_list.update(cx, |list, cx| list.set_chats(chats, LoadState::Loaded, cx));
        cx.run_until_parked();
        assert_eq!(open_title(&workspace, cx), moved_to);
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
    fn leaving_or_deleting_a_group_asks_first(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        enable_helix(cx);
        let focus = focus_probe(&workspace, cx);
        let run = |line: &str, cx: &mut VisualTestContext| {
            cx.simulate_keystrokes(":");
            cx.run_until_parked();
            cx.simulate_input(line);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
        };
        let asking =
            |cx: &mut VisualTestContext| cx.update(|window, cx| window.has_active_dialog(cx));
        // Not in a one-to-one chat: refused before anything is asked.
        run("o grace", cx);
        run("leave", cx);
        assert!(!asking(cx));
        run("o design", cx);
        let told =
            |cx: &mut VisualTestContext| cx.update(|window, cx| window.notifications(cx).len());
        for command in ["leave", "delete-chat"] {
            run(command, cx);
            assert!(asking(cx), "{command} didn't ask");
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            assert!(!asking(cx));
            // Confirmed, it runs: without an account, it is refused.
            run(command, cx);
            let before = told(cx);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
            assert!(!asking(cx));
            assert_eq!(told(cx), before + 1, "{command} didn't run");
            // Focus is back where the command was typed.
            assert_eq!(focus(cx), Focus::Messages);
        }
    }

    #[gpui::test]
    fn a_link_goes_ahead_only_once_confirmed_and_closing_turns_it_down(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let start = |cx: &mut VisualTestContext| {
            let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel();
            let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.show_link(Link::fake(confirm_tx), events_rx, window, cx)
            });
            cx.run_until_parked();
            (confirm_rx, events_tx)
        };
        let phase = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .link_pane()
                    .map(|view| view.read(cx).phase().clone())
            })
        };
        let pane_focused = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                let workspace = workspace.read(cx);
                workspace
                    .link_pane()
                    .is_some_and(|view| view.focus_handle(cx).contains_focused(window, cx))
            })
        };
        let asking =
            |cx: &mut VisualTestContext| cx.update(|window, cx| window.has_active_dialog(cx));

        // Shown in place of the settings, focused.
        let (mut confirm_rx, events) = start(cx);
        assert_eq!(phase(cx), Some(LinkPhase::Connecting));
        assert!(pane_focused(cx));
        // Escape before confirming: the pane goes and the link is turned
        // down (the core sees the confirmation dropped).
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(phase(cx), None);
        assert!(
            confirm_rx
                .try_recv()
                .is_err_and(|err| err == tokio::sync::oneshot::error::TryRecvError::Closed)
        );
        drop(events);

        // Asked for, the name field takes focus; Enter asks first.
        let (mut confirm_rx, events) = start(cx);
        events.send(LinkEvent::AwaitingConfirmation).unwrap();
        cx.run_until_parked();
        assert_eq!(phase(cx), Some(LinkPhase::Confirm));
        cx.simulate_input("Phone");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(asking(cx));
        // Not confirmed in the dialog: nothing is sent.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!asking(cx));
        assert!(
            confirm_rx
                .try_recv()
                .is_err_and(|err| err == tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(confirm_rx.try_recv().as_deref(), Ok("Phone"));
        assert_eq!(phase(cx), Some(LinkPhase::Linking));

        // Confirmed, closing only hides it: the outcome is told anyway.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(phase(cx), None);
        let told =
            |cx: &mut VisualTestContext| cx.update(|window, cx| window.notifications(cx).len());
        let before = told(cx);
        events.send(LinkEvent::Linked).unwrap();
        cx.run_until_parked();
        assert_eq!(told(cx), before + 1);
        assert!(workspace.read_with(cx, |workspace, _| workspace.linking.is_none()));

        // A confirmed link that fails after the pane closed is told too.
        let (_confirm_rx, events) = start(cx);
        events.send(LinkEvent::AwaitingConfirmation).unwrap();
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        // Settings reopened over the pane: it keeps focus, so Escape closes it.
        cx.simulate_keystrokes(&format!("{MOD}-,"));
        cx.run_until_parked();
        assert!(pane_focused(cx));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let before = told(cx);
        events.send(LinkEvent::Failed("no".into())).unwrap();
        cx.run_until_parked();
        assert_eq!(told(cx), before + 1);
        assert!(workspace.read_with(cx, |workspace, _| workspace.linking.is_none()));
    }

    #[gpui::test]
    fn the_safety_code_is_copied_as_shown_and_focus_comes_back(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let focus = focus_probe(&workspace, cx);
        let lines = SafetyLines([
            "00001 00002 00003 00004".into(),
            "00005 00006 00007 00008".into(),
            "00009 00010 00011 00012".into(),
        ]);
        let asking =
            |cx: &mut VisualTestContext| cx.update(|window, cx| window.has_active_dialog(cx));
        let shown = |lines: &SafetyLines, cx: &mut VisualTestContext| {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.show_safety_code("Grace".into(), lines.clone(), window, cx)
            });
            cx.run_until_parked();
            assert!(asking(cx));
        };
        let before = focus(cx);
        // Escape closes without copying.
        shown(&lines, cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!asking(cx));
        assert_eq!(cx.read_from_clipboard(), None);
        // Enter copies the digits as shown, a line each.
        shown(&lines, cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!asking(cx));
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("00001 00002 00003 00004\n00005 00006 00007 00008\n00009 00010 00011 00012")
        );
        assert_eq!(focus(cx), before);
    }

    #[gpui::test]
    fn the_safety_code_is_refused_where_there_is_nobody_to_compare_with(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        enable_helix(cx);
        let told =
            |cx: &mut VisualTestContext| cx.update(|window, cx| window.notifications(cx).len());
        let run = |line: &str, cx: &mut VisualTestContext| {
            cx.simulate_keystrokes(":");
            cx.run_until_parked();
            cx.simulate_input(line);
            cx.simulate_keystrokes("enter");
            cx.run_until_parked();
        };
        // No chat open.
        let before = told(cx);
        run("safety-code", cx);
        assert_eq!(told(cx), before + 1);
        assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
        // In a group, the member is asked for on the command line.
        run("o design", cx);
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.safety_code("", window, cx)
        });
        cx.run_until_parked();
        let prompt = workspace.read_with(cx, |workspace, cx| {
            workspace
                .command_line
                .as_ref()
                .map(|(line, _)| line.read(cx).value(cx))
        });
        assert_eq!(prompt.as_deref(), Some("safety-code "));
    }

    #[gpui::test]
    fn settings_change_from_the_keyboard_and_close_back(cx: &mut TestAppContext) {
        use crate::ui::settings_view::Row;
        let (workspace, cx) = setup(cx);
        let focus = focus_probe(&workspace, cx);
        let row = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace.settings_view.read(cx).selected_row(cx)
            })
        };
        let notifications =
            |cx: &mut VisualTestContext| cx.update(|_, cx| SettingsStore::get(cx).notifications);
        cx.simulate_keystrokes(&format!("{MOD}-,"));
        cx.run_until_parked();
        assert_eq!(focus(cx), Focus::Other, "the settings screen has focus");
        assert!(matches!(row(cx), Some(Row::Appearance(_))));
        // Letters move here: the screen has no text input to steal from.
        cx.simulate_keystrokes("down j down down");
        assert!(matches!(row(cx), Some(Row::Notifications(_))));
        cx.simulate_keystrokes("right");
        assert_eq!(notifications(cx), Notifications::Private);
        cx.simulate_keystrokes("left left");
        assert_eq!(notifications(cx), Notifications::Off);
        // Without an account the list ends with the app's settings.
        cx.simulate_keystrokes("end");
        assert!(matches!(row(cx), Some(Row::Notifications(_))));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!workspace.read_with(cx, |workspace, _| workspace.settings_open));
        assert_eq!(focus(cx), Focus::ChatList);
        // Opening a chat while it is open shows the chat instead.
        cx.simulate_keystrokes(&format!("{MOD}-,"));
        cx.simulate_keystrokes(&format!("{MOD}-1"));
        cx.run_until_parked();
        assert!(!workspace.read_with(cx, |workspace, _| workspace.settings_open));
    }

    #[gpui::test]
    fn an_alert_for_a_chat_not_yet_listed_opens_it_once_listed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let focus = focus_probe(&workspace, cx);
        cx.simulate_keystrokes(&format!("{MOD}-1"));
        cx.simulate_keystrokes(&format!("{MOD}-,"));
        cx.run_until_parked();
        let added = ChatId("just-added".into());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_from_alert(added.clone(), window, cx)
        });
        cx.run_until_parked();
        assert!(!workspace.read_with(cx, |workspace, _| workspace.settings_open));
        assert_eq!(focus(cx), Focus::ChatList);

        workspace.update(cx, |workspace, cx| {
            let mut chats = workspace.chats(cx);
            let mut new = chats[0].clone();
            new.id = added.clone();
            chats.push(new);
            workspace.update_chat_list(cx, |list, cx| list.set_chats(chats, LoadState::Loaded, cx));
        });
        cx.run_until_parked();
        let open = workspace.read_with(cx, |workspace, cx| {
            workspace
                .conversation
                .read(cx)
                .chat()
                .map(|chat| chat.id.clone())
        });
        assert_eq!(open, Some(added));
        assert_eq!(focus(cx), Focus::Composer);
    }

    /// Waits for an unlisted chat as an alert does, does `meanwhile`, then
    /// lists the chat. Returns the chat open after.
    fn wait_then_list(
        cx: &mut VisualTestContext,
        workspace: &Entity<Workspace>,
        meanwhile: impl FnOnce(&mut VisualTestContext),
    ) -> Option<ChatId> {
        let added = ChatId("just-added".into());
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_from_alert(added.clone(), window, cx)
        });
        meanwhile(cx);
        cx.run_until_parked();
        workspace.update(cx, |workspace, cx| {
            let mut chats = workspace.chats(cx);
            let mut new = chats[0].clone();
            new.id = added;
            chats.push(new);
            workspace.update_chat_list(cx, |list, cx| list.set_chats(chats, LoadState::Loaded, cx));
        });
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .conversation
                .read(cx)
                .chat()
                .map(|chat| chat.id.clone())
        })
    }

    #[gpui::test]
    fn a_chat_listed_late_never_takes_focus_from_where_the_user_went(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let focus = focus_probe(&workspace, cx);
        // Typing a command meanwhile: the chat opens, the command line
        // keeps focus.
        let open = wait_then_list(cx, &workspace, |cx| {
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_command_line(&OpenCommandLine, window, cx)
            })
        });
        assert_eq!(open, Some(ChatId("just-added".into())));
        assert_eq!(focus(cx), Focus::Other);
        assert!(workspace.read_with(cx, |workspace, _| workspace.command_line.is_some()));
    }

    #[gpui::test]
    fn a_cancelled_wait_never_moves_focus_later(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let focus = focus_probe(&workspace, cx);
        cx.simulate_keystrokes(&format!("{MOD}-1"));
        // Picking the chat already open cancels the wait, though nothing
        // opens.
        let open = wait_then_list(cx, &workspace, |cx| {
            cx.simulate_keystrokes(&format!("{MOD}-1"))
        });
        assert_ne!(open, Some(ChatId("just-added".into())));
        workspace.update_in(cx, |workspace, window, cx| {
            workspace.focus_chat_list(&FocusChatList, window, cx)
        });
        // Browsing onto it later in the list keeps focus there.
        workspace.update(cx, |workspace, cx| {
            workspace.update_chat_list(cx, |list, cx| list.open(ChatId("just-added".into()), cx))
        });
        cx.run_until_parked();
        assert_eq!(focus(cx), Focus::ChatList);
    }

    #[gpui::test]
    fn a_theme_from_settings_changes_only_its_half(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        workspace.update_in(cx, |workspace, window, cx| {
            SettingsStore::update(cx, |settings| settings.appearance = Appearance::Light).unwrap();
            let dark = theme::by_mode(cx).dark.last().cloned().unwrap();
            workspace.settings_changed(SettingsEvent::Theme(dark.clone()), window, cx);
            let settings = SettingsStore::get(cx);
            assert_eq!(settings.dark_theme, dark);
            assert_eq!(settings.light_theme, theme::DEFAULT_LIGHT);
            // Unlike `:theme`, the appearance stays as chosen.
            assert_eq!(settings.appearance, Appearance::Light);
        });
    }

    #[gpui::test]
    fn dropped_files_are_sent_only_once_confirmed(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        let asking =
            |cx: &mut VisualTestContext| cx.update(|window, cx| window.has_active_dialog(cx));
        let told =
            |cx: &mut VisualTestContext| cx.update(|window, cx| window.notifications(cx).len());
        let drop = |cx: &mut VisualTestContext| {
            workspace.update(cx, |workspace, cx| {
                workspace.conversation.update(cx, |_, cx| {
                    cx.emit(ConversationEvent::Attach(vec!["/tmp/a.txt".into()]))
                })
            });
            cx.run_until_parked();
        };
        enable_helix(cx);
        cx.simulate_keystrokes(":");
        cx.run_until_parked();
        cx.simulate_input("o grace");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        drop(cx);
        assert!(asking(cx));
        let before = told(cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!asking(cx));
        assert_eq!(told(cx), before, "cancelled, nothing was tried");
        // Confirmed, the send runs: without an account, it is refused.
        drop(cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(!asking(cx));
        assert_eq!(told(cx), before + 1);
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
            conversation.message_body(conversation.cursor().unwrap(), cx)
        });
        assert_eq!(clipboard(cx), Some(expected));
    }

    #[gpui::test]
    fn reactions_are_picked_on_the_message_under_the_cursor(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        enable_helix(cx);
        let reacting = |cx: &mut VisualTestContext| {
            workspace.read_with(cx, |workspace, cx| {
                workspace.conversation.read(cx).is_reacting()
            })
        };
        let told =
            |cx: &mut VisualTestContext| cx.update(|window, cx| window.notifications(cx).len());
        // Nothing to react to without a message under the cursor.
        cx.simulate_keystrokes("g g l +");
        assert!(!reacting(cx));
        cx.simulate_keystrokes("k +");
        assert!(reacting(cx));
        // Escape closes the picker, not the cursor.
        cx.simulate_keystrokes("escape");
        assert!(!reacting(cx));
        assert!(workspace.read_with(cx, |w, cx| w.conversation.read(cx).cursor().is_some()));
        // A key picks one and closes it (mock data has no account: refused).
        cx.simulate_keystrokes("+");
        let before = told(cx);
        cx.simulate_keystrokes("1");
        cx.run_until_parked();
        assert!(!reacting(cx));
        assert_eq!(told(cx), before + 1);
        // Digits do nothing while the picker is closed.
        cx.simulate_keystrokes("1");
        cx.run_until_parked();
        assert_eq!(told(cx), before + 1);
        // The standard keymap: same keys, escape bound in its own layer.
        cx.update(|window, cx| {
            SettingsStore::update(cx, |s| s.keymap_mode = KeymapMode::Standard).unwrap();
            window.refresh();
        });
        cx.simulate_keystrokes("up +");
        assert!(reacting(cx));
        cx.simulate_keystrokes("escape");
        assert!(!reacting(cx));
    }

    /// The newest message marked as read, and the open chat's newest.
    fn read_state(
        workspace: &Entity<Workspace>,
        cx: &mut VisualTestContext,
    ) -> (Option<String>, Option<String>) {
        workspace.read_with(cx, |workspace, cx| {
            let conversation = workspace.conversation.read(cx);
            let messages = conversation.messages_entity().unwrap().read(cx);
            let read = messages.read_until().map(|id| id.0.to_string());
            let newest = messages.messages().last().map(|m| m.id.0.to_string());
            (read, newest)
        })
    }

    #[gpui::test]
    fn only_messages_shown_in_an_active_window_are_seen(cx: &mut TestAppContext) {
        let (workspace, cx) = setup(cx);
        cx.update(|window, _| window.activate_window());
        cx.simulate_keystrokes("alt-down");
        cx.run_until_parked();
        let (read, newest) = read_state(&workspace, cx);
        assert!(newest.is_some());
        assert_eq!(read, newest, "a short chat is seen in full on opening");

        // In the background, nothing counts as seen until the window is
        // back in front.
        cx.deactivate_window();
        cx.simulate_keystrokes("alt-down");
        cx.run_until_parked();
        let (read, newest) = read_state(&workspace, cx);
        assert_eq!(read, None);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        assert_eq!(read_state(&workspace, cx).0, newest);
    }

    #[test]
    fn contact_dialogs_say_what_really_happens() {
        use crate::model::OneToOne;
        let chat = |one_to_one| ChatSummary {
            id: ChatId("c".into()),
            title: "Ada".into(),
            is_group: false,
            request: None,
            is_self_chat: false,
            one_to_one,
            preview: None,
            last_activity: None,
            unread: 0,
            muted: None,
            group_ended: false,
            picture: None,
        };
        let said = |op, facts| contact_confirmation(op, &chat(Some(facts))).unwrap().1;
        let incoming = OneToOne {
            side: Side::Incoming,
            ..OneToOne::CONTACT
        };
        // Blocking a request's sender declines the request.
        assert!(said(ContactOp::Block, incoming).contains("decline"));
        // Deleting a contact ends their chat too, not only the user's.
        assert!(said(ContactOp::Delete, OneToOne::CONTACT).contains("their chat with you ends"));
        // Unless they ended it already.
        let ended = OneToOne {
            stage: Stage::Ended,
            ..OneToOne::CONTACT
        };
        assert!(said(ContactOp::Delete, ended).contains("They ended"));
        // Groups and the self chat have no contact to act on.
        assert!(contact_confirmation(ContactOp::Block, &chat(None)).is_err());
    }

    #[test]
    fn a_safety_code_is_asked_for_only_from_someone_to_compare_with() {
        let chat = |is_group, is_self_chat| ChatSummary {
            id: ChatId("c".into()),
            title: "chat".into(),
            is_group,
            request: None,
            is_self_chat,
            one_to_one: None,
            preview: None,
            last_activity: None,
            unread: 0,
            muted: None,
            group_ended: false,
            picture: None,
        };
        let (contact, group, own) = (chat(false, false), chat(true, false), chat(false, true));
        assert!(safety_target(None, "").is_err());
        assert!(safety_target(Some(&own), "").is_err());
        assert!(safety_target(Some(&own), "ada").is_err());
        assert_eq!(
            safety_target(Some(&contact), " "),
            Ok(SafetyTarget::Contact)
        );
        // A name in a one-to-one chat is refused, not ignored.
        assert!(safety_target(Some(&contact), "grace").is_err());
        assert_eq!(safety_target(Some(&group), ""), Ok(SafetyTarget::AskMember));
        assert_eq!(safety_target(Some(&group), "#ab"), Ok(SafetyTarget::Member));
    }
}
