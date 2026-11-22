//! The Git tool window: changed files of every repository in the project.
//!
//! Status comes from `ide-vcs` (`Repos::all()` / `Repo::status()`), refreshed on
//! project open and on `IdeEvent::VcsChanged`. Clicking a file opens it; the
//! panel is read-only for now (stage/commit/diff land with the diff viewer).

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_host::Project;
use ide_vcs::{BranchInfo, ChangeKind};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone)]
struct VcsRow {
    path: PathBuf,
    rel: String,
    kind: Option<ChangeKind>,
    staged: Option<ChangeKind>,
}

/// Emitted when the user asks to see a file's diff against HEAD.
pub struct OpenDiffRequest(pub PathBuf);

pub struct VcsPanel {
    project: Option<Arc<Project>>,
    editor: Entity<EditorArea>,
    commit_message: Entity<InputState>,
    rows: Vec<VcsRow>,
    branch: Option<SharedString>,
    upstream: Option<SharedString>,
    repos: usize,
    message: Option<SharedString>,
    /// Line hunks of the file whose hunks are listed, with its current text.
    hunks: Vec<HunkRow>,
    /// The file the hunks belong to.
    hunk_file: Option<PathBuf>,
    focus_handle: FocusHandle,
}

/// One line hunk of the inspected file, for the change list.
#[derive(Clone)]
struct HunkRow {
    index: usize,
    title: SharedString,
    /// Zero-based first line in the current text (deletions: the following line).
    start: u32,
    lines: u32,
}

impl VcsPanel {
    pub fn new(editor: Entity<EditorArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let commit_message = cx.new(|cx| InputState::new(window, cx).placeholder("Commit message"));
        Self {
            project: None,
            editor,
            commit_message,
            rows: Vec::new(),
            branch: None,
            upstream: None,
            repos: 0,
            message: None,
            hunks: Vec::new(),
            hunk_file: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        self.project = Some(project.clone());
        self.refresh(cx);
    }

    pub fn branch(&self) -> Option<SharedString> {
        self.branch.clone()
    }

    /// Stage every changed file of the primary repository.
    fn stage_all(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let paths: Vec<PathBuf> = self.rows.iter().map(|row| row.path.clone()).collect();
        if paths.is_empty() {
            self.message = Some("Nothing to stage".into());
            cx.notify();
            return;
        }
        let repos = project.repos.read();
        let Some(repo) = repos.primary() else {
            self.message = Some("No Git repository".into());
            cx.notify();
            return;
        };
        match repo.stage(&paths) {
            Ok(()) => self.message = Some(format!("Staged {} files", paths.len()).into()),
            Err(err) => self.message = Some(format!("Stage failed: {err}").into()),
        }
        drop(repos);
        self.refresh(cx);
    }

    /// Lists the line hunks of `path` (the editor's buffer when open, else the
    /// file on disk) so the user can stage them one by one.
    pub fn inspect_hunks(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let text = self
            .editor
            .read(cx)
            .active_text()
            .filter(|_| self.editor.read(cx).active_info().is_some_and(|info| info.path == path))
            .or_else(|| std::fs::read_to_string(&path).ok())
            .unwrap_or_default();
        let repos = project.repos.read();
        let Some(repo) = repos.primary() else {
            self.message = Some("No Git repository".into());
            cx.notify();
            return;
        };
        match repo.line_hunks(&path, &text, false, encoding_rs::UTF_8) {
            Ok(hunks) => {
                self.hunk_file = Some(path);
                self.hunks = hunks
                    .into_iter()
                    .enumerate()
                    .map(|(index, hunk)| HunkRow {
                        index,
                        title: hunk_title(hunk.kind, hunk.start, hunk.lines, hunk.old_lines).into(),
                        start: hunk.start,
                        lines: hunk.lines,
                    })
                    .collect();
                self.message = Some(format!("{} hunk(s)", self.hunks.len()).into());
            }
            Err(err) => self.message = Some(format!("Cannot read hunks: {err}").into()),
        }
        drop(repos);
        cx.notify();
    }

    /// Stages exactly the listed hunks of the inspected file.
    pub fn stage_hunks(&mut self, selected: Vec<usize>, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(path) = self.hunk_file.clone() else { return };
        if selected.is_empty() {
            self.message = Some("Select at least one hunk".into());
            cx.notify();
            return;
        }
        let text = self
            .editor
            .read(cx)
            .active_text()
            .filter(|_| self.editor.read(cx).active_info().is_some_and(|info| info.path == path))
            .or_else(|| std::fs::read_to_string(&path).ok())
            .unwrap_or_default();
        let repos = project.repos.read();
        let Some(repo) = repos.primary() else {
            self.message = Some("No Git repository".into());
            cx.notify();
            return;
        };
        // The backend takes the hunks' indices in the same order `line_hunks`
        // returned them, which is what `HunkRow::index` holds.
        let count = selected.len();
        match repo.stage_hunks(&path, &text, &selected, encoding_rs::UTF_8) {
            Ok(()) => {
                self.message = Some(format!("Staged {count} hunk(s)").into());
                self.hunks.clear();
                self.hunk_file = None;
            }
            Err(err) => self.message = Some(format!("Stage hunk failed: {err}").into()),
        }
        drop(repos);
        self.refresh(cx);
    }

    /// Commit the staged changes with the message field.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let message = self.commit_message.read(cx).value().trim().to_owned();
        if message.is_empty() {
            self.message = Some("Type a commit message first".into());
            cx.notify();
            return;
        }
        let repos = project.repos.read();
        let Some(repo) = repos.primary() else {
            self.message = Some("No Git repository".into());
            cx.notify();
            return;
        };
        match repo.commit(&message, false, false) {
            Ok(id) => {
                let short: String = id.chars().take(8).collect();
                self.message = Some(format!("Committed {short}").into());
                self.commit_message.update(cx, |input, cx| input.set_value("", window, cx));
            }
            Err(err) => self.message = Some(format!("Commit failed: {err}").into()),
        }
        drop(repos);
        self.refresh(cx);
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            self.rows.clear();
            self.branch = None;
            cx.notify();
            return;
        };
        let repos = project.repos.read();
        self.repos = repos.all().len();
        let mut rows = Vec::new();
        let mut branch: Option<SharedString> = None;
        let mut upstream = None;
        for repo in repos.all() {
            if branch.is_none()
                && let Ok(BranchInfo { head, upstream: up, .. }) = repo.branch_info()
            {
                branch = head.map(Into::into);
                upstream = up.map(Into::into);
            }
            let Ok(statuses) = repo.status() else { continue };
            for status in statuses {
                rows.push(VcsRow {
                    path: status.path.0.clone(),
                    rel: status.rel_path,
                    kind: status.unstaged,
                    staged: status.staged,
                });
            }
        }
        rows.sort_by(|a, b| a.rel.cmp(&b.rel));
        tracing::debug!(files = rows.len(), repos = self.repos, branch = ?branch, "vcs refresh");
        self.rows = rows;
        self.branch = branch;
        self.upstream = upstream;
        self.message = if self.repos == 0 { Some("No Git repository".into()) } else { None };
        cx.notify();
    }
}

impl EventEmitter<OpenDiffRequest> for VcsPanel {}

impl Focusable for VcsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for VcsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = Rc::new(self.rows.clone());
        let list_theme = theme.clone();
        let on_open: Rc<dyn Fn(PathBuf, &mut Window, &mut App)> =
            Rc::new(cx.processor(|this: &mut Self, path: PathBuf, window, cx| {
                let editor = this.editor.clone();
                editor.update(cx, |editor, cx| editor.open_file(path, window, cx));
            }));
        let on_diff: Rc<dyn Fn(PathBuf, &mut Window, &mut App)> =
            Rc::new(cx.processor(|_this: &mut Self, path: PathBuf, _window, cx| {
                cx.emit(OpenDiffRequest(path));
            }));
        let on_hunks: Rc<dyn Fn(PathBuf, &mut Window, &mut App)> =
            Rc::new(cx.processor(|this: &mut Self, path: PathBuf, _window, cx| {
                this.inspect_hunks(path, cx);
            }));
        let refresh = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.refresh(cx));
        let branch_label = self.branch.clone().unwrap_or_else(|| "No branch".into());
        let counts = self.rows.len();
        let hover_bg = theme.border;
        let stage_hover = theme.list_hover;
        let commit_hover = theme.primary_hover;
        let hover_theme = theme.clone();

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
                    .justify_between()
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::GitBranch).xsmall().text_color(theme.fg_secondary()))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.fg_secondary())
                                    .child(branch_label),
                            )
                            .when(self.upstream.is_some(), |this| {
                                this.child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(theme.fg_muted())
                                        .child(self.upstream.clone().unwrap_or_default()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("vcs-refresh")
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
            .child(
                div()
                    .h(px(24.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(11.))
                    .text_color(theme.fg_muted())
                    .child(format!("{counts} changed files · {} repositories", self.repos)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(rows.is_empty(), |this| {
                        let empty_theme = hover_theme.clone();
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .text_color(empty_theme.fg_muted())
                                .child(self.message.clone().unwrap_or_else(|| "No local changes".into())),
                        )
                    })
                    .when(!rows.is_empty(), |this| {
                        let rows = rows.clone();
                        let on_open = on_open.clone();
                        let on_diff = on_diff.clone();
                        let on_hunks = on_hunks.clone();
                        this.child(
                            uniform_list("vcs-files", rows.len(), move |range, _window, _cx| {
                                range
                                    .map(|index| {
                                        render_row(
                                            &rows[index],
                                            &list_theme,
                                            on_open.clone(),
                                            on_diff.clone(),
                                            on_hunks.clone(),
                                        )
                                    })
                                    .collect()
                            })
                            .size_full(),
                        )
                    }),
            )
            .when(!self.hunks.is_empty(), |this| {
                let theme = theme.clone();
                let hover = theme.list_hover;
                let all: Vec<usize> = self.hunks.iter().map(|hunk| hunk.index).collect();
                let stage_all = all.clone();
                this.child(
                    div()
                        .border_t_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(px(10.5))
                                        .text_color(theme.fg_secondary())
                                        .child(format!(
                                            "{} · {} change(s)",
                                            self.hunk_file
                                                .as_ref()
                                                .and_then(|path| path.file_name())
                                                .map(|name| name.to_string_lossy().into_owned())
                                                .unwrap_or_default(),
                                            self.hunks.len()
                                        )),
                                )
                                .child(
                                    div()
                                        .id("vcs-stage-hunks")
                                        .h(px(18.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded(px(3.))
                                        .cursor_pointer()
                                        .bg(theme.accent.opacity(0.22))
                                        .hover(move |this| this.bg(hover))
                                        .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                                            this.stage_hunks(stage_all.clone(), cx)
                                        }))
                                        .text_size(px(9.5))
                                        .text_color(theme.foreground)
                                        .child("Stage All Hunks"),
                                ),
                        )
                        .children(self.hunks.iter().map(|hunk| {
                            let hover = theme.list_hover;
                            let index = hunk.index;
                            div()
                                .id(ElementId::Name(format!("vcs-hunk-{index}").into()))
                                .h(px(20.))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .hover(move |this| this.bg(hover))
                                .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                                    this.stage_hunks(vec![index], cx)
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(10.5))
                                        .text_color(theme.foreground)
                                        .child(hunk.title.clone()),
                                )
                                .child(
                                    div().text_size(px(9.)).text_color(theme.fg_muted()).child("stage"),
                                )
                        })),
                )
            })
            .when_some(self.message.clone(), |this, message| {
                this.child(div().px_2().py_1().text_size(px(10.5)).text_color(theme.fg_muted()).child(message))
            })
            .child(
                div()
                    .p_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_w_0().child(Input::new(&self.commit_message)))
                    .child(
                        div()
                            .id("vcs-stage-all")
                            .h(px(26.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border_strong())
                            .hover(move |this| this.bg(stage_hover))
                            .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.stage_all(cx)))
                            .child(div().text_size(px(11.)).text_color(theme.foreground).child("Stage All")),
                    )
                    .child(
                        div()
                            .id("vcs-commit")
                            .h(px(26.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .bg(theme.primary)
                            .hover(move |this| this.bg(commit_hover))
                            .on_click(
                                cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.commit(window, cx)),
                            )
                            .child(div().text_size(px(11.)).text_color(theme.primary_foreground).child("Commit")),
                    ),
            )
    }
}

/// The change list's one-line description of a hunk (1-based lines for the
/// reader, as the editor's line numbers are).
fn hunk_title(kind: ide_vcs::HunkKind, start: u32, lines: u32, old_lines: u32) -> String {
    match kind {
        ide_vcs::HunkKind::Added => format!("+{lines} line(s) at {}", start + 1),
        ide_vcs::HunkKind::Modified => format!("~{lines} line(s) at {} (was {old_lines})", start + 1),
        ide_vcs::HunkKind::Deleted => format!("-{old_lines} line(s) after {start}"),
    }
}

fn kind_letter(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "A",
        ChangeKind::Modified => "M",
        ChangeKind::Deleted => "D",
        ChangeKind::Renamed => "R",
        ChangeKind::Untracked => "U",
        ChangeKind::Ignored => "I",
        ChangeKind::Conflicted => "!",
    }
}

fn kind_color(kind: ChangeKind, theme: &gpui_kit::component::Theme) -> gpui_kit::Hsla {
    match kind {
        ChangeKind::Added | ChangeKind::Untracked => theme.success,
        ChangeKind::Modified => theme.info,
        ChangeKind::Deleted | ChangeKind::Conflicted => theme.danger,
        ChangeKind::Renamed => theme.warning,
        ChangeKind::Ignored => theme.fg_muted(),
    }
}

fn render_row(
    row: &VcsRow,
    theme: &gpui_kit::component::Theme,
    on_open: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    on_diff: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
    on_hunks: Rc<dyn Fn(PathBuf, &mut Window, &mut App)>,
) -> AnyElement {
    let hover = theme.list_hover;
    let shown = row.kind.or(row.staged);
    let (letter, color) = match shown {
        Some(kind) => (kind_letter(kind), kind_color(kind, theme)),
        None => (" ", theme.fg_muted()),
    };
    let path = row.path.clone();
    let diff_path = row.path.clone();
    let hunk_path = row.path.clone();
    div()
        .id(ElementId::Name(format!("vcs-{}", row.rel).into()))
        .h(px(22.))
        .w_full()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .hover(move |this| this.bg(hover))
        .on_click(move |_: &ClickEvent, window, cx| on_open(path.clone(), window, cx))
        .child(div().w(px(12.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(color).child(letter))
        .child(
            div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(theme.foreground).child(row.rel.clone()),
        )
        .child(
            div()
                .id(ElementId::Name(format!("vcs-hunks-{}", row.rel).into()))
                .px_2()
                .py(px(1.))
                .rounded(px(3.))
                .cursor_pointer()
                .text_size(px(10.))
                .text_color(theme.fg_secondary())
                .hover(move |this| this.bg(hover))
                .on_click(move |_: &ClickEvent, window, cx| on_hunks(hunk_path.clone(), window, cx))
                .child("Hunks"),
        )
        .child(
            div()
                .id(ElementId::Name(format!("vcs-diff-{}", row.rel).into()))
                .px_2()
                .py(px(1.))
                .rounded(px(3.))
                .cursor_pointer()
                .text_size(px(10.))
                .text_color(theme.info)
                .hover(move |this| this.bg(hover))
                .on_click(move |_: &ClickEvent, window, cx| on_diff(diff_path.clone(), window, cx))
                .child("Diff"),
        )
        .into_any_element()
}

#[cfg(test)]
mod hunk_tests {
    use super::hunk_title;
    use ide_vcs::HunkKind;

    #[test]
    fn hunk_titles_use_one_based_lines_for_additions() {
        assert_eq!(hunk_title(HunkKind::Added, 0, 3, 0), "+3 line(s) at 1");
        assert_eq!(hunk_title(HunkKind::Added, 9, 1, 0), "+1 line(s) at 10");
    }

    #[test]
    fn modified_hunks_mention_the_original_size() {
        assert_eq!(hunk_title(HunkKind::Modified, 4, 2, 3), "~2 line(s) at 5 (was 3)");
    }

    #[test]
    fn deletions_point_at_the_following_line() {
        // `start` is the line of the current text that now follows the removal.
        assert_eq!(hunk_title(HunkKind::Deleted, 7, 0, 4), "-4 line(s) after 7");
    }
}
