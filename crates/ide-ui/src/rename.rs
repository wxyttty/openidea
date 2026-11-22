//! Rename (`Shift+F6`): the name dialog, the impact summary, and applying the
//! server's edit.
//!
//! The flow matches IDEA's: `Shift+F6` asks the language server whether the
//! symbol can be renamed here (`textDocument/prepareRename`), opens an input
//! prefilled with the current name, computes the edit for the typed name
//! (`textDocument/rename`), shows which files it touches, and applies it on
//! confirmation. Applying goes through `ide_host::ops::lsp`'s edit path, so open
//! editors repaint from the usual `DocPatched` events rather than from a local
//! guess.
//!
//! A server that answers nothing (no `renameProvider`, or a position that is
//! not a symbol) leaves the dialog with a notice instead of silently doing
//! nothing.

use crate::theme::IdeColors as _;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use ide_core::DocId;
use ide_core::text::FileEdit;
use ide_lsp::types::Renamed;
use ide_core::text::WorkspaceEdit;
use std::collections::BTreeMap;

/// What the dialog is doing: asking, ready to rename, or reporting a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameStage {
    /// Waiting for `prepareRename`.
    Checking,
    /// The symbol can be renamed; the input takes the new name.
    Ready,
    /// `prepareRename` answered nothing (not a symbol, or unsupported).
    Unavailable,
    /// Computing the edit.
    Resolving,
    /// The edit is ready: the summary lists the files it rewrites.
    Confirming,
}

/// The rename dialog's state, owned by the workbench.
pub struct RenameDialog {
    pub stage: RenameStage,
    pub input: Entity<InputState>,
    /// The symbol the request is about.
    pub doc: Option<(DocId, ide_core::Pos)>,
    /// The name the symbol has now (the input's starting value).
    pub current_name: String,
    /// Files the pending edit rewrites, with their edit counts, for the summary.
    pub impacts: Vec<(SharedString, usize)>,
    /// `Some` when the server says it did not see every usage.
    pub incomplete: Option<SharedString>,
    /// A failure to show under the input.
    pub error: Option<SharedString>,
    /// The edit to apply on Enter.
    pending: Option<Renamed>,
    /// The host the pending edit applies through.
    host: Option<std::sync::Arc<ide_host::Host>>,
    focus_handle: FocusHandle,
    _submit: Subscription,
}

impl RenameDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("New name"));
        let submit = cx.subscribe(&input, |this: &mut Self, _input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit(cx);
            }
        });
        Self {
            stage: RenameStage::Checking,
            input,
            doc: None,
            current_name: String::new(),
            impacts: Vec::new(),
            incomplete: None,
            error: None,
            pending: None,
            host: None,
            focus_handle: cx.focus_handle(),
            _submit: submit,
        }
    }

    /// `Shift+F6` at the caret: starts the check and opens the dialog.
    pub fn begin(
        &mut self,
        host: std::sync::Arc<ide_host::Host>,
        doc: DocId,
        pos: ide_core::Pos,
        current_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stage = RenameStage::Checking;
        self.host = Some(host.clone());
        self.doc = Some((doc, pos));
        self.current_name = current_name.clone();
        self.impacts.clear();
        self.incomplete = None;
        self.error = None;
        self.pending = None;
        self.input.update(cx, |input, cx| input.set_value(current_name.clone(), window, cx));
        let focus = self.focus_handle.clone();
        window.focus(&focus, cx);
        let input_focus = self.input.read(cx).focus_handle(cx);
        window.focus(&input_focus, cx);
        let runtime = host.runtime.clone();
        cx.spawn(async move |this, cx| {
            let allowed = runtime
                .spawn(async move { ide_host::ops::lsp::lsp_prepare_rename(&host, "main", doc, pos).await })
                .await;
            this.update(cx, |dialog, cx| {
                match allowed {
                    Ok(Ok(Some(_range))) => dialog.stage = RenameStage::Ready,
                    Ok(Ok(None)) => {
                        dialog.stage = RenameStage::Unavailable;
                        dialog.error = Some("This symbol cannot be renamed here".into());
                    }
                    Ok(Err(err)) => {
                        dialog.stage = RenameStage::Unavailable;
                        dialog.error = Some(format!("Rename failed: {err}").into());
                    }
                    Err(err) => {
                        dialog.stage = RenameStage::Unavailable;
                        dialog.error = Some(format!("Rename task failed: {err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Enter: compute the edit for the typed name (first press) or apply it
    /// (second press, once the impact is shown).
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        match self.stage {
            RenameStage::Ready => self.resolve(cx),
            RenameStage::Confirming => self.apply(cx),
            _ => {}
        }
    }

    fn resolve(&mut self, cx: &mut Context<Self>) {
        let Some((doc, pos)) = self.doc else { return };
        let name = self.input.read(cx).value().trim().to_owned();
        if name.is_empty() || name == self.current_name {
            self.error = Some("Enter a different name".into());
            cx.notify();
            return;
        }
        self.stage = RenameStage::Resolving;
        self.error = None;
        let Some(host) = self.host.clone() else { return };
        let runtime = host.runtime.clone();
        cx.spawn(async move |this, cx| {
            let renamed = runtime
                .spawn(async move {
                    ide_host::ops::lsp::lsp_rename(
                        &host,
                        "main",
                        ide_core::RequestId::next(),
                        doc,
                        pos,
                        name,
                    )
                    .await
                })
                .await;
            this.update(cx, |dialog, cx| {
                match renamed {
                    Ok(Ok(renamed)) => {
                        dialog.impacts = summarize(&renamed.edit);
                        dialog.incomplete = renamed.incomplete.map(|reason| format!("{reason:?}").into());
                        dialog.pending = Some(renamed);
                        dialog.stage = RenameStage::Confirming;
                    }
                    Ok(Err(err)) => {
                        dialog.stage = RenameStage::Ready;
                        dialog.error = Some(format!("Rename failed: {err}").into());
                    }
                    Err(err) => {
                        dialog.stage = RenameStage::Ready;
                        dialog.error = Some(format!("Rename task failed: {err}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(renamed) = self.pending.take() else { return };
        let Some(host) = self.host.clone() else { return };
        let runtime = host.runtime.clone();
        cx.spawn(async move |this, cx| {
            let result = runtime
                .spawn(async move {
                    // Applied through the shared `Documents`, so open buffers
                    // update (and repaint from `DocPatched`) before the server's
                    // next response.
                    ide_host::ops::lsp::apply_workspace_edit(&host, "main", renamed.edit).await
                })
                .await;
            this.update(cx, |dialog, cx| {
                match result {
                    Ok(Ok(())) => {
                        dialog.stage = RenameStage::Ready;
                        dialog.error = None;
                        dialog.impacts.clear();
                        cx.emit(RenameDone);
                    }
                    Ok(Err(err)) => dialog.error = Some(format!("Apply failed: {err}").into()),
                    Err(err) => dialog.error = Some(format!("Apply task failed: {err}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.doc = None;
        self.pending = None;
        self.stage = RenameStage::Ready;
        cx.notify();
    }

    /// Whether the dialog should be drawn.
    pub fn is_open(&self) -> bool {
        self.doc.is_some()
    }

    pub fn input_focus(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

/// Emitted once the edit is applied, so the workbench can refresh and close.
pub struct RenameDone;

impl EventEmitter<RenameDone> for RenameDialog {}

impl Focusable for RenameDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RenameDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let hint = match self.stage {
            RenameStage::Checking => "Checking the symbol…",
            RenameStage::Ready => "Enter to compute the rename",
            RenameStage::Unavailable => "Rename is unavailable here",
            RenameStage::Resolving => "Computing the edit…",
            RenameStage::Confirming => "Enter to apply",
        };
        div()
            .id("rename-dialog")
            .w(px(520.))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border_strong())
            .rounded(px(8.))
            .shadow_xl()
            .occlude()
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.foreground)
                    .child(format!("Rename \"{}\"", self.current_name)),
            )
            .child(Input::new(&self.input).id("rename-input"))
            .child(div().text_size(px(10.)).text_color(theme.fg_muted()).child(hint))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .p_2()
                        .rounded(px(3.))
                        .bg(theme.danger.opacity(0.15))
                        .text_size(px(10.5))
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .when(!self.impacts.is_empty(), |this| {
                let theme = theme.clone();
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_1()
                        .py_1()
                        .bg(theme.muted)
                        .rounded(px(4.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(theme.fg_secondary())
                                .child(format!("Changes {} file(s)", self.impacts.len())),
                        )
                        .children(self.impacts.iter().map(|(path, count)| {
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(10.5))
                                .child(div().flex_1().min_w_0().truncate().text_color(theme.foreground).child(path.clone()))
                                .child(div().text_color(theme.fg_muted()).child(format!("{count} edit(s)")))
                        })),
                )
            })
            .when_some(self.incomplete.clone(), |this, note| {
                this.child(
                    div()
                        .text_size(px(10.))
                        .text_color(theme.warning)
                        .child(format!("The server may not have found every usage ({note})")),
                )
            })
    }
}

/// Files the edit rewrites with their edit counts, for the confirmation summary.
fn summarize(edit: &WorkspaceEdit) -> Vec<(SharedString, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for file in &edit.files {
        match file {
            FileEdit::Edit { path, edits, .. } => {
                *counts.entry(path.0.display().to_string()).or_default() += edits.len();
            }
            FileEdit::Create { path, .. } => {
                *counts.entry(path.0.display().to_string()).or_default() += 1;
            }
            FileEdit::Rename { from, to } => {
                *counts.entry(format!("{} → {}", from.0.display(), to.0.display())).or_default() += 1;
            }
            FileEdit::Delete { path, .. } => {
                *counts.entry(path.0.display().to_string()).or_default() += 1;
            }
        }
    }
    counts.into_iter().map(|(path, count)| (path.into(), count)).collect()
}

/// The summary of an edit, for tests and the dialog.
pub fn summarize_edit(edit: &WorkspaceEdit) -> Vec<(SharedString, usize)> {
    summarize(edit)
}

/// The dialog's placement: centred in the window, as IDEA's rename popup.
pub fn dialog_origin(window: Size<Pixels>) -> Point<Pixels> {
    let w = px(520.);
    point(((window.width - w) / 2.).max(px(16.)), px(140.))
}

/// The colour a stage paints the dialog border with.
pub fn stage_color(stage: RenameStage, theme: &Theme) -> Hsla {
    match stage {
        RenameStage::Unavailable => theme.danger,
        RenameStage::Confirming => theme.warning,
        _ => theme.border_strong(),
    }
}

#[cfg(test)]
mod tests {
    use super::{RenameStage, dialog_origin, summarize_edit};
    use gpui_kit::{px, size};
    use ide_core::text::{FileEdit, TextEdit, WorkspaceEdit};
    use ide_core::{IpcPath, Range};

    fn edit(path: &str, count: usize) -> FileEdit {
        FileEdit::Edit {
            path: IpcPath(std::path::PathBuf::from(path)),
            edits: (0..count)
                .map(|_| TextEdit {
                    range: Range {
                        start: ide_core::Pos { line: 0, col: 0 },
                        end: ide_core::Pos { line: 0, col: 0 },
                    },
                    new_text: "x".into(),
                })
                    .collect(),
            version: None,
        }
    }

    #[test]
    fn summary_counts_edits_per_file() {
        let edit = WorkspaceEdit {
            files: vec![edit("src/a.rs", 2), edit("src/b.rs", 1), edit("src/a.rs", 1)],
            preconditions: Vec::new(),
        };
        let summary = summarize_edit(&edit);
        let a = summary.iter().find(|(path, _)| path.as_ref() == "src/a.rs").expect("a.rs listed");
        assert_eq!(a.1, 3, "edits of the same file add up");
        assert_eq!(summary.len(), 2, "one row per file");
    }

    #[test]
    fn summary_reports_resource_operations() {
        let edit = WorkspaceEdit {
            files: vec![
                FileEdit::Rename {
                    from: IpcPath(std::path::PathBuf::from("old.rs")),
                    to: IpcPath(std::path::PathBuf::from("new.rs")),
                },
                FileEdit::Delete { path: IpcPath(std::path::PathBuf::from("gone.rs")), recursive: false },
            ],
            preconditions: Vec::new(),
        };
        let summary = summarize_edit(&edit);
        assert_eq!(summary.len(), 2);
        assert!(summary.iter().any(|(path, _)| path.contains("old.rs → new.rs")));
    }

    #[test]
    fn dialog_is_horizontally_centred() {
        let origin = dialog_origin(size(px(1400.), px(900.)));
        assert_eq!(origin.x, px(440.));
        // A narrow window clamps to its margin instead of going negative.
        let narrow = dialog_origin(size(px(400.), px(600.)));
        assert_eq!(narrow.x, px(16.));
    }

}
