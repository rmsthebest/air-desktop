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

use crate::markdown::{Block, Inline, Markdown};
use crate::model::preview_line;

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
    let mut highlights: Vec<_> = text
        .styles
        .iter()
        .map(|(range, style)| {
            let highlight = HighlightStyle {
                font_weight: style.bold.then_some(FontWeight::BOLD),
                font_style: style.italic.then_some(FontStyle::Italic),
                strikethrough: style.strike.then(|| StrikethroughStyle {
                    thickness: px(1.),
                    ..Default::default()
                }),
                background_color: style.code.then(|| theme.muted.opacity(0.6)),
                ..Default::default()
            };
            (range.clone(), highlight)
        })
        .collect();
    let link_style = HighlightStyle {
        color: Some(theme.link),
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            ..Default::default()
        }),
        ..Default::default()
    };
    highlights.extend(
        text.links
            .iter()
            .map(|(range, _)| (range.clone(), link_style)),
    );
    // Highlights must be sorted and not overlap; links win over styles.
    let highlights = flatten(highlights);
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

/// Splits overlapping highlights into sorted, disjoint ranges, merging the
/// styles that cover each piece (later ones win per field).
fn flatten(
    highlights: Vec<(std::ops::Range<usize>, HighlightStyle)>,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let mut bounds: Vec<usize> = highlights
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    bounds
        .windows(2)
        .filter_map(|piece| {
            let (start, end) = (piece[0], piece[1]);
            let mut covering = highlights
                .iter()
                .filter(|(range, _)| range.start <= start && end <= range.end)
                .map(|(_, style)| *style)
                .peekable();
            covering.peek()?;
            let merged = covering.fold(HighlightStyle::default(), |merged, style| {
                merged.highlight(style)
            });
            Some((start..end, merged))
        })
        .collect()
}

/// Asks before opening links from a message, showing each destination
/// (cleaned of invisible characters, so it can't pose as another).
pub fn confirm_open(targets: Vec<SharedString>, window: &mut Window, cx: &mut App) {
    if targets.is_empty() {
        return;
    }
    window.open_dialog(cx, move |dialog, _, _| {
        let buttons = targets.iter().enumerate().map(|(ix, target)| {
            let target = target.clone();
            Button::new(("open-link", ix))
                .ghost()
                .label(preview_line(&target))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_highlights_become_disjoint_and_sorted() {
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        let italic = HighlightStyle {
            font_style: Some(FontStyle::Italic),
            ..Default::default()
        };
        let flat = flatten(vec![(0..10, bold), (4..6, italic)]);
        let ranges: Vec<_> = flat.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, [0..4, 4..6, 6..10]);
        assert_eq!(flat[1].1, bold.highlight(italic));
        // Gaps between highlights stay unstyled.
        let flat = flatten(vec![(0..2, bold), (5..7, bold)]);
        let ranges: Vec<_> = flat.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, [0..2, 5..7]);
    }
}
