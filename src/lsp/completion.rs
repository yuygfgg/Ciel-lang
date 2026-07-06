use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, Documentation, Position, TextEdit,
};

use super::{
    completion_facts::{CompletionCandidate, CompletionKind},
    server::DocumentFacts,
};
use lsp_types::Url;

impl DocumentFacts {
    pub(super) fn completion(&self, uri: &Url, position: Position) -> Option<CompletionResponse> {
        let (file_id, offset) = self.offset_for_position(uri, position)?;
        let prefix_range = self.completion_prefix_range(file_id, offset);
        let items = self
            .completion_facts
            .complete(&self.source_map, file_id, offset)
            .into_iter()
            .map(|candidate| completion_item(candidate, prefix_range))
            .collect::<Vec<_>>();
        Some(CompletionResponse::Array(items))
    }
}

fn completion_item(
    candidate: CompletionCandidate,
    prefix_range: Option<lsp_types::Range>,
) -> CompletionItem {
    CompletionItem {
        label: candidate.label.clone(),
        kind: Some(completion_item_kind(&candidate.kind)),
        detail: candidate.detail.clone(),
        documentation: candidate
            .detail
            .clone()
            .map(|detail| Documentation::String(detail)),
        text_edit: prefix_range.map(|range| {
            lsp_types::CompletionTextEdit::Edit(TextEdit {
                range,
                new_text: candidate.label,
            })
        }),
        ..CompletionItem::default()
    }
}

fn completion_item_kind(kind: &CompletionKind) -> CompletionItemKind {
    match kind {
        CompletionKind::Function => CompletionItemKind::FUNCTION,
        CompletionKind::Variable => CompletionItemKind::VARIABLE,
        CompletionKind::Parameter => CompletionItemKind::VARIABLE,
        CompletionKind::Field => CompletionItemKind::FIELD,
        CompletionKind::EnumVariant => CompletionItemKind::ENUM_MEMBER,
        CompletionKind::Type => CompletionItemKind::TYPE_PARAMETER,
        CompletionKind::Struct => CompletionItemKind::STRUCT,
        CompletionKind::Enum => CompletionItemKind::ENUM,
        CompletionKind::Interface => CompletionItemKind::INTERFACE,
        CompletionKind::Module => CompletionItemKind::MODULE,
        CompletionKind::Keyword => CompletionItemKind::KEYWORD,
    }
}
