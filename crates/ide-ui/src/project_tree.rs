//! Lazily loaded project tree.
//!
//! Directories are read on expansion with `ide-vfs::read_dir`, which applies the
//! `.gitignore` rules and reports IDEA's "ignored" / "excluded" flags per entry.
//! Only expanded subtrees are materialised, so a monorepo costs one `read_dir`
//! per open folder rather than a full walk.

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_host::Project;
use ide_vfs::EntryKind;
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone)]
struct Row {
    path: PathBuf,
    label: SharedString,
    depth: usize,
    is_dir: bool,
    ignored: bool,
    expanded: bool,
}

pub struct ProjectTree {
    root: Option<PathBuf>,
    rows: Rc<Vec<Row>>,
    expanded: HashSet<PathBuf>,
    selected: Rc<Cell<Option<usize>>>,
    editor: Entity<EditorArea>,
    focus_handle: FocusHandle,
    message: SharedString,
}

impl ProjectTree {
    pub fn new(editor: Entity<EditorArea>, cx: &mut Context<Self>) -> Self {
        Self {
            root: None,
            rows: Rc::new(Vec::new()),
            expanded: HashSet::new(),
            selected: Rc::new(Cell::new(None)),
            editor,
            focus_handle: cx.focus_handle(),
            message: "No project open".into(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        self.root = Some(project.root.clone());
        self.expanded.clear();
        self.expanded.insert(project.root.clone());
        self.selected.set(None);
        self.message = SharedString::default();
        self.rebuild();
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.root.is_some() {
            self.rebuild();
            cx.notify();
        }
    }

    fn rebuild(&mut self) {
        let Some(root) = self.root.clone() else {
            self.rows = Rc::new(Vec::new());
            return;
        };
        let mut rows = Vec::new();
        let label =
            root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
        rows.push(Row {
            path: root.clone(),
            label: label.into(),
            depth: 0,
            is_dir: true,
            ignored: false,
            expanded: true,
        });
        self.push_children(&root, 1, &mut rows);
        self.rows = Rc::new(rows);
    }

    fn push_children(&self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        let Ok(entries) = ide_vfs::read_dir(dir, &[]) else {
            return;
        };
        for entry in entries {
            // VCS metadata is never worth a row.
            if matches!(entry.name.as_str(), ".git" | ".hg" | ".svn") {
                continue;
            }
            let is_dir = entry.kind == EntryKind::Dir || entry.target_kind == Some(EntryKind::Dir);
            let path: PathBuf = entry.path.0;
            let expanded = is_dir && self.expanded.contains(&path);
            out.push(Row {
                path: path.clone(),
                label: entry.name.into(),
                depth,
                is_dir,
                ignored: entry.ignored,
                expanded,
            });
            if expanded {
                self.push_children(&path, depth + 1, out);
            }
        }
    }

    /// IntelliJ semantics: folders toggle on click, files open in a preview tab.
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        self.selected.set(Some(index));
        if row.is_dir {
            if self.expanded.contains(&row.path) {
                self.expanded.remove(&row.path);
            } else {
                self.expanded.insert(row.path.clone());
            }
            self.rebuild();
            cx.notify();
        } else {
            self.editor.update(cx, |editor, cx| editor.open_file(row.path, window, cx));
            cx.notify();
        }
    }
}

impl Focusable for ProjectTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProjectTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = self.rows.clone();
        let selected = self.selected.clone();
        let on_row: Rc<dyn Fn((usize, bool), &mut Window, &mut App)> =
            Rc::new(cx.processor(|this: &mut Self, (index, _double): (usize, bool), window, cx| {
                this.activate(index, window, cx);
            }));
        let empty = rows.is_empty();
        let list_theme = theme.clone();
        let refresh = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.refresh(cx));
        let header_hover = theme.border;

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(
                div()
                    .h(px(28.))
                    .w_full()
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child("Project"),
                    )
                    .child(
                        div()
                            .id("project-refresh")
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(header_hover))
                            .on_click(refresh)
                            .child(Icon::new(IconName::RefreshCw).xsmall().text_color(theme.fg_muted())),
                    ),
            )
            .when(empty, |this| {
                this.child(
                    div().p_3().text_size(px(12.)).text_color(theme.muted_foreground).child(self.message.clone()),
                )
            })
            .when(!empty, |this| {
                this.child(
                    div().flex_1().min_h_0().child(
                        uniform_list("project-tree-rows", rows.len(), move |range, _window, _cx| {
                            range
                                .map(|index| {
                                    row_element(
                                        index,
                                        &rows,
                                        &list_theme,
                                        selected.get() == Some(index),
                                        on_row.clone(),
                                    )
                                })
                                .collect()
                        })
                        .size_full(),
                    ),
                )
            })
            .child(
                div()
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(11.))
                    .text_color(theme.fg_muted())
                    .truncate()
                    .child(
                        self.root
                            .as_ref()
                            .map(|root| SharedString::from(root.display().to_string()))
                            .unwrap_or_else(|| "Open a project with Ctrl+O".into()),
                    ),
            )
    }
}

/// Rows render from a shared snapshot so the `uniform_list` closure can be
/// `'static`; the click handler updates the live view through `cx.processor`.
fn row_element(
    index: usize,
    rows: &Rc<Vec<Row>>,
    theme: &Theme,
    selected: bool,
    on_row: Rc<dyn Fn((usize, bool), &mut Window, &mut App)>,
) -> AnyElement {
    let Some(row) = rows.get(index) else {
        return div().into_any_element();
    };
    let indent = 4. + row.depth as f32 * 12.;
    let expander = if row.is_dir {
        Icon::new(if row.expanded { IconName::ChevronDown } else { IconName::ChevronRight })
            .xsmall()
            .text_color(theme.muted_foreground)
            .into_any_element()
    } else {
        div().w(px(12.)).into_any_element()
    };
    let icon = if row.is_dir {
        Icon::new(if row.expanded { IconName::FolderOpen } else { IconName::Folder })
            .small()
            .text_color(theme.muted_foreground)
            .into_any_element()
    } else {
        Icon::new(IconName::FileCode).small().text_color(theme.muted_foreground).into_any_element()
    };
    let label_color = if row.ignored { theme.fg_muted() } else { theme.foreground };
    let hover = theme.list_hover;
    let active = theme.list_active;

    div()
        .id(ElementId::Name(format!("project-row-{index}").into()))
        .h(px(22.))
        .w_full()
        .pl(px(indent))
        .pr_2()
        .flex()
        .items_center()
        .gap_1()
        .rounded(px(4.))
        .cursor_pointer()
        .when(selected, |this| this.bg(active))
        .when(!selected, |this| this.hover(move |this| this.bg(hover)))
        .on_click(move |event: &ClickEvent, window, cx| {
            on_row((index, event.click_count() > 1), window, cx);
        })
        .child(expander)
        .child(icon)
        .child(div().flex_1().min_w_0().text_size(px(12.5)).text_color(label_color).truncate().child(row.label.clone()))
        .into_any_element()
}
