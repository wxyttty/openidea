//! HTTP Client tool window: compose a request, send it through `ide-http`,
//! and inspect the status line, headers and body.
//!
//! Requests are assembled into the same `.http` text format the Tauri frontend
//! edits (`METHOD URL`, `Header: value`, blank line, body) and executed with
//! `ide_http::run`, so variables, proxies and history behave the same.

use crate::theme::IdeColors as _;
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputContent, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_host::{Host, Project};
use ide_http::{HttpResponse, RunInput};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_BODY_LINES: usize = 1200;

pub struct HttpPanel {
    host: Arc<Host>,
    project: Option<Arc<Project>>,
    method: Entity<InputState>,
    url: Entity<InputState>,
    file_input: Entity<InputState>,
    file: Option<PathBuf>,
    file_text: String,
    blocks: Vec<ide_http::parse::HttpRequestBlock>,
    block: usize,
    envs: Vec<String>,
    env: Option<String>,
    history: Vec<ide_http::HistoryEntry>,
    headers: Entity<InputState>,
    body: Entity<InputState>,
    response: Option<HttpResponse>,
    error: Option<SharedString>,
    running: bool,
    task: Option<Task<()>>,
    _subs: Vec<Subscription>,
    focus_handle: FocusHandle,
}

impl HttpPanel {
    pub fn new(host: Arc<Host>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let method = cx.new(|cx| InputState::new(window, cx).default_value("GET"));
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com/api"));
        let file_input = cx.new(|cx| InputState::new(window, cx).placeholder("requests.http"));
        let headers = cx.new(|cx| InputState::new(window, cx).placeholder("Header: value; Accept: application/json"));
        let body = cx.new(|cx| InputState::new(window, cx).placeholder("Request body"));
        let mut subs = Vec::new();
        for input in [&method, &url, &headers, &body] {
            subs.push(cx.subscribe(input, |this: &mut Self, _input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.send(cx);
                }
            }));
        }
        subs.push(cx.subscribe(&file_input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.load_file(cx);
            }
        }));
        Self {
            host,
            project: None,
            method,
            url,
            file_input,
            file: None,
            file_text: String::new(),
            blocks: Vec::new(),
            block: 0,
            envs: Vec::new(),
            env: None,
            history: Vec::new(),
            headers,
            body,
            response: None,
            error: None,
            running: false,
            task: None,
            _subs: subs,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        self.project = Some(project.clone());
        self.response = None;
        self.error = None;
        self.running = false;
        self.task = None;
        self.file = None;
        self.file_text.clear();
        self.blocks.clear();
        self.block = 0;
        self.envs.clear();
        self.env = None;
        self.history = project.http_history.list();
        cx.notify();
    }

    /// Use `path` as the request file (for `.http` files opened in the editor).
    pub fn use_file(
        &mut self,
        path: PathBuf,
        text: String,
        root: &Path,
        block: usize,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let parsed = ide_http::parse::parse(&text);
        self.file_text = text;
        self.blocks = parsed.requests.iter().map(|request| request.block.clone()).collect();
        self.block = block.min(self.blocks.len().saturating_sub(1));
        self.file = Some(path.clone());
        self.envs = ide_http::environments(Some(&path), Some(root)).unwrap_or_default();
        self.env = if self.envs.len() == 1 { Some(self.envs[0].clone()) } else { None };
        self.error = None;
        if let Some(window) = window {
            let display = path.display().to_string();
            self.file_input.update(cx, |state, cx| state.set_value(InputContent::new(display), window, cx));
        }
        cx.notify();
    }

    pub fn run_current(&mut self, cx: &mut Context<Self>) {
        self.send(cx);
    }

    fn load_file(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            self.error = Some("Open a project first".into());
            cx.notify();
            return;
        };
        let raw = self.file_input.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            self.error = Some("Enter a .http file path".into());
            cx.notify();
            return;
        }
        let path = if Path::new(&raw).is_absolute() { PathBuf::from(&raw) } else { project.root.join(&raw) };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let parsed = ide_http::parse::parse(&text);
                self.file_text = text;
                self.blocks = parsed.requests.iter().map(|request| request.block.clone()).collect();
                self.block = 0;
                self.file = Some(path.clone());
                self.envs = ide_http::environments(Some(&path), Some(&project.root)).unwrap_or_default();
                self.env = None;
                self.error = None;
                self.response = None;
            }
            Err(err) => {
                self.error = Some(format!("Cannot read {}: {err}", path.display()).into());
                self.file = None;
                self.blocks.clear();
            }
        }
        cx.notify();
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let (text, index, file, env) = if self.file.is_some() && !self.blocks.is_empty() {
            (self.file_text.clone(), self.block as u32, self.file.clone(), self.env.clone())
        } else {
            let method = {
                let value = self.method.read(cx).value().trim().to_uppercase();
                if value.is_empty() { "GET".to_owned() } else { value }
            };
            let url = self.url.read(cx).value().trim().to_owned();
            if url.is_empty() {
                self.error = Some("Enter a URL first, or load a .http file".into());
                cx.notify();
                return;
            }
            let headers = self
                .headers
                .read(cx)
                .value()
                .split(';')
                .map(str::trim)
                .filter(|header| !header.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let body = self.body.read(cx).value().to_string();
            let text = format!("{method} {url}\n{headers}\n\n{body}").trim_end().to_owned();
            (text, 0, None, None)
        };
        let root = self.project.as_ref().map(|project| project.root.clone()).unwrap_or_default();
        self.running = true;
        self.error = None;
        self.response = None;
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let cancel = ide_core::CancellationToken::new();
            let run = RunInput { text: &text, index, file: file.as_deref(), root: Some(&root), env: env.as_deref() };
            let result = ide_http::run(run, &cancel).await.map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.running = false;
                    match result {
                        Ok(exchange) => {
                            if let Some(error) = exchange.entry.error.clone() {
                                this.error = Some(error.into());
                            } else {
                                this.response = exchange.response.clone();
                            }
                            if let Some(project) = this.project.clone() {
                                let history = project.http_history.clone();
                                let saved = exchange.clone();
                                std::thread::spawn(move || {
                                    let _ = history.add(&saved);
                                });
                                this.history = project.http_history.list();
                            }
                        }
                        Err(err) => this.error = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }
}

impl Focusable for HttpPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HttpPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let send = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.send(cx));
        let load = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.load_file(cx));
        let running = self.running;
        let error = self.error.clone();
        let response = self.response.clone();
        let mono = theme.mono_font_family.clone();
        let blocks = self.blocks.clone();
        let envs = self.envs.clone();
        let history = self.history.clone();
        let current_block = self.block;
        let current_env = self.env.clone();
        let hover_bg = theme.list_hover;

        let mut response_area = div().flex_1().min_h_0().flex().flex_col();
        if let Some(error) = error {
            response_area =
                response_area.child(div().px_3().py_2().text_size(px(11.5)).text_color(theme.danger).child(error));
        } else if let Some(response) = response {
            let (status_color, status_text) = match response.status {
                200..=299 => (theme.success, format!("{} {}", response.status, response.status_text)),
                300..=399 => (theme.info, format!("{} {}", response.status, response.status_text)),
                400..=499 => (theme.warning, format!("{} {}", response.status, response.status_text)),
                _ => (theme.danger, format!("{} {}", response.status, response.status_text)),
            };
            let size = if response.size >= 1024. {
                format!("{:.1} KB", response.size / 1024.)
            } else {
                format!("{:.0} B", response.size)
            };
            response_area = response_area.child(
                div()
                    .h(px(26.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(11.5))
                    .child(div().text_color(status_color).font_weight(FontWeight::SEMIBOLD).child(status_text))
                    .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(response.version.clone()))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.fg_muted())
                            .child(format!("{:.0} ms", response.total_ms)),
                    )
                    .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(size))
                    .when(response.truncated, |this| {
                        this.child(div().text_size(px(10.)).text_color(theme.warning).child("truncated"))
                    }),
            );
            if !response.headers.is_empty() {
                let mut headers = div().px_3().py_1().flex().flex_col().border_b_1().border_color(theme.border);
                for (name, value) in response.headers.iter().take(8) {
                    headers = headers.child(
                        div()
                            .h(px(15.))
                            .w_full()
                            .whitespace_nowrap()
                            .truncate()
                            .font_family(mono.clone())
                            .text_size(px(10.))
                            .text_color(theme.fg_muted())
                            .child(format!("{name}: {value}")),
                    );
                }
                response_area = response_area.child(headers);
            }
            let lines: Vec<SharedString> = match &response.body {
                Some(body) => body.lines().take(MAX_BODY_LINES).map(|line| line.to_owned().into()).collect(),
                None => vec!["(binary body)".into()],
            };
            response_area = response_area.child(
                div()
                    .id("http-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .font_family(mono.clone())
                    .text_size(px(11.))
                    .text_color(theme.fg())
                    .children(
                        lines.into_iter().map(|line| div().min_h(px(14.)).w_full().whitespace_nowrap().child(line)),
                    ),
            );
        } else {
            response_area = response_area.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.5))
                    .text_color(theme.fg_muted())
                    .child(if running { "Sending…" } else { "Compose a request and press Enter" }),
            );
        }

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
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::new(IconName::Cloud).xsmall().text_color(theme.info))
                    .child(div().w(px(84.)).child(Input::new(&self.method)))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.url)))
                    .child(
                        div()
                            .id("http-send")
                            .h(px(26.))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .bg(theme.primary)
                            .when(running, |this| this.opacity(0.5))
                            .hover(move |this| this.opacity(0.9))
                            .on_click(send)
                            .child(Icon::new(IconName::ArrowUpRight).xsmall().text_color(theme.primary_foreground))
                            .child(div().text_size(px(11.)).text_color(theme.primary_foreground).child("Send")),
                    ),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().w(px(60.)).text_size(px(10.)).text_color(theme.fg_muted()).child("File"))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.file_input)))
                    .child(
                        div()
                            .id("http-load")
                            .h(px(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |this| this.bg(hover_bg))
                            .on_click(load)
                            .text_size(px(10.5))
                            .text_color(theme.fg_secondary())
                            .child("Load"),
                    ),
            )
            .when(!blocks.is_empty(), |this| {
                let mut row = div()
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().pr_1().text_size(px(9.5)).text_color(theme.fg_muted()).child("REQUESTS"));
                for (index, request) in blocks.iter().enumerate() {
                    let active = index == current_block;
                    let label = request.name.clone().unwrap_or_else(|| format!("{} {}", request.method, request.url));
                    let pick = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                        this.block = index.min(this.blocks.len().saturating_sub(1));
                        cx.notify();
                    });
                    row = row.child(
                        div()
                            .id(ElementId::Name(format!("http-block-{index}").into()))
                            .h(px(18.))
                            .px_1p5()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .text_size(px(10.))
                            .whitespace_nowrap()
                            .when(active, |this| this.bg(theme.list_active).text_color(theme.fg_strong()))
                            .when(!active, |this| {
                                this.text_color(theme.fg_muted()).hover(move |this| this.bg(hover_bg))
                            })
                            .on_click(pick)
                            .child(label),
                    );
                }
                row = row
                    .child(div().flex_1())
                    .child(div().pr_1().text_size(px(9.5)).text_color(theme.fg_muted()).child("ENV"));
                let none_active = current_env.is_none();
                let clear = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
                    this.env = None;
                    cx.notify();
                });
                row = row.child(
                    div()
                        .id("http-env-none")
                        .h(px(18.))
                        .px_1p5()
                        .flex()
                        .items_center()
                        .rounded(px(3.))
                        .cursor_pointer()
                        .text_size(px(10.))
                        .when(none_active, |this| this.bg(theme.list_active).text_color(theme.fg_strong()))
                        .when(!none_active, |this| {
                            this.text_color(theme.fg_muted()).hover(move |this| this.bg(hover_bg))
                        })
                        .on_click(clear)
                        .child("none"),
                );
                for env in envs {
                    let active = current_env.as_deref() == Some(env.as_str());
                    let name = env.clone();
                    let pick = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                        this.env = Some(name.clone());
                        cx.notify();
                    });
                    row = row.child(
                        div()
                            .id(ElementId::Name(format!("http-env-{env}").into()))
                            .h(px(18.))
                            .px_1p5()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .text_size(px(10.))
                            .when(active, |this| this.bg(theme.list_active).text_color(theme.fg_strong()))
                            .when(!active, |this| {
                                this.text_color(theme.fg_muted()).hover(move |this| this.bg(hover_bg))
                            })
                            .on_click(pick)
                            .child(env),
                    );
                }
                this.child(row)
            })
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().w(px(60.)).text_size(px(10.)).text_color(theme.fg_muted()).child("Headers"))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.headers))),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().w(px(60.)).text_size(px(10.)).text_color(theme.fg_muted()).child("Body"))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.body))),
            )
            .when(!history.is_empty(), |this| {
                let mut row = div()
                    .h(px(24.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_size(px(9.5)).text_color(theme.fg_muted()).child("HISTORY"));
                for entry in history.iter().take(5) {
                    let method = entry.method.clone();
                    let url = entry.url.clone();
                    let click_method = method.clone();
                    let click_url = url.clone();
                    let status = entry.status.map(|status| status.to_string()).unwrap_or_else(|| "ERR".into());
                    let color = match entry.status {
                        Some(200..=299) => theme.success,
                        Some(300..=399) => theme.info,
                        Some(400..=499) => theme.warning,
                        Some(_) => theme.danger,
                        None => theme.danger,
                    };
                    let pick = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                        this.file = None;
                        this.blocks.clear();
                        this.block = 0;
                        this.method.update(cx, |state, cx| {
                            state.set_value(InputContent::new(click_method.clone()), window, cx)
                        });
                        this.url
                            .update(cx, |state, cx| state.set_value(InputContent::new(click_url.clone()), window, cx));
                        cx.notify();
                    });
                    row = row.child(
                        div()
                            .id(ElementId::Name(format!("http-history-{}", entry.id).into()))
                            .h(px(18.))
                            .px_1p5()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .whitespace_nowrap()
                            .hover(move |this| this.bg(hover_bg))
                            .on_click(pick)
                            .child(div().text_size(px(9.5)).text_color(color).child(status))
                            .child(div().text_size(px(9.5)).text_color(theme.fg_muted()).child(method))
                            .child(
                                div()
                                    .max_w(px(180.))
                                    .truncate()
                                    .text_size(px(9.5))
                                    .text_color(theme.fg_secondary())
                                    .child(url),
                            ),
                    );
                }
                this.child(row)
            })
            .child(response_area)
            .into_any_element()
    }
}
