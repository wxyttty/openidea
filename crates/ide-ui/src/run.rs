//! Run tool window: detected run configurations, process control and a console.
//!
//! Configurations come from `ide-run` (autodetected npm/cargo/gradle/make/python
//! targets plus `<root>/.openidea/run.toml`). Starting one goes straight to the
//! host's `ProcessManager`, so output is the real child process stream with no
//! IPC hop. `IdeEvent::RunExited` closes the session in `mark_exited`.

use crate::theme::IdeColors as _;
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_host::{Host, Project};
use ide_run::{OutputChunk, RunConfig, StartOptions};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

const MAX_LINES: usize = 4000;
const ROW_HEIGHT: f32 = 16.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Stdout,
    Stderr,
    System,
}

impl LineKind {
    fn index(self) -> usize {
        match self {
            LineKind::Stdout => 0,
            LineKind::Stderr => 1,
            LineKind::System => 2,
        }
    }
}

struct RunLine {
    kind: LineKind,
    text: SharedString,
}

pub struct RunPanel {
    host: Arc<Host>,
    project: Option<Arc<Project>>,
    configs: Vec<RunConfig>,
    selected: usize,
    run_id: Option<ide_core::RunId>,
    pid: Option<u32>,
    running: bool,
    lines: Vec<RunLine>,
    pending: [String; 3],
    notice: Option<SharedString>,
    exit: Option<Option<i32>>,
    scroll: UniformListScrollHandle,
    task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

impl RunPanel {
    pub fn new(host: Arc<Host>, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            project: None,
            configs: Vec::new(),
            selected: 0,
            run_id: None,
            pid: None,
            running: false,
            lines: Vec::new(),
            pending: Default::default(),
            notice: None,
            exit: None,
            scroll: UniformListScrollHandle::default(),
            task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        // A process from the previous project must not outlive the switch.
        if let Some(id) = self.run_id.take() {
            let host = self.host.clone();
            std::thread::spawn(move || {
                let _ = host.processes.stop(id);
            });
        }
        let previous = self.configs.get(self.selected).map(|config| config.id.clone());
        self.project = Some(project.clone());
        self.configs = load_configs(&project.root);
        self.selected = previous.and_then(|id| self.configs.iter().position(|config| config.id == id)).unwrap_or(0);
        self.lines.clear();
        self.pending = Default::default();
        self.notice = None;
        self.exit = None;
        self.running = false;
        self.run_id = None;
        self.pid = None;
        self.task = None;
        if self.configs.is_empty() {
            self.notice = Some("No run configurations detected for this project".into());
        }
        cx.notify();
    }

    pub fn selected_config(&self) -> Option<RunConfig> {
        self.configs.get(self.selected).cloned()
    }

    pub fn has_configs(&self) -> bool {
        !self.configs.is_empty()
    }

    pub fn start_selected(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let Some(project) = self.project.clone() else {
            self.notice = Some("Open a project first".into());
            cx.notify();
            return;
        };
        let Some(config) = self.configs.get(self.selected).cloned() else {
            self.notice = Some("No run configuration selected".into());
            cx.notify();
            return;
        };
        if let Err(err) = project.require_trust() {
            self.notice = Some(format!("Restricted Mode: {err}").into());
            cx.notify();
            return;
        }
        self.notice = None;
        self.exit = None;
        self.lines.clear();
        self.pending = Default::default();
        let command =
            std::iter::once(config.command.clone()).chain(config.args.iter().cloned()).collect::<Vec<_>>().join(" ");
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let on_output = move |chunk: OutputChunk| {
            let _ = tx.unbounded_send(chunk);
        };
        tracing::info!(command = %command, "run: starting configuration");
        match self.host.processes.start(
            &project.root,
            &config,
            &StartOptions::default(),
            project.sink.clone(),
            Box::new(on_output),
        ) {
            Ok(info) => {
                tracing::info!(pid = ?info.pid, pty = info.pty, "run: spawned");
                self.running = true;
                self.run_id = Some(info.id);
                self.pid = info.pid;
                self.task = Some(cx.spawn(async move |this, cx| {
                    while let Some(chunk) = rx.next().await {
                        if this.update(cx, |this, cx| this.push_chunk(chunk, cx)).is_err() {
                            break;
                        }
                    }
                }));
                self.scroll.scroll_to_bottom();
            }
            Err(err) => {
                tracing::info!(error = %err, "run: spawn failed");
                self.notice = Some(format!("Cannot start `{}`: {err}", config.name).into());
            }
        }
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.run_id else { return };
        self.lines.push(RunLine { kind: LineKind::System, text: "Stopping…".into() });
        let host = self.host.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = host.processes.stop(id);
            })
            .detach();
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    pub fn mark_exited(&mut self, run: ide_core::RunId, code: Option<i32>, cx: &mut Context<Self>) {
        if self.run_id != Some(run) {
            return;
        }
        self.flush_pending();
        self.running = false;
        self.run_id = None;
        self.pid = None;
        self.task = None;
        tracing::info!(?code, "run: exited");
        self.exit = Some(code);
        // The backend already emits "Process finished with exit code …" as a system
        // chunk; only add a line for terminations it does not describe.
        let described =
            self.lines.last().is_some_and(|line| line.kind == LineKind::System && line.text.contains("exit code"));
        if !described {
            let message = match code {
                Some(0) => "Process finished successfully".to_owned(),
                Some(code) => format!("Process finished with exit code {code}"),
                None => "Process terminated".to_owned(),
            };
            self.lines.push(RunLine { kind: LineKind::System, text: message.into() });
        }
        self.trim();
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.lines.clear();
        self.pending = Default::default();
        self.exit = None;
        self.notice = None;
        cx.notify();
    }

    fn push_chunk(&mut self, chunk: OutputChunk, cx: &mut Context<Self>) {
        match chunk {
            OutputChunk::Stdout { text } => self.push_text(LineKind::Stdout, &text),
            OutputChunk::Stderr { text } => self.push_text(LineKind::Stderr, &text),
            OutputChunk::System { text } => self.push_text(LineKind::System, &text),
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn push_text(&mut self, kind: LineKind, text: &str) {
        let index = kind.index();
        for ch in text.chars() {
            match ch {
                '\n' => {
                    let line = std::mem::take(&mut self.pending[index]);
                    self.lines.push(RunLine { kind, text: line.into() });
                }
                '\r' => self.pending[index].clear(),
                '\t' => self.pending[index].push_str("    "),
                _ => self.pending[index].push(ch),
            }
        }
        self.trim();
    }

    fn flush_pending(&mut self) {
        for index in 0..self.pending.len() {
            if self.pending[index].is_empty() {
                continue;
            }
            // Pending buffers are indexed by LineKind; stdout, stderr, system.
            let kind = [LineKind::Stdout, LineKind::Stderr, LineKind::System][index];
            let line = std::mem::take(&mut self.pending[index]);
            self.lines.push(RunLine { kind, text: line.into() });
        }
    }

    fn trim(&mut self) {
        if self.lines.len() > MAX_LINES {
            self.lines.drain(0..1000);
        }
    }
}

fn load_configs(root: &Path) -> Vec<RunConfig> {
    let saved = ide_run::load_user_configs(root).unwrap_or_default();
    let suppressed = ide_run::load_suppressed_config_ids(root).unwrap_or_default();
    let mut all: Vec<RunConfig> = ide_run::detect(root)
        .into_iter()
        .filter(|config| !suppressed.contains(&config.id) && !saved.iter().any(|saved| saved.id == config.id))
        .collect();
    all.extend(saved);
    all
}

impl Drop for RunPanel {
    fn drop(&mut self) {
        if let Some(id) = self.run_id {
            let host = self.host.clone();
            std::thread::spawn(move || {
                let _ = host.processes.stop(id);
            });
        }
    }
}

impl Focusable for RunPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RunPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let running = self.running;
        let selected = self.selected;
        let notice = self.notice.clone();
        let lines: Rc<Vec<RunLine>> =
            Rc::new(self.lines.iter().map(|line| RunLine { kind: line.kind, text: line.text.clone() }).collect());
        let list_theme = theme.clone();
        let scroll = self.scroll.clone();
        let status = if running {
            match self.pid {
                Some(pid) => format!("Running · pid {pid}"),
                None => "Running".to_owned(),
            }
        } else {
            match self.exit {
                Some(Some(0)) => "Finished".to_owned(),
                Some(Some(code)) => format!("Exited with {code}"),
                Some(None) => "Terminated".to_owned(),
                None => "Ready".to_owned(),
            }
        };
        let command_hint = self.configs.get(self.selected).map(|config| config.name.clone()).unwrap_or_default();
        let config_count = self.configs.len();
        let list_hover = theme.list_hover;
        let line_count = lines.len();

        let header = div()
            .h(px(30.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(Icon::new(IconName::Play).xsmall().text_color(theme.success))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .when(self.configs.is_empty(), |this| {
                        this.child(div().text_size(px(11.)).text_color(theme.fg_muted()).child("No configurations"))
                    })
                    .children(self.configs.iter().enumerate().map(|(index, config)| {
                        let active = index == selected;
                        let select = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                            this.selected = index;
                            this.notice = None;
                            cx.notify();
                        });
                        div()
                            .id(ElementId::Name(format!("run-config-{index}").into()))
                            .h(px(22.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .text_size(px(11.))
                            .whitespace_nowrap()
                            .when(active, |this| this.bg(theme.list_active).text_color(theme.fg_strong()))
                            .when(!active, |this| {
                                this.text_color(theme.fg_secondary()).hover(move |this| this.bg(list_hover))
                            })
                            .on_click(select)
                            .child(config.name.clone())
                    })),
            )
            .when(running, |this| {
                this.child(
                    div()
                        .id("run-stop")
                        .h(px(24.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .bg(theme.danger)
                        .hover(move |this| this.opacity(0.9))
                        .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.stop(cx)))
                        .child(Icon::new(IconName::Square).xsmall().text_color(theme.danger_foreground))
                        .child(div().text_size(px(11.)).text_color(theme.danger_foreground).child("Stop")),
                )
            })
            .when(!running, |this| {
                this.child(
                    div()
                        .id("run-start")
                        .h(px(24.))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_1()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .bg(theme.primary)
                        .when(config_count == 0, |this| this.opacity(0.5))
                        .hover(move |this| this.opacity(0.9))
                        .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.start_selected(cx)))
                        .child(Icon::new(IconName::Play).xsmall().text_color(theme.primary_foreground))
                        .child(div().text_size(px(11.)).text_color(theme.primary_foreground).child("Run")),
                )
            })
            .child(
                div()
                    .id("run-clear")
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(move |this| this.bg(list_hover))
                    .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.clear(cx)))
                    .child(Icon::new(IconName::Trash).xsmall().text_color(theme.fg_muted())),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(header)
            .when_some(notice, |this, notice| {
                this.child(div().px_2().py(px(2.)).text_size(px(10.5)).text_color(theme.warning).child(notice))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(lines.is_empty(), |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(11.5))
                                .text_color(theme.fg_muted())
                                .child(if config_count == 0 {
                                    "Add a run configuration in .openidea/run.toml".to_owned()
                                } else {
                                    format!("Press Run to start `{command_hint}`")
                                }),
                        )
                    })
                    .when(!lines.is_empty(), |this| {
                        this.child(
                            uniform_list("run-console", lines.len(), move |range, _window, _cx| {
                                range.map(|index| render_line(&lines[index], &list_theme)).collect()
                            })
                            .track_scroll(&scroll)
                            .size_full(),
                        )
                    }),
            )
            .child(
                div()
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(10.5))
                    .text_color(theme.fg_muted())
                    .child(status)
                    .child(format!("{line_count} lines")),
            )
            .into_any_element()
    }
}

fn render_line(line: &RunLine, theme: &gpui_kit::component::Theme) -> AnyElement {
    let color = match line.kind {
        LineKind::Stdout => theme.foreground,
        LineKind::Stderr => theme.danger,
        LineKind::System => theme.accent,
    };
    div()
        .h(px(ROW_HEIGHT))
        .w_full()
        .px_2()
        .flex()
        .items_center()
        .whitespace_nowrap()
        .font_family(theme.mono_font_family.clone())
        .text_size(px(12.))
        .text_color(color)
        .child(line.text.clone())
        .into_any_element()
}
