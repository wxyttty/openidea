//! The editor area: a tab strip over one native code editor per open file.
//!
//! Buffers are the project's shared [`ide_workspace::documents::Documents`],
//! not private copies: opening a file registers it with the language servers,
//! every keystroke is forwarded as an incremental `didChange`, saved files get
//! `didSave`, and diagnostics/fixes other subsystems produce round-trip back
//! through `IdeEvent::DocPatched`. Loading and saving still go through
//! `ide-workspace`, so encoding, BOM, line endings and `.editorconfig` behave
//! exactly as in the Tauri build.

use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::base::input::{
    Diagnostic, DiagnosticSet, DiagnosticSeverity, Position, RangeDecoration, RangeDecorationCollection,
    RangeDecorationStyle, RopeExt,
};
use gpui_kit::component::input::{Editor, EditorState, InputContent, InputEvent};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::{ChangeSet, DocId, LanguageId, TextChange};
use ide_host::Project;
use ide_text::LineEnding;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Tab {
    path: PathBuf,
    name: SharedString,
    doc: DocId,
    state: Entity<EditorState>,
    language_id: LanguageId,
    language: SharedString,
    encoding: SharedString,
    line_ending: LineEnding,
    /// Version of the shared `Documents` buffer this tab's text corresponds to.
    version: u32,
    /// `\n`-normalized text, as `Documents` stores it.
    text: String,
    dirty: bool,
    read_only: bool,
    /// Keeps the `InputEvent::Change` subscription alive with the tab.
    _change: Option<Subscription>,
    /// Breakpoint line highlights, adjusted automatically across edits.
    breakpoint_decorations: Option<RangeDecorationCollection>,
    /// Find-in-file match highlights.
    find_decorations: Option<RangeDecorationCollection>,
}

impl Tab {
    fn errors(&self, cx: &App) -> (usize, usize) {
        let Some(set) = self.state.read(cx).diagnostics() else { return (0, 0) };
        let (mut errors, mut warnings) = (0, 0);
        for entry in set.iter() {
            match entry.diagnostic.severity {
                DiagnosticSeverity::Error => errors += 1,
                DiagnosticSeverity::Warning => warnings += 1,
                _ => {}
            }
        }
        (errors, warnings)
    }
}

/// What the status bar shows for the active tab.
#[derive(Clone)]
pub struct TabInfo {
    pub name: SharedString,
    pub path: PathBuf,
    pub language: SharedString,
    pub encoding: SharedString,
    pub line_ending: &'static str,
    pub dirty: bool,
    pub read_only: bool,
}

pub struct EditorArea {
    project: Option<Arc<Project>>,
    tabs: Vec<Tab>,
    /// 0-based breakpoint lines by file, mirrored from the workbench store.
    breakpoints: BTreeMap<PathBuf, Vec<u32>>,
    soft_wrap: bool,
    /// Code font size in px (Settings › Editor › Font size).
    font_size: f32,
    active: Option<usize>,
    notice: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl EditorArea {
    pub fn new(_host: Arc<ide_host::Host>, cx: &mut Context<Self>) -> Self {
        Self {
            project: None,
            tabs: Vec::new(),
            breakpoints: BTreeMap::new(),
            soft_wrap: false,
            font_size: 13.,
            active: None,
            notice: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        for tab in std::mem::take(&mut self.tabs) {
            project.documents.close(tab.doc);
        }
        self.active = None;
        self.notice = None;
        self.project = Some(project.clone());
        cx.notify();
    }

    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn active_info(&self) -> Option<TabInfo> {
        let tab = self.tabs.get(self.active?)?;
        Some(TabInfo {
            name: tab.name.clone(),
            path: tab.path.clone(),
            language: tab.language.clone(),
            encoding: tab.encoding.clone(),
            line_ending: match tab.line_ending {
                LineEnding::Lf => "LF",
                LineEnding::CrLf => "CRLF",
                LineEnding::Cr => "CR",
            },
            dirty: tab.dirty,
            read_only: tab.read_only,
        })
    }

    pub fn notice(&self) -> Option<&SharedString> {
        self.notice.as_ref()
    }

    /// Open `path`, reusing the tab that already holds it.
    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file_at(path, 0, 0, window, cx);
    }

    /// Open `path` and put the caret on `line`/`col` (0-based), for search hits
    /// and Problems entries.
    pub fn open_file_at(&mut self, path: PathBuf, line: u32, col: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.notice = None;
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.active = Some(index);
            self.place_cursor(index, line, col, window, cx);
            cx.notify();
            return;
        }
        let Some(project) = self.project.clone() else {
            self.notice = Some("Open a project first".into());
            cx.notify();
            return;
        };
        let info = match project.documents.open(&path) {
            Ok(info) => info,
            Err(err) => {
                self.notice = Some(format!("Cannot open {}: {err}", path.display()).into());
                cx.notify();
                return;
            }
        };
        let language: SharedString = language_for(&path).into();
        let text = info.text.clone();
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx);
            state.set_value(InputContent::new(text.clone()), window, cx);
            state.set_highlighter(language.clone(), cx);
            state.set_readonly(info.read_only, cx);
            state
        });
        if self.soft_wrap {
            state.update(cx, |state, cx| state.set_soft_wrap(true, window, cx));
        }
        // Completion, hover and go-to-definition are answered from the same
        // document buffer the server watches.
        let provider = std::rc::Rc::new(crate::lsp::IdeLspProvider::new(project.clone(), info.id));
        provider.install(cx.entity(), &state, cx);
        let name =
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        let watched_doc = info.id;
        let watched_project = project.clone();
        let change = cx.subscribe(&state, move |this: &mut Self, _state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_edited(watched_doc, &watched_project, cx);
            }
        });
        self.tabs.push(Tab {
            path,
            name: name.into(),
            doc: info.id,
            state,
            language_id: info.language.clone(),
            language,
            encoding: info.encoding.into(),
            line_ending: info.line_ending,
            version: info.version,
            text,
            dirty: info.dirty,
            read_only: info.read_only,
            _change: None,
            breakpoint_decorations: None,
            find_decorations: None,
        });
        self.active = Some(self.tabs.len() - 1);
        let index = self.tabs.len() - 1;
        self.place_cursor(index, line, col, window, cx);
        // The subscription must live as long as the tab.
        self.tabs[index]._change = Some(change);
        self.apply_breakpoints(index, cx);
        cx.notify();
    }

    /// Apply Editor › Soft wrap to every tab and to files opened later.
    pub fn set_soft_wrap_default(&mut self, wrap: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.soft_wrap = wrap;
        for tab in &self.tabs {
            tab.state.update(cx, |state, cx| state.set_soft_wrap(wrap, window, cx));
        }
        cx.notify();
    }

    pub fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.font_size = size.clamp(8., 28.);
        cx.notify();
    }

    /// Update the breakpoint lines shown in the editor; existing tabs re-highlight.
    pub fn set_breakpoints(&mut self, breakpoints: BTreeMap<PathBuf, Vec<u32>>, cx: &mut Context<Self>) {
        self.breakpoints = breakpoints;
        for index in 0..self.tabs.len() {
            self.apply_breakpoints(index, cx);
        }
    }

    /// The active tab's path and 0-based caret line.
    pub fn active_location(&self, cx: &App) -> Option<(PathBuf, u32)> {
        let tab = self.tabs.get(self.active?)?;
        let line = tab.state.read(cx).cursor_position().line;
        Some((tab.path.clone(), line))
    }

    /// Repaint the breakpoint decorations of one tab.
    fn apply_breakpoints(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else { return };
        let state = tab.state.clone();
        let path = tab.path.clone();
        let lines = self.breakpoints.get(&path).cloned().unwrap_or_default();
        let mut tint = cx.theme().danger;
        tint.a = 0.18;
        let ranges: Vec<std::ops::Range<usize>> = {
            let state = state.read(cx);
            let rope = state.text();
            let count = rope.lines_len();
            lines
                .iter()
                .filter_map(|line| {
                    let start_line = (*line as usize).min(count.saturating_sub(1));
                    let start = rope.line_start_offset(start_line);
                    let end = if start_line + 1 < count {
                        rope.line_start_offset(start_line + 1)
                    } else {
                        rope.line_end_offset(start_line)
                    };
                    (end > start).then_some(start..end)
                })
                .collect()
        };
        let decorations = ranges
            .into_iter()
            .map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(tint))
            .collect::<Vec<_>>();
        match self.tabs[index].breakpoint_decorations.clone() {
            Some(collection) => collection.set(decorations, cx),
            None => {
                let collection =
                    state.update(cx, |state, cx| state.create_range_decorations_collection(decorations, cx));
                self.tabs[index].breakpoint_decorations = Some(collection);
            }
        }
    }

    /// Push the editor's new text into the shared document as an incremental
    /// change (this is what the language servers see).
    fn on_edited(&mut self, doc: DocId, project: &Arc<Project>, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.doc == doc) else { return };
        let text = self.tabs[index].state.read(cx).value().to_string();
        if text == self.tabs[index].text {
            return;
        }
        let changes = ide_text::Document::new(&self.tabs[index].text).diff(&text);
        match project.documents.edit(doc, self.tabs[index].version, changes) {
            Ok(version) => {
                let tab = &mut self.tabs[index];
                tab.version = version;
                tab.text = text;
                tab.dirty = true;
            }
            Err(err) => {
                // Version drift (another subsystem patched first): resync from
                // the live buffer rather than dropping the user's edit.
                tracing::warn!("document edit rejected: {err}");
                if let Ok(info) = project.documents.snapshot(doc) {
                    let tab = &mut self.tabs[index];
                    tab.version = info.version;
                    tab.text = info.text;
                }
            }
        }
        cx.notify();
    }

    /// `IdeEvent::DocPatched`: another subsystem changed the buffer (formatting,
    /// refactoring, an LSP workspace edit). Apply it to the editor too.
    pub fn apply_patch(
        &mut self,
        doc: DocId,
        base_version: u32,
        version: u32,
        changes: Vec<TextChange>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.tabs.iter().position(|tab| tab.doc == doc) else { return };
        // Our own edits come back with the pre-edit version; the buffer already
        // has them.
        if self.tabs[index].version != base_version {
            return;
        }
        let mut text_doc = ide_text::Document::new(&self.tabs[index].text);
        let text_version = text_doc.version();
        if text_doc.apply(text_version, &ChangeSet { changes }).is_err() {
            return;
        }
        let text = text_doc.text();
        let tab = &mut self.tabs[index];
        tab.version = version;
        tab.text = text.clone();
        let state = tab.state.clone();
        state.update(cx, |state, cx| {
            state.set_value(InputContent::new(text), window, cx);
        });
        cx.notify();
    }

    pub fn set_dirty(&mut self, doc: DocId, dirty: bool, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.doc == doc)
            && tab.dirty != dirty
        {
            tab.dirty = dirty;
            cx.notify();
        }
    }

    /// Replay `didOpen` for every tab. Called after a project is trusted in the
    /// same session: documents opened while restricted were never sent to the
    /// language servers.
    pub fn replay_lsp(&self) {
        let Some(project) = &self.project else { return };
        for tab in &self.tabs {
            project.lsp.did_open(tab.doc, &tab.path, &tab.language_id, tab.version, &tab.text);
        }
    }

    pub fn set_language(&mut self, doc: DocId, language: &str, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.doc == doc) else { return };
        let highlighter: SharedString = language_for_id(language).into();
        tab.language = highlighter.clone();
        tab.state.update(cx, |state, cx| state.set_highlighter(highlighter, cx));
        cx.notify();
    }

    /// Install the latest diagnostics of `path` on every tab showing it.
    pub fn set_diagnostics(&mut self, path: &Path, diagnostics: &[ide_core::Diagnostic], cx: &mut Context<Self>) {
        for tab in self.tabs.iter_mut().filter(|tab| tab.path == path) {
            tab.state.update(cx, |state, cx| {
                // The set is built from the editor's own rope (gpui packages a
                // newer ropey than the backend crates use).
                let mut set = DiagnosticSet::new(state.text());
                for diagnostic in diagnostics {
                    set.push(gpui_diagnostic(diagnostic));
                }
                if let Some(target) = state.diagnostics_mut() {
                    *target = set;
                }
                cx.notify();
            });
        }
        cx.notify();
    }

    fn place_cursor(&mut self, index: usize, line: u32, col: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else { return };
        let state = tab.state.clone();
        state.update(cx, |state, cx| {
            state.set_cursor_position(Position::new(line, col), window, cx);
        });
    }

    /// Case-insensitive occurrences of `query` in the active tab as 0-based
    /// (line, column); capped at 2000 so a one-letter query stays cheap.
    pub fn find_matches(&self, query: &str) -> Vec<(u32, u32)> {
        if query.is_empty() {
            return Vec::new();
        }
        let Some(tab) = self.active.and_then(|index| self.tabs.get(index)) else {
            return Vec::new();
        };
        let text = &tab.text;
        let mut out = Vec::new();
        fn position(text: &str, offset: usize) -> (u32, u32) {
            let line = text[..offset].matches('\n').count() as u32;
            let line_start = text[..offset].rfind('\n').map(|index| index + 1).unwrap_or(0);
            let col = text[line_start..offset].encode_utf16().count() as u32;
            (line, col)
        }
        let lower = text.to_lowercase();
        let needle = query.to_lowercase();
        if lower.len() == text.len() && !needle.is_empty() {
            let mut start = 0;
            while let Some(offset) = lower[start..].find(&needle) {
                let at = start + offset;
                out.push(position(text, at));
                if out.len() >= 2000 {
                    break;
                }
                start = at + needle.len();
            }
        } else {
            for (at, _) in text.match_indices(query) {
                out.push(position(text, at));
                if out.len() >= 2000 {
                    break;
                }
            }
        }
        out
    }

    /// Highlight every case-insensitive occurrence of `query` in the active
    /// tab (empty clears them).
    pub fn highlight_find(&mut self, query: &str, cx: &mut Context<Self>) {
        let Some(index) = self.active else { return };
        let ranges = {
            let state = self.tabs[index].state.read(cx);
            let text = state.text().to_string();
            let mut ranges = Vec::new();
            if !query.is_empty() {
                let lower = text.to_lowercase();
                let needle = query.to_lowercase();
                if lower.len() == text.len() && !needle.is_empty() {
                    let mut start = 0;
                    while let Some(offset) = lower[start..].find(&needle) {
                        let at = start + offset;
                        ranges.push(at..at + needle.len());
                        if ranges.len() >= 2000 {
                            break;
                        }
                        start = at + needle.len();
                    }
                }
            }
            ranges
        };
        let mut tint = cx.theme().warning;
        tint.a = 0.26;
        let decorations = ranges
            .into_iter()
            .map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(tint))
            .collect::<Vec<_>>();
        match self.tabs[index].find_decorations.clone() {
            Some(collection) => collection.set(decorations, cx),
            None => {
                let collection = self.tabs[index]
                    .state
                    .update(cx, |state, cx| state.create_range_decorations_collection(decorations, cx));
                self.tabs[index].find_decorations = Some(collection);
            }
        }
        cx.notify();
    }

    /// Move the caret of the active tab to a 0-based line/column and focus it.
    pub fn goto_line(&mut self, line: u32, col: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else { return };
        self.place_cursor(index, line, col, window, cx);
        self.focus_current(window, cx);
        cx.notify();
    }

    pub fn focus_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active.and_then(|index| self.tabs.get(index)) {
            Some(tab) => {
                let handle = tab.state.read(cx).focus_handle(cx).clone();
                window.focus(&handle, cx);
            }
            None => {
                let handle = self.focus_handle.clone();
                window.focus(&handle, cx);
            }
        }
    }

    pub fn save_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else {
            self.notice = Some("No file to save".into());
            cx.notify();
            return;
        };
        self.save(index, window, cx);
    }

    pub fn save_all(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        for index in 0..self.tabs.len() {
            if self.tabs[index].dirty {
                self.save(index, _window, cx);
            }
        }
    }

    fn save(&mut self, index: usize, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else { return };
        let Some(tab) = self.tabs.get(index) else { return };
        if tab.read_only {
            self.notice = Some("This document is read-only".into());
            cx.notify();
            return;
        }
        match project.documents.save(tab.doc) {
            Ok(()) => {
                self.tabs[index].dirty = false;
                self.notice = None;
            }
            Err(err) => self.notice = Some(format!("Save failed: {err}").into()),
        }
        cx.notify();
    }

    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.active else { return };
        if self.tabs.get(index).is_some_and(|tab| tab.dirty) {
            self.notice = Some("Save the file before closing it (Ctrl+S)".into());
            cx.notify();
            return;
        }
        if let Some(project) = self.project.clone()
            && let Some(tab) = self.tabs.get(index)
        {
            project.documents.close(tab.doc);
        }
        self.tabs.remove(index);
        self.active = if self.tabs.is_empty() { None } else { Some(index.min(self.tabs.len() - 1)) };
        self.focus_current(window, cx);
        cx.notify();
    }

    pub fn cycle(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let current = self.active.unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(self.tabs.len() as isize);
        self.active = Some(next as usize);
        cx.notify();
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = Some(index);
        self.focus_current(window, cx);
        cx.notify();
    }

    fn render_tabs(&self, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut strip = div()
            .h(px(33.))
            .w_full()
            .flex()
            .items_center()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .overflow_hidden();
        for (index, tab) in self.tabs.iter().enumerate() {
            let active = self.active == Some(index);
            let select = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                this.select(index, window, cx);
            });
            let close = cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                this.active = Some(index);
                this.close_active(window, cx);
            });
            let (errors, warnings) = tab.errors(cx);
            strip = strip.child(
                div()
                    .id(ElementId::Name(format!("editor-tab-{index}").into()))
                    .h_full()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_r_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(active, |this| this.bg(theme.tab_active))
                    .hover(move |this| this.bg(theme.tab_active))
                    .on_click(select)
                    .child(Icon::new(IconName::FileCode).xsmall().text_color(if active {
                        theme.primary
                    } else {
                        theme.fg_muted()
                    }))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(if active { theme.foreground } else { theme.fg_secondary() })
                            .child(tab.name.clone()),
                    )
                    .when(errors > 0, |this| {
                        this.child(div().text_size(px(10.5)).text_color(theme.danger).child(format!("{errors}")))
                    })
                    .when(errors == 0 && warnings > 0, |this| {
                        this.child(div().text_size(px(10.5)).text_color(theme.warning).child(format!("{warnings}")))
                    })
                    .when(tab.dirty, |this| this.child(div().w(px(6.)).h(px(6.)).rounded_full().bg(theme.primary)))
                    .child(
                        div()
                            .id(ElementId::Name(format!("editor-tab-close-{index}").into()))
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .cursor_pointer()
                            .hover(move |this| this.bg(theme.border))
                            .on_click(close)
                            .child(Icon::new(IconName::X).xsmall().text_color(theme.fg_muted())),
                    ),
            );
        }
        strip
    }
}

impl Focusable for EditorArea {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorArea {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let active_state = self.active.and_then(|index| self.tabs.get(index)).map(|tab| tab.state.clone());
        let has_active = active_state.is_some();
        let tabs = self.render_tabs(&theme, cx);
        let notice = self.notice.clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .child(tabs)
            .when_some(notice, |this, notice| {
                this.child(
                    div().px_3().py_1().text_size(px(11.5)).bg(theme.muted).text_color(theme.warning).child(notice),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when_some(active_state, |this, state| {
                        this.child(Editor::new(&state).text_size(px(self.font_size)).size_full())
                    })
                    .when(!has_active, |this| this.child(empty_state(&theme))),
            )
    }
}

fn empty_state(theme: &gpui_kit::component::Theme) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(Icon::new(IconName::FileCode).large().text_color(theme.fg_muted()))
        .child(div().text_size(px(13.)).text_color(theme.fg_secondary()).child("No file open"))
        .child(
            div()
                .text_size(px(11.5))
                .text_color(theme.fg_muted())
                .child("Click a file in the Project tool window, or press Ctrl+P"),
        )
}

fn gpui_diagnostic(diagnostic: &ide_core::Diagnostic) -> Diagnostic {
    Diagnostic {
        range: Position::new(diagnostic.range.start.line, diagnostic.range.start.col)
            ..Position::new(diagnostic.range.end.line, diagnostic.range.end.col),
        severity: match diagnostic.severity {
            ide_core::Severity::Error => DiagnosticSeverity::Error,
            ide_core::Severity::Warning => DiagnosticSeverity::Warning,
            ide_core::Severity::Info => DiagnosticSeverity::Info,
            ide_core::Severity::Hint => DiagnosticSeverity::Hint,
        },
        code: diagnostic.code.clone().map(Into::into),
        code_description: None,
        source: diagnostic.source.clone().or_else(|| diagnostic.server.clone()).map(Into::into),
        message: diagnostic.message.clone().into(),
        related_information: None,
        tags: None,
        data: None,
    }
}

/// `ide-core` language ids (`LanguageId`, lower-case names) to the highlighter
/// names `gpui-component`/syntect knows.
fn language_for_id(id: &str) -> &'static str {
    let id = id.to_ascii_lowercase();
    let id = id.rsplit('.').next().unwrap_or(&id);
    match id {
        "rust" => "rust",
        "typescript" => "typescript",
        "tsx" | "typescriptreact" => "tsx",
        "javascript" | "javascriptreact" | "jsx" => "javascript",
        "python" => "python",
        "java" => "java",
        "kotlin" => "kotlin",
        "go" => "go",
        "c" => "c",
        "cpp" | "c++" | "objective-c" | "objective-c++" => "cpp",
        "csharp" | "c#" => "csharp",
        "ruby" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "scala" => "scala",
        "lua" => "lua",
        "zig" => "zig",
        "elixir" => "elixir",
        "html" => "html",
        "css" | "scss" | "sass" | "less" => "css",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "markdown" => "markdown",
        "shell" | "bash" | "sh" | "zsh" => "bash",
        "sql" => "sql",
        "protobuf" | "proto" => "proto",
        "diff" => "diff",
        "svelte" => "svelte",
        "astro" => "astro",
        "ejs" => "ejs",
        "erb" => "erb",
        "cmake" => "cmake",
        "makefile" | "make" => "make",
        "graphql" => "graphql",
        _ => "plaintext",
    }
}

/// The highlighter name syntect knows for `path` (extension based; used before
/// the document's own language is known).
pub fn language_for(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or_default().to_ascii_lowercase();
    let id = match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "py" | "pyi" => "python",
        "java" => "java",
        "kt" | "kts" | "ktm" => "kotlin",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" | "phtml" => "php",
        "swift" => "swift",
        "scala" => "scala",
        "lua" => "lua",
        "zig" => "zig",
        "ex" | "exs" => "elixir",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" | "mdx" => "markdown",
        "sh" | "bash" | "zsh" => "bash",
        "sql" => "sql",
        "proto" | "protobuf" => "proto",
        "diff" | "patch" => "diff",
        "svelte" => "svelte",
        "astro" => "astro",
        "ejs" => "ejs",
        "erb" => "erb",
        "cmake" => "cmake",
        "mk" => "make",
        "graphql" => "graphql",
        _ => "plaintext",
    };
    language_for_id(id)
}
