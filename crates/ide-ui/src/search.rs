//! Find in Path, over `ide-search`.
//!
//! The scan runs on a worker thread and streams `FileMatches` as files finish,
//! so a large project shows hits immediately. Results are flattened into
//! file/hit rows and clicking one opens the file at the matching position.

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::{CancellationToken, Pos, RequestId};
use ide_host::{Host, Project};
use ide_search::{FileMatches, ReplaceEvent, ReplaceTarget, SearchContext, SearchQuery, SearchScope};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// Stop collecting after this many files; the status line says when it is hit.
const MAX_FILES: usize = 500;

#[derive(Clone)]
struct SearchRow {
    /// A file header row when true, a hit otherwise.
    header: bool,
    path: PathBuf,
    rel: String,
    line: u32,
    col: u32,
    preview: String,
    count: usize,
    /// The replacement preview for a hit (the query text with the replacement).
    replacement: Option<String>,
}

/// The fields of IDEA's search bar: what to match, where it may lie, and which
/// files to consider.
#[derive(Clone, Debug, Default)]
pub struct SearchOptions {
    pub is_regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Only in / only outside comments and strings.
    pub context: SearchContext,
    /// Whitespace-separated file masks (`*.rs`, `src/**`, `!*.test.ts`).
    pub include: String,
    pub exclude: String,
}

impl SearchOptions {
    /// The backend query for `pattern` under these options.
    pub fn query(&self, pattern: String) -> SearchQuery {
        SearchQuery {
            pattern,
            is_regex: self.is_regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            include: split_masks(&self.include),
            exclude: split_masks(&self.exclude),
            scope: SearchScope::Project,
            max_matches: 10_000,
            context: self.context,
            include_excluded: false,
        }
    }
}

/// The line text with the current query's ranges replaced, for the result row's
/// preview of what Replace would do.
pub fn replace_in_line(text: &str, ranges: &[(usize, usize)], replacement: &str) -> String {
    if ranges.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, len) in ranges {
        let start = (*start).min(text.len());
        let end = (start + *len).min(text.len());
        if start < cursor || start >= text.len() {
            continue;
        }
        out.push_str(&text[cursor..start]);
        out.push_str(replacement);
        cursor = end;
    }
    out.push_str(&text[cursor.min(text.len())..]);
    out
}

/// Splits a mask field into entries. A `!` prefix marks an exclusion, which the
/// backend takes from the exclude list; `SearchOptions::query` keeps the two
/// lists separate, so this only trims and drops blanks.
fn split_masks(masks: &str) -> Vec<String> {
    masks.split_whitespace().map(|mask| mask.trim_start_matches('!').to_owned()).filter(|mask| !mask.is_empty()).collect()
}

pub struct SearchPanel {
    host: Arc<Host>,
    project: Option<Arc<Project>>,
    editor: Entity<EditorArea>,
    query: Entity<InputState>,
    replace: Entity<InputState>,
    files: Vec<FileMatches>,
    running: bool,
    truncated: bool,
    message: Option<SharedString>,
    /// Search options: regex, case, whole word, context, include/exclude masks.
    options: SearchOptions,
    /// Per-match replacements already applied from the results list.
    replaced_files: std::collections::HashSet<(PathBuf, u32, u32)>,
    /// The include / exclude mask fields.
    include: Entity<InputState>,
    exclude: Entity<InputState>,
    cancel: CancellationToken,
    generation: u64,
    focus_handle: FocusHandle,
    request_task: Option<Task<()>>,
    _submit: Subscription,
    _replace_submit: Subscription,
}

impl SearchPanel {
    pub fn new(host: Arc<Host>, editor: Entity<EditorArea>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search in project…"));
        let submit = cx.subscribe(&query, |this: &mut Self, _query, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.run(cx);
            }
        });
        let replace = cx.new(|cx| InputState::new(window, cx).placeholder("Replace with…"));
        let replace_submit = cx.subscribe(&replace, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.replace_all(cx);
            }
        });
        Self {
            host,
            project: None,
            editor,
            query,
            replace,
            files: Vec::new(),
            running: false,
            truncated: false,
            message: None,
            options: SearchOptions::default(),
            replaced_files: std::collections::HashSet::new(),
            include: cx.new(|cx| InputState::new(window, cx).placeholder("File masks, e.g. *.rs src/**")),
            exclude: cx.new(|cx| InputState::new(window, cx).placeholder("Exclude, e.g. *.test.ts")),
            cancel: CancellationToken::new(),
            generation: 0,
            request_task: None,
            _replace_submit: replace_submit,
            focus_handle: cx.focus_handle(),
            _submit: submit,
        }
    }

    pub fn set_project(&mut self, project: &Arc<Project>, cx: &mut Context<Self>) {
        self.project = Some(project.clone());
        self.files.clear();
        self.message = None;
        cx.notify();
    }

    /// Focus the query field (called when the tool window opens).
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.query.read(cx).focus_handle(cx).clone();
        window.focus(&handle, cx);
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            self.message = Some("Open a project first".into());
            cx.notify();
            return;
        };
        let pattern = self.query.read(cx).value().trim().to_owned();
        if pattern.is_empty() {
            self.files.clear();
            self.message = None;
            cx.notify();
            return;
        }
        self.cancel.cancel();
        let cancel = CancellationToken::new();
        self.cancel = cancel.clone();
        self.generation += 1;
        let generation = self.generation;
        self.files.clear();
        self.running = true;
        self.truncated = false;
        self.message = None;
        cx.notify();

        tracing::debug!(pattern = %self.query.read(cx).value(), "search started");
        let (tx, mut rx) = unbounded::<FileMatches>();
        let root = project.root.clone();
        let query = self.options.query(pattern);
        std::thread::Builder::new()
            .name("openidea-search".into())
            .spawn(move || {
                let on_file = move |m: FileMatches| {
                    let _ = tx.unbounded_send(m);
                };
                let overlay = |_: &std::path::Path| None;
                if let Err(err) = ide_search::search(&root, &query, &overlay, &cancel, &on_file) {
                    tracing::warn!("search failed: {err}");
                }
            })
            .expect("spawn search thread");

        cx.spawn(async move |this, cx| {
            while let Some(file) = rx.next().await {
                let mut done = false;
                let updated = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        done = true;
                        return;
                    }
                    if this.files.len() < MAX_FILES {
                        this.files.push(file);
                        this.files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
                    } else {
                        this.truncated = true;
                    }
                    cx.notify();
                });
                if updated.is_err() || done {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.running = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The current search options.
    pub fn options(&self) -> &SearchOptions {
        &self.options
    }

    /// Toggles one option and re-runs the search when there already is a
    /// pattern, so the result list always matches the bar.
    pub fn set_option(&mut self, update: impl FnOnce(&mut SearchOptions), cx: &mut Context<Self>) {
        update(&mut self.options);
        if !self.query.read(cx).value().trim().is_empty() {
            self.run(cx);
        } else {
            cx.notify();
        }
    }

    /// Sets the file masks (include and exclude) and re-runs.
    pub fn set_masks(&mut self, include: String, exclude: String, cx: &mut Context<Self>) {
        self.options.include = include;
        self.options.exclude = exclude;
        if !self.query.read(cx).value().trim().is_empty() {
            self.run(cx);
        } else {
            cx.notify();
        }
    }

    /// Replaces one match and leaves every other result alone.
    ///
    /// This is what the "Replace" button on a result row does: IDEA's per-match
    /// replace, the counterpart of Replace All.
    pub fn replace_match(
        &mut self,
        path: PathBuf,
        line: u32,
        col: u32,
        replacement: String,
        cx: &mut Context<Self>,
    ) {
        let target =
            ReplaceTarget { path: ide_core::IpcPath(path.clone()), only: Some(vec![Pos { line, col }]), skip: None };
        let query = self.options.query(self.query.read(cx).value().trim().to_owned());
        let host = self.host.clone();
        let summary = Arc::new(std::sync::Mutex::new(None));
        let (tx, mut rx) = unbounded::<Result<(u32, u32), String>>();
        self.host.runtime.spawn(async move {
            let slot = summary.clone();
            let channel = ide_host::ops::Channel::new(move |event: ReplaceEvent| {
                if let ReplaceEvent::Done { summary } = event
                    && let Ok(mut slot) = slot.lock()
                {
                    *slot = Some(summary);
                }
            });
            let result = ide_host::ops::search::replace_in_files(
                &host,
                "main",
                RequestId::next(),
                query,
                replacement,
                false,
                vec![target],
                false,
                channel,
            )
            .await
            .map_err(|err| err.to_string())
            .map(|()| {
                summary
                    .lock()
                    .ok()
                    .and_then(|slot| slot.clone())
                    .map(|summary| (summary.replacements, summary.files))
                    .unwrap_or((0, 0))
            });
            let _ = tx.unbounded_send(result);
        });
        self.request_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok((count, files)) => {
                            this.replaced_files.insert((path.clone(), line, col));
                            this.message = Some(format!("{count} replacement(s) in {files} file(s)").into());
                        }
                        Err(err) => this.message = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    /// The preview text a match would become with the current replacement.
    pub fn replace_preview(&self, matched: &str, cx: &App) -> String {
        let replacement = self.replace.read(cx).value().to_string();
        if replacement.is_empty() {
            return matched.to_owned();
        }
        matched.to_owned().replace(matched, &replacement)
    }

    fn replace_all(&mut self, cx: &mut Context<Self>) {
        let pattern = self.query.read(cx).value().trim().to_owned();
        if pattern.is_empty() {
            self.message = Some("Enter a search pattern first".into());
            cx.notify();
            return;
        }
        if self.files.is_empty() {
            self.message = Some("Search first, then replace".into());
            cx.notify();
            return;
        }
        let replacement = self.replace.read(cx).value().to_string();
        let query = self.options.query(pattern);
        let targets: Vec<ReplaceTarget> = self
            .files
            .iter()
            .map(|file| {
                let only: Vec<Pos> = file
                    .matches
                    .iter()
                    .flat_map(|line| {
                        line.ranges.iter().map(move |(start, _)| Pos { line: line.line, col: line.text_offset + start })
                    })
                    .collect();
                ReplaceTarget { path: file.path.clone(), only: Some(only), skip: None }
            })
            .collect();
        self.message = Some("Replacing…".into());
        let host = self.host.clone();
        let summary = Arc::new(std::sync::Mutex::new(None));
        let (tx, mut rx) = unbounded::<Result<(u32, u32), String>>();
        self.host.runtime.spawn(async move {
            let slot = summary.clone();
            let channel = ide_host::ops::Channel::new(move |event: ReplaceEvent| {
                if let ReplaceEvent::Done { summary } = event
                    && let Ok(mut slot) = slot.lock()
                {
                    *slot = Some(summary);
                }
            });
            let request = RequestId::next();
            let result = ide_host::ops::search::replace_in_files(
                &host,
                "main",
                request,
                query,
                replacement,
                false,
                targets,
                false,
                channel,
            )
            .await;
            let message = match result {
                Ok(()) => {
                    let summary = summary.lock().ok().and_then(|slot| slot.clone());
                    let (replacements, files) =
                        summary.map(|summary| (summary.replacements, summary.files)).unwrap_or((0, 0));
                    Ok((replacements, files))
                }
                Err(err) => Err(err.to_string()),
            };
            let _ = tx.unbounded_send(message);
        });
        self.request_task = Some(cx.spawn(async move |this, cx| {
            while let Some(result) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok((replacements, files)) => {
                            this.message = Some(format!("{replacements} replacements in {files} files").into());
                            this.run(cx);
                            return;
                        }
                        Err(err) => this.message = Some(err.into()),
                    }
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    fn rows(&self, replacement: Option<&str>) -> Vec<SearchRow> {
        let mut rows = Vec::new();
        for file in &self.files {
            rows.push(SearchRow {
                header: true,
                path: file.path.0.clone(),
                rel: file.rel_path.clone(),
                line: 0,
                col: 0,
                preview: String::new(),
                count: file.matches.len(),
                replacement: None,
            });
            for hit in &file.matches {
                rows.push(SearchRow {
                    header: false,
                    path: file.path.0.clone(),
                    rel: file.rel_path.clone(),
                    line: hit.line,
                    col: hit.ranges.first().map(|(start, _)| *start).unwrap_or(0),
                    preview: hit.text.clone(),
                    count: 0,
                    replacement: replacement.map(|replacement| {
                        let ranges: Vec<(usize, usize)> =
                            hit.ranges.iter().map(|(start, len)| (*start as usize, *len as usize)).collect();
                        replace_in_line(&hit.text, &ranges, replacement)
                    }),
                });
            }
        }
        rows
    }
}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let replacement = {
            let value = self.replace.read(cx).value().to_string();
            (!value.is_empty()).then_some(value)
        };
        let rows = Rc::new(self.rows(replacement.as_deref()));
        let total_matches: usize = self.files.iter().map(|f| f.matches.len()).sum();
        let summary = if self.running {
            format!("Searching… {} files, {total_matches} matches", self.files.len())
        } else if self.files.is_empty() {
            self.message.clone().map(|m| m.to_string()).unwrap_or_else(|| "No matches".to_string())
        } else {
            format!(
                "{} files, {total_matches} matches{}",
                self.files.len(),
                if self.truncated { " (first 500 files)" } else { "" }
            )
        };
        let replace_all = cx.listener(|this: &mut Self, _: &ClickEvent, _window, cx| this.replace_all(cx));
        let replace_hover = theme.list_hover;
        let on_open: Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)> =
            Rc::new(cx.processor(|this: &mut Self, (path, line, col): (PathBuf, u32, u32), window, cx| {
                let editor = this.editor.clone();
                editor.update(cx, |editor, cx| editor.open_file_at(path, line, col, window, cx));
            }));
        let list_rows = rows.clone();
        let list_theme = theme.clone();
        let on_open = on_open.clone();
        let on_replace: Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)> =
            Rc::new(cx.processor(|this: &mut Self, (path, line, col): (PathBuf, u32, u32), _window, cx| {
                let replacement = this.replace.read(cx).value().to_string();
                this.replace_match(path, line, col, replacement, cx);
            }));
        let replaced = self.replaced_files.clone();

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
                    .justify_between()
                    .bg(theme.muted)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.fg_secondary())
                            .child("Find in Path"),
                    )
                    .child(div().text_size(px(10.5)).text_color(theme.fg_muted()).child(summary.clone())),
            )
            .child(
                div()
                    .p_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_w_0().child(Input::new(&self.query)))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.replace)))
                    .child(
                        div()
                            .id("search-replace-all")
                            .h(px(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .border_1()
                            .border_color(theme.border)
                            .hover(move |this| this.bg(replace_hover))
                            .on_click(replace_all)
                            .text_size(px(10.))
                            .text_color(theme.fg_secondary())
                            .child("Replace All"),
                    ),
            )
            .child(option_row(self, &theme, cx))
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
                                .child(if self.running { "Searching…" } else { "Type a pattern and press Enter" }),
                        )
                    })
                    .when(!rows.is_empty(), |this| {
                        this.child(
                            uniform_list("search-results", list_rows.len(), move |range, _window, _cx| {
                                range
                                    .map(|index| {
                                        let row = &list_rows[index];
                                        let is_replaced = replaced.contains(&(row.path.clone(), row.line, row.col));
                                        render_row(row, &list_theme, on_open.clone(), on_replace.clone(), is_replaced)
                                    })
                                    .collect()
                            })
                            .size_full(),
                        )
                    }),
            )
    }
}

/// The option row under the search fields: regex, case, whole word, context and
/// the include/exclude masks, each a chip that re-runs the search.
fn option_row(panel: &mut SearchPanel, theme: &gpui_kit::component::Theme, cx: &mut Context<SearchPanel>) -> AnyElement {
    let hover = theme.list_hover;
    let chip = |id: &'static str,
                label: &'static str,
                active: bool,
                theme: &gpui_kit::component::Theme,
                hover: Hsla|
     -> gpui_kit::Stateful<gpui_kit::Div> {
        div()
            .id(id)
            .h(px(18.))
            .px_1()
            .flex()
            .items_center()
            .rounded(px(3.))
            .cursor_pointer()
            .when(active, |this| this.bg(theme.accent.opacity(0.22)))
            .hover(move |this| this.bg(hover))
            .text_size(px(9.5))
            .text_color(if active { theme.foreground } else { theme.fg_muted() })
            .child(label)
    };
    let regex = panel.options.is_regex;
    let case = panel.options.case_sensitive;
    let word = panel.options.whole_word;
    let context = panel.options.context;
    let include = panel.options.include.clone();
    let exclude = panel.options.exclude.clone();

    div()
        .id("search-options")
        .px_2()
        .py_1()
        .flex()
        .items_center()
        .gap_1()
        .border_b_1()
        .border_color(theme.border)
        .child(
            chip("search-regex", ".*", regex, theme, hover)
                .on_click(cx.listener(|this: &mut SearchPanel, _: &ClickEvent, _window, cx| {
                    this.set_option(|options| options.is_regex = !options.is_regex, cx)
                })),
        )
        .child(
            chip("search-case", "Aa", case, theme, hover)
                .on_click(cx.listener(|this: &mut SearchPanel, _: &ClickEvent, _window, cx| {
                    this.set_option(|options| options.case_sensitive = !options.case_sensitive, cx)
                })),
        )
        .child(
            chip("search-word", "Word", word, theme, hover)
                .on_click(cx.listener(|this: &mut SearchPanel, _: &ClickEvent, _window, cx| {
                    this.set_option(|options| options.whole_word = !options.whole_word, cx)
                })),
        )
        .child(
            chip("search-context", context_label(context), context != SearchContext::Any, theme, hover)
                .on_click(cx.listener(|this: &mut SearchPanel, _: &ClickEvent, _window, cx| {
                    this.set_option(|options| options.context = next_context(options.context), cx)
                })),
        )
        .child(div().flex_1())
        .child(div().w(px(160.)).child(Input::new(&panel.include).text_size(px(10.))))
        .child(div().w(px(120.)).child(Input::new(&panel.exclude).text_size(px(10.))))
        .child(
            chip("search-masks", "Apply", false, theme, hover).on_click(cx.listener(
                |this: &mut SearchPanel, _: &ClickEvent, _window, cx| {
                    let include = this.include.read(cx).value().to_string();
                    let exclude = this.exclude.read(cx).value().to_string();
                    this.set_masks(include, exclude, cx);
                },
            )),
        )
        .into_any_element()
}

/// The label for a search context filter.
pub fn context_label(context: SearchContext) -> &'static str {
    match context {
        SearchContext::Any => "Anywhere",
        SearchContext::InComments => "In comments",
        SearchContext::InStringLiterals => "In strings",
        SearchContext::ExceptComments => "Not comments",
        SearchContext::ExceptStringLiterals => "Not strings",
        SearchContext::ExceptCommentsAndStringLiterals => "Plain code",
    }
}

/// The next context filter, in IDEA's order.
pub fn next_context(context: SearchContext) -> SearchContext {
    match context {
        SearchContext::Any => SearchContext::InComments,
        SearchContext::InComments => SearchContext::InStringLiterals,
        SearchContext::InStringLiterals => SearchContext::ExceptComments,
        SearchContext::ExceptComments => SearchContext::ExceptStringLiterals,
        SearchContext::ExceptStringLiterals => SearchContext::ExceptCommentsAndStringLiterals,
        SearchContext::ExceptCommentsAndStringLiterals => SearchContext::Any,
    }
}

fn render_row(
    row: &SearchRow,
    theme: &gpui_kit::component::Theme,
    on_open: Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)>,
    on_replace: Rc<dyn Fn((PathBuf, u32, u32), &mut Window, &mut App)>,
    replaced: bool,
) -> AnyElement {
    if row.header {
        return div()
            .h(px(22.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(11.5))
            .text_color(theme.fg_secondary())
            .child(Icon::new(IconName::FileCode).xsmall().text_color(theme.info))
            .child(div().truncate().child(row.rel.clone()))
            .child(div().text_size(px(10.5)).text_color(theme.fg_muted()).child(format!("{}", row.count)))
            .into_any_element();
    }
    let path = row.path.clone();
    let line = row.line;
    let col = row.col;
    let preview = row.preview.clone();
    let replace_path = row.path.clone();
    let hover = theme.list_hover;
    div()
        .id(ElementId::Name(format!("search-hit-{}-{}", path.display(), line).into()))
        .h(px(22.))
        .pl(px(24.))
        .pr_2()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .hover(move |this| this.bg(hover))
        .on_click(move |_: &ClickEvent, window, cx| on_open((path.clone(), line, col), window, cx))
        .child(div().w(px(36.)).text_size(px(10.5)).text_color(theme.fg_muted()).child(format!("{}", line + 1)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .text_color(if replaced { theme.fg_muted() } else { theme.foreground })
                .child(preview),
        )
        .when_some(row.replacement.clone(), |this, replacement| {
            this.child(
                div()
                    .max_w(px(220.))
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(theme.success)
                    .child(replacement),
            )
        })
        .when(replaced, |this| {
            this.child(div().text_size(px(9.5)).text_color(theme.success).child("replaced"))
        })
        .when(!replaced, |this| {
            this.child(
                div()
                    .id(ElementId::Name(format!("search-replace-{}-{}", replace_path.display(), line).into()))
                    .h(px(18.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .border_1()
                    .border_color(theme.border)
                    .hover(move |this| this.bg(hover))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        on_replace((replace_path.clone(), line, col), window, cx)
                    })
                    .text_size(px(9.5))
                    .text_color(theme.fg_secondary())
                    .child("Replace"),
            )
        })
        .into_any_element()
}
