//! About, licenses, logs and debug info, shown in place of the settings rows.
//! Everything here is local: nothing is sent, and a link opens only through
//! the usual confirm dialog.

use std::ops::Range;

use gpui::{
    ClipboardItem, EventEmitter, ScrollHandle, ScrollStrategy, SharedString,
    UniformListScrollHandle, uniform_list,
};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    Task, Window, div, px,
};

use crate::about::{self, License, Page};
use crate::actions::{
    CloseSettings, CopyInfo, SelectFirst, SelectLast, SelectNext, SelectPrev, SettingNext,
};
use crate::session::{DebugInfo, Session};
use crate::ui::{key_context, markdown_view};

pub enum InfoEvent {
    Close,
}

/// A row of the about page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Source,
    Problem,
    Licenses,
    Logs,
    Debug,
}

/// Where the screen is: a page, or one license of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Showing {
    Page(Page),
    License(usize),
}

pub struct InfoView {
    focus_handle: FocusHandle,
    showing: Showing,
    /// The row or list line selected, on pages that have them.
    selected: usize,
    /// The license list, read when first needed.
    licenses: Vec<License>,
    /// The log lines when the page opened.
    logs: Vec<SharedString>,
    list_scroll: UniformListScrollHandle,
    text_scroll: ScrollHandle,
    session: Option<Entity<Session>>,
    debug: Option<Result<DebugInfo, SharedString>>,
    loading: Option<Task<()>>,
}

impl EventEmitter<InfoEvent> for InfoView {}

impl Focusable for InfoView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl InfoView {
    pub fn new(session: Option<Entity<Session>>, page: Page, cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            focus_handle: cx.focus_handle(),
            showing: Showing::Page(Page::About),
            selected: 0,
            licenses: Vec::new(),
            logs: Vec::new(),
            list_scroll: UniformListScrollHandle::new(),
            text_scroll: ScrollHandle::new(),
            session,
            debug: None,
            loading: None,
        };
        view.open(page, cx);
        view
    }

    pub fn open(&mut self, page: Page, cx: &mut Context<Self>) {
        self.showing = Showing::Page(page);
        self.selected = 0;
        self.text_scroll.set_offset(gpui::point(px(0.), px(0.)));
        match page {
            Page::About => {}
            Page::Licenses => {
                if self.licenses.is_empty() {
                    self.licenses = about::licenses();
                }
            }
            Page::Logs => {
                self.show_logs(crate::logs::latest());
            }
            Page::Debug => self.load_debug(cx),
        }
        cx.notify();
    }

    fn show_logs(&mut self, lines: Vec<String>) {
        self.logs = lines.into_iter().map(Into::into).collect();
        // The newest are what is being looked for.
        self.selected = self.logs.len().saturating_sub(1);
        self.list_scroll
            .scroll_to_item(self.selected, ScrollStrategy::Bottom);
    }

    fn load_debug(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            self.debug = Some(Err("That needs an account".into()));
            return;
        };
        self.debug = None;
        let load = session.read(cx).debug_info(cx);
        self.loading = Some(cx.spawn(async move |this, cx| {
            let info = load.await.map_err(SharedString::from);
            _ = this.update(cx, |this, cx| {
                this.debug = Some(info);
                cx.notify();
            });
        }));
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::Source, Row::Problem, Row::Licenses, Row::Logs];
        if self.session.is_some() {
            rows.push(Row::Debug);
        }
        rows
    }

    fn line_count(&self) -> usize {
        match self.showing {
            Showing::Page(Page::About) => self.rows().len(),
            Showing::Page(Page::Licenses) => self.licenses.len(),
            Showing::Page(Page::Logs) => self.logs.len(),
            Showing::Page(Page::Debug) | Showing::License(_) => 0,
        }
    }

    fn select(&mut self, by: isize, cx: &mut Context<Self>) {
        let count = self.line_count();
        if count == 0 {
            // A page of text: move it instead.
            let mut offset = self.text_scroll.offset();
            let moved = offset.y - px(by.clamp(-1, 1) as f32 * 80.);
            let lowest = -self.text_scroll.max_offset().y;
            offset.y = if by == isize::MIN {
                px(0.)
            } else if by == isize::MAX {
                lowest
            } else {
                moved.clamp(lowest, px(0.))
            };
            self.text_scroll.set_offset(offset);
        } else {
            self.selected = self
                .selected
                .saturating_add_signed(by)
                .min(count.saturating_sub(1));
            self.list_scroll
                .scroll_to_item(self.selected, ScrollStrategy::Center);
        }
        cx.notify();
    }

    /// Enter: what the selected line leads to.
    fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.showing {
            Showing::Page(Page::About) => match self.rows().get(self.selected).copied() {
                Some(Row::Source) if !about::REPOSITORY.is_empty() => {
                    markdown_view::confirm_open(vec![about::REPOSITORY.into()], window, cx)
                }
                Some(Row::Problem) => {
                    if let Some(issues) = about::issues() {
                        markdown_view::confirm_open(vec![issues.into()], window, cx);
                    }
                }
                Some(Row::Licenses) => self.open(Page::Licenses, cx),
                Some(Row::Logs) => self.open(Page::Logs, cx),
                Some(Row::Debug) => self.open(Page::Debug, cx),
                Some(Row::Source) | None => {}
            },
            Showing::Page(Page::Licenses) if self.selected < self.licenses.len() => {
                self.showing = Showing::License(self.selected);
                self.text_scroll.set_offset(gpui::point(px(0.), px(0.)));
                cx.notify();
            }
            _ => {}
        }
    }

    /// Escape: a step back, and out from the about page.
    fn back(&mut self, cx: &mut Context<Self>) {
        match self.showing {
            Showing::Page(Page::About) => cx.emit(InfoEvent::Close),
            Showing::License(ix) => {
                self.showing = Showing::Page(Page::Licenses);
                self.selected = ix;
                self.list_scroll.scroll_to_item(ix, ScrollStrategy::Center);
                cx.notify();
            }
            Showing::Page(_) => self.open(Page::About, cx),
        }
    }

    /// What copying this page would take (seam tests).
    #[cfg(all(test, feature = "seam"))]
    pub fn shown_text(&self) -> Option<String> {
        self.text()
    }

    /// What copying takes on this page.
    fn text(&self) -> Option<String> {
        match self.showing {
            Showing::Page(Page::About) => Some(format!("Air Desktop {}", about::version())),
            Showing::Page(Page::Licenses) => None,
            Showing::Page(Page::Logs) => Some(
                self.logs
                    .iter()
                    .map(SharedString::as_ref)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Showing::Page(Page::Debug) => match &self.debug {
                Some(Ok(info)) => Some(
                    info.rows()
                        .iter()
                        .map(|(label, value)| format!("{label}: {value}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            },
            Showing::License(ix) => self.licenses.get(ix).map(|license| license.text.clone()),
        }
    }

    fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            crate::ui::show_info("Copied".into(), window, cx);
        }
    }

    fn line(&self, ix: usize, text: SharedString, focused: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        div()
            .h(px(24.))
            .px_3()
            .flex_none()
            .truncate()
            .text_sm()
            .when(ix == self.selected, |line| {
                line.rounded_md()
                    .bg(if focused { theme.accent } else { theme.muted })
            })
            .child(text)
            .into_any_element()
    }

    fn render_about(&self, focused: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let row = |ix: usize, label: &'static str, value: String| {
            h_flex()
                .gap_4()
                .px_3()
                .py_1p5()
                .text_sm()
                .rounded_md()
                .when(ix == self.selected, |row| {
                    row.bg(if focused { theme.accent } else { theme.muted })
                })
                .child(div().w(px(180.)).flex_none().child(label))
                .child(
                    div()
                        .min_w_0()
                        .text_color(theme.muted_foreground)
                        .child(value),
                )
        };
        let unpublished = "not published yet";
        let mut list = v_flex()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Air Desktop"),
            )
            .child(div().text_sm().child(about::version()))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "A keyboard-first desktop client for the Air messenger. Free software under \
                 the GNU Affero General Public License, version 3 or later, with no warranty. \
                 It is made with Air's own crates (aircoreclient, aircommon, airapiclient, airprotos), \
                 under the same license.",
            ));
        let mut rows = v_flex().gap_0p5();
        for (ix, kind) in self.rows().into_iter().enumerate() {
            rows = rows.child(match kind {
                Row::Source => row(
                    ix,
                    "Source code",
                    match about::REPOSITORY {
                        "" => unpublished.to_string(),
                        repository => repository.to_string(),
                    },
                ),
                Row::Problem => row(
                    ix,
                    "Report a problem",
                    about::issues().unwrap_or_else(|| unpublished.to_string()),
                ),
                Row::Licenses => row(ix, "Licenses", "of the crates this is made of".into()),
                Row::Logs => row(ix, "Logs", "kept in memory only".into()),
                Row::Debug => row(ix, "Debug info", "of this account".into()),
            });
        }
        list = list.child(rows);
        list.into_any_element()
    }

    fn render_license(&self, ix: usize, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let Some(license) = self.licenses.get(ix) else {
            return div().into_any_element();
        };
        let used_by = license
            .used_by
            .iter()
            .map(|used| format!("{} {}", used.name, used.version))
            .collect::<Vec<_>>()
            .join(", ");
        v_flex()
            .gap_3()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(license.name.clone()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(used_by),
            )
            .child(div().text_xs().child(license.text.clone()))
            .into_any_element()
    }

    fn render_debug(&self, cx: &App) -> AnyElement {
        let theme = cx.theme();
        match &self.debug {
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Loading…")
                .into_any_element(),
            Some(Err(error)) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(error.clone())
                .into_any_element(),
            Some(Ok(info)) => v_flex()
                .gap_1()
                .children(info.rows().into_iter().map(|(label, value)| {
                    h_flex()
                        .gap_4()
                        .text_sm()
                        .child(div().w(px(240.)).flex_none().child(label))
                        .child(div().min_w_0().child(value))
                }))
                .into_any_element(),
        }
    }
}

impl Render for InfoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let theme = cx.theme();
        let (title, hint): (SharedString, &str) = match self.showing {
            Showing::Page(Page::About) => ("About".into(), "enter opens · esc back"),
            Showing::Page(Page::Licenses) => (
                format!("Licenses ({})", self.licenses.len()).into(),
                "enter reads · esc back",
            ),
            Showing::Page(Page::Logs) => ("Logs".into(), "y copies · esc back"),
            Showing::Page(Page::Debug) => ("Debug info".into(), "y copies · esc back"),
            Showing::License(_) => ("License".into(), "y copies · esc back"),
        };
        let long_list = matches!(
            self.showing,
            Showing::Page(Page::Licenses) | Showing::Page(Page::Logs)
        );
        let warning = matches!(self.showing, Showing::Page(Page::Logs)).then(|| {
            div()
                .flex_none()
                .px_4()
                .py_2()
                .text_sm()
                .text_color(theme.warning)
                .child("Logs can name your contacts. Check them before sharing.")
        });
        let body: AnyElement = if long_list {
            let count = self.line_count();
            uniform_list(
                "info-lines",
                count,
                cx.processor(move |this, range: Range<usize>, window, cx| {
                    let focused = this.focus_handle.is_focused(window);
                    range
                        .map(|ix| {
                            let text: SharedString = match this.showing {
                                Showing::Page(Page::Logs) => this.logs[ix].clone(),
                                _ => {
                                    let license = &this.licenses[ix];
                                    format!("{} · {} crates", license.name, license.used_by.len())
                                        .into()
                                }
                            };
                            this.line(ix, text, focused, cx)
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.list_scroll)
            .flex_1()
            .p_2()
            .into_any_element()
        } else {
            let content = match self.showing {
                Showing::Page(Page::About) => self.render_about(focused, cx),
                Showing::License(ix) => self.render_license(ix, cx),
                _ => self.render_debug(cx),
            };
            div()
                .id("info-text")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.text_scroll)
                .p_4()
                .child(content)
                .into_any_element()
        };
        v_flex()
            .key_context(key_context("Info", cx))
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.select(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select(isize::MIN, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.select(isize::MAX, cx)))
            .on_action(cx.listener(|this, _: &SettingNext, window, cx| this.activate(window, cx)))
            .on_action(cx.listener(|this, _: &CopyInfo, window, cx| this.copy(window, cx)))
            .on_action(cx.listener(|this, _: &CloseSettings, _, cx| this.back(cx)))
            .size_full()
            .bg(theme.background)
            .child(
                h_flex()
                    .flex_none()
                    .h(px(52.))
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title)
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    ),
            )
            .children(warning)
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(
        page: Page,
        cx: &mut gpui::TestAppContext,
    ) -> (Entity<InfoView>, &mut gpui::VisualTestContext) {
        let (window, view) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::settings::SettingsStore::open(None, cx);
            crate::theme::init(None, cx).unwrap();
            gpui_kit::open_window(Default::default(), cx, |_, cx| {
                cx.new(|cx| InfoView::new(None, page, cx))
            })
            .unwrap()
        });
        (
            view,
            gpui::VisualTestContext::from_window(window, cx).into_mut(),
        )
    }

    #[gpui::test]
    fn the_pages_lead_to_a_license_and_back_out(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open(Page::About, cx);
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        cx.update(|_, cx| {
            let closed = closed.clone();
            cx.subscribe(&view, move |_, event, _| {
                let InfoEvent::Close = event;
                closed.set(true);
            })
            .detach();
        });
        view.update_in(cx, |view, window, cx| {
            assert_eq!(view.rows()[2], Row::Licenses);
            view.selected = 2;
            view.activate(window, cx);
            assert_eq!(view.showing, Showing::Page(Page::Licenses));
            assert!(!view.licenses.is_empty());
            view.select(1, cx);
            view.activate(window, cx);
            assert_eq!(view.showing, Showing::License(1));
            assert_eq!(view.text(), Some(view.licenses[1].text.clone()));
            view.back(cx);
            assert_eq!(view.showing, Showing::Page(Page::Licenses));
            assert_eq!(view.selected, 1, "back on the line it came from");
            view.back(cx);
            assert_eq!(view.showing, Showing::Page(Page::About));
            view.back(cx);
        });
        cx.run_until_parked();
        assert!(closed.get());
    }

    #[gpui::test]
    fn without_an_account_there_is_no_debug_row(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open(Page::About, cx);
        view.read_with(cx, |view, _| assert!(!view.rows().contains(&Row::Debug)));
    }

    /// Without an account the debug page says so, rather than loading on.
    #[gpui::test]
    fn the_debug_page_without_an_account_refuses(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open(Page::Debug, cx);
        view.read_with(cx, |view, _| {
            assert!(matches!(view.debug, Some(Err(_))));
        });
    }

    #[gpui::test]
    fn logs_open_at_the_newest_and_copy_as_shown(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open(Page::Logs, cx);
        view.update(cx, |view, cx| {
            view.show_logs(vec!["first".into(), "second".into()]);
            assert_eq!(view.selected, 1);
            assert_eq!(view.text().as_deref(), Some("first\nsecond"));
            view.select(isize::MIN, cx);
            assert_eq!(view.selected, 0);
        });
    }
}
