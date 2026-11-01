//! Diagnostics store and the Problems tool window.
//!
//! Language servers (and every other producer) report through
//! `IdeEvent::Diagnostics`, one full replacement per file and source. The store
//! keeps the latest set per `(path, source)`, so a producer can clear its own
//! diagnostics without touching another's. The Problems panel and the editor
//! decorations read from the same store.

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::{Diagnostic, Severity};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone)]
pub struct ProblemItem {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
    pub severity: Severity,
    pub message: String,
    pub source: String,
}

impl ProblemItem {
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// Diagnostics of every open/produced file, keyed by path and producer.
#[derive(Default)]
pub struct DiagnosticsStore {
    files: HashMap<PathBuf, BTreeMap<String, Vec<Diagnostic>>>,
}

impl DiagnosticsStore {
    pub fn apply(&mut self, path: &Path, source: &str, items: Vec<Diagnostic>) {
        if items.is_empty() {
            if let Some(by_source) = self.files.get_mut(path) {
                by_source.remove(source);
                if by_source.is_empty() {
                    self.files.remove(path);
                }
            }
            return;
        }
        self.files.entry(path.to_path_buf()).or_default().insert(source.to_owned(), items);
    }

    pub fn clear(&mut self) {
        self.files.clear();
    }

    pub fn for_path(&self, path: &Path) -> Vec<Diagnostic> {
        self.files.get(path).map(|by_source| by_source.values().flatten().cloned().collect()).unwrap_or_default()
    }

    /// Errors and warnings across the project.
    pub fn counts(&self) -> (usize, usize) {
        let mut errors = 0;
        let mut warnings = 0;
        for diagnostic in self.files.values().flat_map(|by_source| by_source.values().flatten()) {
            match diagnostic.severity {
                Severity::Error => errors += 1,
                Severity::Warning => warnings += 1,
                _ => {}
            }
        }
        (errors, warnings)
    }

    /// Every problem, file by file, line by line.
    pub fn items(&self) -> Vec<ProblemItem> {
        let mut paths: Vec<&PathBuf> = self.files.keys().collect();
        paths.sort();
        let mut out = Vec::new();
        for path in paths {
            let mut items: Vec<ProblemItem> = self.files[path]
                .iter()
                .flat_map(|(source, diagnostics)| {
                    diagnostics.iter().map(move |diagnostic| ProblemItem {
                        path: path.clone(),
                        line: diagnostic.range.start.line,
                        col: diagnostic.range.start.col,
                        severity: diagnostic.severity,
                        message: diagnostic.message.clone(),
                        source: source.clone(),
                    })
                })
                .collect();
            items.sort_by_key(|item| (item.line, item.col, item.severity as u8));
            out.extend(items);
        }
        out
    }
}

pub type SharedDiagnostics = Rc<RefCell<DiagnosticsStore>>;

pub fn shared_diagnostics() -> SharedDiagnostics {
    Rc::new(RefCell::new(DiagnosticsStore::default()))
}

pub struct ProblemsPanel {
    store: SharedDiagnostics,
    editor: Entity<EditorArea>,
    focus_handle: FocusHandle,
}

impl ProblemsPanel {
    pub fn new(store: SharedDiagnostics, editor: Entity<EditorArea>, cx: &mut Context<Self>) -> Self {
        Self { store, editor, focus_handle: cx.focus_handle() }
    }

    /// Re-read the store; called by the workbench when diagnostics change.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }
}

impl Focusable for ProblemsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProblemsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let items = self.store.borrow().items();
        let (errors, warnings) = self.store.borrow().counts();
        let rows = Rc::new(items);
        let row_theme = theme.clone();
        let on_open: Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)> =
            Rc::new(cx.processor(|this: &mut Self, (path, line, col): (PathBuf, u32, u32), window, cx| {
                let editor = this.editor.clone();
                editor.update(cx, |editor, cx| editor.open_file_at(path, line, col, window, cx));
            }));

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
                    .gap_3()
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child("Problems"),
                    )
                    .child(div().text_size(px(11.)).text_color(theme.danger).child(format!("{errors} errors")))
                    .child(div().text_size(px(11.)).text_color(theme.warning).child(format!("{warnings} warnings")))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.fg_muted())
                            .child("Language servers publish through ide-lsp on every edit"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(rows.is_empty(), |this| {
                        this.child(
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .text_color(theme.fg_muted())
                                .child("No problems reported"),
                        )
                    })
                    .when(!rows.is_empty(), |this| {
                        let rows = rows.clone();
                        this.child(
                            uniform_list("problems-list", rows.len(), move |range, _window, _cx| {
                                range
                                    .map(|index| {
                                        let item = &rows[index];
                                        render_problem_row(item, &row_theme, on_open.clone())
                                    })
                                    .collect()
                            })
                            .size_full(),
                        )
                    }),
            )
    }
}

/// Row renderer for the `uniform_list` closure (which cannot borrow the panel).
fn render_problem_row(
    item: &ProblemItem,
    theme: &Theme,
    on_open: Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)>,
) -> AnyElement {
    let (icon, color) = match item.severity {
        Severity::Error => (IconName::CircleAlert, theme.danger),
        Severity::Warning => (IconName::TriangleAlert, theme.warning),
        _ => (IconName::Info, theme.info),
    };
    let path = item.path.clone();
    let line = item.line;
    let col = item.col;
    div()
        .id(ElementId::Name(format!("problem-{}-{}-{}", item.path.display(), line, col).into()))
        .h(px(24.))
        .w_full()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(3.))
        .cursor_pointer()
        .hover(move |this| this.bg(theme.list_hover))
        .on_click(move |_: &ClickEvent, window, cx| on_open((path.clone(), line, col), window, cx))
        .child(Icon::new(icon).xsmall().text_color(color))
        .child(
            div()
                .max_w(px(520.))
                .truncate()
                .text_size(px(12.))
                .text_color(theme.foreground)
                .child(item.message.clone()),
        )
        .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(theme.fg_muted()).child(format!(
            "{}:{}",
            item.file_name(),
            item.line + 1
        )))
        .into_any_element()
}
