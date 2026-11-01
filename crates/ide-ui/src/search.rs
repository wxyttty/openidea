//! Find in Path, over `ide-search`.
//!
//! The scan runs on a worker thread and streams `FileMatches` as files finish,
//! so a large project shows hits immediately. Results are flattened into
//! file/hit rows and clicking one opens the file at the matching position.

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::CancellationToken;
use ide_host::Project;
use ide_search::{FileMatches, SearchContext, SearchQuery, SearchScope};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// Stop collecting after this many files; the status line says when it is hit.
const MAX_FILES: usize = 500;

#[derive(Clone)]
struct SearchRow {
    /// A file header row when true, a hit otherwise.
    header: bool,
    path: PathBuf,
    rel: String,
    line: u32,
    col: u32,
    preview: String,
    count: usize,
}

pub struct SearchPanel {
    project: Option<Arc<Project>>,
    editor: Entity<EditorArea>,
    query: Entity<InputState>,
    files: Vec<FileMatches>,
    running: bool,
    truncated: bool,
    message: Option<SharedString>,
    cancel: CancellationToken,
    generation: u64,
    focus_handle: FocusHandle,
    _submit: Subscription,
}

impl SearchPanel {
    pub fn new(editor: Entity<EditorArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search in project…"));
        let submit = cx.subscribe(&query, |this: &mut Self, _query, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.run(cx);
            }
        });
        Self {
            project: None,
            editor,
            query,
            files: Vec::new(),
            running: false,
            truncated: false,
            message: None,
            cancel: CancellationToken::new(),
            generation: 0,
            focus_handle: cx.focus_handle(),
            _submit: submit,
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        self.project = Some(project.clone());
        self.files.clear();
        self.message = None;
        cx.notify();
    }

    /// Focus the query field (called when the tool window opens).
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.query.read(cx).focus_handle(cx).clone();
        window.focus(&handle, cx);
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            self.message = Some("Open a project first".into());
            cx.notify();
            return;
        };
        let pattern = self.query.read(cx).value().trim().to_owned();
        if pattern.is_empty() {
            self.files.clear();
            self.message = None;
            cx.notify();
            return;
        }
        self.cancel.cancel();
        let cancel = CancellationToken::new();
        self.cancel = cancel.clone();
        self.generation += 1;
        let generation = self.generation;
        self.files.clear();
        self.running = true;
        self.truncated = false;
        self.message = None;
        cx.notify();

        tracing::debug!(pattern = %self.query.read(cx).value(), "search started");
        let (tx, mut rx) = unbounded::<FileMatches>();
        let root = project.root.clone();
        let query = SearchQuery {
            pattern,
            is_regex: false,
            case_sensitive: false,
            whole_word: false,
            include: Vec::new(),
            exclude: Vec::new(),
            scope: SearchScope::Project,
            max_matches: 10_000,
            context: SearchContext::Any,
        };
        std::thread::Builder::new()
            .name("openidea-search".into())
            .spawn(move || {
                let on_file = move |m: FileMatches| {
                    let _ = tx.unbounded_send(m);
                };
                let overlay = |_: &std::path::Path| None;
                if let Err(err) = ide_search::search(&root, &query, &overlay, &cancel, &on_file) {
                    tracing::warn!("search failed: {err}");
                }
            })
            .expect("spawn search thread");

        cx.spawn(async move |this, cx| {
            while let Some(file) = rx.next().await {
                let mut done = false;
                let updated = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        done = true;
                        return;
                    }
                    if this.files.len() < MAX_FILES {
                        this.files.push(file);
                        this.files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
                    } else {
                        this.truncated = true;
                    }
                    cx.notify();
                });
                if updated.is_err() || done {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.running = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn rows(&self) -> Vec<SearchRow> {
        let mut rows = Vec::new();
        for file in &self.files {
            rows.push(SearchRow {
                header: true,
                path: file.path.0.clone(),
                rel: file.rel_path.clone(),
                line: 0,
                col: 0,
                preview: String::new(),
                count: file.matches.len(),
            });
            for hit in &file.matches {
                rows.push(SearchRow {
                    header: false,
                    path: file.path.0.clone(),
                    rel: file.rel_path.clone(),
                    line: hit.line,
                    col: hit.ranges.first().map(|(start, _)| *start).unwrap_or(0),
                    preview: hit.text.clone(),
                    count: 0,
                });
            }
        }
        rows
    }
}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = Rc::new(self.rows());
        let total_matches: usize = self.files.iter().map(|f| f.matches.len()).sum();
        let summary = if self.running {
            format!("Searching… {} files, {total_matches} matches", self.files.len())
        } else if self.files.is_empty() {
            self.message.clone().map(|m| m.to_string()).unwrap_or_else(|| "No matches".to_string())
        } else {
            format!(
                "{} files, {total_matches} matches{}",
                self.files.len(),
                if self.truncated { " (first 500 files)" } else { "" }
            )
        };
        let on_open: Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)> =
            Rc::new(cx.processor(|this: &mut Self, (path, line, col): (PathBuf, u32, u32), window, cx| {
                let editor = this.editor.clone();
                editor.update(cx, |editor, cx| editor.open_file_at(path, line, col, window, cx));
            }));
        let list_rows = rows.clone();
        let list_theme = theme.clone();
        let on_open = on_open.clone();

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
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child("Find in Path"),
                    )
                    .child(div().text_size(px(10.5)).text_color(theme.fg_muted()).child(summary.clone())),
            )
            .child(div().p_2().border_b_1().border_color(theme.border).child(Input::new(&self.query)))
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
                                .child(if self.running { "Searching…" } else { "Type a pattern and press Enter" }),
                        )
                    })
                    .when(!rows.is_empty(), |this| {
                        this.child(
                            uniform_list("search-results", list_rows.len(), move |range, _window, _cx| {
                                range.map(|index| render_row(&list_rows[index], &list_theme, on_open.clone())).collect()
                            })
                            .size_full(),
                        )
                    }),
            )
    }
}

fn render_row(
    row: &SearchRow,
    theme: &gpui_kit::component::Theme,
    on_open: Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)>,
) -> AnyElement {
    if row.header {
        return div()
            .h(px(22.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(11.5))
            .text_color(theme.fg_secondary())
            .child(Icon::new(IconName::FileCode).xsmall().text_color(theme.info))
            .child(div().truncate().child(row.rel.clone()))
            .child(div().text_size(px(10.5)).text_color(theme.fg_muted()).child(format!("{}", row.count)))
            .into_any_element();
    }
    let path = row.path.clone();
    let line = row.line;
    let col = row.col;
    let preview = row.preview.clone();
    div()
        .id(ElementId::Name(format!("search-hit-{}-{}", path.display(), line).into()))
        .h(px(22.))
        .pl(px(24.))
        .pr_2()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .hover(move |this| this.bg(theme.list_hover))
        .on_click(move |_: &ClickEvent, window, cx| on_open((path.clone(), line, col), window, cx))
        .child(div().w(px(36.)).text_size(px(10.5)).text_color(theme.fg_muted()).child(format!("{}", line + 1)))
        .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).text_color(theme.foreground).child(preview))
        .into_any_element()
}
