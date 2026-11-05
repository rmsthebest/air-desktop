//! Message markdown, parsed into blocks of styled text for the views.
//! Message bodies are peer-controlled, so the result is plain data that
//! renders without side effects: images become their alt text, raw HTML is
//! shown as text, nesting and size are bounded, and only `http`, `https` and
//! `mailto` destinations become links (which the UI opens only after the
//! user confirms).

use std::ops::Range;

use gpui_kit::SharedString;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Longest source rendered, in bytes; the rest is cut with a notice.
const MAX_SOURCE: usize = 32 * 1024;
/// Deepest nesting of quotes and lists kept; deeper content is flattened
/// into the innermost kept container.
const MAX_DEPTH: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub struct Markdown {
    pub blocks: Vec<Block>,
    /// The source was longer than [`MAX_SOURCE`] and was cut.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Inline),
    Heading(Inline),
    Quote(Vec<Block>),
    List {
        /// The first number of an ordered list.
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Code(SharedString),
    Rule,
}

/// A run of text with styled ranges and links (byte ranges into `text`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    pub styles: Vec<(Range<usize>, Style)>,
    pub links: Vec<(Range<usize>, SharedString)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
}

/// A link destination the user may open, or `None` for anything else
/// (`javascript:`, `file:`, `data:`, relative paths…).
pub fn link_target(url: &str) -> Option<SharedString> {
    let (scheme, rest) = url.trim().split_once(':')?;
    let allowed = ["http", "https", "mailto"]
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed));
    (allowed && !rest.is_empty()).then(|| url.trim().to_string().into())
}

enum Container {
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Item(Vec<Block>),
}

impl Container {
    fn blocks(&mut self) -> Option<&mut Vec<Block>> {
        match self {
            Container::Quote(blocks) | Container::Item(blocks) => Some(blocks),
            Container::List { .. } => None,
        }
    }
}

#[derive(Default)]
struct Builder {
    root: Vec<Block>,
    containers: Vec<Container>,
    /// Quotes and lists opened past [`MAX_DEPTH`], whose ends to ignore.
    flattened: usize,
    inline: Option<Inline>,
    heading: bool,
    code: Option<String>,
    style: Style,
    link: Option<(usize, Option<SharedString>)>,
    image: bool,
}

impl Builder {
    fn push_block(&mut self, block: Block) {
        let target = self
            .containers
            .iter_mut()
            .rev()
            .find_map(Container::blocks)
            .unwrap_or(&mut self.root);
        target.push(block);
    }

    /// Ends the open run of text, if any, as a paragraph or heading.
    fn close_inline(&mut self) {
        if let Some(inline) = self.inline.take()
            && !inline.text.trim().is_empty()
        {
            let block = if std::mem::take(&mut self.heading) {
                Block::Heading(inline)
            } else {
                Block::Paragraph(inline)
            };
            self.push_block(block);
        }
    }

    fn text(&mut self, text: &str, style: Style) {
        if let Some(code) = &mut self.code {
            code.push_str(text);
            return;
        }
        let inline = self.inline.get_or_insert_with(Inline::default);
        let start = inline.text.len();
        inline.text.push_str(text);
        if style != Style::default() {
            inline.styles.push((start..inline.text.len(), style));
        }
    }

    fn open(&mut self, container: Container) {
        self.close_inline();
        if self.containers.len() >= MAX_DEPTH {
            self.flattened += 1;
        } else {
            self.containers.push(container);
        }
    }

    fn close(&mut self) {
        self.close_inline();
        if self.flattened > 0 {
            self.flattened -= 1;
            return;
        }
        match self.containers.pop() {
            Some(Container::Quote(blocks)) => self.push_block(Block::Quote(blocks)),
            Some(Container::List { start, items }) => self.push_block(Block::List { start, items }),
            Some(Container::Item(blocks)) => match self.containers.last_mut() {
                Some(Container::List { items, .. }) => items.push(blocks),
                // An item whose list was flattened: keep its content.
                _ => blocks.into_iter().for_each(|block| self.push_block(block)),
            },
            None => {}
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(Tag::Paragraph) => self.close_inline(),
            Event::Start(Tag::Heading { .. }) => {
                self.close_inline();
                self.heading = true;
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) => self.close_inline(),
            Event::Start(Tag::BlockQuote(_)) => self.open(Container::Quote(Vec::new())),
            Event::Start(Tag::List(start)) => self.open(Container::List {
                start,
                items: Vec::new(),
            }),
            Event::Start(Tag::Item) => self.open(Container::Item(Vec::new())),
            Event::End(TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item) => self.close(),
            Event::Start(Tag::CodeBlock(_)) => {
                self.close_inline();
                self.code = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(code) = self.code.take() {
                    let code = code.strip_suffix('\n').unwrap_or(&code).to_string();
                    self.push_block(Block::Code(code.into()));
                }
            }
            Event::Rule => {
                self.close_inline();
                self.push_block(Block::Rule);
            }
            // Table rows become lines of cells.
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => self.close_inline(),
            Event::End(TagEnd::TableCell) => self.text("   ", Style::default()),
            Event::Start(Tag::Strong) => self.style.bold = true,
            Event::End(TagEnd::Strong) => self.style.bold = false,
            Event::Start(Tag::Emphasis) => self.style.italic = true,
            Event::End(TagEnd::Emphasis) => self.style.italic = false,
            Event::Start(Tag::Strikethrough) => self.style.strike = true,
            Event::End(TagEnd::Strikethrough) => self.style.strike = false,
            Event::Start(Tag::Link { dest_url, .. }) => {
                let start = self.inline.as_ref().map_or(0, |inline| inline.text.len());
                self.link = Some((start, link_target(&dest_url)));
            }
            Event::End(TagEnd::Link) => {
                if let Some((start, Some(target))) = self.link.take()
                    && let Some(inline) = &mut self.inline
                    && start < inline.text.len()
                {
                    inline.links.push((start..inline.text.len(), target));
                }
            }
            // Never fetched: the alt text stands in for the image.
            Event::Start(Tag::Image { .. }) => self.image = true,
            Event::End(TagEnd::Image) => self.image = false,
            Event::Text(text) => {
                let style = Style {
                    italic: self.style.italic || self.image,
                    ..self.style
                };
                self.text(&text, style);
            }
            Event::Code(code) => {
                let style = Style {
                    code: true,
                    ..self.style
                };
                self.text(&code, style);
            }
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html, self.style),
            Event::SoftBreak => self.text(" ", Style::default()),
            Event::HardBreak => self.text("\n", Style::default()),
            Event::TaskListMarker(done) => {
                self.text(if done { "☑ " } else { "☐ " }, Style::default())
            }
            Event::FootnoteReference(name) => self.text(&format!("[{name}]"), self.style),
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Block> {
        self.close_inline();
        if let Some(code) = self.code.take() {
            self.push_block(Block::Code(code.into()));
        }
        while !self.containers.is_empty() || self.flattened > 0 {
            self.close();
        }
        self.root
    }
}

pub fn parse(source: &str) -> Markdown {
    let truncated = source.len() > MAX_SOURCE;
    let source = if truncated {
        let mut end = MAX_SOURCE;
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        &source[..end]
    } else {
        source
    };
    // As upstream: GFM's special blockquotes stay off.
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let mut builder = Builder::default();
    for event in Parser::new_ext(source, options) {
        builder.event(event);
    }
    Markdown {
        blocks: builder.finish(),
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(markdown: &str) -> Inline {
        match parse(markdown).blocks.as_slice() {
            [Block::Paragraph(inline)] => inline.clone(),
            other => panic!("not one paragraph: {other:?}"),
        }
    }

    fn depth(blocks: &[Block]) -> usize {
        blocks
            .iter()
            .map(|block| match block {
                Block::Quote(blocks) => 1 + depth(blocks),
                Block::List { items, .. } => {
                    1 + items.iter().map(|item| 1 + depth(item)).max().unwrap_or(0)
                }
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn inline_styles_and_links_cover_their_text() {
        let inline = paragraph("**bold** *it* ~~no~~ `x` [site](https://air.ms)");
        let text_of = |range: &Range<usize>| &inline.text[range.clone()];
        assert_eq!(inline.text, "bold it no x site");
        let styled: Vec<_> = inline
            .styles
            .iter()
            .map(|(range, style)| (text_of(range), *style))
            .collect();
        assert_eq!(
            styled,
            [
                (
                    "bold",
                    Style {
                        bold: true,
                        ..Style::default()
                    }
                ),
                (
                    "it",
                    Style {
                        italic: true,
                        ..Style::default()
                    }
                ),
                (
                    "no",
                    Style {
                        strike: true,
                        ..Style::default()
                    }
                ),
                (
                    "x",
                    Style {
                        code: true,
                        ..Style::default()
                    }
                ),
            ]
        );
        assert_eq!(inline.links.len(), 1);
        assert_eq!(text_of(&inline.links[0].0), "site");
        assert_eq!(inline.links[0].1, "https://air.ms");
    }

    #[test]
    fn only_web_and_mail_links_are_links() {
        for url in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "/relative",
            "air.ms",
            "https:",
        ] {
            let inline = paragraph(&format!("[click]({url})"));
            assert_eq!(inline.text, "click");
            assert!(inline.links.is_empty(), "{url} became a link");
        }
        assert!(link_target("HTTPS://air.ms").is_some());
        assert!(link_target("mailto:ada@air.ms").is_some());
        // Autolinks go through the same check.
        assert_eq!(paragraph("<https://air.ms>").links.len(), 1);
        assert!(paragraph("<javascript:alert(1)>").links.is_empty());
    }

    #[test]
    fn images_and_html_render_as_inert_text() {
        let inline = paragraph("![a cat](https://tracker.example/pixel.png) <img src=x>");
        assert_eq!(inline.text, "a cat <img src=x>");
        assert!(inline.links.is_empty());
        assert!(!inline.text.contains("tracker"));
    }

    #[test]
    fn blocks_keep_their_structure() {
        let blocks =
            parse("# Title\n\n> quoted\n\n- one\n- two\n\n3. three\n\n```\ncode\n```\n\n---")
                .blocks;
        let heading = |text: &str| {
            Block::Heading(Inline {
                text: text.into(),
                ..Inline::default()
            })
        };
        let para = |text: &str| {
            Block::Paragraph(Inline {
                text: text.into(),
                ..Inline::default()
            })
        };
        assert_eq!(
            blocks,
            [
                heading("Title"),
                Block::Quote(vec![para("quoted")]),
                Block::List {
                    start: None,
                    items: vec![vec![para("one")], vec![para("two")]],
                },
                Block::List {
                    start: Some(3),
                    items: vec![vec![para("three")]],
                },
                Block::Code("code".into()),
                Block::Rule,
            ]
        );
    }

    #[test]
    fn hostile_input_stays_bounded() {
        // Deep nesting would otherwise recurse once per level when shown.
        let nested = parse(&format!("{} deep", ">".repeat(10_000)));
        assert!(depth(&nested.blocks) <= MAX_DEPTH);
        let lists = parse(&"- ".repeat(5_000));
        assert!(depth(&lists.blocks) <= MAX_DEPTH);
        // Size is cut at a char boundary, and the cut is reported.
        let huge = parse(&"é".repeat(MAX_SOURCE));
        assert!(huge.truncated);
        assert!(!parse("short").truncated);
    }
}
