//! Right pane: details about the open chat. Toggleable.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, FontWeight, IntoElement, div, px};

use crate::model::{ChatSummary, Member};
use crate::ui::widgets::avatar;

/// `members` is the open group's, once loaded.
pub fn details_pane(
    chat: Option<&ChatSummary>,
    members: Option<&[Member]>,
    cx: &App,
) -> impl IntoElement {
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
                .child(div().text_sm().text_color(theme.muted_foreground).child(
                    if chat.is_self_chat {
                        "Your devices"
                    } else if chat.is_group {
                        "Group"
                    } else {
                        "Contact"
                    },
                ))
        })
        .when_some(members, |pane, members| {
            pane.child(
                v_flex()
                    .w_full()
                    .pt_4()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} members", members.len())),
                    )
                    .children(members.iter().map(|member| member_row(member, cx))),
            )
        })
}

fn member_row(member: &Member, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    // What is said about the member, after their (peer-chosen) name.
    let note = if member.removing {
        Some("being removed")
    } else if member.own {
        Some("you")
    } else {
        None
    };
    h_flex()
        .w_full()
        .gap_2()
        .child(avatar(&member.name, px(24.), cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(member.name.clone()),
        )
        .when_some(note, |row, note| {
            row.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(note),
            )
        })
}
