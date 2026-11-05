//! The `:` prompt at the bottom of the window (Helix's command mode).
//! Parsing and completion live in `crate::command_line`; this view only
//! edits the line and emits what was submitted.

use gpui::{EventEmitter, Focusable as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, IntoElement, Render, Subscription, Window, div};

use crate::actions::{CommandCancel, CommandComplete};
use crate::command_line::complete;

pub enum CommandLineEvent {
    Submit(String),
    Cancel,
}

pub struct CommandLine {
    input: Entity<InputState>,
    themes: Vec<String>,
    chats: Vec<String>,
    completions: Vec<(String, String)>,
    /// Completion that Tab accepts next.
    selected: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CommandLineEvent> for CommandLine {}

impl CommandLine {
    pub fn new(
        themes: Vec<String>,
        chats: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscriptions =
            vec![
                cx.subscribe_in(
                    &input,
                    window,
                    |this, input, event: &InputEvent, _, cx| match event {
                        InputEvent::Change => {
                            this.completions =
                                complete(&input.read(cx).value(), &this.themes, &this.chats);
                            this.selected = 0;
                            cx.notify();
                        }
                        InputEvent::PressEnter { .. } => {
                            let line = input.read(cx).value().to_string();
                            cx.emit(CommandLineEvent::Submit(line));
                        }
                        _ => {}
                    },
                ),
            ];
        input.read(cx).focus_handle(cx).focus(window, cx);
        let completions = complete("", &themes, &chats);
        Self {
            input,
            themes,
            chats,
            completions,
            selected: 0,
            _subscriptions: subscriptions,
        }
    }

    #[cfg(test)]
    pub fn value(&self, cx: &gpui::App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn accept_completion(
        &mut self,
        _: &CommandComplete,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((line, _)) = self.completions.get(self.selected).cloned() else {
            return;
        };
        self.selected = (self.selected + 1) % self.completions.len();
        // `set_value` emits no Change, so the list stays as it is and the
        // next Tab cycles to the following completion until the user types.
        self.input
            .update(cx, |input, cx| input.set_value(line, window, cx));
        cx.notify();
    }
}

impl Render for CommandLine {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let completions = self
            .completions
            .iter()
            .take(8)
            .enumerate()
            .map(|(ix, (line, help))| {
                h_flex()
                    .gap_4()
                    .px_3()
                    .py_0p5()
                    .rounded(theme.radius)
                    .when(ix == self.selected, |row| row.bg(theme.accent))
                    .child(div().flex_1().child(line.clone()))
                    .child(div().text_color(theme.muted_foreground).child(help.clone()))
            });
        v_flex()
            .key_context("CommandLine")
            .on_action(cx.listener(Self::accept_completion))
            .on_action(cx.listener(|_, _: &CommandCancel, _, cx| cx.emit(CommandLineEvent::Cancel)))
            .w_full()
            .when(!self.completions.is_empty(), |this| {
                this.child(
                    v_flex()
                        .p_1()
                        .border_t_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .text_sm()
                        .children(completions),
                )
            })
            .child(
                h_flex()
                    .px_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(":"))
                    .child(Input::new(&self.input).appearance(false)),
            )
    }
}
