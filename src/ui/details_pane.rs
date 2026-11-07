//! Right pane: details about the open chat. Toggleable.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, FontWeight, IntoElement, div, px};

use crate::model::{ChatSummary, Member};
use crate::ui::widgets::avatar;

/// Members listed; others can invite any number.
const MAX_ROWS: usize = 100;

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
                .when(
                    !chat.is_group && !chat.is_self_chat && chat.request.is_none(),
                    |pane| {
                        pane.child(
                            div()
                                .pt_4()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(":safety-code to check this is really them"),
                        )
                    },
                )
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
                    .children(
                        members
                            .iter()
                            .take(MAX_ROWS)
                            .map(|member| member_row(member, cx)),
                    )
                    .when(members.len() > MAX_ROWS, |list| {
                        list.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("and {} more", members.len() - MAX_ROWS)),
                        )
                    }),
            )
        })
}

fn member_row(member: &Member, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    // What is said about the member, after their (peer-chosen) name.
    let note = if member.removing {
        format!("being removed · #{}", member.tag)
    } else if member.own {
        "you".to_string()
    } else {
        format!("#{}", member.tag)
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
        .child(
            div()
                .flex_none()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(note),
        )
}
