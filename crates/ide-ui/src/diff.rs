//! Git diff viewer for the bottom tool window.
//!
//! The comparison is against `HEAD`: the old side comes from the repository
//! (`Repo::file_bytes_at`), the new side from the open document when the file is
//! being edited, else from disk. `ide_vcs::diff_hunks` computes the line hunks;
//! this module turns them into a unified diff with three lines of context.

use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_host::Project;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

const CONTEXT: usize = 3;
const ROW_HEIGHT: f32 = 16.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Added,
    Removed,
    Context,
    Header,
    Gap,
}

#[derive(Clone)]
struct DiffRow {
    kind: RowKind,
    old_no: Option<u32>,
    new_no: Option<u32>,
    text: String,
}

pub struct DiffPanel {
    project: Option<Arc<Project>>,
    file: Option<PathBuf>,
    title: SharedString,
    rows: Vec<DiffRow>,
    notice: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl DiffPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            project: None,
            file: None,
            title: "Diff".into(),
            rows: Vec::new(),
            notice: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        self.project = Some(project.clone());
        self.file = None;
        self.rows.clear();
        self.title = "Diff".into();
        self.notice = None;
        cx.notify();
    }

    /// Show `path` compared with `HEAD`.
    pub fn show(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.file = Some(path.clone());
        self.notice = None;
        self.title = path
            .strip_prefix(&self.project.as_ref().map(|project| project.root.clone()).unwrap_or_default())
            .unwrap_or(&path)
            .display()
            .to_string()
            .into();
        self.rows.clear();
        let Some(project) = self.project.clone() else {
            self.notice = Some("Open a project first".into());
            cx.notify();
            return;
        };
        let new_text = project
            .documents
            .text_of(&path)
            .or_else(|| ide_vfs::load_text(&path).ok().map(|loaded| loaded.text))
            .unwrap_or_default();
        let old_text = {
            let repos = project.repos.read();
            repos
                .for_path(&path)
                .and_then(|repo| repo.file_bytes_at(&path, None).ok().flatten())
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        };
        self.rows = build_rows(&old_text, &new_text);
        if self.rows.is_empty() {
            self.notice = Some("No changes against HEAD".into());
        }
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.file.clone() {
            self.show(path, cx);
        }
    }
}

impl Focusable for DiffPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DiffPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = Rc::new(self.rows.clone());
        let list_theme = theme.clone();
        let refresh = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.refresh(cx));
        let changed = rows.iter().filter(|row| matches!(row.kind, RowKind::Added | RowKind::Removed)).count();
        let hover_bg = theme.border;

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                div()
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::new(IconName::GitCompare).xsmall().text_color(theme.fg_secondary()))
                    .child(div().text_size(px(11.)).text_color(theme.fg_secondary()).child(self.title.clone()))
                    .child(div().flex_1())
                    .when(changed > 0, |this| {
                        this.child(
                            div()
                                .text_size(px(10.5))
                                .text_color(theme.fg_muted())
                                .child(format!("{changed} changed lines · vs HEAD")),
                        )
                    })
                    .child(
                        div()
                            .id("diff-refresh")
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(hover_bg))
                            .on_click(refresh)
                            .child(Icon::new(IconName::RefreshCw).xsmall().text_color(theme.fg_muted())),
                    ),
            )
            .when_some(self.notice.clone(), |this, notice| {
                this.child(div().px_2().py_1().text_size(px(10.5)).text_color(theme.warning).child(notice))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .font_family(theme.mono_font_family.clone())
                    .when(rows.is_empty(), |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .text_color(theme.fg_muted())
                                .child("Select a file in the Git tool window and press Diff"),
                        )
                    })
                    .when(!rows.is_empty(), |this| {
                        this.child(
                            uniform_list("diff-rows", rows.len(), move |range, _window, _cx| {
                                range.map(|index| render_row(&rows[index], &list_theme)).collect()
                            })
                            .size_full(),
                        )
                    }),
            )
            .into_any_element()
    }
}

fn build_rows(old_text: &str, new_text: &str) -> Vec<DiffRow> {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let hunks = ide_vcs::diff_hunks(old_text, new_text);
    if hunks.is_empty() && old_text == new_text {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut old_index = 0usize;
    let mut new_index = 0usize;
    for hunk in hunks {
        let old_start = hunk.old_start as usize;
        let new_start = hunk.start as usize;
        // Context right before the hunk.
        let context_end = old_start.min(old_lines.len());
        let context_start = context_end.saturating_sub(CONTEXT).max(old_index);
        if context_start > old_index && old_index > 0 {
            rows.push(DiffRow { kind: RowKind::Gap, old_no: None, new_no: None, text: "⋯".into() });
        }
        let mut i = context_start.max(old_index);
        while i < context_end {
            rows.push(DiffRow {
                kind: RowKind::Context,
                old_no: Some(i as u32 + 1),
                new_no: Some((new_index + (i - old_index)) as u32 + 1),
                text: old_lines[i].to_owned(),
            });
            i += 1;
        }
        if hunk.old_lines > 0 {
            rows.push(DiffRow {
                kind: RowKind::Header,
                old_no: None,
                new_no: None,
                text: format!("@@ -{},{} +{},{} @@", old_start + 1, hunk.old_lines, new_start + 1, hunk.lines),
            });
        }
        for offset in 0..hunk.old_lines as usize {
            if let Some(line) = old_lines.get(old_start + offset) {
                rows.push(DiffRow {
                    kind: RowKind::Removed,
                    old_no: Some((old_start + offset) as u32 + 1),
                    new_no: None,
                    text: (*line).to_owned(),
                });
            }
        }
        for offset in 0..hunk.lines as usize {
            if let Some(line) = new_lines.get(new_start + offset) {
                rows.push(DiffRow {
                    kind: RowKind::Added,
                    old_no: None,
                    new_no: Some((new_start + offset) as u32 + 1),
                    text: (*line).to_owned(),
                });
            }
        }
        old_index = old_start.saturating_add(hunk.old_lines as usize).min(old_lines.len());
        new_index = new_start.saturating_add(hunk.lines as usize).min(new_lines.len());
    }
    // Trailing context.
    if old_index < old_lines.len() {
        let end = (old_index + CONTEXT).min(old_lines.len());
        for i in old_index..end {
            rows.push(DiffRow {
                kind: RowKind::Context,
                old_no: Some(i as u32 + 1),
                new_no: Some((new_index + (i - old_index)) as u32 + 1),
                text: old_lines[i].to_owned(),
            });
        }
    }
    if rows.is_empty() {
        // Untracked or identical: show the new side as additions.
        for (index, line) in new_lines.iter().enumerate() {
            rows.push(DiffRow {
                kind: RowKind::Added,
                old_no: None,
                new_no: Some(index as u32 + 1),
                text: (*line).to_owned(),
            });
        }
    }
    rows
}

fn render_row(row: &DiffRow, theme: &gpui_kit::component::Theme) -> AnyElement {
    let (background, foreground, sign) = match row.kind {
        RowKind::Added => (Some(tint(theme.success, 0.14)), theme.fg(), "+"),
        RowKind::Removed => (Some(tint(theme.danger, 0.14)), theme.fg(), "-"),
        RowKind::Context => (None, theme.fg_secondary(), " "),
        RowKind::Header => (Some(tint(theme.info, 0.12)), theme.info, "@"),
        RowKind::Gap => (None, theme.fg_muted(), "⋯"),
    };
    let bg = background.unwrap_or_else(|| theme.background);
    let old_no = row.old_no.map(|number| number.to_string()).unwrap_or_default();
    let new_no = row.new_no.map(|number| number.to_string()).unwrap_or_default();
    div()
        .h(px(ROW_HEIGHT))
        .w_full()
        .flex()
        .items_center()
        .whitespace_nowrap()
        .bg(bg)
        .text_size(px(11.5))
        .child(div().w(px(46.)).pr_2().text_right().text_color(theme.fg_muted()).child(old_no))
        .child(div().w(px(46.)).pr_2().text_right().text_color(theme.fg_muted()).child(new_no))
        .child(div().w(px(14.)).text_color(foreground).child(sign))
        .child(div().flex_1().min_w_0().overflow_hidden().text_color(foreground).child(row.text.clone()))
        .into_any_element()
}

fn tint(color: gpui_kit::Hsla, alpha: f32) -> gpui_kit::Hsla {
    let mut tinted = color;
    tinted.a = alpha;
    tinted
}
