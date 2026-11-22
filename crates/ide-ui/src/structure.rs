//! Structure view: the open file's symbols as a tree.
//!
//! `ide-language`'s `Syntax::outline` parses the buffer with the file's own
//! grammar and returns nested `OutlineItem`s (namespace → class → method, with
//! kinds, details, visibility and the ranges needed to jump). This panel turns
//! that tree into rows: kinds are coloured, containers expand and collapse, and
//! a click puts the caret on the symbol.
//!
//! It reads the editor's *current* text, so the tree follows unsaved edits
//! without a save or a language server.

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::{LanguageId, OutlineItem, SymbolKind};
use ide_language::Syntax;
use std::path::PathBuf;
use std::sync::Arc;

/// One visible row of the tree.
struct StructureRow {
    /// Indentation depth.
    depth: usize,
    name: SharedString,
    detail: Option<SharedString>,
    kind: SymbolKind,
    /// The symbol's identifier position (0-based), for navigation.
    line: u32,
    col: u32,
    /// Whether the row has children in the outline.
    has_children: bool,
    expanded: bool,
    /// Path of child indices from the root, so a click can be mapped back.
    path: Vec<usize>,
}

pub struct StructurePanel {
    editor: Entity<EditorArea>,
    syntax: Arc<Syntax>,
    /// The file the tree was built for.
    file: Option<PathBuf>,
    rows: Vec<StructureRow>,
    /// Expanded containers, by child-index path.
    expanded: std::collections::HashSet<Vec<usize>>,
    notice: Option<SharedString>,
    language: SharedString,
    focus_handle: FocusHandle,
}

impl StructurePanel {
    pub fn new(editor: Entity<EditorArea>, syntax: Arc<Syntax>, cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            syntax,
            file: None,
            rows: Vec::new(),
            expanded: std::collections::HashSet::new(),
            notice: None,
            language: "".into(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Rebuilds the tree from the editor's active buffer.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(info) = self.editor.read(cx).active_info() else {
            self.file = None;
            self.rows.clear();
            self.notice = Some("No file open".into());
            cx.notify();
            return;
        };
        let Some(text) = self.editor.read(cx).active_text() else {
            return;
        };
        let language = LanguageId::new(info.language.to_string());
        let items = self.syntax.outline(&language, &text);
        self.file = Some(info.path);
        self.language = info.language.clone();
        if items.is_empty() {
            self.notice = Some(format!("No symbols found ({})", info.language).into());
            self.rows.clear();
            cx.notify();
            return;
        }
        self.notice = None;
        // Containers start open: a file's top level is what a reader wants.
        self.expanded.clear();
        self.rows = flatten(&items, 0, &mut Vec::new(), true, &mut self.expanded);
        cx.notify();
    }

    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    pub fn names(&self) -> Vec<(usize, String)> {
        self.rows.iter().map(|row| (row.depth, row.name.to_string())).collect()
    }

    pub fn toggle(&mut self, path: Vec<usize>, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.rows = self.rebuild(cx);
        cx.notify();
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) -> Vec<StructureRow> {
        let Some(info) = self.editor.read(cx).active_info() else { return Vec::new() };
        let Some(text) = self.editor.read(cx).active_text() else { return Vec::new() };
        let language = LanguageId::new(info.language.to_string());
        let items = self.syntax.outline(&language, &text);
        let mut expanded = std::mem::take(&mut self.expanded);
        let rows = flatten(&items, 0, &mut Vec::new(), true, &mut expanded);
        self.expanded = expanded;
        rows
    }
}

/// Flattens the outline into rows, honouring `expanded`.
fn flatten(
    items: &[OutlineItem],
    depth: usize,
    prefix: &mut Vec<usize>,
    parent_open: bool,
    expanded: &mut std::collections::HashSet<Vec<usize>>,
) -> Vec<StructureRow> {
    let mut rows = Vec::new();
    if !parent_open {
        return rows;
    }
    for (index, item) in items.iter().enumerate() {
        prefix.push(index);
        let path = prefix.clone();
        let has_children = !item.children.is_empty();
        // Containers are open unless the reader closed them; leaves ignore it.
        let is_open = has_children && !expanded.contains(&closed_key(&path));
        rows.push(StructureRow {
            depth,
            name: item.name.clone().into(),
            detail: item.detail.clone().map(Into::into),
            kind: item.kind,
            line: item.selection_range.start.line,
            col: item.selection_range.start.col,
            has_children,
            expanded: is_open,
            path: path.clone(),
        });
        if has_children {
            rows.extend(flatten(&item.children, depth + 1, prefix, is_open, expanded));
        }
        prefix.pop();
    }
    rows
}

/// `expanded` stores the *closed* containers, so a fresh tree starts open.
fn closed_key(path: &[usize]) -> Vec<usize> {
    let mut key = Vec::with_capacity(path.len() + 1);
    key.push(usize::MAX);
    key.extend_from_slice(path);
    key
}

/// Icon for a symbol kind, as IDEA's structure view shows them.
fn kind_icon(kind: SymbolKind, theme: &Theme) -> (IconName, Hsla) {
    match kind {
        SymbolKind::Class | SymbolKind::Struct => (IconName::Box, theme.info),
        SymbolKind::Interface | SymbolKind::Trait => (IconName::Braces, theme.info),
        SymbolKind::Enum => (IconName::List, theme.warning),
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => {
            (IconName::Play, theme.success)
        }
        SymbolKind::Field | SymbolKind::Property | SymbolKind::Variable => {
            (IconName::Tag, theme.fg_secondary())
        }
        SymbolKind::Constant => (IconName::Hash, theme.warning),
        SymbolKind::Module | SymbolKind::Namespace | SymbolKind::Package => {
            (IconName::Folder, theme.fg_secondary())
        }
        _ => (IconName::FileCode, theme.fg_muted()),
    }
}

impl Focusable for StructurePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for StructurePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let row_theme = theme.clone();
        let hover = theme.list_hover;
        let rows: std::rc::Rc<Vec<(usize, SharedString, Option<SharedString>, bool, bool, Vec<usize>, u32, u32, SymbolKind)>> =
            std::rc::Rc::new(
                self.rows
                    .iter()
                    .map(|row| {
                        (
                            row.depth,
                            row.name.clone(),
                            row.detail.clone(),
                            row.has_children,
                            row.expanded,
                            row.path.clone(),
                            row.line,
                            row.col,
                            row.kind,
                        )
                    })
                    .collect(),
            );
        let entity = cx.entity();
        let file = self.file.clone();
        let editor = self.editor.clone();
        let open: std::rc::Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)> = {
            let editor = editor;
            std::rc::Rc::new(move |(path, line, col): (PathBuf, u32, u32), window, cx| {
                editor.update(cx, |editor, cx| editor.open_file_at(path, line, col, window, cx));
            })
        };
        let refresh = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.refresh(cx));

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
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child("Structure"),
                    )
                    .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(self.language.clone()))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("structure-refresh")
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
                this.child(div().px_2().py_1().text_size(px(10.5)).text_color(theme.fg_muted()).child(notice))
            })
            .child(
                div()
                    .id("structure-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .flex()
                    .flex_col()
                    .children(rows.iter().enumerate().map(|(index, row)| {
                        let (depth, name, detail, has_children, expanded, path, line, col, kind) = row;
                        let (depth, line, col) = (*depth, *line, *col);
                        let (has_children, expanded) = (*has_children, *expanded);
                        let path = path.clone();
                        let file = file.clone();
                        let (icon, color) = kind_icon(*kind, &row_theme);
                        let hover = row_theme.list_hover;
                        let toggle = {
                            let entity = entity.clone();
                            let path = path.clone();
                            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                                entity.update(cx, |panel: &mut StructurePanel, cx| panel.toggle(path.clone(), cx));
                            }
                        };
                        let open = open.clone();
                        let target = file.clone().map(|path| (path, line, col));
                        div()
                            .id(ElementId::Name(format!("structure-row-{index}").into()))
                            .h(px(22.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .pl(px(4. + depth as f32 * 12.))
                            .pr_1()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(hover))
                            .on_click(move |_: &ClickEvent, window, cx| {
                                if let Some(target) = target.clone() {
                                    open(target, window, cx);
                                }
                            })
                            .when(has_children, |this| {
                                let glyph = if expanded { IconName::ChevronDown } else { IconName::ChevronRight };
                                this.child(
                                    div()
                                        .id(ElementId::Name(format!("structure-toggle-{index}").into()))
                                        .size(px(14.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(2.))
                                        .on_click(toggle)
                                        .child(Icon::new(glyph).xsmall().text_color(row_theme.fg_muted())),
                                )
                            })
                            .when(!has_children, |this| this.child(div().size(px(14.))))
                            .child(Icon::new(icon).xsmall().text_color(color))
                            .child(
                                div()
                                    .max_w(px(220.))
                                    .truncate()
                                    .text_size(px(11.5))
                                    .text_color(row_theme.foreground)
                                    .child(name.clone()),
                            )
                            .when_some(detail.clone(), |this, detail| {
                                this.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(10.))
                                        .text_color(row_theme.fg_muted())
                                        .child(detail),
                                )
                            })
                    })),
            )
    }
}

/// The panel's icon.
pub const ICON: IconName = IconName::ListTree;

#[cfg(test)]
mod tests {
    use super::flatten;
    use ide_core::{OutlineItem, Pos, Range, SymbolKind};
    use std::collections::HashSet;

    fn item(name: &str, kind: SymbolKind, children: Vec<OutlineItem>) -> OutlineItem {
        OutlineItem {
            name: name.into(),
            detail: None,
            kind,
            range: Range { start: Pos { line: 0, col: 0 }, end: Pos { line: 1, col: 0 } },
            selection_range: Range { start: Pos { line: 0, col: 0 }, end: Pos { line: 0, col: 4 } },
            children,
            visibility: None,
        }
    }

    #[test]
    fn a_fresh_tree_shows_containers_open() {
        let items = vec![item(
            "Svc",
            SymbolKind::Class,
            vec![item("run", SymbolKind::Method, vec![])],
        )];
        let mut expanded = HashSet::new();
        let rows = flatten(&items, 0, &mut Vec::new(), true, &mut expanded);
        assert_eq!(rows.len(), 2, "class and its method");
        assert_eq!(rows[1].depth, 1);
        assert!(rows[0].expanded);
    }

    #[test]
    fn a_closed_container_hides_its_children() {
        let items = vec![item(
            "Svc",
            SymbolKind::Class,
            vec![item("run", SymbolKind::Method, vec![])],
        )];
        // Closing the root of the tree (path [0]) is stored under a sentinel key.
        let mut expanded = HashSet::new();
        expanded.insert(super::closed_key(&[0]));
        let rows = flatten(&items, 0, &mut Vec::new(), true, &mut expanded);
        assert_eq!(rows.len(), 1, "only the class row");
        assert!(!rows[0].expanded);
    }

    #[test]
    fn leaves_are_never_expandable() {
        let items = vec![item("CONST", SymbolKind::Constant, vec![])];
        let rows = flatten(&items, 0, &mut Vec::new(), true, &mut HashSet::new());
        assert!(!rows[0].has_children);
    }
}