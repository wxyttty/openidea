//! Language-server features for the editor element.
//!
//! `gpui-component`'s editor asks its `Lsp` providers for completions, hovers
//! and definitions. This module answers them from `ide-lsp` through the open
//! document's own buffer, so the server sees exactly the text the user edits.
//! Diagnostics take the other path (`IdeEvent::Diagnostics` → Problems panel
//! and gutter, see [`crate::problems`]).

use anyhow::Result;
use gpui_kit::base::input::{
    CompletionProvider, DefinitionProvider, HoverProvider, Rope, RopeExt as _, ShowDocumentHandler,
};
use gpui_kit::*;
use ide_core::{CancellationToken, DocId, Pos};
use ide_host::Project;
use ide_lsp::types::{CompletionKind, GotoKind, Markup};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionItemLabelDetails, CompletionList, CompletionResponse, Documentation,
    Hover, HoverContents, InsertTextFormat, LocationLink, MarkupContent, MarkupKind, Position, Range,
    ShowDocumentParams, TextEdit, Uri,
};
use std::rc::Rc;
use std::sync::Arc;

pub struct IdeLspProvider {
    project: Arc<Project>,
    doc: DocId,
}

impl IdeLspProvider {
    pub fn new(project: Arc<Project>, doc: DocId) -> Self {
        Self { project, doc }
    }

    /// Install the providers and the go-to-definition navigation hook on an
    /// editor state. `editor` opens the target file when the user follows a
    /// definition.
    pub fn install(
        self: &Rc<Self>,
        editor: Entity<crate::editor::EditorArea>,
        state: &Entity<gpui_kit::component::input::EditorState>,
        cx: &mut App,
    ) {
        let provider = self.clone();
        let show_document: ShowDocumentHandler = Rc::new(move |params: &ShowDocumentParams, window, cx| {
            let Some(path) = uri_to_path(&params.uri) else { return false };
            let (line, col) = params.selection.map(|range| (range.start.line, range.start.character)).unwrap_or((0, 0));
            editor.update(cx, |editor, cx| editor.open_file_at(path, line, col, window, cx));
            true
        });
        state.update(cx, |state, cx| {
            let lsp = state.lsp_mut();
            lsp.completion_provider = Some(provider.clone());
            lsp.hover_provider = Some(provider.clone());
            lsp.definition_provider = Some(provider);
            lsp.show_document = Some(show_document);
            cx.notify();
        });
    }
}

fn path_to_uri(path: &std::path::Path) -> Option<Uri> {
    let url = url::Url::from_file_path(path).ok()?;
    url.as_str().parse().ok()
}

fn uri_to_path(uri: &Uri) -> Option<std::path::PathBuf> {
    url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()
}

fn position_of(text: &Rope, offset: usize) -> Pos {
    let position = text.offset_to_position(offset);
    Pos::new(position.line, position.character)
}

fn range_to_lsp(range: ide_core::Range) -> Range {
    Range::new(Position::new(range.start.line, range.start.col), Position::new(range.end.line, range.end.col))
}

impl CompletionProvider for IdeLspProvider {
    /// Pop the menu after identifier characters and member-access punctuation,
    /// matching IDEA's default autopopup.
    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        new_text.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '$' | '#'))
    }

    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: lsp_types::CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let lsp = self.project.lsp.clone();
        let doc = self.doc;
        let pos = position_of(text, offset);
        cx.spawn(async move |_cx| {
            let cancel = CancellationToken::new();
            match lsp.completion(doc, pos, None, cancel).await {
                Ok(list) => {
                    tracing::debug!(items = list.items.len(), ?pos, "lsp completions");
                    Ok(CompletionResponse::List(CompletionList {
                        is_incomplete: list.is_incomplete,
                        items: list.items.into_iter().map(completion_item).collect(),
                    }))
                }
                Err(err) => Err(anyhow::anyhow!(err.to_string())),
            }
        })
    }
}

impl HoverProvider for IdeLspProvider {
    fn hover(&self, text: &Rope, offset: usize, _window: &mut Window, cx: &mut App) -> Task<Result<Option<Hover>>> {
        let lsp = self.project.lsp.clone();
        let doc = self.doc;
        let pos = position_of(text, offset);
        cx.spawn(async move |_cx| {
            let cancel = CancellationToken::new();
            match lsp.hover(doc, pos, cancel).await {
                Ok(Some(hover)) => {
                    tracing::debug!(?pos, "lsp hover");
                    Ok(Some(Hover {
                        contents: HoverContents::Markup(markup_content(hover.contents)),
                        range: hover.range.map(range_to_lsp),
                    }))
                }
                Ok(None) => Ok(None),
                Err(err) => Err(anyhow::anyhow!(err.to_string())),
            }
        })
    }
}

impl DefinitionProvider for IdeLspProvider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let lsp = self.project.lsp.clone();
        let doc = self.doc;
        let pos = position_of(text, offset);
        cx.spawn(async move |_cx| {
            let locations = lsp.goto(doc, pos, GotoKind::Definition).await.unwrap_or_default();
            tracing::debug!(count = locations.len(), ?pos, "lsp goto");
            Ok(locations
                .into_iter()
                .filter_map(|location| {
                    let target_uri = path_to_uri(&location.path.0)?;
                    let range = range_to_lsp(location.range);
                    Some(LocationLink {
                        target_uri,
                        target_range: range,
                        target_selection_range: range,
                        origin_selection_range: None,
                    })
                })
                .collect())
        })
    }
}

fn markup_content(markup: Markup) -> MarkupContent {
    MarkupContent {
        kind: if markup.kind.eq_ignore_ascii_case("markdown") { MarkupKind::Markdown } else { MarkupKind::PlainText },
        value: markup.value,
    }
}

fn completion_item(item: ide_lsp::types::CompletionItem) -> CompletionItem {
    let edit =
        item.replace_range.map(|range| TextEdit { range: range_to_lsp(range), new_text: item.insert_text.clone() });
    CompletionItem {
        label: item.label,
        label_details: item
            .label_detail
            .map(|description| CompletionItemLabelDetails { detail: None, description: Some(description) }),
        kind: item.kind.map(completion_kind),
        detail: item.detail,
        documentation: item.documentation.map(|markup| Documentation::MarkupContent(markup_content(markup))),
        deprecated: item.deprecated.then_some(true),
        preselect: item.preselect.then_some(true),
        sort_text: item.sort_text,
        filter_text: item.filter_text,
        insert_text: Some(item.insert_text),
        insert_text_format: item.is_snippet.then_some(InsertTextFormat::SNIPPET),
        text_edit: edit.map(Into::into),
        ..Default::default()
    }
}

fn completion_kind(kind: CompletionKind) -> CompletionItemKind {
    match kind {
        CompletionKind::Text => CompletionItemKind::TEXT,
        CompletionKind::Method => CompletionItemKind::METHOD,
        CompletionKind::Function => CompletionItemKind::FUNCTION,
        CompletionKind::Constructor => CompletionItemKind::CONSTRUCTOR,
        CompletionKind::Field => CompletionItemKind::FIELD,
        CompletionKind::Variable => CompletionItemKind::VARIABLE,
        CompletionKind::Class => CompletionItemKind::CLASS,
        CompletionKind::Interface => CompletionItemKind::INTERFACE,
        CompletionKind::Module => CompletionItemKind::MODULE,
        CompletionKind::Property => CompletionItemKind::PROPERTY,
        CompletionKind::Unit => CompletionItemKind::UNIT,
        CompletionKind::Value => CompletionItemKind::VALUE,
        CompletionKind::Enum => CompletionItemKind::ENUM,
        CompletionKind::Keyword => CompletionItemKind::KEYWORD,
        CompletionKind::Snippet => CompletionItemKind::SNIPPET,
        CompletionKind::Color => CompletionItemKind::COLOR,
        CompletionKind::File => CompletionItemKind::FILE,
        CompletionKind::Reference => CompletionItemKind::REFERENCE,
        CompletionKind::Folder => CompletionItemKind::FOLDER,
        CompletionKind::EnumMember => CompletionItemKind::ENUM_MEMBER,
        CompletionKind::Constant => CompletionItemKind::CONSTANT,
        CompletionKind::Struct => CompletionItemKind::STRUCT,
        CompletionKind::Event => CompletionItemKind::EVENT,
        CompletionKind::Operator => CompletionItemKind::OPERATOR,
        CompletionKind::TypeParameter => CompletionItemKind::TYPE_PARAMETER,
    }
}
