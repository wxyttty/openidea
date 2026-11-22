//! Search Structurally (ast-grep): find code by its syntax shape rather than its
//! text.
//!
//! A pattern such as `if ($COND) { $$$BODY }` matches syntax trees through
//! `ide-analysis`'s `search_structurally`, which runs on the host runtime and
//! streams matches file by file. The panel mirrors IDEA's Structural Search:
//! pattern, language, optional replacement template, the matches grouped by
//! file with a per-file count, and Replace All applying the replacements as a
//! workspace edit (so open editors repaint from `DocPatched`).
//!
//! `$name` matches one node and `$$$list` any sequence, the same placeholder
//! vocabulary `ide-analysis` documents for the backend.

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use ide_analysis::structural::StructuralQuery;
use ide_host::ops::analysis::StructuralEvent;
use ide_core::LanguageId;
use ide_host::Host;
use ide_search::SearchScope;
use std::path::PathBuf;
use std::sync::Arc;

/// One result row: a file header or a match inside it.
enum StructuralRow {
    File { rel_path: SharedString, count: usize },
    Match { path: PathBuf, line: u32, col: u32, text: SharedString },
}

pub struct StructuralPanel {
    host: Arc<Host>,
    editor: Entity<EditorArea>,
    pattern: Entity<InputState>,
    replacement: Entity<InputState>,
    language: SharedString,
    rows: Vec<StructuralRow>,
    running: bool,
    notice: Option<SharedString>,
    summary: Option<SharedString>,
    task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

impl StructuralPanel {
    pub fn new(host: Arc<Host>, editor: Entity<EditorArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pattern = cx.new(|cx| InputState::new(window, cx).placeholder("if ($COND) { $$$BODY }"));
        let replacement = cx.new(|cx| InputState::new(window, cx).placeholder("Replacement (optional)"));
        cx.subscribe(&pattern, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.run(cx);
            }
        })
        .detach();
        Self {
            host,
            editor,
            pattern,
            replacement,
            language: "rust".into(),
            rows: Vec::new(),
            running: false,
            notice: None,
            summary: None,
            task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn language(&self) -> SharedString {
        self.language.clone()
    }

    /// Restricts the search to one language (the pattern is parsed as it).
    pub fn set_language(&mut self, language: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.language = language.into();
        cx.notify();
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The matches found, as `(file, line, column, text)` rows.
    pub fn matches(&self) -> Vec<(PathBuf, u32, u32, String)> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                StructuralRow::Match { path, line, col, text } => {
                    Some((path.clone(), *line, *col, text.to_string()))
                }
                StructuralRow::File { .. } => None,
            })
            .collect()
    }

    /// Runs the search; matches stream in as files finish.
    pub fn run(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let pattern = self.pattern.read(cx).value().trim().to_owned();
        if pattern.is_empty() {
            self.notice = Some("Enter a structural pattern first".into());
            cx.notify();
            return;
        }
        let replacement = self.replacement.read(cx).value().trim().to_owned();
        let query = StructuralQuery {
            pattern,
            language: LanguageId::new(self.language.to_string()),
            replacement: (!replacement.is_empty()).then_some(replacement),
            max_matches: 0,
        };
        self.running = true;
        self.notice = None;
        self.rows.clear();
        self.summary = None;
        cx.notify();

        let host = self.host.clone();
        let (tx, mut rx) = unbounded::<StructuralEvent>();
        self.host.runtime.spawn(async move {
            let channel = ide_host::ops::Channel::new(move |event: StructuralEvent| {
                let _ = tx.unbounded_send(event);
            });
            let _ = ide_host::ops::analysis::structural_search(
                &host,
                "main",
                ide_core::RequestId::next(),
                SearchScope::Project,
                query,
                channel,
            )
            .await;
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    match event {
                        StructuralEvent::File { file } => {
                            if file.matches.is_empty() {
                                return;
                            }
                            panel.rows.push(StructuralRow::File {
                                rel_path: file.rel_path.clone().into(),
                                count: file.matches.len(),
                            });
                            for found in file.matches {
                                panel.rows.push(StructuralRow::Match {
                                    path: file.path.0.clone(),
                                    line: found.range.start.line,
                                    col: found.range.start.col,
                                    text: found.line_text.clone().into(),
                                });
                            }
                        }
                        StructuralEvent::Done { summary } => {
                            panel.running = false;
                            panel.summary = Some(
                                format!(
                                    "{} matches in {} files · {} ms",
                                    summary.matches, summary.files, summary.elapsed_ms
                                )
                                .into(),
                            );
                            if summary.limit_hit {
                                panel.notice = Some("The match limit was reached; narrow the pattern".into());
                            }
                            if summary.matches == 0 {
                                panel.notice = Some("No code matches this pattern".into());
                            }
                        }
                    }
                    cx.notify();
                });
            }
        }));
    }

    /// Applies the replacement of every match as one workspace edit.
    pub fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let replacement = self.replacement.read(cx).value().trim().to_owned();
        if replacement.is_empty() {
            self.notice = Some("Enter a replacement template first".into());
            cx.notify();
            return;
        }
        // Re-running with the replacement makes the backend attach the
        // substituted text to every match; the panel then applies those.
        self.notice = Some("Replacing…".into());
        cx.notify();
        let pattern = self.pattern.read(cx).value().trim().to_owned();
        if pattern.is_empty() {
            return;
        }
        let query = StructuralQuery {
            pattern,
            language: LanguageId::new(self.language.to_string()),
            replacement: Some(replacement),
            max_matches: 0,
        };
        let host = self.host.clone();
        let editor = self.editor.clone();
        let (tx, mut rx) = unbounded::<Result<(usize, usize), String>>();
        self.host.runtime.spawn(async move {
            let runtime = host.runtime.clone();
            let apply_host = host.clone();
            let report = runtime
                .spawn(async move {
                    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                    let slot = collected.clone();
                    let channel = ide_host::ops::Channel::new(move |event: StructuralEvent| {
                        if let StructuralEvent::File { file } = event
                            && let Ok(mut slot) = slot.lock()
                        {
                            for found in file.matches {
                                slot.push((file.path.clone(), found.range, found.replacement));
                            }
                        }
                    });
                    ide_host::ops::analysis::structural_search(
                        &host,
                        "main",
                        ide_core::RequestId::next(),
                        SearchScope::Project,
                        query,
                        channel,
                    )
                    .await
                    .map(|()| collected.lock().map(|slot| slot.clone()).unwrap_or_default())
                })
                .await;
            let message = match report {
                Ok(Ok(matches)) => {
                    let mut files = std::collections::BTreeSet::new();
                    let mut edits = Vec::new();
                    for (path, range, replacement) in matches {
                        let Some(replacement) = replacement else { continue };
                        files.insert(path.clone());
                        edits.push((path, range, replacement));
                    }
                    // Group by file into one workspace edit.
                    let mut by_file: std::collections::BTreeMap<PathBuf, Vec<(ide_core::Range, String)>> =
                        Default::default();
                    for (path, range, replacement) in edits {
                        by_file.entry(path.0.clone()).or_default().push((range, replacement));
                    }
                    let mut workspace = ide_core::WorkspaceEdit { files: Vec::new(), preconditions: Vec::new() };
                    for (path, edits) in by_file {
                        workspace.files.push(ide_core::FileEdit::Edit {
                            path: ide_core::IpcPath(path),
                            edits: edits
                                .into_iter()
                                .map(|(range, new_text)| ide_core::TextEdit { range, new_text })
                                .collect(),
                            version: None,
                        });
                    }
                    let count = workspace.files.iter().map(|f| match f {
                        ide_core::FileEdit::Edit { edits, .. } => edits.len(),
                        _ => 0,
                    }).sum::<usize>();
                    let file_count = workspace.files.len();
                    let applied = ide_host::ops::lsp::apply_workspace_edit(&apply_host, "main", workspace).await;
                    match applied {
                        Ok(()) => {
                            let _ = &editor;
                            Ok((count, file_count))
                        }
                        Err(err) => Err(err.to_string()),
                    }
                }
                Ok(Err(err)) => Err(err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            let _ = tx.unbounded_send(message);
        });
        let _ = window;
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    match result {
                        Ok((count, files)) => {
                            panel.notice = None;
                            panel.summary = Some(format!("{count} replacements in {files} files").into());
                        }
                        Err(err) => panel.notice = Some(format!("Replace failed: {err}").into()),
                    }
                    cx.notify();
                });
            }
        }));
    }

    pub fn input_focus(&self, cx: &App) -> FocusHandle {
        self.pattern.read(cx).focus_handle(cx)
    }
}

impl Focusable for StructuralPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for StructuralPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let row_theme = theme.clone();
        let hover = theme.list_hover;
        let rows: std::rc::Rc<Vec<(bool, SharedString, SharedString, Option<(PathBuf, u32, u32)>)>> = std::rc::Rc::new(
            self.rows
                .iter()
                .map(|row| match row {
                    StructuralRow::File { rel_path, count } => {
                        (true, rel_path.clone(), format!("{count} match(es)").into(), None)
                    }
                    StructuralRow::Match { path, line, col, text } => {
                        (false, text.clone(), format!("{}:{}", line + 1, col + 1).into(), Some((path.clone(), *line, *col)))
                    }
                })
                .collect::<Vec<_>>(),
        );
        let editor = self.editor.clone();
        let open: std::rc::Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)> = {
            let editor = editor;
            std::rc::Rc::new(move |(path, line, col): (PathBuf, u32, u32), window, cx| {
                editor.update(cx, |editor, cx| editor.open_file_at(path, line, col, window, cx));
            })
        };
        let run = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.run(cx));
        let replace = cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| this.replace_all(window, cx));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(&self.pattern).id("structural-pattern")))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(theme.fg_muted())
                                    .child(self.language.clone()),
                            )
                            .child(Button::new("structural-run").label("Search").small().on_click(run))
                            .child(Button::new("structural-replace").label("Replace All").small().on_click(replace)),
                    )
                    .child(Input::new(&self.replacement).id("structural-replacement")),
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
            .when_some(self.summary.clone(), |this, summary| {
                let theme = theme.clone();
                this.child(
                    div().px_2().py_1().text_size(px(10.)).text_color(theme.fg_muted()).child(summary),
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
                                .child(if self.running {
                                    "Searching…"
                                } else {
                                    "Enter a pattern: $name matches one node, $$$list any sequence"
                                }),
                        )
                    })
                    .when(!rows.is_empty(), |this| {
                        let rows = rows.clone();
                        this.child(
                            div()
                                .id("structural-list")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p_1()
                                .flex()
                                .flex_col()
                                .children(rows.iter().enumerate().map(|(index, (is_file, primary, secondary, target))| {
                                    let is_file = *is_file;
                                    let target = *target == None;
                                    let _ = target;
                                    let hover = row_theme.list_hover;
                                    let open = open.clone();
                                    let target = rows[index].3.clone();
                                    div()
                                        .id(ElementId::Name(format!("structural-row-{index}").into()))
                                        .h(px(22.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .rounded(px(3.))
                                        .when(is_file, |this| this.bg(row_theme.muted))
                                        .when(!is_file, |this| this.cursor_pointer().hover(move |this| this.bg(hover)))
                                        .when_some(target, |this, target| {
                                            this.on_click(move |_: &ClickEvent, window, cx| open(target.clone(), window, cx))
                                        })
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(px(if is_file { 11. } else { 11.5 }))
                                                .when(is_file, |this| this.font_weight(FontWeight::SEMIBOLD))
                                                .text_color(if is_file { row_theme.fg_secondary() } else { row_theme.foreground })
                                                .child(primary.clone()),
                                        )
                                        .child(
                                            div()
                                                .max_w(px(240.))
                                                .truncate()
                                                .text_size(px(10.5))
                                                .text_color(row_theme.fg_muted())
                                                .child(secondary.clone()),
                                        )
                                })),
                        )
                    }),
            )
    }
}
