//! SSH tool window: the configurations of Settings › Tools › SSH and their
//! sessions.
//!
//! `ide-ssh` owns the connections (`SshManager::session` / `connect` /
//! `disconnect`), the saved secrets and the `~/.ssh/config` bridge. This panel
//! lists the configurations with their `user@host:port` label, marks the ones
//! already connected, and lets the user connect, test or disconnect — the
//! version of IDEA's SSH console that the native shell needs for remote work.
//!
//! Connecting runs on the host runtime: an SSH handshake must not block a frame,
//! and a host whose key is unknown surfaces the error instead of hanging.

use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::remote_settings::SshConfiguration;
use ide_host::Host;
use std::collections::BTreeSet;
use std::sync::Arc;

pub struct SshPanel {
    host: Arc<Host>,
    configs: Vec<SshConfiguration>,
    /// Ids with a live session.
    connected: BTreeSet<String>,
    /// Id of the configuration whose action is running.
    busy: Option<String>,
    notice: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl SshPanel {
    pub fn new(host: Arc<Host>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self {
            host,
            configs: Vec::new(),
            connected: BTreeSet::new(),
            busy: None,
            notice: None,
            focus_handle: cx.focus_handle(),
        };
        panel.reload(cx);
        panel
    }

    /// Re-reads the configurations from the manager (which the settings page
    /// keeps in step with `settings.toml`).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.configs = self.host.ssh.configs();
        self.connected = self.host.ssh.connected().into_iter().collect();
        if self.configs.is_empty() {
            self.notice = Some("No SSH configurations (Settings › Tools › SSH)".into());
        } else {
            self.notice = None;
        }
        cx.notify();
    }

    pub fn configs(&self) -> &[SshConfiguration] {
        &self.configs
    }

    pub fn is_connected(&self, id: &str) -> bool {
        self.connected.contains(id)
    }

    /// Opens (or reuses) the session for one configuration.
    pub fn connect(&mut self, id: String, cx: &mut Context<Self>) {
        self.busy = Some(id.clone());
        self.notice = None;
        cx.notify();
        let manager = self.host.ssh.clone();
        let runtime = self.host.runtime.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime.spawn(async move { manager.session(&id).await }).await;
            this.update(cx, |panel, cx| {
                panel.busy = None;
                match result {
                    Ok(Ok(_session)) => {
                        panel.connected = panel.host.ssh.connected().into_iter().collect();
                        panel.notice = Some("Connected".into());
                    }
                    Ok(Err(err)) => panel.notice = Some(format!("Connection failed: {err}").into()),
                    Err(err) => panel.notice = Some(format!("Connection task failed: {err}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Runs the manager's connection test (host key, authentication, a shell).
    pub fn test(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(config) = self.configs.iter().find(|config| config.id == id).cloned() else { return };
        self.busy = Some(id);
        self.notice = None;
        cx.notify();
        let manager = self.host.ssh.clone();
        let runtime = self.host.runtime.clone();
        cx.spawn(async move |this, cx| {
            let secret = manager.secret(&config);
            let result = runtime.spawn(async move { manager.test(&config, secret).await }).await;
            this.update(cx, |panel, cx| {
                panel.busy = None;
                match result {
                    Ok(Ok(message)) => panel.notice = Some(format!("OK · {message}").into()),
                    Ok(Err(err)) => panel.notice = Some(format!("Test failed: {err}").into()),
                    Err(err) => panel.notice = Some(format!("Test task failed: {err}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Drops the session of one configuration.
    pub fn disconnect(&mut self, id: String, cx: &mut Context<Self>) {
        self.host.ssh.disconnect(&id);
        self.connected = self.host.ssh.connected().into_iter().collect();
        self.notice = Some("Disconnected".into());
        cx.notify();
    }
}

impl Focusable for SshPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SshPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let row_theme = theme.clone();
        let hover = theme.list_hover;
        let configs: std::rc::Rc<Vec<(String, String, bool, bool)>> = std::rc::Rc::new(
            self.configs
                .iter()
                .map(|config| {
                    (
                        config.id.clone(),
                        format!("{}  ({})", config.name, config.label()),
                        self.connected.contains(&config.id),
                        self.busy.as_deref() == Some(config.id.as_str()),
                    )
                })
                .collect(),
        );
        let refresh = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.reload(cx));

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
                    .child(Icon::new(IconName::Cloud).xsmall().text_color(theme.info))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child("SSH"),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(theme.fg_muted())
                            .child(format!("{} connection(s)", self.connected.len())),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("ssh-refresh")
                            .h(px(18.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(hover))
                            .on_click(refresh)
                            .child(Icon::new(IconName::RefreshCw).xsmall().text_color(theme.fg_secondary())),
                    ),
            )
            .when_some(self.notice.clone(), |this, notice| {
                let theme = theme.clone();
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(px(10.5))
                        .text_color(theme.fg_secondary())
                        .child(notice),
                )
            })
            .child(
                div()
                    .id("ssh-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .flex()
                    .flex_col()
                    .children(configs.iter().enumerate().map(|(index, (id, label, connected, busy))| {
                        let (id, label, connected, busy) = (id.clone(), label.clone(), *connected, *busy);
                        let hover = row_theme.list_hover;
                        let dot = if connected { row_theme.success } else { row_theme.fg_muted() };
                        let toggle_id = id.clone();
                        let test_id = id.clone();
                        // One button whose click means "open" or "close",
                        // depending on the session state.
                        let action_id = id.clone();
                        let action: std::rc::Rc<dyn Fn(&mut Self, &mut Window, &mut Context<Self>)> = if connected {
                            let id = action_id.clone();
                            std::rc::Rc::new(move |this: &mut Self, _window, cx| this.disconnect(id.clone(), cx))
                        } else {
                            let id = action_id.clone();
                            std::rc::Rc::new(move |this: &mut Self, _window, cx| this.connect(id.clone(), cx))
                        };
                        let (action_label, action_icon) =
                            if connected { ("Disconnect", IconName::Square) } else { ("Connect", IconName::Play) };
                        div()
                            .id(ElementId::Name(format!("ssh-row-{index}").into()))
                            .h(px(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(px(3.))
                            .hover(move |this| this.bg(hover))
                            .child(div().size(px(6.)).rounded_full().bg(dot))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.5))
                                    .text_color(row_theme.foreground)
                                    .child(label),
                            )
                            .when(busy, |this| {
                                this.child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(row_theme.warning)
                                        .child("working…"),
                                )
                            })
                            .child(
                                div()
                                    .id(ElementId::Name(format!("ssh-test-{index}").into()))
                                    .h(px(18.))
                                    .px_1()
                                    .flex()
                                    .items_center()
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .hover(move |this| this.bg(hover))
                                    .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                                        this.test(test_id.clone(), cx)
                                    }))
                                    .text_size(px(9.5))
                                    .text_color(row_theme.fg_secondary())
                                    .child("Test"),
                            )
                            .child(
                                div()
                                    .id(ElementId::Name(format!("ssh-action-{index}").into()))
                                    .h(px(18.))
                                    .px_1()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .rounded(px(3.))
                                    .cursor_pointer()
                                    .hover(move |this| this.bg(hover))
                                    .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                                        action(this, window, cx)
                                    }))
                                    .text_size(px(9.5))
                                    .text_color(row_theme.fg_secondary())
                                    .child(Icon::new(action_icon).xsmall())
                                    .child(action_label),
                            )
                    })),
            )
    }
}
