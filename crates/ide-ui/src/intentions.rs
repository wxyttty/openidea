//! Code insight: context actions ("intentions") at the caret.
//!
//! `Alt+Enter` asks every language server for code actions over the caret's
//! range and lists them in a popup: quick fixes, refactorings and source
//! actions. Picking one resolves it (some servers fill in the edit lazily) and
//! applies it through `ide-host`'s `lsp_apply_code_action`, which patches the
//! shared document — the open editor updates through the usual `DocPatched`
//! event.
//!
//! The list keeps the LSP kinds visible so a "Refactor" section can be told
//! from a "Quick Fix" one, and shows servers' disabled reasons instead of
//! hiding an entry the server offered but refuses right now.

use crate::editor::EditorArea;
use crate::theme::IdeColors as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::StatefulInteractiveElement as _;
use gpui_kit::*;
use ide_core::{DocId, Pos, Range};
use ide_host::Host;
use ide_lsp::types::CodeAction;
use std::sync::Arc;

/// One entry of the intentions popup.
#[derive(Clone)]
pub struct Intention {
    pub title: SharedString,
    /// `quickfix`, `refactor.extract`, … (`None` when a server does not say).
    pub kind: Option<SharedString>,
    /// Why the server will not run it here, if it said so.
    pub disabled: Option<SharedString>,
    pub preferred: bool,
    action: CodeAction,
}

impl Intention {
    /// Wrap one server action for the popup.
    pub fn from_action(action: CodeAction) -> Self {
        Self {
            title: action.title.clone().into(),
            kind: action.kind.clone().map(Into::into),
            disabled: action.disabled_reason.clone().map(Into::into),
            preferred: action.is_preferred,
            action,
        }
    }

    /// The section heading: the first segment of the LSP kind, title-cased
    /// (`quickfix` → "Quick Fix", `refactor.extract` → "Refactor").
    pub fn section(&self) -> &'static str {
        match self.kind.as_deref().and_then(|kind| kind.split('.').next()) {
            Some("quickfix") => "Quick Fixes",
            Some("refactor") => "Refactorings",
            Some("source") => "Source Actions",
            _ => "Actions",
        }
    }
}

/// The intentions popup for one request: the actions found at the caret.
pub struct IntentionsPopup {
    intentions: Vec<Intention>,
    /// Filtered indices into `intentions`, for keyboard selection.
    matches: Vec<usize>,
    selected: usize,
    query: String,
    /// Shown under the list when the servers reported nothing usable.
    notice: Option<SharedString>,
    focus_handle: FocusHandle,
}

impl IntentionsPopup {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            intentions: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            query: String::new(),
            notice: None,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.intentions.is_empty()
    }

    pub fn len(&self) -> usize {
        self.intentions.len()
    }

    /// Replaces the list. The preferred action (usually the fix a server wants
    /// to offer first) starts selected, as IDEA preselects it.
    pub fn set_intentions(&mut self, mut intentions: Vec<Intention>, notice: Option<SharedString>) {
        // Preferred first, then by the order the servers answered.
        intentions.sort_by_key(|intention| !intention.preferred);
        self.selected = intentions.iter().position(|intention| intention.preferred).unwrap_or(0);
        self.intentions = intentions;
        self.notice = notice;
        self.matches = (0..self.intentions.len()).collect();
        self.query.clear();
    }

    pub fn intention(&self, index: usize) -> Option<&Intention> {
        self.intentions.get(index)
    }

    /// The entry the keyboard selection points at.
    pub fn selected_intention(&self) -> Option<&Intention> {
        self.matches.get(self.selected).and_then(|index| self.intentions.get(*index))
    }

    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1) % self.matches.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + self.matches.len() - 1) % self.matches.len();
        }
    }

    /// Narrows the list to entries whose title contains `query`
    /// (case-insensitive).
    pub fn filter(&mut self, query: &str) {
        self.query = query.to_owned();
        let needle = query.to_lowercase();
        self.matches = self
            .intentions
            .iter()
            .enumerate()
            .filter(|(_, intention)| needle.is_empty() || intention.title.to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
    }
}

impl Focusable for IntentionsPopup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for IntentionsPopup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let row_theme = theme.clone();
        let rows = self.matches.iter().filter_map(|index| self.intentions.get(*index)).cloned().collect::<Vec<_>>();
        let selected = self.selected;
        let hover = theme.list_hover;
        let entity = cx.entity();
        let pick = move |index: usize, _: &ClickEvent, _: &mut Window, cx: &mut App| {
            entity.update(cx, |popup, cx| {
                popup.selected = index;
                cx.emit(PickIntention);
                cx.notify();
            });
        };

        div()
            .id("intentions-popup")
            .w(px(480.))
            .max_h(px(360.))
            .flex()
            .flex_col()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border_strong())
            .rounded(px(6.))
            .shadow_xl()
            .occlude()
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
                    .child(Icon::new(IconName::Info).xsmall().text_color(theme.info))
                    .child(div().flex_1().text_size(px(11.)).text_color(theme.fg_secondary()).child(
                        match (&self.query, self.matches.len()) {
                            (query, _) if !query.is_empty() => format!("Filter: {query}"),
                            (_, count) => format!("{count} actions"),
                        },
                    ))
                    .child(div().text_size(px(9.5)).text_color(theme.fg_muted()).child("Enter · Esc")),
            )
            .child(
                div()
                    .id("intentions-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_1()
                    .flex()
                    .flex_col()
                    .gap_0()
                    .when(rows.is_empty(), |this| {
                        this.child(
                            div()
                                .p_3()
                                .text_size(px(11.5))
                                .text_color(theme.fg_muted())
                                .child(self.notice.clone().unwrap_or_else(|| "No actions here".into())),
                        )
                    })
                    .children(rows.into_iter().enumerate().map(|(index, intention)| {
                        let kind = intention.kind.clone();
                        let disabled = intention.disabled.clone();
                        let preferred = intention.preferred;
                        let is_selected = index == selected;
                        let click = pick.clone();
                        div()
                            .id(ElementId::Name(format!("intention-{index}").into()))
                            .h(px(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded(px(3.))
                            .when(is_selected, |this| this.bg(row_theme.secondary))
                            .hover(move |this| this.bg(hover))
                            .cursor_pointer()
                            .on_click(move |event, window, cx| click(index, event, window, cx))
                            .child(
                                div()
                                    .w(px(60.))
                                    .text_size(px(9.5))
                                    .text_color(row_theme.fg_muted())
                                    .child(intention.section()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(if disabled.is_some() {
                                        row_theme.fg_muted()
                                    } else {
                                        row_theme.foreground
                                    })
                                    .child(intention.title.clone()),
                            )
                            .when(preferred, |this| {
                                this.child(
                                    div()
                                        .px_1()
                                        .rounded(px(2.))
                                        .bg(row_theme.accent.opacity(0.2))
                                        .text_size(px(9.))
                                        .text_color(row_theme.accent)
                                        .child("preferred"),
                                )
                            })
                            .when_some(disabled, |this, reason| {
                                this.child(
                                    div()
                                        .max_w(px(160.))
                                        .truncate()
                                        .text_size(px(9.5))
                                        .text_color(row_theme.warning)
                                        .child(reason),
                                )
                            })
                            .when_some(kind, |this, kind| {
                                this.child(
                                    div()
                                        .max_w(px(120.))
                                        .truncate()
                                        .text_size(px(9.))
                                        .text_color(row_theme.fg_muted())
                                        .child(kind),
                                )
                            })
                    })),
            )
    }
}

/// Emitted when the keyboard or a click picked an entry.
pub struct PickIntention;

impl EventEmitter<PickIntention> for IntentionsPopup {}

impl IntentionSection {
    /// Asks the language servers for the actions at `pos` and fills the popup.
    ///
    /// Runs on the host runtime: the LSP round trip must not block the UI. An
    /// empty answer keeps the popup open with a notice, so `Alt+Enter` never
    /// looks broken.
    pub fn request(&mut self, host: Arc<Host>, doc: DocId, pos: Pos, range: Range, cx: &mut Context<Self>) {
        self.open = true;
        self.notice = None;
        self.doc = Some((doc, pos));
        self.popup.update(cx, |popup, cx| {
            popup.set_intentions(Vec::new(), Some("Asking the language server…".into()));
            cx.notify();
        });
        let runtime = host.runtime.clone();
        let host_for_task = host.clone();
        cx.spawn(async move |this, cx| {
            let answer = runtime
                .spawn(async move {
                    ide_host::ops::lsp::lsp_code_action_query(
                        &host_for_task,
                        "main",
                        ide_core::RequestId::next(),
                        doc,
                        range,
                        Vec::new(),
                    )
                    .await
                })
                .await;
            let (intentions, notice) = match answer {
                Ok(Ok(query)) => {
                    let intentions = query.actions.into_iter().map(intention_of).collect::<Vec<_>>();
                    let notice = intentions
                        .is_empty()
                        .then(|| match query.advertised_kinds {
                            Some(kinds) if kinds.is_empty() => {
                                SharedString::from("The language server offers no code actions")
                            }
                            _ => SharedString::from("Nothing to fix or refactor here"),
                        });
                    (intentions, notice)
                }
                Ok(Err(err)) => (Vec::new(), Some(SharedString::from(format!("Code actions failed: {err}")))),
                Err(err) => (Vec::new(), Some(SharedString::from(format!("Code action task failed: {err}")))),
            };
            this.update(cx, |section, cx| {
                section.popup.update(cx, |popup, cx| {
                    popup.set_intentions(intentions, notice);
                    cx.notify();
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn intention_of(action: CodeAction) -> Intention {
    Intention::from_action(action)
}

/// The popup plus its open state: the workbench renders it above the editor and
/// keeps one per window, so a second `Alt+Enter` replaces the first list.
pub struct IntentionSection {
    pub popup: Entity<IntentionsPopup>,
    pub open: bool,
    pub notice: Option<SharedString>,
    /// The position the actions were requested at, applied on pick.
    pub doc: Option<(DocId, Pos)>,
    popup_focus: FocusHandle,
    _pick: Subscription,
}

impl IntentionSection {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, editor: Entity<EditorArea>, host: Arc<Host>) -> Self {
        let popup = cx.new(IntentionsPopup::new);
        let popup_focus = popup.read(cx).focus_handle.clone();
        let pick = cx.subscribe(&popup, move |this: &mut Self, _popup, _: &PickIntention, cx| {
            this.apply_selected(editor.clone(), host.clone(), cx);
        });
        let _ = window;
        Self {
            popup,
            open: false,
            notice: None,
            doc: None,
            popup_focus,
            _pick: pick,
        }
    }

    /// Applies the selected action through the host and closes the popup.
    fn apply_selected(&mut self, editor: Entity<EditorArea>, host: Arc<Host>, cx: &mut Context<Self>) {
        let Some((doc, _)) = self.doc else { return };
        let Some(intention) = self.popup.read(cx).selected_intention().cloned() else { return };
        if intention.disabled.is_some() {
            self.notice = intention.disabled.clone();
            cx.notify();
            return;
        }
        self.open = false;
        let runtime = host.runtime.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn(async move {
                    ide_host::ops::lsp::lsp_apply_code_action(&host, "main", doc, intention.action).await
                })
                .await
                .unwrap_or_else(|err| Err(ide_core::IdeError::internal(format!("code action task: {err}"))));
            this.update(cx, |section, cx| {
                match result {
                    Ok(()) => {
                        section.notice = None;
                    }
                    Err(err) => section.notice = Some(format!("Code action failed: {err}").into()),
                }
                let _ = &editor;
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.doc = None;
        cx.notify();
    }

    /// Handles a key while the popup is open; `true` means it was consumed.
    pub fn on_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        if !self.open {
            return false;
        }
        match key {
            "up" => self.popup.update(cx, |popup, cx| {
                popup.select_prev();
                cx.notify();
            }),
            "down" => self.popup.update(cx, |popup, cx| {
                popup.select_next();
                cx.notify();
            }),
            "escape" => {
                self.close(cx);
                return true;
            }
            "enter" => {
                cx.emit(PickIntention);
                return true;
            }
            _ => return false,
        }
        true
    }

    pub fn popup_focus(&self, _cx: &App) -> FocusHandle {
        self.popup_focus.clone()
    }
}

impl Render for IntentionSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .when(self.open, |this| this.child(self.popup.clone()))
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .bg(theme.warning.opacity(0.15))
                        .text_size(px(10.5))
                        .text_color(theme.warning)
                        .child(notice),
                )
            })
    }
}

impl EventEmitter<PickIntention> for IntentionSection {}

/// The popup's placement: just under the caret's line, clamped to the window.
pub fn popup_origin(caret: Point<Pixels>, window: Size<Pixels>) -> Point<Pixels> {
    let width = px(480.);
    let height = px(360.);
    let x = caret.x.min((window.width - width - px(8.)).max(px(8.))).max(px(8.));
    let y = if caret.y + px(24.) + height > window.height {
        (caret.y - height - px(8.)).max(px(8.))
    } else {
        caret.y + px(24.)
    };
    point(x, y)
}
