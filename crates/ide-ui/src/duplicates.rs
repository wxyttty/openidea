//! Locate Duplicates: token-based clone detection across the project
//! (`ide-analysis`'s CPD-style search).
//!
//! The panel mirrors IDEA's: a settings row (minimum fragment length, whether
//! identifiers and literals are anonymised so renamed copies still match), a
//! Run button that scans on the host runtime, and the groups found — each with
//! its size in tokens and lines, its language, and one row per occurrence that
//! opens the file at the fragment.
//!
//! The scan reads unsaved buffers first (`load` in `ide-host`), so a fragment
//! the user just edited is found as it is on screen.

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_analysis::duplicates::{DuplicateOptions, DuplicateGroup};
use ide_host::Host;
use std::path::PathBuf;
use std::sync::Arc;

/// One row of the result list: a group header or an occurrence.
enum DuplicateRow {
    Group { group: usize },
    Fragment { group: usize, fragment: usize },
}

pub struct DuplicatesPanel {
    host: Arc<Host>,
    editor: Entity<EditorArea>,
    options: DuplicateOptions,
    groups: Vec<DuplicateGroup>,
    rows: Vec<DuplicateRow>,
    /// Rendered rows of the last frame (the list closure cannot borrow self).
    render_rows: Vec<(bool, SharedString, SharedString, Option<(PathBuf, u32, u32)>)>,
    running: bool,
    notice: Option<SharedString>,
    summary: Option<SharedString>,
    task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

impl DuplicatesPanel {
    pub fn new(host: Arc<Host>, editor: Entity<EditorArea>, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            editor,
            options: DuplicateOptions::default(),
            groups: Vec::new(),
            rows: Vec::new(),
            render_rows: Vec::new(),
            running: false,
            notice: None,
            summary: None,
            task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Scans the project. Runs on the host runtime: a project-wide tokenisation
    /// must not block the frame.
    pub fn run(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        self.running = true;
        self.notice = None;
        let host = self.host.clone();
        let runtime = host.runtime.clone();
        let options = DuplicateOptions {
            min_tokens: self.options.min_tokens,
            anonymize_identifiers: self.options.anonymize_identifiers,
            anonymize_literals: self.options.anonymize_literals,
        };
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let report = runtime
                .spawn(async move {
                    ide_host::ops::analysis::analysis_locate_duplicates(
                        &host,
                        "main",
                        ide_core::RequestId::next(),
                        ide_search::SearchScope::Project,
                        options,
                    )
                    .await
                })
                .await;
            this.update(cx, |panel, cx| {
                panel.running = false;
                match report {
                    Ok(Ok(report)) => {
                        panel.summary = Some(
                            format!(
                                "{} group(s) · {} files · {} tokens · {} ms",
                                report.groups.len(),
                                report.files,
                                report.tokens,
                                report.elapsed_ms
                            )
                            .into(),
                        );
                        panel.notice = report.truncated.then(|| {
                            SharedString::from("More groups were found than listed; raise the minimum length")
                        });
                        panel.set_groups(report.groups);
                    }
                    Ok(Err(err)) => panel.notice = Some(format!("Locate Duplicates failed: {err}").into()),
                    Err(err) => panel.notice = Some(format!("Locate Duplicates task failed: {err}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn set_groups(&mut self, groups: Vec<DuplicateGroup>) {
        self.rows.clear();
        for (index, group) in groups.iter().enumerate() {
            self.rows.push(DuplicateRow::Group { group: index });
            for fragment in 0..group.fragments.len() {
                self.rows.push(DuplicateRow::Fragment { group: index, fragment });
            }
        }
        self.groups = groups;
    }

    pub fn groups(&self) -> &[DuplicateGroup] {
        &self.groups
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    pub fn options(&self) -> DuplicateOptions {
        DuplicateOptions {
            min_tokens: self.options.min_tokens,
            anonymize_identifiers: self.options.anonymize_identifiers,
            anonymize_literals: self.options.anonymize_literals,
        }
    }

    /// Changes the minimum fragment length; a scan picks it up.
    pub fn set_min_tokens(&mut self, min_tokens: u32, cx: &mut Context<Self>) {
        self.options.min_tokens = min_tokens.clamp(10, 500);
        cx.notify();
    }

    /// Toggles identifier anonymisation (finds copies with renamed variables).
    pub fn toggle_identifiers(&mut self, cx: &mut Context<Self>) {
        self.options.anonymize_identifiers = !self.options.anonymize_identifiers;
        cx.notify();
    }

    /// Toggles literal anonymisation (finds copies with different constants).
    pub fn toggle_literals(&mut self, cx: &mut Context<Self>) {
        self.options.anonymize_literals = !self.options.anonymize_literals;
        cx.notify();
    }
}

impl Focusable for DuplicatesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DuplicatesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let row_theme = theme.clone();
        let hover = theme.list_hover;
        // Flatten the result into rows the list closure can own.
        self.render_rows = self
            .rows
            .iter()
            .map(|row| match row {
                DuplicateRow::Group { group } => {
                    let group = &self.groups[*group];
                    (
                        true,
                        format!("{} tokens · {} lines · {}", group.tokens, group.lines, group.language.0).into(),
                        format!("{} occurrence(s)", group.fragments.len()).into(),
                        None,
                    )
                }
                DuplicateRow::Fragment { group, fragment } => {
                    let fragment = &self.groups[*group].fragments[*fragment];
                    (
                        false,
                        fragment.rel_path.clone().into(),
                        format!(
                            "{}{}",
                            fragment.range.start.line + 1,
                            if fragment.text.is_empty() { String::new() } else { format!(" · {}", first_line(&fragment.text)) }
                        )
                        .into(),
                        Some((fragment.path.0.clone(), fragment.range.start.line, fragment.range.start.col)),
                    )
                }
            })
            .collect();
        let rows = self.render_rows.clone();
        let editor = self.editor.clone();
        let open: std::rc::Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)> = {
            let editor = editor;
            std::rc::Rc::new(move |(path, line, col): (PathBuf, u32, u32), window, cx| {
                editor.update(cx, |editor, cx| editor.open_file_at(path, line, col, window, cx));
            })
        };

        let run = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.run(cx));
        let longer = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
            let next = this.options.min_tokens + 20;
            this.set_min_tokens(next, cx);
        });
        let shorter = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
            let next = this.options.min_tokens.saturating_sub(20);
            this.set_min_tokens(next, cx);
        });
        let ids = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.toggle_identifiers(cx));
        let literals = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.toggle_literals(cx));
        let budget = self.options.min_tokens;

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                div()
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child("Duplicates"),
                    )
                    .child(
                        div()
                            .id("duplicates-longer")
                            .h(px(18.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(hover))
                            .on_click(longer)
                            .text_size(px(10.))
                            .text_color(theme.fg_secondary())
                            .child("+20"),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.fg_muted())
                            .child(format!("min {budget} tokens")),
                    )
                    .child(
                        div()
                            .id("duplicates-shorter")
                            .h(px(18.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(hover))
                            .on_click(shorter)
                            .text_size(px(10.))
                            .text_color(theme.fg_secondary())
                            .child("-20"),
                    )
                    .child(toggle("duplicates-ids", "ignore names", self.options.anonymize_identifiers, &theme, hover, ids))
                    .child(toggle(
                        "duplicates-literals",
                        "ignore literals",
                        self.options.anonymize_literals,
                        &theme,
                        hover,
                        literals,
                    ))
                    .child(div().flex_1())
                    .when_some(self.summary.clone(), |this, summary| {
                        this.child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(summary))
                    })
                    .child(Button::new("duplicates-run").label("Find Duplicates").small().on_click(run)),
            )
            .when_some(self.notice.clone(), |this, notice| {
                let theme = theme.clone();
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .bg(theme.warning.opacity(0.12))
                        .text_size(px(10.5))
                        .text_color(theme.warning)
                        .child(notice),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(rows.is_empty(), |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .text_color(theme.fg_muted())
                                .child(if self.running { "Scanning…" } else { "No duplicates found yet" }),
                        )
                    })
                    .when(!rows.is_empty(), |this| {
                        let rows = rows.clone();
                        this.child(
                            div()
                                .id("duplicates-list")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p_1()
                                .flex()
                                .flex_col()
                                .children(rows.into_iter().enumerate().map(move |(index, (is_group, primary, secondary, target))| {
                                    let hover = row_theme.list_hover;
                                    let open = open.clone();
                                    div()
                                        .id(ElementId::Name(format!("duplicate-row-{index}").into()))
                                        .h(px(22.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .rounded(px(3.))
                                        .when(is_group, |this| this.bg(row_theme.muted))
                                        .when(!is_group, |this| this.cursor_pointer().hover(move |this| this.bg(hover)))
                                        .when_some(target, |this, target| {
                                            this.on_click(move |_: &ClickEvent, window, cx| open(target.clone(), window, cx))
                                        })
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(px(if is_group { 11. } else { 11.5 }))
                                                .when(is_group, |this| this.font_weight(FontWeight::SEMIBOLD))
                                                .text_color(if is_group { row_theme.fg_secondary() } else { row_theme.foreground })
                                                .child(primary),
                                        )
                                        .child(
                                            div()
                                                .max_w(px(320.))
                                                .truncate()
                                                .text_size(px(10.5))
                                                .text_color(row_theme.fg_muted())
                                                .child(secondary),
                                        )
                                })),
                        )
                    }),
            )
    }
}

fn toggle(
    id: &'static str,
    label: &'static str,
    active: bool,
    theme: &Theme,
    hover: Hsla,
    click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(ElementId::Name(id.into()))
        .h(px(18.))
        .px_1()
        .flex()
        .items_center()
        .rounded(px(3.))
        .cursor_pointer()
        .when(active, |this| this.bg(theme.accent.opacity(0.22)))
        .hover(move |this| this.bg(hover))
        .on_click(click)
        .text_size(px(9.5))
        .text_color(if active { theme.foreground } else { theme.fg_muted() })
        .child(label)
        .into_any_element()
}

/// The first line of a fragment, trimmed, for the second column.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::first_line;

    #[test]
    fn first_line_strips_indentation() {
        assert_eq!(first_line("    let x = 1;\n    let y = 2;"), "let x = 1;");
        assert_eq!(first_line(""), "");
        assert_eq!(first_line("\n\nheader"), "");
    }

    #[test]
    fn first_line_is_capped() {
        let long = "a".repeat(200);
        assert_eq!(first_line(&long).chars().count(), 80);
    }
}

/// The panel's icon, used where the square is a tool stripe.
pub const ICON: IconName = IconName::Copy;
