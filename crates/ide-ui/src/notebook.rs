//! Native Notebook tool window: cells, editing, and kernel execution.
//!
//! Renders `ide-notebook`'s lossless snapshot and drives its kernel:
//! *Run Cell* / *Run All* send code through `ide-host`'s `notebook::execute`,
//! which streams notebook outputs back as `NotebookOutputEvent`s while the
//! kernel runs. *Interrupt* cancels the active request, *Restart* reloads the
//! document with kernel restart. The interpreter is the project's Python SDK
//! unless the header names one explicitly.
//!
//! Execution needs a trusted project and a Python with `ipykernel`; both
//! failures surface in the header notice rather than a silent no-op.

use std::path::PathBuf;
use std::sync::Arc;

use crate::theme::IdeColors as _;
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, InputContent};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_host::Host;
use ide_notebook::{NotebookCell, NotebookEdit, NotebookSnapshot};

struct CellEditor {
    cell: String,
    state: Entity<EditorState>,
}

/// What the execution task sends back to the UI thread.
enum ExecuteEvent {
    /// One streamed notebook output; the panel re-reads the document.
    Output(ide_host::notebook::NotebookOutputEvent),
    /// The batch finished (`Ok` final snapshot, or the error text).
    Done(Result<NotebookSnapshot, String>),
}

pub struct NotebookPanel {
    host: Arc<Host>,
    project: Option<Arc<ide_host::Project>>,
    path: Option<PathBuf>,
    snapshot: Option<NotebookSnapshot>,
    notice: Option<SharedString>,
    cell_editor: Option<CellEditor>,
    task: Option<Task<()>>,
    cell_task: Option<Task<()>>,
    /// The streaming execution task.
    exec_task: Option<Task<()>>,
    /// Cells the kernel is running right now.
    executing: std::collections::BTreeSet<String>,
    /// The request of the running execution, for *Interrupt*.
    request: Option<ide_core::RequestId>,
    /// The interpreter the user named; `None` uses the project's Python SDK.
    python: Option<SharedString>,
    focus_handle: FocusHandle,
}

fn json_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>().join(""),
        other => other.to_string(),
    }
}

/// The displayable text of an output cell and whether it is an error.
fn output_text(output: &serde_json::Value) -> (String, bool) {
    if let Some(text) = output.get("text") {
        let stderr = output.get("name").and_then(|name| name.as_str()) == Some("stderr");
        return (json_text(text), stderr);
    }
    if let Some(traceback) = output.get("traceback").and_then(|items| items.as_array()) {
        let text = traceback.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>().join("\n");
        return (text, true);
    }
    if let Some(data) = output.get("data")
        && let Some(text) = data.get("text/plain")
    {
        return (json_text(text), false);
    }
    (String::new(), false)
}

impl NotebookPanel {
    pub fn new(host: Arc<Host>, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            project: None,
            path: None,
            snapshot: None,
            notice: None,
            cell_editor: None,
            task: None,
            cell_task: None,
            exec_task: None,
            executing: std::collections::BTreeSet::new(),
            request: None,
            python: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<ide_host::Project>, cx: &mut Context<Self>) {
        self.project = Some(project.clone());
        self.path = None;
        self.snapshot = None;
        self.notice = None;
        cx.notify();
    }

    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    pub fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            self.notice = Some("Open a project first".into());
            cx.notify();
            return;
        };
        self.path = Some(path.clone());
        self.snapshot = None;
        self.notice = Some(format!("Loading {}…", path.display()).into());
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        // Through the host's notebook op: it blocks on the runtime's blocking
        // pool instead of spawning a bare OS thread per open.
        let host = self.host.clone();
        let open_path = path.clone();
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::notebook::open(&host, "main", open_path.to_string_lossy().into_owned())
                .await
                .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(snapshot) => {
                            this.notice = None;
                            this.snapshot = Some(snapshot);
                        }
                        Err(err) => this.notice = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn open_cell_editor(&mut self, cell: &NotebookCell, window: &mut Window, cx: &mut Context<Self>) {
        let source = cell.source.clone();
        let language: SharedString = if cell.cell_type == "code" { "python".into() } else { "markdown".into() };
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx);
            state.set_value(InputContent::new(source), window, cx);
            state.set_highlighter(language, cx);
            state
        });
        self.cell_editor = Some(CellEditor { cell: cell.id.clone(), state });
        cx.notify();
    }

    fn save_cell(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.cell_editor.take() else { return };
        let source = editor.state.read(cx).value().to_string();
        let Some(snapshot) = self.snapshot.clone() else { return };
        let edit = NotebookEdit::SetSource { cell: editor.cell, source };
        let id = snapshot.id.clone();
        let version = snapshot.version;
        let host = self.host.clone();
        let save_id = id.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = async {
                ide_host::ops::notebook::edit(&host, "main", id, version, edit).await.map_err(|err| err.to_string())?;
                ide_host::ops::notebook::save(&host, "main", save_id).await.map_err(|err| err.to_string())
            }
            .await;
            let _ = tx.unbounded_send(result);
        });
        self.cell_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(snapshot) => {
                            this.snapshot = Some(snapshot);
                            this.notice = None;
                        }
                        Err(err) => this.notice = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn save_snapshot(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else { return };
        let host = self.host.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::notebook::save(&host, "main", snapshot.id).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.cell_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(snapshot) => this.snapshot = Some(snapshot),
                        Err(err) => this.notice = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.path.clone() {
            self.open_path(path, cx);
        }
    }

    /// The panel's notice (the last status or failure).
    pub fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    /// Whether a notebook is loaded.
    pub fn is_open(&self) -> bool {
        self.snapshot.is_some()
    }

    /// The loaded notebook's cell ids, for tests and diagnostics.
    pub fn cell_ids(&self) -> Vec<String> {
        self.snapshot.as_ref().map(|snapshot| snapshot.cells.iter().map(|cell| cell.id.clone()).collect()).unwrap_or_default()
    }

    /// Drops the loaded notebook (as closing the tool window's file does).
    pub fn close_document(&mut self, cx: &mut Context<Self>) {
        self.path = None;
        self.snapshot = None;
        self.cell_editor = None;
        self.executing.clear();
        self.notice = None;
        cx.notify();
    }

    /// The cell ids the kernel is running.
    pub fn executing(&self) -> Vec<String> {
        self.executing.iter().cloned().collect()
    }

    /// The interpreter in use (`None` means the project's Python SDK).
    pub fn python(&self) -> Option<SharedString> {
        self.python.clone()
    }

    /// Names an interpreter, or clears it to go back to SDK detection.
    pub fn set_python(&mut self, python: Option<String>, cx: &mut Context<Self>) {
        self.python = python.filter(|path| !path.trim().is_empty()).map(Into::into);
        cx.notify();
    }

    /// Runs `cells` (in order) and streams their outputs in.
    ///
    /// One execution is admitted at a time — the backend refuses a second run
    /// while the kernel is busy — so `executing` also doubles as the guard the
    /// buttons read to disable themselves.
    pub fn execute_cells(&mut self, cells: Vec<String>, cx: &mut Context<Self>) {
        if cells.is_empty() {
            self.notice = Some("No code cells to run".into());
            cx.notify();
            return;
        }
        if !self.executing.is_empty() {
            self.notice = Some("The kernel is busy; interrupt it first".into());
            cx.notify();
            return;
        }
        let Some(snapshot) = self.snapshot.clone() else {
            self.notice = Some("Open a notebook first".into());
            cx.notify();
            return;
        };
        self.executing = cells.iter().cloned().collect();
        self.notice = None;
        let request = ide_core::RequestId::next();
        self.request = Some(request);
        let host = self.host.clone();
        let python = self.python.as_ref().map(|path| path.to_string());
        let id = snapshot.id.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<ExecuteEvent>();
        self.host.runtime.spawn(async move {
            let channel = ide_host::ops::Channel::new({
                let tx = tx.clone();
                move |event: ide_host::notebook::NotebookOutputEvent| {
                    let _ = tx.unbounded_send(ExecuteEvent::Output(event));
                }
            });
            let result = ide_host::ops::notebook::execute(&host, "main", id, cells, python, request, channel)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(ExecuteEvent::Done(result));
        });
        self.exec_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    match event {
                        ExecuteEvent::Output(output) => {
                            // The event carries the version and the cells it
                            // changed; re-reading the snapshot is an in-memory
                            // call and keeps the document view authoritative.
                            panel.refresh_snapshot(&output.id, cx);
                        }
                        ExecuteEvent::Done(result) => {
                            panel.executing.clear();
                            panel.request = None;
                            match result {
                                Ok(snapshot) => {
                                    panel.snapshot = Some(snapshot);
                                    panel.notice = None;
                                }
                                Err(err) => panel.notice = Some(err.into()),
                            }
                        }
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    /// Runs every code cell of the notebook in order.
    pub fn run_all(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else {
            self.notice = Some("Open a notebook first".into());
            cx.notify();
            return;
        };
        let cells = snapshot
            .cells
            .iter()
            .filter(|cell| cell.cell_type == "code" && !cell.source.trim().is_empty())
            .map(|cell| cell.id.clone())
            .collect::<Vec<_>>();
        self.execute_cells(cells, cx);
    }

    /// Cancels the running execution (the kernel keeps its state).
    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.request else { return };
        self.host.cancel_request(request);
        self.notice = Some("Interrupting…".into());
        cx.notify();
    }

    /// Restarts the kernel: the document is reloaded with kernel restart.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else { return };
        let host = self.host.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let id = snapshot.id.clone();
        self.host.runtime.spawn(async move {
            let result =
                ide_host::ops::notebook::restart(&host, "main", id).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.cell_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    match result {
                        Ok(snapshot) => {
                            panel.snapshot = Some(snapshot);
                            panel.notice = Some("Kernel restarted".into());
                            panel.executing.clear();
                        }
                        Err(err) => panel.notice = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    /// Re-reads the document after a streamed output (in-memory, no I/O).
    fn refresh_snapshot(&mut self, id: &str, cx: &mut Context<Self>) {
        match ide_host::ops::notebook::get(&self.host, "main", id.to_owned()) {
            Ok(snapshot) => self.snapshot = Some(snapshot),
            Err(err) => self.notice = Some(format!("Notebook refresh failed: {err}").into()),
        }
        cx.notify();
    }

    /// Empties one cell's outputs (and its execution count), as IDEA's
    /// *Clear Output* does.
    pub fn clear_outputs(&mut self, cell: String, cx: &mut Context<Self>) {
        let Some(snapshot) = self.snapshot.clone() else { return };
        let host = self.host.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let id = snapshot.id.clone();
        let version = snapshot.version;
        let save_id = id.clone();
        self.host.runtime.spawn(async move {
            let result = async {
                let edit = NotebookEdit::SetOutputs { cell, outputs: Vec::new(), execution_count: None };
                ide_host::ops::notebook::edit(&host, "main", id, version, edit)
                    .await
                    .map_err(|err| err.to_string())?;
                ide_host::ops::notebook::save(&host, "main", save_id).await.map_err(|err| err.to_string())
            }
            .await;
            let _ = tx.unbounded_send(result);
        });
        self.cell_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    match result {
                        Ok(snapshot) => panel.snapshot = Some(snapshot),
                        Err(err) => panel.notice = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn render_cells(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(snapshot) = &self.snapshot else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.5))
                .text_color(theme.fg_muted())
                .child(self.notice.clone().unwrap_or_else(|| "Open a .ipynb file and choose View › Notebook".into()))
                .into_any_element();
        };
        let mono = theme.mono_font_family.clone();
        let mut list =
            div().id("notebook-cells").flex_1().min_h_0().flex().flex_col().gap_2().p_2().overflow_y_scroll();
        for cell in &snapshot.cells {
            let code = cell.cell_type == "code";
            let count = cell.execution_count.map(|count| format!("[{count}]")).unwrap_or_default();
            let cell_id = cell.id.clone();
            let open = cx.listener(move |this: &mut Self, _: &MouseDownEvent, window, cx| {
                if let Some(cell) =
                    this.snapshot.as_ref().and_then(|snapshot| snapshot.cells.iter().find(|cell| cell.id == cell_id))
                {
                    let cell = cell.clone();
                    this.open_cell_editor(&cell, window, cx);
                }
            });
            let running = self.executing.contains(&cell.id);
            let run_id = cell.id.clone();
            let run = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.execute_cells(vec![run_id.clone()], cx);
            });
            let clear_id = cell.id.clone();
            let clear = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.clear_outputs(clear_id.clone(), cx);
            });
            let mut card = div()
                .id(ElementId::Name(format!("notebook-cell-{}", cell.id).into()))
                .test_support()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, open)
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded(px(4.))
                .border_1()
                .border_color(if running { theme.accent } else { theme.border })
                .bg(if code { theme.background } else { theme.muted })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(if code { theme.accent } else { theme.info })
                                .child(if code { format!("CODE {count}") } else { "MARKDOWN".to_owned() }),
                        )
                        .when(running, |this| {
                            this.child(
                                div().text_size(px(9.5)).text_color(theme.warning).child("running…"),
                            )
                        })
                        .when(!cell.id.is_empty(), |this| {
                            this.child(div().text_size(px(9.)).text_color(theme.fg_muted()).child(cell.id.clone()))
                        })
                        .child(div().flex_1())
                        .when(code, |this| {
                            this.child(
                                div()
                                    .id(ElementId::Name(format!("notebook-run-{}", cell.id).into()))
                                    .h(px(18.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .bg(theme.success.opacity(0.18))
                                    .hover(move |this| this.bg(theme.success.opacity(0.3)))
                                    .on_click(run)
                                    .test_support()
                                    .text_size(px(9.5))
                                    .text_color(theme.success)
                                    .child(Icon::new(IconName::Play).xsmall())
                                    .child("Run"),
                            )
                        })
                        .when(code && !cell.outputs.is_empty(), |this| {
                            this.child(
                                div()
                                    .id(ElementId::Name(format!("notebook-clear-{}", cell.id).into()))
                                    .h(px(18.))
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .hover(move |this| this.bg(theme.list_hover))
                                    .on_click(clear)
                                    .text_size(px(9.5))
                                    .text_color(theme.fg_secondary())
                                    .child("Clear"),
                            )
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .font_family(mono.clone())
                        .text_size(px(10.5))
                        .text_color(theme.foreground)
                        .child(cell.source.clone()),
                );
            for output in &cell.outputs {
                let (text, is_error) = output_text(output);
                if text.trim().is_empty() {
                    continue;
                }
                card = card.child(
                    div()
                        .w_full()
                        .border_t_1()
                        .border_color(theme.border)
                        .pt_1()
                        .font_family(mono.clone())
                        .text_size(px(10.))
                        .text_color(if is_error { theme.danger } else { theme.fg_secondary() })
                        .child(text),
                );
            }
            list = list.child(card);
        }
        list.into_any_element()
    }
}

impl Focusable for NotebookPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for NotebookPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let hover = theme.list_hover;
        let reload = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.reload(cx));
        let path =
            self.path.as_ref().map(|path| path.display().to_string()).unwrap_or_else(|| "No notebook".to_owned());
        let counts = self.snapshot.as_ref().map_or(String::new(), |snapshot| {
            let code = snapshot.cells.iter().filter(|cell| cell.cell_type == "code").count();
            let markdown = snapshot.cells.len() - code;
            format!("{} code · {} markdown", code, markdown)
        });

        div()
            .id("notebook-panel")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .test_support()
            .child(
                div()
                    .h(px(28.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::new(IconName::BookOpen).xsmall().text_color(theme.warning))
                    .child(div().text_size(px(11.)).truncate().text_color(theme.foreground).child(path))
                    .child(div().flex_1())
                    .when_some(self.snapshot.as_ref().map(|snapshot| snapshot.dirty), |this, dirty| {
                        this.child(
                            div()
                                .text_size(px(9.5))
                                .text_color(if dirty { theme.warning } else { theme.fg_muted() })
                                .child(if dirty { "modified" } else { "saved" }),
                        )
                    })
                    .child(div().text_size(px(9.5)).text_color(theme.fg_muted()).child(counts))
                    .when(self.snapshot.is_some(), |this| {
                        let busy = !self.executing.is_empty();
                        let success = theme.success;
                        this.child(
                            div()
                                .id("notebook-run-all")
                                .h(px(20.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_1()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .when(!busy, |this| {
                                    this.bg(success.opacity(0.18))
                                        .hover(move |this| this.bg(success.opacity(0.3)))
                                        .on_click(
                                            cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                                                this.run_all(cx)
                                            }),
                                        )
                                })
                                .test_support()
                                .text_size(px(10.))
                                .text_color(if busy { theme.fg_muted() } else { success })
                                .child(Icon::new(IconName::Play).xsmall())
                                .child("Run All"),
                        )
                    })
                    .when(!self.executing.is_empty(), |this| {
                        let danger = theme.danger;
                        this.child(
                            div()
                                .id("notebook-interrupt")
                                .h(px(20.))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .bg(danger.opacity(0.18))
                                .hover(move |this| this.bg(danger.opacity(0.3)))
                                .on_click(
                                    cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.interrupt(cx)),
                                )
                                .test_support()
                                .text_size(px(10.))
                                .text_color(danger)
                                .child("Interrupt"),
                        )
                    })
                    .when(self.snapshot.is_some(), |this| {
                        let hover = hover;
                        this.child(
                            div()
                                .id("notebook-restart")
                                .h(px(20.))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .border_1()
                                .border_color(theme.border)
                                .hover(move |this| this.bg(hover))
                                .on_click(
                                    cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.restart(cx)),
                                )
                                .test_support()
                                .text_size(px(10.))
                                .text_color(theme.fg_secondary())
                                .child("Restart"),
                        )
                    })
                    .child(
                        div()
                            .id("notebook-reload")
                            .h(px(20.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |this| this.bg(hover))
                            .on_click(reload)
                            .text_size(px(10.))
                            .text_color(theme.fg_secondary())
                            .child("Reload"),
                    )
                    .when(self.snapshot.as_ref().is_some_and(|snapshot| snapshot.dirty), |this| {
                        this.child(
                            div()
                                .id("notebook-save")
                                .h(px(20.))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .bg(theme.primary)
                                .hover(move |this| this.opacity(0.9))
                                .on_click(
                                    cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.save_snapshot(cx)),
                                )
                                .text_size(px(10.))
                                .text_color(theme.primary_foreground)
                                .child("Save"),
                        )
                    }),
            )
            .child(self.render_cells(&theme, cx))
            .children(self.cell_editor.as_ref().map(|editor| {
                let state = editor.state.clone();
                let hover = theme.list_hover;
                let save = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.save_cell(cx));
                let cancel = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                    this.cell_editor = None;
                    cx.notify();
                });
                let backdrop = cx.listener(|this: &mut Self, _: &MouseDownEvent, _window, cx| {
                    this.cell_editor = None;
                    cx.notify();
                });
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("notebook-cell-backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(theme.overlay)
                            .on_mouse_down(MouseButton::Left, backdrop),
                    )
                    .child(
                        div()
                            .w(px(720.))
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .bg(theme.popover)
                            .border_1()
                            .border_color(theme.border_strong())
                            .rounded(px(8.))
                            .shadow_xl()
                            .occlude()
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.foreground)
                                    .child("Edit cell"),
                            )
                            .child(
                                div()
                                    .h(px(300.))
                                    .border_1()
                                    .border_color(theme.border)
                                    .rounded(px(4.))
                                    .child(Editor::new(&state).text_size(px(11.)).size_full()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .justify_end()
                                    .child(
                                        div()
                                            .id("notebook-cell-cancel")
                                            .h(px(24.))
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .rounded(px(4.))
                                            .cursor_pointer()
                                            .border_1()
                                            .border_color(theme.border)
                                            .hover(move |this| this.bg(hover))
                                            .on_click(cancel)
                                            .text_size(px(11.))
                                            .text_color(theme.fg_secondary())
                                            .child("Cancel"),
                                    )
                                    .child(
                                        div()
                                            .id("notebook-cell-save")
                                            .h(px(24.))
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .rounded(px(4.))
                                            .cursor_pointer()
                                            .bg(theme.primary)
                                            .hover(move |this| this.opacity(0.9))
                                            .on_click(save)
                                            .text_size(px(11.))
                                            .text_color(theme.primary_foreground)
                                            .child("Save"),
                                    ),
                            ),
                    )
                    .into_any_element()
            }))
            .child(
                div()
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(10.))
                    .text_color(theme.fg_muted())
                    .child("Python kernel")
                    .child(self.snapshot.as_ref().map_or(String::new(), |snapshot| {
                        format!("nbformat {}.{}", snapshot.nbformat, snapshot.nbformat_minor)
                    })),
            )
    }
}
