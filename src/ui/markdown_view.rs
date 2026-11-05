//! Renders parsed message markdown (see `markdown.rs`). Links are never
//! opened directly: clicking one asks first, showing where it really goes.

use gpui::{
    AnyElement, App, ElementId, FontStyle, FontWeight, HighlightStyle, InteractiveText,
    SharedString, StrikethroughStyle, StyledText, UnderlineStyle, Window,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{div, px};

use crate::markdown::{Block, Inline, Markdown, link_label};

/// Renders `markdown`; `id` must be unique among rendered messages.
pub fn render(markdown: &Markdown, id: SharedString, cx: &App) -> AnyElement {
    let mut counter = 0;
    let body = v_flex()
        .gap_1p5()
        .children(blocks(&markdown.blocks, &id, &mut counter, cx));
    body.when(markdown.truncated, |body| {
        body.child(
            div()
                .text_xs()
                .italic()
                .opacity(0.7)
                .child("Message too long to show in full"),
        )
    })
    .into_any_element()
}

fn blocks(blocks: &[Block], id: &SharedString, counter: &mut usize, cx: &App) -> Vec<AnyElement> {
    let theme = cx.theme();
    blocks
        .iter()
        .map(|block| match block {
            Block::Paragraph(text) => inline(text, id, counter, cx),
            Block::Heading(text) => div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_lg()
                .child(inline(text, id, counter, cx))
                .into_any_element(),
            Block::Quote(children) => v_flex()
                .gap_1()
                .pl_2()
                .border_l_2()
                .border_color(theme.border)
                .opacity(0.85)
                .children(self::blocks(children, id, counter, cx))
                .into_any_element(),
            Block::List { start, items } => v_flex()
                .gap_0p5()
                .children(items.iter().enumerate().map(|(ix, item)| {
                    let marker = match start {
                        Some(start) => format!("{}.", start.saturating_add(ix as u64)),
                        None => "•".to_string(),
                    };
                    h_flex()
                        .items_start()
                        .gap_1p5()
                        .child(div().flex_none().min_w(px(14.)).child(marker))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_0p5()
                                .children(self::blocks(item, id, counter, cx)),
                        )
                }))
                .into_any_element(),
            Block::Code(code) => div()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .bg(theme.muted.opacity(0.5))
                .font_family(theme.mono_font_family.clone())
                .text_sm()
                .child(code.clone())
                .into_any_element(),
            Block::Rule => div().h(px(1.)).my_1().bg(theme.border).into_any_element(),
        })
        .collect()
}

fn inline(text: &Inline, id: &SharedString, counter: &mut usize, cx: &App) -> AnyElement {
    let theme = cx.theme();
    // The runs are already sorted and disjoint (see `Inline`).
    let highlights = text.styles.iter().map(|(range, style)| {
        let highlight = HighlightStyle {
            color: style.link.then_some(theme.link),
            font_weight: style.bold.then_some(FontWeight::BOLD),
            font_style: style.italic.then_some(FontStyle::Italic),
            strikethrough: style.strike.then(|| StrikethroughStyle {
                thickness: px(1.),
                ..Default::default()
            }),
            underline: style.link.then(|| UnderlineStyle {
                thickness: px(1.),
                ..Default::default()
            }),
            background_color: style.code.then(|| theme.muted.opacity(0.6)),
            ..Default::default()
        };
        (range.clone(), highlight)
    });
    let styled = StyledText::new(text.text.clone()).with_highlights(highlights);
    *counter += 1;
    if text.links.is_empty() {
        return div().child(styled).into_any_element();
    }
    let ranges = text.links.iter().map(|(range, _)| range.clone()).collect();
    let targets: Vec<_> = text
        .links
        .iter()
        .map(|(_, target)| target.clone())
        .collect();
    InteractiveText::new(ElementId::from((id.clone(), *counter)), styled)
        .on_click(ranges, move |ix, window, cx| {
            if let Some(target) = targets.get(ix) {
                confirm_open(vec![target.clone()], window, cx);
            }
        })
        .into_any_element()
}

/// Asks before opening links from a message, showing where each one goes:
/// its host (or mail address) first, then the whole target. Targets are
/// normalized ASCII (see [`link_target`]), so what is shown is what opens.
///
/// [`link_target`]: crate::markdown::link_target
pub fn confirm_open(targets: Vec<SharedString>, window: &mut Window, cx: &mut App) {
    if targets.is_empty() {
        return;
    }
    window.open_dialog(cx, move |dialog, _, cx| {
        let muted = cx.theme().muted_foreground;
        let buttons = targets.iter().enumerate().map(|(ix, target)| {
            let (destination, full) = link_label(target);
            let target = target.clone();
            Button::new(("open-link", ix))
                .ghost()
                .h_auto()
                .py_1()
                .child(
                    v_flex()
                        .items_start()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(destination))
                        .child(div().text_xs().text_color(muted).child(full)),
                )
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    cx.open_url(&target);
                })
        });
        dialog
            .title(if targets.len() == 1 {
                "Open this link?"
            } else {
                "Open a link?"
            })
            .w(px(520.))
            .child(v_flex().gap_1().children(buttons))
    });
}
