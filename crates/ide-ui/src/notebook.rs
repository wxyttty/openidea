//! Read-only native Notebook tool window.
//!
//! Renders `ide-notebook`'s lossless snapshot: markdown and code cells with
//! their sources, execution counts and the text of their outputs. Editing and
//! kernel execution stay with the Tauri shell until the GPUI editor gains a
//! multi-line cell widget.

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

pub struct NotebookPanel {
    host: Arc<Host>,
    project: Option<Arc<ide_host::Project>>,
    path: Option<PathBuf>,
    snapshot: Option<NotebookSnapshot>,
    notice: Option<SharedString>,
    cell_editor: Option<CellEditor>,
    task: Option<Task<()>>,
    cell_task: Option<Task<()>>,
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
        let root = project.root.clone();
        std::thread::spawn(move || {
            let notebooks = project.notebooks.clone();
            let result = notebooks.open(&path).map_err(|err| err.to_string());
            let _ = root;
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
            let mut card = div()
                .id(ElementId::Name(format!("notebook-cell-{}", cell.id).into()))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, open)
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded(px(4.))
                .border_1()
                .border_color(theme.border)
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
                        .when(!cell.id.is_empty(), |this| {
                            this.child(div().text_size(px(9.)).text_color(theme.fg_muted()).child(cell.id.clone()))
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
            .relative()
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
                    .child("Read-only viewer · kernel execution needs a Python env with ipykernel")
                    .child(self.snapshot.as_ref().map_or(String::new(), |snapshot| {
                        format!("nbformat {}.{}", snapshot.nbformat, snapshot.nbformat_minor)
                    })),
            )
    }
}
