//! Debug tool window: DAP sessions driven through `ide-host`'s `DebugManager`.
//!
//! Starting a session uses the shared `ide_host::ops::debug::debug_start` path
//! (trust checks, adapter resolution, python environment), then every
//! `IdeEvent::Debug` for the session feeds the call stack, variables and
//! console here. Requests run on the host's tokio runtime and post results
//! back through a channel so the GPUI thread never blocks.

use crate::theme::IdeColors as _;
use futures::StreamExt;
use gpui_kit::StatefulInteractiveElement as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputContent, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::DebugSessionId;
use ide_core::event::DebugEvent;
use ide_dap::{LaunchConfig, Scope, StackFrame, StepKind, ThreadInfo, Variable};
use ide_host::{Host, Project};
use ide_run::RunConfig;
use std::path::Path;
use std::sync::Arc;

const MAX_OUTPUT: usize = 800;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DebugAction {
    Resume,
    Pause,
    StepOver,
    StepInto,
    StepOut,
    Stop,
}

#[derive(Clone, PartialEq, Eq)]
enum Status {
    Idle,
    Starting,
    Running,
    Stopped { reason: String },
    Terminated,
}

struct DebugLine {
    category: String,
    text: SharedString,
}

struct StateUpdate {
    seq: u64,
    threads: Vec<ThreadInfo>,
    frames: Vec<StackFrame>,
    frame: Option<i64>,
    scopes: Vec<(Scope, Vec<Variable>)>,
}

pub struct DebugPanel {
    host: Arc<Host>,
    project: Option<Arc<Project>>,
    session: Option<DebugSessionId>,
    name: SharedString,
    status: Status,
    thread: Option<i64>,
    frame: Option<i64>,
    threads: Vec<ThreadInfo>,
    frames: Vec<StackFrame>,
    scopes: Vec<(Scope, Vec<Variable>)>,
    lines: Vec<DebugLine>,
    pending: String,
    notice: Option<SharedString>,
    watches: Vec<(String, Option<String>)>,
    watch_input: Entity<InputState>,
    clear_watch_input: bool,
    watch_seq: u64,
    repl_input: Entity<InputState>,
    clear_repl_input: bool,
    repl_task: Option<Task<()>>,
    seq: u64,
    task: Option<Task<()>>,
    _watch_subscription: Subscription,
    _repl_subscription: Subscription,
    focus_handle: FocusHandle,
}

impl DebugPanel {
    pub fn new(host: Arc<Host>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let watch_input = cx.new(|cx| InputState::new(window, cx).placeholder("Watch expression…"));
        let subscription = cx.subscribe(&watch_input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit_watch(cx);
            }
        });
        let repl_input = cx.new(|cx| InputState::new(window, cx).placeholder("Evaluate expression…"));
        let repl_subscription = cx.subscribe(&repl_input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit_repl(cx);
            }
        });
        Self {
            host,
            project: None,
            session: None,
            name: "Debug".into(),
            status: Status::Idle,
            thread: None,
            frame: None,
            threads: Vec::new(),
            frames: Vec::new(),
            scopes: Vec::new(),
            lines: Vec::new(),
            pending: String::new(),
            notice: None,
            watches: Vec::new(),
            watch_input,
            clear_watch_input: false,
            watch_seq: 0,
            repl_input,
            clear_repl_input: false,
            repl_task: None,
            seq: 0,
            task: None,
            _watch_subscription: subscription,
            _repl_subscription: repl_subscription,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        if let Some(session) = self.session {
            let host = self.host.clone();
            let runtime = host.runtime.clone();
            std::thread::spawn(move || {
                runtime.spawn(async move {
                    let _ = host.debugger.stop(session).await;
                });
            });
        }
        self.project = Some(project.clone());
        self.reset(cx);
    }

    pub fn show_notice(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(text.into());
        cx.notify();
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        self.status = Status::Idle;
        self.thread = None;
        self.frame = None;
        self.threads.clear();
        self.frames.clear();
        self.scopes.clear();
        self.lines.clear();
        self.pending.clear();
        self.notice = None;
        self.task = None;
        cx.notify();
    }

    pub fn start(&mut self, launch: LaunchConfig, cx: &mut Context<Self>) {
        if self.session.is_some() {
            self.notice = Some("A debug session is already active".into());
            cx.notify();
            return;
        }
        self.status = Status::Starting;
        self.name = launch.name.clone().into();
        self.notice = None;
        self.lines.clear();
        self.pending.clear();
        self.threads.clear();
        self.frames.clear();
        self.scopes.clear();
        self.thread = None;
        self.frame = None;
        self.push_line("console", &format!("Starting `{}` with {}…", launch.name, launch.adapter));
        let host = self.host.clone();
        let runtime = host.runtime.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        runtime.spawn(async move {
            let result = ide_host::ops::debug::debug_start(&host, "main", launch).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(session) => {
                            this.session = Some(session);
                            if this.status == Status::Starting {
                                this.status = Status::Running;
                            }
                        }
                        Err(err) => {
                            this.status = Status::Idle;
                            this.notice = Some(err.clone().into());
                            this.push_line("stderr", &err);
                        }
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    pub fn on_event(&mut self, session: DebugSessionId, event: DebugEvent, cx: &mut Context<Self>) {
        // Child sessions started by the adapter share the ownership sink; keep the
        // root session's state authoritative.
        if self.session.is_some_and(|active| active != session) {
            return;
        }
        match &event {
            DebugEvent::Started { name, adapter, parent } => {
                if parent.is_some() {
                    return;
                }
                self.session = Some(session);
                self.name = name.clone().into();
                self.status = Status::Running;
                self.push_line("console", &format!("Attached to {adapter}"));
            }
            DebugEvent::Initialized => {
                self.status = Status::Running;
            }
            DebugEvent::Stopped { thread_id, reason, description, .. } => {
                self.thread = Some(*thread_id);
                self.frame = None;
                self.status = Status::Stopped { reason: description.clone().unwrap_or_else(|| reason.clone()) };
                self.fetch_state(cx, false);
            }
            DebugEvent::Continued { thread_id, .. } => {
                self.thread = Some(*thread_id);
                self.frame = None;
                self.frames.clear();
                self.scopes.clear();
                self.status = Status::Running;
            }
            DebugEvent::Output { category, output } => {
                self.push_text(category, output);
            }
            DebugEvent::Terminated => {
                self.status = Status::Terminated;
                self.session = None;
                self.frames.clear();
                self.scopes.clear();
                self.pending.clear();
                self.push_line("console", "Session terminated");
            }
            DebugEvent::Exited { code } => {
                self.push_line("console", &format!("Program exited with code {code}"));
            }
            DebugEvent::HotCodeReplace { change_type, message } => {
                self.push_line("console", &format!("Hot code replace {change_type}: {message}"));
            }
            DebugEvent::BreakpointsChanged { .. } | DebugEvent::Thread { .. } => {}
        }
        cx.notify();
    }

    pub fn act(&mut self, action: DebugAction, cx: &mut Context<Self>) {
        let Some(session) = self.session else { return };
        match action {
            DebugAction::Resume | DebugAction::StepOver | DebugAction::StepInto | DebugAction::StepOut
                if !self.stopped() =>
            {
                return;
            }
            DebugAction::Pause if self.stopped() => return,
            _ => {}
        }
        let thread = self.thread.unwrap_or(1);
        let host = self.host.clone();
        let runtime = host.runtime.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        runtime.spawn(async move {
            let result = match action {
                DebugAction::Resume => host.debugger.resume(session, thread).await,
                DebugAction::Pause => host.debugger.pause(session, thread).await,
                DebugAction::StepOver => host.debugger.step(session, thread, StepKind::Over).await,
                DebugAction::StepInto => host.debugger.step(session, thread, StepKind::Into).await,
                DebugAction::StepOut => host.debugger.step(session, thread, StepKind::Out).await,
                DebugAction::Stop => host.debugger.stop(session).await,
            };
            let _ = tx.unbounded_send(result.map_err(|err| err.to_string()));
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                if let Err(err) = result {
                    let _ = this.update(cx, |this, cx| {
                        this.notice = Some(err.into());
                        cx.notify();
                    });
                }
            }
        }));
        match action {
            DebugAction::Resume | DebugAction::StepInto | DebugAction::StepOver | DebugAction::StepOut => {
                self.status = Status::Running;
                self.frames.clear();
                self.scopes.clear();
            }
            DebugAction::Pause | DebugAction::Stop => {}
        }
        cx.notify();
    }

    fn fetch_state(&mut self, cx: &mut Context<Self>, keep_frame: bool) {
        let Some(session) = self.session else { return };
        let seq = self.seq + 1;
        self.seq = seq;
        let host = self.host.clone();
        let runtime = host.runtime.clone();
        let thread_hint = self.thread;
        let frame_hint = if keep_frame { self.frame } else { None };
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        runtime.spawn(async move {
            let threads = host.debugger.threads(session).await.unwrap_or_default();
            let thread = thread_hint.or_else(|| threads.first().map(|thread| thread.id));
            let frames = match thread {
                Some(thread) => host.debugger.stack_trace(session, thread).await.unwrap_or_default(),
                None => Vec::new(),
            };
            let frame = frame_hint
                .filter(|frame| frames.iter().any(|candidate| candidate.id == *frame))
                .or_else(|| frames.first().map(|frame| frame.id));
            let mut scopes = Vec::new();
            if let Some(frame) = frame {
                if let Ok(list) = host.debugger.scopes(session, frame).await {
                    for scope in list {
                        let variables =
                            host.debugger.variables(session, scope.variables_reference).await.unwrap_or_default();
                        scopes.push((scope, variables));
                    }
                }
            }
            let _ = tx.unbounded_send(StateUpdate { seq, threads, frames, frame, scopes });
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(update) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    if update.seq < this.seq {
                        return;
                    }
                    this.threads = update.threads;
                    this.frames = update.frames;
                    this.frame = update.frame;
                    this.scopes = update.scopes;
                    if this.thread.is_none() {
                        this.thread = this.frames.first().map(|_| 1);
                    }
                    if this.stopped() && !this.watches.is_empty() {
                        this.refresh_watches(cx);
                    }
                    cx.notify();
                });
            }
        }));
    }

    fn select_frame(&mut self, frame: i64, cx: &mut Context<Self>) {
        self.frame = Some(frame);
        self.fetch_state(cx, true);
        cx.notify();
    }

    fn submit_watch(&mut self, cx: &mut Context<Self>) {
        let expression = self.watch_input.read(cx).value().trim().to_owned();
        if expression.is_empty() {
            return;
        }
        if !self.watches.iter().any(|(watched, _)| *watched == expression) {
            self.watches.push((expression, None));
        }
        self.clear_watch_input = true;
        self.refresh_watches(cx);
        cx.notify();
    }

    fn submit_repl(&mut self, cx: &mut Context<Self>) {
        let expression = self.repl_input.read(cx).value().trim().to_owned();
        if expression.is_empty() {
            return;
        }
        self.clear_repl_input = true;
        self.push_line("console", &format!("> {expression}"));
        let Some(session) = self.session else {
            self.push_line("stderr", "No active debug session");
            cx.notify();
            return;
        };
        if !self.stopped() {
            self.push_line("stderr", "Session is running; pause before evaluating");
            cx.notify();
            return;
        }
        let frame = self.frame;
        let host = self.host.clone();
        let runtime = host.runtime.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        runtime.spawn(async move {
            let result = host.debugger.evaluate(session, frame, expression).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.repl_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(variable) => {
                            let text = match variable.type_name {
                                Some(type_name) => format!("{}  ·  {type_name}", variable.value),
                                None => variable.value,
                            };
                            this.push_line("stdout", &text);
                        }
                        Err(err) => this.push_line("stderr", &err),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn refresh_watches(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session else { return };
        if self.watches.is_empty() || !self.stopped() {
            return;
        }
        let frame = self.frame;
        let expressions: Vec<String> = self.watches.iter().map(|(expression, _)| expression.clone()).collect();
        let host = self.host.clone();
        let runtime = host.runtime.clone();
        let seq = self.watch_seq + 1;
        self.watch_seq = seq;
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        runtime.spawn(async move {
            let mut values = Vec::with_capacity(expressions.len());
            for expression in expressions {
                let value = match host.debugger.evaluate(session, frame, expression).await {
                    Ok(variable) => match variable.type_name {
                        Some(type_name) => format!("{}  ·  {type_name}", variable.value),
                        None => variable.value,
                    },
                    Err(err) => format!("⚠ {err}"),
                };
                values.push(value);
            }
            let _ = tx.unbounded_send((seq, values));
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some((seq, values)) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    if seq == this.watch_seq {
                        for (watch, value) in this.watches.iter_mut().zip(values) {
                            watch.1 = Some(value);
                        }
                        cx.notify();
                    }
                });
            }
        }));
    }

    fn push_text(&mut self, category: &str, text: &str) {
        for ch in text.chars() {
            match ch {
                '\n' => {
                    let line = std::mem::take(&mut self.pending);
                    self.lines.push(DebugLine { category: category.to_owned(), text: line.into() });
                }
                '\r' => self.pending.clear(),
                _ => self.pending.push(ch),
            }
        }
        if self.lines.len() > MAX_OUTPUT {
            self.lines.drain(0..200);
        }
    }

    fn push_line(&mut self, category: &str, text: &str) {
        self.lines.push(DebugLine { category: category.to_owned(), text: text.to_owned().into() });
        if self.lines.len() > MAX_OUTPUT {
            self.lines.drain(0..200);
        }
    }

    fn clear_output(&mut self, cx: &mut Context<Self>) {
        self.lines.clear();
        self.pending.clear();
        cx.notify();
    }

    fn status_text(&self) -> String {
        match &self.status {
            Status::Idle => "No session".to_owned(),
            Status::Starting => "Starting…".to_owned(),
            Status::Running => "Running".to_owned(),
            Status::Stopped { reason } => format!("Paused: {reason}"),
            Status::Terminated => "Session finished".to_owned(),
        }
    }

    fn stopped(&self) -> bool {
        matches!(self.status, Status::Stopped { .. })
    }

    fn active(&self) -> bool {
        self.session.is_some()
    }
}

/// Builds the DAP launch request for a run configuration with a debug adapter.
///
/// Cargo/Gradle/Java launch builders from the TypeScript frontend are not ported
/// yet; program/args are passed straight through for shell/node/python targets.
pub fn launch_config_for(config: &RunConfig, root: &Path) -> Option<LaunchConfig> {
    let adapter = config.debug_adapter.clone()?;
    let (program, args) =
        if matches!(config.kind.as_str(), "shell" | "node" | "python" | "pytest") && !config.args.is_empty() {
            (Some(config.args[0].clone()), config.args[1..].to_vec())
        } else {
            (None, config.args.clone())
        };
    Some(LaunchConfig {
        name: config.name.clone(),
        adapter,
        request: "launch".into(),
        program,
        args,
        cwd: config.cwd.clone().or_else(|| Some(root.to_path_buf().into())),
        env: config.env.clone(),
        extra_json: None,
    })
}

impl Focusable for DebugPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DebugPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.clear_watch_input {
            self.clear_watch_input = false;
            self.watch_input.update(cx, |state, cx| state.set_value(InputContent::new(""), window, cx));
        }
        if self.clear_repl_input {
            self.clear_repl_input = false;
            self.repl_input.update(cx, |state, cx| state.set_value(InputContent::new(""), window, cx));
        }
        let theme = cx.theme().clone();
        let status = self.status_text();
        let stopped = self.stopped();
        let active = self.active();
        let name = self.name.clone();
        let notice = self.notice.clone();
        let thread_id = self.thread;
        let selected_frame = self.frame;
        let frames: Vec<StackFrame> = self.frames.clone();
        let scopes: Vec<(Scope, Vec<Variable>)> = self.scopes.clone();
        let lines: Vec<(String, SharedString)> =
            self.lines.iter().map(|line| (line.category.clone(), line.text.clone())).collect();
        let session = self.session;
        let thread_count = self.threads.len();
        let watches: Vec<(String, Option<String>)> = self.watches.clone();
        let list_hover = theme.list_hover;

        let btn = |id: &'static str, icon: IconName, label: &'static str, enabled: bool, tint: Hsla| {
            div()
                .id(id)
                .h(px(22.))
                .px_1p5()
                .flex()
                .items_center()
                .gap_1()
                .rounded(px(4.))
                .when(enabled, |this| this.cursor_pointer().hover(move |this| this.bg(list_hover)))
                .when(!enabled, |this| this.opacity(0.4))
                .child(Icon::new(icon).xsmall().text_color(if enabled { tint } else { theme.fg_muted() }))
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(if enabled { theme.fg() } else { theme.fg_muted() })
                        .child(label),
                )
        };
        let on_resume = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.act(DebugAction::Resume, cx));
        let on_pause = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.act(DebugAction::Pause, cx));
        let on_over = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.act(DebugAction::StepOver, cx));
        let on_into = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.act(DebugAction::StepInto, cx));
        let on_out = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.act(DebugAction::StepOut, cx));
        let on_stop = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.act(DebugAction::Stop, cx));
        let on_clear = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.clear_output(cx));

        let toolbar = div()
            .h(px(28.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(Icon::new(IconName::Bug).xsmall().text_color(theme.warning))
            .child(div().text_size(px(11.)).text_color(theme.fg_strong()).child(name))
            .child(div().px_1())
            .child(btn("debug-resume", IconName::Play, "Resume", stopped, theme.success).on_click(on_resume))
            .child(btn("debug-pause", IconName::Pause, "Pause", active && !stopped, theme.warning).on_click(on_pause))
            .child(btn("debug-step-over", IconName::ArrowRight, "Over", stopped, theme.info).on_click(on_over))
            .child(btn("debug-step-into", IconName::ArrowDown, "Into", stopped, theme.info).on_click(on_into))
            .child(btn("debug-step-out", IconName::ArrowUp, "Out", stopped, theme.info).on_click(on_out))
            .child(btn("debug-stop", IconName::Square, "Stop", active, theme.danger).on_click(on_stop))
            .child(div().flex_1())
            .child(div().text_size(px(10.5)).text_color(theme.fg_muted()).child(status));

        let frames_theme = theme.clone();
        let frames_panel = div()
            .w(px(330.))
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .text_size(px(10.))
                    .text_color(theme.fg_muted())
                    .child("Call Stack"),
            )
            .when(frames.is_empty(), |this| {
                this.child(div().px_2().py_1().text_size(px(11.)).text_color(theme.fg_muted()).child(if active {
                    "No frames"
                } else {
                    "Start a debug session with Shift+F9"
                }))
            })
            .child(div().id("debug-frames").flex_1().min_h_0().overflow_y_scroll().children(
                frames.iter().enumerate().map(|(index, frame)| {
                    let frame_hover = frames_theme.list_hover;
                    let id = frame.id;
                    let selected = selected_frame == Some(id);
                    let prefixes = "  ".repeat(index.min(6));
                    let path = frame.path.as_ref().map(|path| path.display().to_string()).unwrap_or_default();
                    let file = Path::new(&path)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone());
                    div()
                        .id(ElementId::Name(format!("frame-{index}").into()))
                        .h(px(20.))
                        .w_full()
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .when(selected, |this| this.bg(frames_theme.list_active))
                        .hover(move |this| this.bg(frame_hover))
                        .on_click(
                            cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| this.select_frame(id, cx)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.))
                                .text_color(if selected { frames_theme.fg_strong() } else { frames_theme.fg() })
                                .child(format!("{prefixes}{}", frame.name)),
                        )
                        .child(div().text_size(px(10.)).text_color(frames_theme.fg_muted()).child(if path.is_empty() {
                            format!("{}", frame.line + 1)
                        } else {
                            format!("{file}:{}", frame.line + 1)
                        }))
                }),
            ));

        let vars_panel = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .text_size(px(10.))
                    .text_color(theme.fg_muted())
                    .child("Watches · Enter to add"),
            )
            .child(div().px_1().pb_1().child(Input::new(&self.watch_input)))
            .when(watches.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .pb(px(2.))
                        .text_size(px(10.))
                        .text_color(theme.fg_muted())
                        .child("Type an expression and press Enter"),
                )
            })
            .children(watches.iter().take(4).map(|(expression, value)| {
                div()
                    .h(px(18.))
                    .w_full()
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .whitespace_nowrap()
                    .text_size(px(11.))
                    .child(div().text_color(theme.link).child(expression.clone()))
                    .child(div().text_color(theme.fg_muted()).child("="))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.fg())
                            .child(value.clone().unwrap_or_else(|| "…".into())),
                    )
            }))
            .child(
                div()
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(10.))
                    .text_color(theme.fg_muted())
                    .child(format!("Variables{}", thread_id.map(|id| format!(" · thread {id}")).unwrap_or_default())),
            )
            .child(div().id("debug-vars").flex_1().min_h_0().overflow_y_scroll().p_1().children(
                scopes.iter().enumerate().map(|(scope_index, (scope, variables))| {
                    div()
                        .flex()
                        .flex_col()
                        .child(div().h(px(18.)).text_size(px(10.5)).text_color(theme.warning).child(format!(
                            "{} ({})",
                            scope.name,
                            variables.len()
                        )))
                        .children(variables.iter().enumerate().map(|(index, variable)| {
                            div()
                                .id(ElementId::Name(format!("var-{scope_index}-{index}").into()))
                                .h(px(18.))
                                .w_full()
                                .px_1()
                                .flex()
                                .items_center()
                                .gap_1()
                                .whitespace_nowrap()
                                .text_size(px(11.))
                                .child(div().text_color(theme.link).child(variable.name.clone()))
                                .child(div().text_color(theme.fg_muted()).child("="))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(theme.fg())
                                        .child(variable.value.clone()),
                                )
                                .when_some(variable.type_name.clone(), |this, type_name| {
                                    this.child(div().text_size(px(9.5)).text_color(theme.fg_muted()).child(type_name))
                                })
                        }))
                }),
            ));

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(toolbar)
            .when_some(notice, |this, notice| {
                this.child(div().px_2().py(px(2.)).text_size(px(10.5)).text_color(theme.danger).child(notice))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(frames_panel)
                    .child(vars_panel),
            )
            .child(
                div()
                    .h(px(150.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(20.))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(px(10.))
                            .text_color(theme.fg_muted())
                            .child("Console · Enter to evaluate")
                            .child(
                                div()
                                    .id("debug-clear")
                                    .px_1()
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .hover(move |this| this.bg(list_hover))
                                    .on_click(on_clear)
                                    .child("Clear"),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .justify_end()
                            .overflow_hidden()
                            .font_family(theme.mono_font_family.clone())
                            .text_size(px(11.))
                            .children(lines.iter().rev().take(8).rev().map(|(category, text)| {
                                let color = match category.as_str() {
                                    "stderr" => theme.danger,
                                    "stdout" => theme.fg(),
                                    "telemetry" => theme.fg_muted(),
                                    _ => theme.fg_secondary(),
                                };
                                div()
                                    .h(px(15.))
                                    .w_full()
                                    .px_2()
                                    .whitespace_nowrap()
                                    .truncate()
                                    .text_color(color)
                                    .child(text.clone())
                            })),
                    )
                    .child(div().px_1().pb_1().child(Input::new(&self.repl_input))),
            )
            .child(
                div()
                    .h(px(18.))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(10.))
                    .text_color(theme.fg_muted())
                    .child(match session {
                        Some(session) => format!("Session {}", session.0),
                        None => "No session".to_owned(),
                    })
                    .child(format!("{thread_count} threads · {} frames", self.frames.len())),
            )
            .into_any_element()
    }
}
