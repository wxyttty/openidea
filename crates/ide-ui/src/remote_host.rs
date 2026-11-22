//! Remote Host tool window: the deployment server's filesystem.
//!
//! IDEA's remote host view lists the server root and lets an editor open a file
//! from it. `ide-remote` provides the leaves:
//!
//! * `RemoteService::server_root` resolves the configured server's root;
//! * `RemoteService::list` gives one directory's entries (`ide-ssh`'s `RemoteFs`);
//! * `RemoteService::open_remote` copies a remote file into the local cache and
//!   returns the path an editor opens (saving uploads it back — that plumbing is
//!   in `ide-host`'s document layer, not here).
//!
//! The panel navigates the tree with a path bar: the root starts at the server
//! root, a folder row descends into it, `..` goes up, and a file row opens the
//! remote copy. Without a deployment server it says so instead of showing an
//! empty tree.

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use futures::StreamExt;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_host::Host;
use ide_ssh::{FsConnection, RemoteEntry};
use std::sync::Arc;

pub struct RemoteHostPanel {
    host: Arc<Host>,
    editor: Entity<EditorArea>,
    /// The deployment server the tree belongs to.
    server: Option<String>,
    /// The connection to that server's filesystem.
    connection: Option<FsConnection>,
    path: Entity<InputState>,
    entries: Vec<RemoteEntry>,
    /// Set while a listing or an open is in flight.
    busy: bool,
    /// A remote copy waiting for the next frame to open it (the task has no
    /// `Window`).
    pending_open: Option<std::path::PathBuf>,
    notice: Option<SharedString>,
    task: Option<Task<()>>,
    focus_handle: FocusHandle,
}

impl RemoteHostPanel {
    pub fn new(host: Arc<Host>, editor: Entity<EditorArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path = cx.new(|cx| InputState::new(window, cx).placeholder("Remote path"));
        Self {
            host,
            editor,
            server: None,
            connection: None,
            path,
            entries: Vec::new(),
            busy: false,
            pending_open: None,
            notice: None,
            task: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn entries(&self) -> &[RemoteEntry] {
        &self.entries
    }

    pub fn current_path(&self, cx: &App) -> String {
        self.path.read(cx).value().to_string()
    }

    pub fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    pub fn server(&self) -> Option<&String> {
        self.server.as_ref()
    }

    /// Connects the tree to a deployment server: resolves its root and lists it.
    pub fn select_server(&mut self, server: String, cx: &mut Context<Self>) {
        self.server = Some(server.clone());
        self.entries.clear();
        // Deployment servers whose filesystem is the local one still list
        // through the same code path; SFTP servers need their SSH id, which the
        // host's settings carry.
        let connection = match self.host.project("main") {
            Ok(project) => {
                let settings = project.settings.effective();
                ide_remote::server(&settings, Some(&server)).ok().map(|config| match config.kind {
                    ide_core::remote_settings::DeploymentType::Local => FsConnection::Local,
                    ide_core::remote_settings::DeploymentType::Sftp => FsConnection::Sftp { ssh: config.ssh_config.clone() },
                })
            }
            Err(_) => None,
        };
        self.connection = connection;
        let host = self.host.clone();
        let server_id = server.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.busy = true;
        self.notice = Some("Connecting…".into());
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::deploy::deploy_server_root("main", &host, server_id)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    panel.busy = false;
                    match result {
                        Ok(root) => {
                            panel.notice = None;
                            panel.open_path(root, cx);
                        }
                        Err(err) => panel.notice = Some(format!("Cannot reach the server root: {err}").into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    /// Lists one remote directory.
    pub fn open_path(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(connection) = self.connection.clone() else {
            self.notice = Some("No deployment server selected".into());
            cx.notify();
            return;
        };
        self.busy = true;
        let host = self.host.clone();
        let path_for_task = path.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::deploy::remote_list(&host, connection, path_for_task)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        let path_input = self.path.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    panel.busy = false;
                    match result {
                        Ok(mut entries) => {
                            entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
                            panel.entries = entries;
                            panel.notice = None;
                        }
                        Err(err) => panel.notice = Some(format!("Cannot list the folder: {err}").into()),
                    }
                    cx.notify();
                });
                let _ = path_input;
            }
        }));
        // The bar shows where the tree is (not a local path).
        let _ = path;
        cx.notify();
    }

    /// Descends into a folder or opens a file's remote copy.
    pub fn activate(&mut self, entry: RemoteEntry, cx: &mut Context<Self>) {
        if entry.is_dir {
            self.open_path(entry.path, cx);
            return;
        }
        let Some(connection) = self.connection.clone() else { return };
        self.busy = true;
        let host = self.host.clone();
        let editor = self.editor.clone();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.host.runtime.spawn(async move {
            let result = ide_host::ops::deploy::remote_open(&host, connection, entry.path)
                .await
                .map_err(|err| err.to_string());
            let _ = tx.unbounded_send(result);
        });
        let _ = &editor;
        self.task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |panel, cx| {
                    panel.busy = false;
                    match result {
                        Ok(path) => {
                            // The remote copy lands in the local cache; opening it
                            // is queued because this task has no `Window`.
                            panel.pending_open = Some(path.0.clone());
                        }
                        Err(err) => panel.notice = Some(format!("Cannot open the remote file: {err}").into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    /// Goes to the parent folder of `path`.
    pub fn go_up(&mut self, path: &str, cx: &mut Context<Self>) {
        let parent = std::path::Path::new(path)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned());
        if parent != path {
            self.open_path(parent, cx);
        }
    }

    /// Re-lists the current folder.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let path = self.current_path(cx);
        if path.is_empty() {
            self.notice = Some("Select a deployment server first".into());
            cx.notify();
            return;
        }
        self.open_path(path, cx);
    }
}

impl Focusable for RemoteHostPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RemoteHostPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(path) = self.pending_open.take() {
            self.editor.update(cx, |editor, cx| editor.open_file(path, window, cx));
        }
        let theme = cx.theme().clone();
        let hover = theme.list_hover;
        let server = self.server.clone().unwrap_or_else(|| "no server".to_owned());
        let entries: std::rc::Rc<Vec<(String, bool, u64)>> =
            std::rc::Rc::new(self.entries.iter().map(|entry| (entry.name.clone(), entry.is_dir, entry.size)).collect());
        let row_theme = theme.clone();
        let select = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
            if let Some(server) = this.server.clone() {
                this.select_server(server, cx);
            } else {
                this.refresh(cx);
            }
        });
        let up = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| {
            let path = this.current_path(cx);
            this.go_up(&path, cx);
        });

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
                            .child("Remote Host"),
                    )
                    .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(server))
                    .child(div().flex_1())
                    .when(self.busy, |this| {
                        this.child(div().text_size(px(9.5)).text_color(theme.warning).child("working…"))
                    })
                    .child(
                        div()
                            .id("remote-up")
                            .h(px(18.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(hover))
                            .on_click(up)
                            .child(Icon::new(IconName::ArrowUp).xsmall().text_color(theme.fg_secondary())),
                    )
                    .child(
                        div()
                            .id("remote-refresh")
                            .h(px(18.))
                            .px_1()
                            .flex()
                            .items_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(hover))
                            .on_click(select)
                            .child(Icon::new(IconName::RefreshCw).xsmall().text_color(theme.fg_secondary())),
                    ),
            )
            .child(div().px_2().py_1().child(Input::new(&self.path).text_size(px(10.5))))
            .when_some(self.notice.clone(), |this, notice| {
                let theme = theme.clone();
                this.child(div().px_2().py_1().text_size(px(10.5)).text_color(theme.fg_secondary()).child(notice))
            })
            .child(
                div()
                    .id("remote-list")
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
                                .child("Connect a deployment server to browse its files"),
                        )
                    })
                    .children(entries.iter().enumerate().map(|(index, (name, is_dir, size))| {
                        let hover = row_theme.list_hover;
                        let entry = self.entries.get(index).cloned();
                        let activate = cx.listener(move |this: &mut Self, _: &ClickEvent, _window, cx| {
                            if let Some(entry) = entry.clone() {
                                this.activate(entry, cx);
                            }
                        });
                        div()
                            .id(ElementId::Name(format!("remote-entry-{index}").into()))
                            .h(px(20.))
                            .px_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(hover))
                            .on_click(activate)
                            .child(
                                Icon::new(if *is_dir { IconName::Folder } else { IconName::FileCode })
                                    .xsmall()
                                    .text_color(if *is_dir { row_theme.info } else { row_theme.fg_muted() }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(row_theme.foreground)
                                    .child(name.clone()),
                            )
                            .when(!*is_dir, |this| {
                                this.child(
                                    div()
                                        .text_size(px(9.))
                                        .text_color(row_theme.fg_muted())
                                        .child(format_size(*size)),
                                )
                            })
                    })),
            )
    }
}

/// A compact file size for the row.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// The panel's icon.
pub const ICON: IconName = IconName::Cloud;

#[cfg(test)]
mod tests {
    use super::format_size;

    #[test]
    fn sizes_read_at_the_matching_unit() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }
}