//! Every user-facing command is an action, so it can be bound in the keymap,
//! listed in the command palette and shown in the shortcuts overview.

use gpui::{Action, SharedString};
use schemars::JsonSchema;
use serde::Deserialize;

gpui_kit::actions!(
    air,
    [
        Quit,
        OpenPalette,
        ShowShortcuts,
        NextChat,
        PrevChat,
        NextUnreadChat,
        PrevUnreadChat,
        FocusChatList,
        FocusConversation,
        ToggleDetails,
        ToggleAppearance,
        ToggleHelixMode,
        SelectNext,
        SelectPrev,
        // Helix-style
        FindChat,
        PickTheme,
        OpenCommandLine,
        JumpBack,
        JumpForward,
        FocusMessages,
        FocusComposer,
        FocusNextPane,
        SelectFirst,
        SelectLast,
        CursorDown,
        CursorUp,
        CursorFirst,
        CursorLast,
        HalfPageDown,
        HalfPageUp,
        YankMessage,
        OpenLinks,
        EditMessage,
        EditLastMessage,
        ReplyToMessage,
        PickReaction,
        SaveAttachment,
        AttachFile,
        OpenSettings,
        CloseSettings,
        SettingNext,
        SettingPrev,
        CancelCompose,
        AddContact,
        NewGroup,
        InviteToGroup,
        AcceptRequest,
        DeclineRequest,
        ClearCursor,
        CommandComplete,
        CommandCancel,
        SettingRemove,
        LinkDevice,
        LinkExistingAccount,
        CancelLink,
        ShowSafetyCode,
        BlockContact,
        UnblockContact,
        ReportSpam,
        RetractRequest,
        DeleteChat,
        DeleteMessage,
        DeleteMessageForEveryone,
        ShowMessageDetails,
        ToggleMute,
    ]
);

/// Opens the chat at a position in the list (0-based), e.g. for `ctrl-1`.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = air)]
pub struct OpenChatAt {
    pub index: usize,
}

/// Toggles a quick reaction (0-based, see `model::QUICK_REACTIONS`) on
/// the message under the cursor, while the reaction picker is open.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = air)]
pub struct React {
    pub index: usize,
}

/// A command as listed in the palette and the shortcuts overview, with the
/// key context its binding lives in.
pub struct Command {
    pub label: SharedString,
    pub action: Box<dyn Action>,
    pub context: &'static str,
}

/// Commands offered in the palette: those that make sense from anywhere.
pub fn commands() -> Vec<Command> {
    fn cmd(label: &str, action: impl Action, context: &'static str) -> Command {
        Command {
            label: SharedString::from(label.to_string()),
            action: Box::new(action),
            context,
        }
    }
    vec![
        cmd("Command palette", OpenPalette, "Workspace"),
        cmd("Find chat", FindChat, "Workspace"),
        cmd("Pick theme", PickTheme, "Workspace"),
        cmd("Next chat", NextChat, "Workspace"),
        cmd("Previous chat", PrevChat, "Workspace"),
        cmd("Next unread chat", NextUnreadChat, "Workspace"),
        cmd("Previous unread chat", PrevUnreadChat, "Workspace"),
        cmd("Jump back", JumpBack, "Workspace"),
        cmd("Jump forward", JumpForward, "Workspace"),
        cmd("Focus chat list", FocusChatList, "Workspace"),
        cmd("Focus messages", FocusMessages, "Workspace"),
        cmd("Write a message", FocusComposer, "Workspace"),
        cmd("Add contact by username", AddContact, "Workspace"),
        cmd("New group", NewGroup, "Workspace"),
        cmd("Invite to the open group", InviteToGroup, "Workspace"),
        cmd("Attach a file", AttachFile, "Workspace"),
        cmd("Settings", OpenSettings, "Workspace"),
        cmd("Link a device", LinkDevice, "Workspace"),
        cmd("Compare safety code", ShowSafetyCode, "Workspace"),
        cmd("Accept contact request", AcceptRequest, "Workspace"),
        cmd("Decline contact request", DeclineRequest, "Workspace"),
        cmd("Retract contact request", RetractRequest, "Workspace"),
        cmd("Block contact", BlockContact, "Workspace"),
        cmd("Unblock contact", UnblockContact, "Workspace"),
        cmd("Report spam", ReportSpam, "Workspace"),
        cmd("Delete chat", DeleteChat, "Workspace"),
        cmd("Mute or unmute chat", ToggleMute, "Workspace"),
        cmd("Toggle details pane", ToggleDetails, "Workspace"),
        cmd(
            "Cycle appearance (system/light/dark)",
            ToggleAppearance,
            "Workspace",
        ),
        cmd("Toggle Helix keymap", ToggleHelixMode, "Workspace"),
        cmd("Keyboard shortcuts", ShowShortcuts, "Workspace"),
        cmd("Quit", Quit, "Workspace"),
    ]
}

/// Short human label for any of our actions, used by the which-key popup
/// and the shortcuts overview.
pub fn label(action: &dyn Action) -> String {
    let name = action.name();
    let label = match name.strip_prefix("air::").unwrap_or(name) {
        "Quit" => "quit",
        "OpenPalette" => "command palette",
        "ShowShortcuts" => "keyboard shortcuts",
        "NextChat" => "next chat",
        "PrevChat" => "previous chat",
        "NextUnreadChat" => "next unread",
        "PrevUnreadChat" => "previous unread",
        "FocusChatList" => "chat list",
        "FocusConversation" => "conversation",
        "ToggleDetails" => "toggle details",
        "ToggleAppearance" => "cycle appearance",
        "ToggleHelixMode" => "toggle helix keymap",
        "SelectNext" => "next",
        "SelectPrev" => "previous",
        "FindChat" => "find chat",
        "PickTheme" => "pick theme",
        "OpenCommandLine" => "command line",
        "JumpBack" => "jump back",
        "JumpForward" => "jump forward",
        "FocusMessages" => "messages",
        "FocusComposer" => "insert mode",
        "FocusNextPane" => "next pane",
        "SelectFirst" | "CursorFirst" => "first",
        "SelectLast" | "CursorLast" => "last",
        "CursorDown" => "next message",
        "CursorUp" => "previous message",
        "HalfPageDown" => "half page down",
        "HalfPageUp" => "half page up",
        "YankMessage" => "copy message",
        "OpenLinks" => "open links",
        "EditMessage" => "edit message",
        "EditLastMessage" => "edit last message",
        "ReplyToMessage" => "reply",
        "PickReaction" => "react",
        "React" => "toggle reaction",
        "SaveAttachment" => "save file",
        "AttachFile" => "attach file",
        "OpenSettings" => "settings",
        "CloseSettings" => "close settings",
        "SettingNext" => "next value",
        "SettingPrev" => "previous value",
        "SettingRemove" => "remove or unlink",
        "LinkDevice" => "link a device",
        "LinkExistingAccount" => "link to an existing account",
        "CancelLink" => "cancel linking",
        "ShowSafetyCode" => "safety code",
        "AddContact" => "add contact",
        "NewGroup" => "new group",
        "InviteToGroup" => "invite to group",
        "AcceptRequest" => "accept request",
        "DeclineRequest" => "decline request",
        "RetractRequest" => "retract request",
        "BlockContact" => "block contact",
        "UnblockContact" => "unblock contact",
        "ReportSpam" => "report spam",
        "DeleteChat" => "delete chat",
        "DeleteMessage" => "delete message for me",
        "DeleteMessageForEveryone" => "delete message for everyone",
        "ShowMessageDetails" => "message details",
        "ToggleMute" => "mute or unmute",
        "CancelCompose" => "cancel reply or edit",
        "ClearCursor" => "clear selection",
        "OpenChatAt" => "open chat n",
        other => other,
    };
    label.to_string()
}
