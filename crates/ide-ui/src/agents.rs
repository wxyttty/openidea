//! Agents tool window: ACP coding-agent tasks of the project.
//!
//! Runs on `ide-agent` through the same `ide-host::ops::agent` calls the Tauri
//! shell used: providers discovered from PATH/config, tasks streamed through a
//! subscription, prompts created/sent on the host runtime. The MVP shows the
//! conversation (user/agent/thought/tool entries), starts a local task with the
//! selected provider and cancels a running turn; permissions, worktree
//! isolation and agent settings stay with the Tauri shell for now.

use std::path::PathBuf;
use std::sync::Arc;

use crate::theme::IdeColors as _;
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_agent::{
    AgentEntry, AgentEvent, AgentPrompt, AgentProvider, AgentStatus, AgentTask, AgentToolStatus, Isolation,
    NewAgentTask,
};
use ide_host::Host;

pub struct AgentPanel {
    host: Arc<Host>,
    project: Option<Arc<ide_host::Project>>,
    providers: Vec<AgentProvider>,
    selected_provider: usize,
    tasks: Vec<AgentTask>,
    selected_task: Option<String>,
    entries: Vec<AgentEntry>,
    prompt: Entity<InputState>,
    notice: Option<SharedString>,
    subscribed: Option<PathBuf>,
    sending: bool,
    clear_prompt: bool,
    _prompt_subscription: Subscription,
    event_task: Option<Task<()>>,
    request_task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Starting => "starting",
        AgentStatus::Running => "running",
        AgentStatus::NeedsInput => "needs input",
        AgentStatus::Ready => "ready",
        AgentStatus::Failed => "failed",
        AgentStatus::Limited => "limited",
        AgentStatus::Archived => "archived",
    }
}

fn tool_status(status: AgentToolStatus) -> &'static str {
    match status {
        AgentToolStatus::Pending => "pending",
        AgentToolStatus::Running => "running",
        AgentToolStatus::Completed => "done",
        AgentToolStatus::Failed => "failed",
    }
}

impl AgentPanel {
    pub fn new(host: Arc<Host>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prompt = cx.new(|cx| InputState::new(window, cx).placeholder("Prompt the selected agent…"));
        let prompt_subscription = cx.subscribe(&prompt, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.send(cx);
            }
        });
        Self {
            host,
            project: None,
            providers: Vec::new(),
            selected_provider: 0,
            tasks: Vec::new(),
            selected_task: None,
            entries: Vec::new(),
            prompt,
            notice: None,
            subscribed: None,
            sending: false,
            clear_prompt: false,
            _prompt_subscription: prompt_subscription,
            event_task: None,
            request_task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<ide_host::Project>, cx: &mut Context<Self>) {
        self.project = Some(project.clone());
        self.providers.clear();
        self.tasks.clear();
        self.entries.clear();
        self.selected_task = None;
        self.selected_provider = 0;
        self.notice = None;
        self.subscribed = None;
        cx.notify();
    }

    pub fn ensure(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.subscribed.is_none() {
            self.refresh(cx);
            self.subscribe(cx);
        }
        let handle = self.focus_handle.clone();
        window.focus(&handle, cx);
        cx.notify();
        let entity = cx.entity();
        window.on_next_frame(move |window, cx| {
            let _ = entity.update(cx, |this, cx| {
                let handle = this.prompt.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
                cx.notify();
            });
        });
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        if project.require_trust().is_err() {
            self.notice = Some("Agents are disabled in Restricted Mode — trust the project first".into());
            cx.notify();
            return;
        }
        match ide_host::ops::agent::providers(&self.host, "main") {
            Ok(providers) => {
                if self.selected_provider >= providers.len() {
                    self.selected_provider = 0;
                }
                self.providers = providers;
                if self.providers.is_empty() {
                    self.notice = Some("No agent providers configured".into());
                } else {
                    self.notice = None;
                }
            }
            Err(err) => self.notice = Some(err.to_string().into()),
        }
        cx.notify();
    }

    fn subscribe(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        if self.subscribed.as_ref() == Some(&project.root) {
            return;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<AgentEvent>();
        let sink: ide_host::ops::Emit<AgentEvent> = Arc::new(move |event| {
            let _ = tx.unbounded_send(event);
        });
        match ide_host::ops::agent::subscribe(&self.host, "main", sink) {
            Ok(tasks) => {
                self.tasks = tasks;
                self.subscribed = Some(project.root.clone());
            }
            Err(err) => {
                self.notice = Some(err.to_string().into());
                cx.notify();
                return;
            }
        }
        self.event_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.on_event(event, cx);
                });
            }
        }));
        cx.notify();
    }

    fn on_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::Task { task } => {
                if let Some(existing) = self.tasks.iter_mut().find(|existing| existing.id == task.id) {
                    *existing = task.clone();
                } else {
                    self.tasks.push(task.clone());
                }
                if self.selected_task.is_none() {
                    self.selected_task = Some(task.id.clone());
                    self.entries = ide_host::ops::agent::entries(&self.host, "main", task.id, None).unwrap_or_default();
                }
            }
            AgentEvent::Removed { id } => {
                self.tasks.retain(|task| task.id != id);
                if self.selected_task.as_deref() == Some(id.as_str()) {
                    self.selected_task = None;
                    self.entries.clear();
                }
            }
            AgentEvent::Truncated { task, .. } => {
                if self.selected_task.as_deref() == Some(task.as_str())
                    && let Ok(entries) = ide_host::ops::agent::entries(&self.host, "main", task, None)
                {
                    self.entries = entries;
                }
            }
            AgentEvent::Entry { task, subagent: None, index, entry } => {
                if self.selected_task.as_deref() == Some(task.as_str()) {
                    let index = index as usize;
                    if index < self.entries.len() {
                        self.entries[index] = entry;
                    } else {
                        while self.entries.len() < index {
                            self.entries.push(AgentEntry::Agent { text: String::new() });
                        }
                        self.entries.push(entry);
                    }
                }
            }
            AgentEvent::Entry { .. } => {}
        }
        cx.notify();
    }

    fn select_task(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected_task = Some(id.clone());
        self.entries = ide_host::ops::agent::entries(&self.host, "main", id, None).unwrap_or_default();
        cx.notify();
    }

    fn prompt_text(&self, cx: &App) -> String {
        self.prompt.read(cx).value().trim().to_owned()
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        let text = self.prompt_text(cx);
        if text.is_empty() {
            return;
        }
        if !self.project.as_ref().is_some_and(|project| project.require_trust().is_ok()) {
            self.notice = Some("Trust the project to run agents".into());
            cx.notify();
            return;
        }
        self.clear_prompt = true;
        self.notice = None;
        let prompt = AgentPrompt { text, files: Vec::new(), images: Vec::new() };
        if let Some(task) = self.selected_task.clone() {
            let host = self.host.clone();
            let (tx, mut rx) = futures::channel::mpsc::unbounded();
            self.host.runtime.spawn(async move {
                let result = ide_host::ops::agent::prompt(&host, "main", task, prompt, true)
                    .await
                    .map_err(|err| err.to_string());
                let _ = tx.unbounded_send(result);
            });
            self.request_task = Some(cx.spawn(async move |this, cx| {
                while let Some(result) = rx.next().await {
                    let _ = this.update(cx, |this, cx| {
                        if let Err(err) = result {
                            this.notice = Some(err.into());
                        }
                        cx.notify();
                    });
                }
            }));
            cx.notify();
            return;
        }
        let Some(provider) = self.providers.get(self.selected_provider).cloned() else {
            self.notice = Some("Select an agent provider first".into());
            cx.notify();
            return;
        };
        if !provider.installed {
            self.notice = Some(format!("{} is not installed ({})", provider.name, provider.command).into());
            cx.notify();
            return;
        }
        self.sending = true;
        let host = self.host.clone();
        let request = NewAgentTask {
            project_root: None,
            prompt,
            provider: provider.id.clone(),
            config: Vec::new(),
            isolation: Isolation::Local,
            base: None,
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::agent::create(&host, "main", request).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.request_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.sending = false;
                    match result {
                        Ok(task) => {
                            this.selected_task = Some(task.id.clone());
                            this.tasks.push(task.clone());
                            this.entries =
                                ide_host::ops::agent::entries(&this.host, "main", task.id, None).unwrap_or_default();
                        }
                        Err(err) => {
                            tracing::info!(%err, "agent: create failed");
                            this.notice = Some(err.into());
                        }
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(task) = self.selected_task.clone() else { return };
        if let Err(err) = ide_host::ops::agent::cancel(&self.host, "main", task, None) {
            self.notice = Some(err.to_string().into());
        }
        cx.notify();
    }

    fn entry_element(entry: &AgentEntry, theme: &gpui_kit::component::Theme) -> AnyElement {
        let mono = theme.mono_font_family.clone();
        let (label, text, color) = match entry {
            AgentEntry::User { text, .. } => ("YOU", text.clone(), theme.accent),
            AgentEntry::Agent { text } => ("AGENT", text.clone(), theme.foreground),
            AgentEntry::Thought { text } => ("THOUGHT", text.clone(), theme.fg_muted()),
            AgentEntry::Tool { title, kind, status, output, .. } => {
                let output = output.as_deref().unwrap_or("").trim();
                let text = if output.is_empty() { title.clone() } else { format!("{title}\n{output}") };
                ("TOOL", format!("{kind} · {}\n{text}", tool_status(*status)), theme.info)
            }
            AgentEntry::Plan { items } => {
                let text = items
                    .iter()
                    .map(|item| format!("• [{:?}] {}", item.status, item.content))
                    .collect::<Vec<_>>()
                    .join("\n");
                ("PLAN", text, theme.warning)
            }
            AgentEntry::Subagent { title, status, .. } => (
                "SUBAGENT",
                format!("{} · {status:?}", title.clone().unwrap_or_else(|| "subagent".into())),
                theme.fg_secondary(),
            ),
            AgentEntry::Message { incoming, text } => {
                ("MESSAGE", format!("{} {text}", if *incoming { "→" } else { "←" }), theme.fg_secondary())
            }
            AgentEntry::Permission { title, options, chosen, .. } => (
                "PERMISSION",
                format!(
                    "{title}{}",
                    chosen
                        .clone()
                        .or_else(|| Some(
                            options.iter().map(|option| option.name.clone()).collect::<Vec<_>>().join(" / ")
                        ))
                        .map(|detail| format!(" · {detail}"))
                        .unwrap_or_default()
                ),
                theme.warning,
            ),
            AgentEntry::Error { message } => ("ERROR", message.clone(), theme.danger),
        };
        div()
            .w_full()
            .flex()
            .gap_2()
            .p_2()
            .rounded(px(4.))
            .bg(theme.muted)
            .child(div().w(px(64.)).flex_shrink_0().text_size(px(8.5)).text_color(color).child(label))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_family(mono)
                    .text_size(px(10.5))
                    .text_color(theme.fg_secondary())
                    .child(text),
            )
            .into_any_element()
    }
}

impl Focusable for AgentPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AgentPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.clear_prompt {
            self.clear_prompt = false;
            self.prompt.update(cx, |input, cx| input.set_value("", window, cx));
        }
        let theme = cx.theme().clone();
        let hover = theme.list_hover;
        let send = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.send(cx));
        let cancel = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.cancel(cx));
        let notice = self.notice.clone();

        let mut providers = div().flex().flex_col().gap_1().p_2();
        providers = providers.child(div().text_size(px(9.)).text_color(theme.fg_muted()).child("TASKS"));
        for task in &self.tasks {
            let id = task.id.clone();
            let active = self.selected_task.as_deref() == Some(task.id.as_str());
            let pick =
                cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| this.select_task(id.clone(), cx));
            providers = providers.child(
                div()
                    .id(ElementId::Name(format!("agent-task-{}", task.id).into()))
                    .h(px(22.))
                    .w_full()
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .when(active, |this| this.bg(theme.list_active))
                    .hover(move |this| this.bg(hover))
                    .on_click(pick)
                    .child(Icon::new(IconName::Bot).xsmall().text_color(theme.fg_muted()))
                    .child(
                        div().truncate().text_size(px(10.)).text_color(theme.fg_secondary()).child(task.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(8.5))
                            .text_color(if task.status == AgentStatus::Running {
                                theme.success
                            } else {
                                theme.fg_muted()
                            })
                            .child(status_label(task.status)),
                    ),
            );
        }
        providers = providers.child(div().pt_2().text_size(px(9.)).text_color(theme.fg_muted()).child("PROVIDERS"));
        for (index, provider) in self.providers.iter().enumerate() {
            let active = index == self.selected_provider;
            let pick = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                this.selected_provider = index;
                cx.notify();
            });
            providers = providers.child(
                div()
                    .id(ElementId::Name(format!("agent-provider-{index}").into()))
                    .h(px(22.))
                    .w_full()
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .when(active, |this| this.bg(theme.list_active))
                    .hover(move |this| this.bg(hover))
                    .on_click(pick)
                    .child(div().size(px(6.)).rounded_full().bg(if provider.installed {
                        theme.success
                    } else {
                        theme.fg_muted()
                    }))
                    .child(
                        div().truncate().text_size(px(10.5)).text_color(theme.foreground).child(provider.name.clone()),
                    ),
            );
        }

        let mut conversation =
            div().id("agent-conversation").flex_1().min_h_0().flex().flex_col().gap_1().p_2().overflow_y_scroll();
        if let Some(task) = self.selected_task.as_ref().and_then(|id| self.tasks.iter().find(|task| &task.id == id))
            && let Some(error) = &task.error
        {
            conversation = conversation.child(
                div()
                    .w_full()
                    .p_2()
                    .rounded(px(4.))
                    .bg(theme.muted)
                    .text_size(px(10.))
                    .text_color(theme.danger)
                    .child(format!("Task failed: {error}")),
            );
        }
        if self.entries.is_empty() {
            conversation = conversation.child(
                div().text_size(px(11.)).text_color(theme.fg_muted()).child("Start a task by typing a prompt below."),
            );
        }
        for entry in &self.entries {
            conversation = conversation.child(Self::entry_element(entry, &theme));
        }

        let title = self
            .selected_task
            .as_ref()
            .and_then(|id| self.tasks.iter().find(|task| &task.id == id))
            .map(|task| format!("{} · {} · {}", task.title, task.provider, status_label(task.status)))
            .unwrap_or_else(|| "Agents".to_owned());

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
                    .child(Icon::new(IconName::Bot).xsmall().text_color(theme.info))
                    .child(div().truncate().text_size(px(11.)).text_color(theme.foreground).child(title))
                    .child(div().flex_1())
                    .when_some(notice, |this, notice| {
                        this.child(div().truncate().text_size(px(10.)).text_color(theme.warning).child(notice))
                    })
                    .when(self.selected_task.is_some(), |this| {
                        this.child(
                            div()
                                .id("agent-cancel")
                                .h(px(18.))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .border_1()
                                .border_color(theme.border)
                                .hover(move |this| this.bg(hover))
                                .on_click(cancel)
                                .text_size(px(9.5))
                                .text_color(theme.fg_secondary())
                                .child("Cancel"),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(
                        div()
                            .id("agent-sidebar")
                            .w(px(190.))
                            .flex_shrink_0()
                            .h_full()
                            .bg(theme.muted)
                            .border_r_1()
                            .border_color(theme.border)
                            .overflow_y_scroll()
                            .child(providers),
                    )
                    .child(conversation),
            )
            .child(
                div()
                    .p_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_w_0().child(Input::new(&self.prompt)))
                    .child(
                        div()
                            .id("agent-send")
                            .h(px(26.))
                            .px_3()
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .bg(theme.primary)
                            .hover(move |this| this.opacity(0.9))
                            .on_click(send)
                            .text_size(px(11.))
                            .text_color(theme.primary_foreground)
                            .child("Send"),
                    ),
            )
    }
}
