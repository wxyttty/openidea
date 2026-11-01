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
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_host::Host;
use ide_notebook::NotebookSnapshot;

pub struct NotebookPanel {
    _host: Arc<Host>,
    project: Option<Arc<ide_host::Project>>,
    path: Option<PathBuf>,
    snapshot: Option<NotebookSnapshot>,
    notice: Option<SharedString>,
    task: Option<Task<()>>,
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
            _host: host,
            project: None,
            path: None,
            snapshot: None,
            notice: None,
            task: None,
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

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.path.clone() {
            self.open_path(path, cx);
        }
    }

    fn render_cells(&self, theme: &gpui_kit::component::Theme) -> AnyElement {
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
            let mut card = div()
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
                    ),
            )
            .child(self.render_cells(&theme))
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
