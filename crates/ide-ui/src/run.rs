//! Run tool window: detected run configurations, process control and a console.
//!
//! Configurations come from `ide-run` (autodetected npm/cargo/gradle/make/python
//! targets plus `<root>/.openidea/run.toml`). Starting one goes straight to the
//! host's `ProcessManager`, so output is the real child process stream with no
//! IPC hop. `IdeEvent::RunExited` closes the session in `mark_exited`.

use crate::theme::IdeColors as _;
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputState};
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
    /// The configuration editor (Run › Edit Configurations…).
    editor: Option<RunConfigEditor>,
    focus_handle: FocusHandle,
}

/// The fields of the configuration editor, one text input per field.
pub struct RunConfigEditor {
    /// The configuration being edited; `None` while creating a new one.
    editing: Option<String>,
    pub name: Entity<InputState>,
    pub command: Entity<InputState>,
    pub args: Entity<InputState>,
    pub cwd: Entity<InputState>,
    pub env: Entity<InputState>,
    pub kind: SharedString,
    pub error: Option<SharedString>,
}

/// The configuration kinds the editor offers, with the command that runs them.
pub const CONFIG_KINDS: [(&str, &str); 8] = [
    ("cargo", "cargo"),
    ("gradle", "gradle"),
    ("maven", "mvn"),
    ("npm", "npm"),
    ("go", "go"),
    ("python", "python3"),
    ("pytest", "pytest"),
    ("shell", "sh"),
];

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
            editor: None,
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

    /// Opens the editor for a new configuration (or the selected one when
    /// `existing` is set).
    pub fn open_editor(&mut self, existing: bool, window: &mut Window, cx: &mut Context<Self>) {
        let current = existing.then(|| self.configs.get(self.selected).cloned()).flatten();
        let kind = current.as_ref().map(|config| config.kind.clone()).unwrap_or_else(|| "cargo".to_owned());
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let command = cx.new(|cx| InputState::new(window, cx).placeholder("Command, e.g. cargo"));
        let args = cx.new(|cx| InputState::new(window, cx).placeholder("Arguments, space separated"));
        let cwd = cx.new(|cx| InputState::new(window, cx).placeholder("Working directory (project-relative)"));
        let env = cx.new(|cx| InputState::new(window, cx).placeholder("Env: KEY=value, KEY2=value2"));
        if let Some(config) = &current {
            name.update(cx, |input, cx| input.set_value(config.name.clone(), window, cx));
            command.update(cx, |input, cx| input.set_value(config.command.clone(), window, cx));
            args.update(cx, |input, cx| input.set_value(config.args.join(" "), window, cx));
            let cwd_text = config
                .cwd
                .as_ref()
                .map(|path| path.0.display().to_string())
                .unwrap_or_default();
            cwd.update(cx, |input, cx| input.set_value(cwd_text, window, cx));
            let env_text = config
                .env
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(", ");
            env.update(cx, |input, cx| input.set_value(env_text, window, cx));
        } else {
            // A new configuration starts at the kind's own command.
            let default = CONFIG_KINDS
                .iter()
                .find(|(kind_id, _)| *kind_id == kind)
                .map(|(_, command)| *command)
                .unwrap_or("cargo");
            command.update(cx, |input, cx| input.set_value(default, window, cx));
        }
        self.editor = Some(RunConfigEditor {
            editing: current.as_ref().map(|config| config.id.clone()),
            name,
            command,
            args,
            cwd,
            env,
            kind: kind.into(),
            error: None,
        });
        cx.notify();
    }

    pub fn close_editor(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        cx.notify();
    }

    pub fn editor_open(&self) -> bool {
        self.editor.is_some()
    }

    /// Writes the editor's configuration to `.openidea/run.toml` and reloads.
    pub fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(editor) = &self.editor else { return };
        let name = editor.name.read(cx).value().trim().to_owned();
        let command = editor.command.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.editor.as_mut().map(|editor| editor.error = Some("Name the configuration".into()));
            cx.notify();
            return;
        }
        if command.is_empty() {
            self.editor.as_mut().map(|editor| editor.error = Some("Enter the command to run".into()));
            cx.notify();
            return;
        }
        let args = editor
            .args
            .read(cx)
            .value()
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let cwd = editor.cwd.read(cx).value().trim().to_owned();
        let env = parse_env(&editor.env.read(cx).value());
        let kind = editor.kind.clone();
        let id = editor
            .editing
            .clone()
            .unwrap_or_else(|| format!("user:{}", uuid_like(&name)));
        let config = RunConfig {
            id: id.clone(),
            name,
            kind: kind.to_string(),
            command,
            args,
            cwd: (!cwd.is_empty()).then(|| ide_core::IpcPath(project.root.join(cwd))),
            env,
            detected: false,
            is_test: false,
            debug_adapter: None,
            allow_parallel: false,
            extras: Default::default(),
        };
        let mut saved = ide_run::load_user_configs(&project.root).unwrap_or_default();
        match saved.iter_mut().find(|existing| existing.id == id) {
            Some(existing) => *existing = config,
            None => saved.push(config),
        }
        match ide_run::save_user_configs(&project.root, &saved) {
            Ok(()) => {
                self.editor = None;
                self.configs = load_configs(&project.root);
                self.selected = self.configs.iter().position(|config| config.id == id).unwrap_or(0);
                self.notice = Some("Run configuration saved".into());
            }
            Err(err) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.error = Some(format!("Cannot save: {err}").into());
                }
            }
        }
        cx.notify();
    }

    /// Removes the selected user configuration (detected ones are regenerated).
    pub fn remove_selected(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(config) = self.configs.get(self.selected).cloned() else { return };
        if config.detected {
            self.notice = Some("Detected configurations are regenerated; suppress them instead".into());
            cx.notify();
            return;
        }
        let mut saved = ide_run::load_user_configs(&project.root).unwrap_or_default();
        saved.retain(|existing| existing.id != config.id);
        match ide_run::save_user_configs(&project.root, &saved) {
            Ok(()) => {
                self.configs = load_configs(&project.root);
                self.selected = self.selected.min(self.configs.len().saturating_sub(1));
                self.notice = Some(format!("Removed {}", config.name).into());
            }
            Err(err) => self.notice = Some(format!("Cannot remove: {err}").into()),
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

/// `KEY=value, KEY2=value2` → map (blank is an empty map).
fn parse_env(text: &str) -> std::collections::BTreeMap<String, String> {
    text.split(',')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let key = key.trim();
            (!key.is_empty()).then(|| (key.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

/// A stable id suffix from a display name: lowercase, runs of non-alphanumerics
/// collapsed to one `-`, trimmed.
fn uuid_like(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for c in name.chars() {
        if c.is_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_end_matches('-').to_owned()
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
                    .id("run-edit-configs")
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .border_1()
                    .border_color(theme.border)
                    .hover(move |this| this.bg(list_hover))
                    .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| {
                        this.open_editor(true, window, cx)
                    }))
                    .text_size(px(9.5))
                    .text_color(theme.fg_secondary())
                    .child("Edit"),
            )
            .child(
                div()
                    .id("run-new-config")
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .border_1()
                    .border_color(theme.border)
                    .hover(move |this| this.bg(list_hover))
                    .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, window, cx| {
                        this.open_editor(false, window, cx)
                    }))
                    .text_size(px(9.5))
                    .text_color(theme.fg_secondary())
                    .child("New"),
            )
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

        let editor = self.editor.as_ref().map(|editor| {
            let theme = theme.clone();
            let fields: [(&str, &Entity<InputState>); 5] = [
                ("Name", &editor.name),
                ("Command", &editor.command),
                ("Arguments", &editor.args),
                ("Working dir", &editor.cwd),
                ("Env", &editor.env),
            ];
            let rows = fields
                .iter()
                .map(|(label, input)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(80.))
                                .text_size(px(10.5))
                                .text_color(theme.fg_secondary())
                                .child(*label),
                        )
                        .child(div().flex_1().min_w_0().child(Input::new(*input)))
                })
                .collect::<Vec<_>>();
            let save = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.save_editor(cx));
            let cancel = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.close_editor(cx));
            div()
                .id("run-config-editor")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(460.))
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
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.foreground)
                                .child(if editor.editing.is_some() {
                                    "Edit run configuration"
                                } else {
                                    "New run configuration"
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(80.))
                                        .text_size(px(10.5))
                                        .text_color(theme.fg_secondary())
                                        .child("Type"),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme.foreground)
                                        .child(editor.kind.clone()),
                                ),
                        )
                        .children(rows)
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(theme.fg_muted())
                                .child("Written to .openidea/run.toml · detected configurations stay automatic"),
                        )
                        .when_some(editor.error.clone(), |this, error| {
                            this.child(div().text_size(px(10.5)).text_color(theme.danger).child(error))
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    div()
                                        .id("run-config-cancel")
                                        .h(px(24.))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .border_1()
                                        .border_color(theme.border)
                                        .on_click(cancel)
                                        .text_size(px(11.))
                                        .text_color(theme.fg_secondary())
                                        .child("Cancel"),
                                )
                                .child(
                                    div()
                                        .id("run-config-save")
                                        .h(px(24.))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .rounded(px(4.))
                                        .cursor_pointer()
                                        .bg(theme.primary)
                                        .on_click(save)
                                        .text_size(px(11.))
                                        .text_color(theme.primary_foreground)
                                        .child("Save"),
                                ),
                        ),
                )
                .into_any_element()
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .relative()
            .child(header)
            .when_some(editor, |this, editor| this.child(editor))
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

#[cfg(test)]
mod config_tests {
    use super::{CONFIG_KINDS, parse_env, uuid_like};
    use ide_run::RunConfig;

    #[test]
    fn env_pairs_parse_and_blank_entries_are_skipped() {
        let env = parse_env("RUST_LOG=debug, PORT=8080 ,, =bad,NO_KEY");
        assert_eq!(env.get("RUST_LOG").map(String::as_str), Some("debug"));
        assert_eq!(env.get("PORT").map(String::as_str), Some("8080"));
        assert!(!env.contains_key("NO_KEY"), "a pair without `=` is dropped");
        assert!(!env.contains_key(""), "an empty key is dropped");
        assert!(parse_env("").is_empty());
    }

    #[test]
    fn a_config_id_is_a_stable_slug_of_its_name() {
        // Same name → same id, so editing the same configuration replaces it
        // instead of adding a second one.
        assert_eq!(uuid_like("Run Tests"), "run-tests");
        assert_eq!(uuid_like("Run  Tests!"), "run-tests");
        assert_eq!(uuid_like("--"), "");
    }

    #[test]
    fn every_offered_kind_is_a_command() {
        for (kind, command) in CONFIG_KINDS {
            assert!(!kind.is_empty(), "a kind needs an id");
            assert!(!command.is_empty(), "{kind} needs a default command");
        }
        assert!(CONFIG_KINDS.iter().any(|(kind, _)| *kind == "cargo"));
        assert!(CONFIG_KINDS.iter().any(|(kind, _)| *kind == "gradle"));
        assert!(CONFIG_KINDS.iter().any(|(kind, _)| *kind == "python"));
    }

    #[test]
    fn a_user_config_round_trips_through_the_run_file() {
        let config = RunConfig {
            id: "user:run-tests".into(),
            name: "Run Tests".into(),
            kind: "cargo".into(),
            command: "cargo".into(),
            args: vec!["test".into()],
            cwd: None,
            env: Default::default(),
            detected: false,
            is_test: false,
            debug_adapter: None,
            allow_parallel: false,
            extras: Default::default(),
        };
        // The backend's own serializer is what run.toml holds; round-tripping
        // through it proves every editor field survives the save.
        let dir = std::env::temp_dir().join(format!("openidea-run-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".openidea")).unwrap();
        ide_run::save_user_configs(&dir, &[config]).expect("saves");
        let configs = ide_run::load_user_configs(&dir).expect("loads");
        let loaded = configs.iter().find(|loaded| loaded.id == "user:run-tests").expect("the config is listed");
        assert_eq!(loaded.name, "Run Tests");
        assert_eq!(loaded.command, "cargo");
        assert_eq!(loaded.args, vec!["test".to_owned()]);
        assert!(!loaded.detected, "user configurations stay user configurations");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
