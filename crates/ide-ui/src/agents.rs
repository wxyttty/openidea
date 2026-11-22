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
    /// How the next task is isolated: the shared checkout or its own worktree.
    isolation: Isolation,
    /// Whether the ACP Registry list is showing.
    registry_open: bool,
    /// Registry agents, when loaded.
    registry: Vec<ide_agent::registry::RegistryAgent>,
    /// The registry's notice (warning, install progress, failure).
    registry_notice: Option<SharedString>,
    /// Id of the registry agent being installed.
    installing: Option<String>,
    _prompt_subscription: Subscription,
    event_task: Option<Task<()>>,
    request_task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

/// Whether a permission option rejects (rendered as a dangerous action).
pub fn permission_is_destructive(kind: &str) -> bool {
    kind.starts_with("reject")
}

/// The label the isolation control shows.
pub fn isolation_label(isolation: Isolation) -> &'static str {
    match isolation {
        Isolation::Local => "Isolation: local checkout",
        Isolation::Worktree => "Isolation: worktree",
    }
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
            isolation: Isolation::Local,
            registry_open: false,
            registry: Vec::new(),
            registry_notice: None,
            installing: None,
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
        match ide_host::ops::agent::agent_providers(&self.host, "main") {
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
        let channel = ide_host::ops::Channel::new(move |event| {
            let _ = tx.unbounded_send(event);
        });
        match ide_host::ops::agent::agent_subscribe(&self.host, "main", channel, "gpui".into()) {
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
                let task = *task;
                if let Some(existing) = self.tasks.iter_mut().find(|existing| existing.id == task.id) {
                    *existing = task.clone();
                } else {
                    self.tasks.push(task.clone());
                }
                if self.selected_task.is_none() {
                    self.selected_task = Some(task.id.clone());
                    self.entries = ide_host::ops::agent::agent_entries(&self.host, "main", task.id, None).unwrap_or_default();
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
                    && let Ok(entries) = ide_host::ops::agent::agent_entries(&self.host, "main", task, None)
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
            AgentEvent::Append { task, subagent: None, index, from, text } => {
                // The streamed text repeats from `from` (UTF-16 units) what the entry
                // already holds, so only the tail is appended.
                if self.selected_task.as_deref() == Some(task.as_str())
                    && let Some(entry) = self.entries.get_mut(index as usize)
                {
                    let (held, tail) = match entry {
                        AgentEntry::Agent { text } => (text.len(), text.as_str()),
                        AgentEntry::Thought { text, .. } => (text.len(), text.as_str()),
                        _ => (0, ""),
                    };
                    let _ = tail;
                    if (from as usize) <= held && held < from as usize + text.len() {
                        let rest = text[held - from as usize..].to_owned();
                        match entry {
                            AgentEntry::Agent { text } => text.push_str(&rest),
                            AgentEntry::Thought { text, .. } => text.push_str(&rest),
                            _ => {}
                        }
                    }
                }
            }
            AgentEvent::Entry { .. } | AgentEvent::Append { .. } => {}
            AgentEvent::Limits { .. }
            | AgentEvent::ModelCatalog { .. }
            | AgentEvent::Providers
            | AgentEvent::Attention { .. } => {}
        }
        cx.notify();
    }

    fn select_task(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected_task = Some(id.clone());
        self.entries = ide_host::ops::agent::agent_entries(&self.host, "main", id, None).unwrap_or_default();
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
                let result = ide_host::ops::agent::agent_prompt(&host, "main", task, prompt, true)
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
            isolation: self.isolation,
            base: None,
            mcp_overrides: Default::default(),
            skill_overrides: Default::default(),
            idle: false,
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::agent::agent_create(&host, "main", request).await.map_err(|err| err.to_string());
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
                                ide_host::ops::agent::agent_entries(&this.host, "main", task.id, None).unwrap_or_default();
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
        if let Err(err) = ide_host::ops::agent::agent_cancel(&self.host, "main", task, None) {
            self.notice = Some(err.to_string().into());
        }
        cx.notify();
    }

    /// Answers a pending permission request: `option` is one of the request's
    /// option ids, or `None` to cancel the request.
    ///
    /// The agent blocks its turn until the request is answered, so this is what
    /// lets a tool-needing task continue instead of waiting forever.
    pub fn respond(&mut self, permission: u32, option: Option<String>, cx: &mut Context<Self>) {
        let Some(task) = self.selected_task.clone() else { return };
        match ide_host::ops::agent::agent_respond(&self.host, "main", task.clone(), permission, option) {
            Ok(()) => {
                // Re-read the conversation so the entry shows its chosen option
                // immediately, before the next streamed event arrives.
                self.entries = ide_host::ops::agent::agent_entries(&self.host, "main", task, None).unwrap_or_default();
                self.notice = None;
            }
            Err(err) => self.notice = Some(err.to_string().into()),
        }
        cx.notify();
    }

    /// The isolation a new task runs in (`Local` shares the checkout,
    /// `Worktree` gives the agent its own branch).
    pub fn isolation(&self) -> Isolation {
        self.isolation
    }

    /// Switches how the next task is isolated.
    pub fn set_isolation(&mut self, isolation: Isolation, cx: &mut Context<Self>) {
        self.isolation = isolation;
        cx.notify();
    }

    /// Opens the ACP Registry list (`registry_list`), showing what is installed
    /// and what can be installed or updated.
    pub fn open_registry(&mut self, cx: &mut Context<Self>) {
        self.registry_open = true;
        self.registry_notice = Some("Loading the registry…".into());
        cx.notify();
        let host = self.host.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::agent::agent_registry_list(&host, ide_core::RequestId::next())
                .await
                .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.request_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    match result {
                        Ok(registry) => {
                            panel.registry = registry.agents;
                            panel.registry_notice = registry.warning.map(Into::into);
                        }
                        Err(err) => panel.registry_notice = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
    }

    pub fn close_registry(&mut self, cx: &mut Context<Self>) {
        self.registry_open = false;
        cx.notify();
    }

    /// Whether the registry list is showing (for tests and the header).
    pub fn registry_is_open(&self) -> bool {
        self.registry_open
    }

    pub fn registry_agents(&self) -> &[ide_agent::registry::RegistryAgent] {
        &self.registry
    }

    /// Installs (or updates) one registry agent, streaming its progress.
    pub fn install_agent(&mut self, id: String, cx: &mut Context<Self>) {
        self.installing = Some(id.clone());
        self.registry_notice = Some(format!("Installing {id}…").into());
        cx.notify();
        let host = self.host.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        let progress = tx.clone();
        self.host.runtime.spawn(async move {
            let channel = ide_host::ops::Channel::new(move |event: ide_tools::InstallEvent| {
                if let ide_tools::InstallEvent::Line { text } = event {
                    let _ = progress.unbounded_send(text);
                }
            });
            let install_id = id.clone();
            let result =
                ide_host::ops::agent::agent_registry_install(&host, install_id, ide_core::RequestId::next(), channel)
            .await
            .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(match result {
                Ok(()) => format!("\u{2713} {id} installed"),
                Err(err) => format!("Install failed: {err}"),
            });
        });
        self.request_task = Some(cx.spawn(async move |this, cx| {
            while let Some(line) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    panel.installing = None;
                    panel.registry_notice = Some(line.into());
                    panel.refresh(cx);
                    cx.notify();
                });
            }
        }));
    }

    /// The provider's offered settings (models, modes…) for the selected task.
    pub fn config_options(&self) -> Vec<ide_agent::AgentConfigOption> {
        self.selected_task
            .as_ref()
            .and_then(|id| self.tasks.iter().find(|task| &task.id == id))
            .and_then(|task| self.providers.iter().find(|provider| provider.id == task.provider))
            .map(|provider| provider.config.clone())
            .unwrap_or_default()
    }

    /// Changes one of the task's settings (`model`, `mode`, …).
    pub fn set_config(&mut self, option: String, value: String, cx: &mut Context<Self>) {
        let Some(task) = self.selected_task.clone() else { return };
        let host = self.host.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result =
                ide_host::ops::agent::agent_set_config(&host, "main", task, option, value).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.request_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    match result {
                        Ok(()) => panel.refresh(cx),
                        Err(err) => panel.notice = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn entry_element(entry: &AgentEntry, theme: &gpui_kit::component::Theme) -> AnyElement {
        let mono = theme.mono_font_family.clone();
        let (label, text, color) = match entry {
            AgentEntry::User { text, .. } => ("YOU", text.clone(), theme.accent),
            AgentEntry::Agent { text } => ("AGENT", text.clone(), theme.foreground),
            AgentEntry::Thought { text, .. } => ("THOUGHT", text.clone(), theme.fg_muted()),
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
            AgentEntry::Question { title, questions, chosen, .. } => (
                "QUESTION",
                format!(
                    "{title} · {} · {}",
                    questions.iter().map(|q| q.question.clone()).collect::<Vec<_>>().join(" / "),
                    chosen.clone().unwrap_or_else(|| "pending".into())
                ),
                theme.warning,
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
        let entry_theme = theme.clone();
        let warning = theme.warning;
        let danger = theme.danger;
        let success = theme.success;
        let border = theme.border;
        let list_hover = theme.list_hover;
        let fg_secondary = theme.fg_secondary();
        for entry in &self.entries {
            conversation = conversation.child(Self::entry_element(entry, &entry_theme));
            // A pending permission renders its options as buttons: the agent
            // blocks its turn until one is answered.
            if let AgentEntry::Permission { id, title, options, chosen, .. } = entry
                && chosen.is_none()
            {
                let mut row = div().w_full().flex().items_center().gap_2().p_2().rounded(px(4.));
                row = row.child(
                    div()
                        .text_size(px(10.))
                        .text_color(warning)
                        .child(format!("Permission needed: {title}")),
                );
                for option in options {
                    let option_id = option.id.clone();
                    let permission = *id;
                    let destructive = permission_is_destructive(&option.kind);
                    let color = if destructive { danger } else { success };
                    row = row.child(
                        div()
                            .id(ElementId::Name(format!("agent-permission-{id}-{}", option.id).into()))
                            .h(px(20.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .bg(color.opacity(0.18))
                            .hover(move |this| this.bg(color.opacity(0.32)))
                            .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                                this.respond(permission, Some(option_id.clone()), cx)
                            }))
                            .text_size(px(10.))
                            .text_color(color)
                            .child(option.name.clone()),
                    );
                }
                let cancel_permission = *id;
                row = row.child(
                    div()
                        .id(ElementId::Name(format!("agent-permission-cancel-{id}").into()))
                        .h(px(20.))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .cursor_pointer()
                        .border_1()
                        .border_color(border)
                        .hover(move |this| this.bg(list_hover))
                        .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                            this.respond(cancel_permission, None, cx)
                        }))
                        .text_size(px(10.))
                        .text_color(fg_secondary)
                        .child("Cancel"),
                );
                conversation = conversation.child(row);
            }
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
                    })
                    .child(
                        div()
                            .id("agent-isolation")
                            .h(px(18.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |this| this.bg(hover))
                            .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                                let next = match this.isolation {
                                    Isolation::Local => Isolation::Worktree,
                                    Isolation::Worktree => Isolation::Local,
                                };
                                this.set_isolation(next, cx);
                            }))
                            .text_size(px(9.5))
                            .text_color(theme.fg_secondary())
                            .child(isolation_label(self.isolation)),
                    )
                    .child(
                        div()
                            .id("agent-registry")
                            .h(px(18.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |this| this.bg(hover))
                            .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                                this.open_registry(cx)
                            }))
                            .text_size(px(9.5))
                            .text_color(theme.fg_secondary())
                            .child("Registry"),
                    )
                    .when_some(self.config_options().first().cloned(), |this, option| {
                        // One compact control per advertised setting: clicking
                        // cycles to the next value (models, modes, thought levels).
                        let theme = theme.clone();
                        let config_hover = theme.list_hover;
                        let choices = option.values.clone();
                        let current = option
                            .current
                            .clone()
                            .or_else(|| choices.first().map(|value| value.value.clone()))
                            .unwrap_or_default();
                        let label = choices
                            .iter()
                            .find(|value| value.value == current)
                            .map(|value| value.name.clone())
                            .unwrap_or_else(|| option.name.clone());
                        this.child(
                            div()
                                .id("agent-config")
                                .h(px(18.))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded(px(3.))
                                .cursor_pointer()
                                .border_1()
                                .border_color(theme.border)
                                .hover(move |this| this.bg(config_hover))
                                .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                                    let choices = choices.clone();
                                    let current = current.clone();
                                    let option_id = option.id.clone();
                                    let next = choices
                                        .iter()
                                        .position(|value| value.value == current)
                                        .map(|index| (index + 1) % choices.len().max(1))
                                        .and_then(|index| choices.get(index))
                                        .map(|value| value.value.clone());
                                    if let Some(value) = next {
                                        this.set_config(option_id, value, cx);
                                    }
                                }))
                                .text_size(px(9.5))
                                .text_color(theme.fg_secondary())
                                .child(format!("{}: {label}", option.name)),
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
            .when(self.registry_open, |this| {
                let theme = theme.clone();
                let hover = theme.list_hover;
                let accent = theme.accent;
                let foreground = theme.foreground;
                let muted = theme.fg_muted();
                let fg_muted = muted;
                let agents = std::rc::Rc::new(
                    self.registry
                        .iter()
                        .map(|agent| {
                            (
                                agent.id.clone(),
                                agent.name.clone(),
                                agent.version.clone(),
                                agent.installed.clone(),
                                agent.update,
                                agent.distribution.is_some(),
                                agent.description.clone(),
                            )
                        })
                        .collect::<Vec<_>>(),
                );
                this.child(
                    div()
                        .id("agent-registry-panel")
                        .absolute()
                        .top(px(30.))
                        .left(px(0.))
                        .right(px(0.))
                        .bottom(px(0.))
                        .bg(theme.background)
                        .border_t_1()
                        .border_color(theme.border)
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(24.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .bg(theme.muted)
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.fg_secondary())
                                        .child("ACP Registry"),
                                )
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .id("agent-registry-close")
                                        .h(px(18.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded(px(3.))
                                        .cursor_pointer()
                                        .hover(move |this| this.bg(hover))
                                        .on_click(cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                                            this.close_registry(cx)
                                        }))
                                        .text_size(px(9.5))
                                        .text_color(theme.fg_secondary())
                                        .child("Close"),
                                ),
                        )
                        .when_some(self.registry_notice.clone(), |this, notice| {
                            this.child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .text_size(px(10.))
                                    .text_color(theme.warning)
                                    .child(notice),
                            )
                        })
                        .child(
                            div()
                                .id("agent-registry-list")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p_1()
                                .flex()
                                .flex_col()
                                .children(agents.iter().enumerate().map(|(index, agent)| {
                                    let (id, name, version, installed, update, installable, description) = agent.clone();
                                    let hover = hover;
                                    let detail = match (&installed, update) {
                                        (Some(current), true) => format!("{current} → {version} (update)"),
                                        (Some(current), false) => format!("installed {current}"),
                                        (None, _) => version.clone(),
                                    };
                                    let install_id = id.clone();
                                    div()
                                        .id(ElementId::Name(format!("registry-agent-{index}").into()))
                                        .h(px(28.))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .rounded(px(3.))
                                        .hover(move |this| this.bg(hover))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_size(px(11.))
                                                        .text_color(foreground)
                                                        .child(name),
                                                )
                                                .child(
                                                    div()
                                                        .truncate()
                                                        .text_size(px(9.5))
                                                        .text_color(fg_muted)
                                                        .child(description),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(9.5))
                                                .text_color(fg_muted)
                                                .child(detail),
                                        )
                                        .when(installable, |this| {
                                            this.child(
                                                div()
                                                    .id(ElementId::Name(format!("registry-install-{index}").into()))
                                                    .h(px(18.))
                                                    .px_2()
                                                    .flex()
                                                    .items_center()
                                                    .rounded(px(3.))
                                                    .cursor_pointer()
                                                    .bg(accent.opacity(0.2))
                                                    .hover(move |this| this.bg(accent.opacity(0.32)))
                                                    .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                                                        this.install_agent(install_id.clone(), cx)
                                                    }))
                                                    .text_size(px(9.5))
                                                    .text_color(foreground)
                                                    .child(if update { "Update" } else { "Install" }),
                                            )
                                        })
                                })),
                        ),
                )
            })
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
