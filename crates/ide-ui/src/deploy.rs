//! SFTP / Deployment tool window: upload a file or the project to the servers in
//! Settings › Build, Execution, Deployment › Deployment, and compare the local
//! tree with what is deployed.
//!
//! `ide-remote` owns the work: `RemoteService` maps local paths to remote ones
//! per server, honours the exclusion patterns, uploads through `ide-ssh`'s
//! `RemoteFs`, and reports progress as a cancellable background task (the status
//! bar shows it). This panel is the desktop face of those calls:
//!
//! * *Upload* sends the active file to the default (or chosen) server;
//! * *Upload Project* walks the whole project;
//! * *Compare* lists each file's [`SyncStatus`] (local-only, newer one side,
//!   different) so the user can see what a sync would do;
//! * the notice line reports files transferred, skipped and failed.
//!
//! Without a deployment server configured the panel says so and offers nothing
//! to click, rather than failing on every action.

use crate::editor::EditorArea;
use futures::StreamExt;
use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_host::Host;
use ide_remote::{SyncEntry, SyncStatus, TransferReport};
use std::path::PathBuf;
use std::sync::Arc;

pub struct DeployPanel {
    host: Arc<Host>,
    editor: Entity<EditorArea>,
    /// The deployment servers configured for the project.
    servers: Vec<String>,
    server: Option<String>,
    /// The last transfer's outcome.
    notice: Option<SharedString>,
    /// `Compare` results: what each file's local/remote state is.
    entries: Vec<SyncEntry>,
    comparing: bool,
    uploading: bool,
    task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

impl DeployPanel {
    pub fn new(host: Arc<Host>, editor: Entity<EditorArea>, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            editor,
            servers: Vec::new(),
            server: None,
            notice: None,
            entries: Vec::new(),
            comparing: false,
            uploading: false,
            task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Re-reads the deployment servers from the project's settings.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let settings = match &self.host.project("main") {
            Ok(project) => project.settings.effective(),
            Err(_) => self.host.global_settings.effective(),
        };
        self.servers = settings.deployment.servers.iter().map(|server| server.id.clone()).collect();
        if self.server.as_ref().is_none_or(|server| !self.servers.contains(server)) {
            self.server = self.servers.first().cloned();
        }
        if self.servers.is_empty() {
            self.notice = Some("No deployment servers (Settings › Deployment)".into());
        } else {
            self.notice = None;
        }
        cx.notify();
    }

    pub fn servers(&self) -> &[String] {
        &self.servers
    }

    pub fn selected_server(&self) -> Option<&String> {
        self.server.as_ref()
    }

    pub fn select_server(&mut self, server: String, cx: &mut Context<Self>) {
        self.server = Some(server);
        cx.notify();
    }

    pub fn entries(&self) -> &[SyncEntry] {
        &self.entries
    }

    pub fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    /// Uploads the active editor's file to the selected server.
    pub fn upload_active(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.editor.read(cx).active_info().map(|info| info.path) else {
            self.notice = Some("Open a file to upload".into());
            cx.notify();
            return;
        };
        self.upload(vec![path], cx);
    }

    /// Uploads the whole project.
    pub fn upload_project(&mut self, cx: &mut Context<Self>) {
        let Ok(project) = self.host.project("main") else {
            self.notice = Some("Open a project first".into());
            cx.notify();
            return;
        };
        let root = project.root.clone();
        self.upload(vec![root], cx);
    }

    fn upload(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.servers.is_empty() {
            self.notice = Some("No deployment servers configured".into());
            cx.notify();
            return;
        }
        if self.uploading {
            return;
        }
        self.uploading = true;
        self.notice = Some("Uploading…".into());
        let host = self.host.clone();
        let server = self.server.clone();
        let paths: Vec<ide_core::IpcPath> = paths.into_iter().map(ide_core::IpcPath).collect();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result =
                ide_host::ops::deploy::deploy_upload("main", &host, paths, server, ide_core::RequestId::next())
                    .await
                    .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    panel.uploading = false;
                    panel.notice = Some(match result {
                        Ok(report) => report_line("Uploaded", &report),
                        Err(err) => format!("Upload failed: {err}").into(),
                    });
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    /// Compares the project tree with what is deployed (the selected server).
    pub fn compare(&mut self, cx: &mut Context<Self>) {
        let Ok(project) = self.host.project("main") else {
            self.notice = Some("Open a project first".into());
            cx.notify();
            return;
        };
        if self.servers.is_empty() {
            self.notice = Some("No deployment servers configured".into());
            cx.notify();
            return;
        }
        self.comparing = true;
        self.notice = Some("Comparing…".into());
        let host = self.host.clone();
        let server = self.server.clone();
        let root = project.root.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::deploy::deploy_compare(
                "main",
                &host,
                ide_core::IpcPath(root),
                server,
                ide_core::RequestId::next(),
            )
            .await
            .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    panel.comparing = false;
                    match result {
                        Ok(entries) => {
                            let changed = entries
                                .iter()
                                .filter(|entry| !matches!(entry.status, SyncStatus::Same))
                                .count();
                            panel.notice =
                                Some(format!("{} file(s) differ from the deployed copy", changed).into());
                            panel.entries = entries;
                        }
                        Err(err) => panel.notice = Some(format!("Compare failed: {err}").into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    /// Downloads the deployed copy of the selected file.
    pub fn download_active(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.editor.read(cx).active_info().map(|info| info.path) else {
            self.notice = Some("Open a file to download its deployed copy".into());
            cx.notify();
            return;
        };
        if self.servers.is_empty() {
            self.notice = Some("No deployment servers configured".into());
            cx.notify();
            return;
        }
        self.notice = Some("Downloading…".into());
        let host = self.host.clone();
        let server = self.server.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::deploy::deploy_download(
                "main",
                &host,
                vec![ide_core::IpcPath(path)],
                server,
                ide_core::RequestId::next(),
            )
            .await
            .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    panel.notice = Some(match result {
                        Ok(report) => report_line("Downloaded", &report),
                        Err(err) => format!("Download failed: {err}").into(),
                    });
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }
}

fn report_line(verb: &str, report: &TransferReport) -> SharedString {
    let errors = report.errors.len();
    format!(
        "{verb} {} file(s) · {} skipped{}",
        report.files,
        report.skipped,
        if errors > 0 { format!(" · {errors} failed") } else { String::new() }
    )
    .into()
}

/// The label a sync status shows, with its colour role.
pub fn status_label(status: SyncStatus) -> (&'static str, bool) {
    match status {
        SyncStatus::LocalOnly => ("local only", true),
        SyncStatus::RemoteOnly => ("remote only", true),
        SyncStatus::LocalNewer => ("local newer", true),
        SyncStatus::RemoteNewer => ("remote newer", true),
        SyncStatus::Different => ("different", true),
        SyncStatus::Same => ("same", false),
    }
}

impl Focusable for DeployPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DeployPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let hover = theme.list_hover;
        let server = self.server.clone().unwrap_or_else(|| "no server".to_owned());
        let entries: std::rc::Rc<Vec<(String, &'static str, bool)>> = std::rc::Rc::new(
            self.entries
                .iter()
                .map(|entry| {
                    let (label, changed) = status_label(entry.status);
                    (entry.path.clone(), label, changed)
                })
                .collect(),
        );
        let row_theme = theme.clone();
        let refresh = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.reload(cx));
        let upload = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.upload_active(cx));
        let upload_all = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.upload_project(cx));
        let download = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.download_active(cx));
        let compare = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.compare(cx));
        let servers = self.servers.clone();
        let selected = self.server.clone();

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
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(Icon::new(IconName::Cloud).xsmall().text_color(theme.info))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child("Deployment"),
                    )
                    .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(server))
                    .child(div().flex_1())
                    .children(servers.iter().map(|name| {
                        let id = name.clone();
                        let active = selected.as_deref() == Some(name.as_str());
                        let hover = hover;
                        div()
                            .id(ElementId::Name(format!("deploy-server-{name}").into()))
                            .h(px(18.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .when(active, |this| this.bg(theme.accent.opacity(0.22)))
                            .hover(move |this| this.bg(hover))
                            .on_click(cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                                this.select_server(id.clone(), cx)
                            }))
                            .text_size(px(9.))
                            .text_color(theme.fg_secondary())
                            .child(name.clone())
                    }))
                    .child(
                        div()
                            .id("deploy-refresh")
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
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(button("deploy-upload", "Upload File", theme.list_hover, upload))
                    .child(button("deploy-upload-all", "Upload Project", theme.list_hover, upload_all))
                    .child(button("deploy-download", "Download File", theme.list_hover, download))
                    .child(button("deploy-compare", "Compare", theme.list_hover, compare))
                    .when(self.uploading || self.comparing, |this| {
                        this.child(div().text_size(px(9.5)).text_color(theme.warning).child("working…"))
                    }),
            )
            .when_some(self.notice.clone(), |this, notice| {
                let theme = theme.clone();
                this.child(
                    div().px_2().py_1().text_size(px(10.5)).text_color(theme.fg_secondary()).child(notice),
                )
            })
            .child(
                div()
                    .id("deploy-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .flex()
                    .flex_col()
                    .when(entries.is_empty(), |this| {
                        this.child(
                            div()
                                .p_3()
                                .text_size(px(11.))
                                .text_color(row_theme.fg_muted())
                                .child("Compare lists what differs from the deployed copy"),
                        )
                    })
                    .children(entries.iter().enumerate().map(|(index, (path, label, changed))| {
                        let hover = row_theme.list_hover;
                        div()
                            .id(ElementId::Name(format!("deploy-entry-{index}").into()))
                            .h(px(20.))
                            .px_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(px(3.))
                            .hover(move |this| this.bg(hover))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(if *changed { row_theme.foreground } else { row_theme.fg_muted() })
                                    .child(path.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(9.5))
                                    .text_color(if *changed { row_theme.warning } else { row_theme.fg_muted() })
                                    .child(*label),
                            )
                    })),
            )
    }
}

fn button(
    id: &'static str,
    label: &'static str,
    hover: Hsla,
    click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(20.))
        .px_2()
        .flex()
        .items_center()
        .rounded(px(3.))
        .cursor_pointer()
        .border_1()
        .border_color(hover)
        .hover(move |this| this.bg(hover))
        .on_click(click)
        .text_size(px(9.5))
        .child(label)
        .into_any_element()
}

/// The panel's icon.
pub const ICON: IconName = IconName::Cloud;

#[cfg(test)]
mod tests {
    use super::status_label;
    use ide_remote::SyncStatus;

    #[test]
    fn every_sync_status_has_a_label_and_only_same_is_quiet() {
        for (status, label) in [
            (SyncStatus::LocalOnly, "local only"),
            (SyncStatus::RemoteOnly, "remote only"),
            (SyncStatus::LocalNewer, "local newer"),
            (SyncStatus::RemoteNewer, "remote newer"),
            (SyncStatus::Different, "different"),
            (SyncStatus::Same, "same"),
        ] {
            let (text, changed) = status_label(status);
            assert_eq!(text, label);
            assert_eq!(changed, status != SyncStatus::Same, "{label} marks itself changed");
        }
    }
}
