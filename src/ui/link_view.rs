//! Linking a new device from this one (`:link <code>`), shown in place of
//! the settings. It only shows where the link stands and takes the new
//! device's name; the workspace runs the link and confirms it with the
//! user (the new device gets full access to the account).

use gpui::{EventEmitter, SharedString};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Subscription,
    Window, div, px,
};

use crate::actions::CancelLink;
use crate::ui::key_context;

#[derive(Debug, Clone, PartialEq)]
pub enum LinkPhase {
    Connecting,
    /// The new device knows the code: the user names it and confirms.
    Confirm,
    Linking,
    Linked,
    Failed(SharedString),
}

pub enum LinkViewEvent {
    /// Enter in the name field: confirm linking the device under this name
    /// (empty keeps its own).
    Confirm(String),
    Close,
}

pub struct LinkView {
    focus_handle: FocusHandle,
    name: Entity<InputState>,
    phase: LinkPhase,
    _subscription: Subscription,
}

impl EventEmitter<LinkViewEvent> for LinkView {}

impl Focusable for LinkView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.phase == LinkPhase::Confirm {
            self.name.focus_handle(cx)
        } else {
            self.focus_handle.clone()
        }
    }
}

impl LinkView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Leave empty to keep the device's own name")
        });
        let subscription = cx.subscribe_in(&name, window, |this, name, event, _, cx| {
            if let InputEvent::PressEnter { .. } = event
                && this.phase == LinkPhase::Confirm
            {
                cx.emit(LinkViewEvent::Confirm(name.read(cx).value().to_string()));
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            name,
            phase: LinkPhase::Connecting,
            _subscription: subscription,
        }
    }

    #[cfg(test)]
    pub fn phase(&self) -> &LinkPhase {
        &self.phase
    }

    /// Shows `phase`, keeping focus in the pane if it had it.
    pub fn set_phase(&mut self, phase: LinkPhase, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focus_handle.contains_focused(window, cx)
            || self.name.focus_handle(cx).is_focused(window);
        self.phase = phase;
        if focused {
            self.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn cancel(&mut self, _: &CancelLink, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(LinkViewEvent::Close);
    }
}

impl Render for LinkView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let text = |text: SharedString| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let body: AnyElement = match &self.phase {
            LinkPhase::Connecting => {
                text("Connecting to the new device…".into()).into_any_element()
            }
            LinkPhase::Confirm => v_flex()
                .gap_2()
                .child(text(
                    "The new device has the code. Name it, then press Enter to link it.".into(),
                ))
                .child(Input::new(&self.name))
                .child(div().text_sm().text_color(theme.danger).child(
                    "A linked device has full access to your Air account, \
                             including adding and removing devices (even this one). \
                             Only link devices you own.",
                ))
                .into_any_element(),
            LinkPhase::Linking => {
                text("Linking… The new device is set up next.".into()).into_any_element()
            }
            LinkPhase::Linked => text("Linked. The new device is ready.".into()).into_any_element(),
            LinkPhase::Failed(message) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(message.clone())
                .into_any_element(),
        };
        v_flex()
            .key_context(key_context("LinkDevice", cx))
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .size_full()
            .p_6()
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Link a device"),
            )
            .child(div().max_w(px(520.)).child(body))
            .child(text(
                if matches!(self.phase, LinkPhase::Linking) {
                    "Esc closes this; linking goes on."
                } else {
                    "Esc to close."
                }
                .into(),
            ))
    }
}
