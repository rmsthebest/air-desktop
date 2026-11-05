//! Right pane: details about the open chat. Toggleable.

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, FontWeight, IntoElement, div, px};

use crate::model::ChatSummary;
use crate::ui::widgets::avatar;

pub fn details_pane(chat: Option<&ChatSummary>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .size_full()
        .items_center()
        .gap_2()
        .pt_8()
        .px_4()
        .bg(theme.sidebar)
        .when_some(chat, |pane, chat| {
            pane.child(avatar(&chat.title, px(72.), cx))
                .child(
                    div()
                        .pt_2()
                        .max_w_full()
                        .truncate()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(chat.title.clone()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(if chat.is_group { "Group" } else { "Contact" }),
                )
        })
}
