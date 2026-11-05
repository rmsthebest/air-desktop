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
        SelectNext,
        SelectPrev,
    ]
);

/// Opens the chat at a position in the list (0-based), e.g. for `ctrl-1`.
#[derive(Clone, PartialEq, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = air)]
pub struct OpenChatAt {
    pub index: usize,
}

/// A command as listed in the palette and the shortcuts overview, with the
/// key context its binding lives in.
pub struct Command {
    pub label: SharedString,
    pub action: Box<dyn Action>,
    pub context: &'static str,
}

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
        cmd("Next chat", NextChat, "Workspace"),
        cmd("Previous chat", PrevChat, "Workspace"),
        cmd("Next unread chat", NextUnreadChat, "Workspace"),
        cmd("Previous unread chat", PrevUnreadChat, "Workspace"),
        cmd("Focus chat list", FocusChatList, "Workspace"),
        cmd("Focus conversation", FocusConversation, "Workspace"),
        cmd("Toggle details pane", ToggleDetails, "Workspace"),
        cmd(
            "Cycle appearance (system/light/dark)",
            ToggleAppearance,
            "Workspace",
        ),
        cmd("Keyboard shortcuts", ShowShortcuts, "Workspace"),
        cmd("Quit", Quit, "Workspace"),
    ]
}
