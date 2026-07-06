use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    path::{Path, PathBuf},
};

use lsp_server::{Connection, Message, Request, RequestId, Response};
use lsp_types::{
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    GotoDefinitionParams, Hover, HoverContents, HoverParams, InitializeParams, InlayHint,
    InlayHintKind, InlayHintLabel, InlayHintParams, Location, MarkedString, OneOf,
    ParameterInformation, ParameterLabel, Position, PublishDiagnosticsParams, Range, SemanticToken,
    SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensServerCapabilities, ServerCapabilities, SignatureHelp, SignatureHelpOptions,
    SignatureHelpParams, SignatureInformation, TextDocumentSyncCapability, TextDocumentSyncKind,
    Url,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
        Notification,
    },
    request::{
        Completion, GotoDefinition, HoverRequest, InlayHintRequest, Request as LspRequest,
        SemanticTokensFullRequest, SignatureHelpRequest,
    },
};
use serde::Serialize;

use crate::{
    ast::{self, BindingMutability},
    checked::CheckedProgram,
    ciel_display::{format_function_signature, format_typed_binding},
    diagnostic::Diagnostic as CielDiagnostic,
    driver::{CompileOptions, FrontendAnalysis, analyze_frontend_lossy},
    hir::{
        self, ConstraintArg, ConstraintExpr, Expr, ExprKind, ForInit, FunctionReturnType, NameRef,
        NameRefKind, Pattern, PatternNameKind, Stmt, StmtKind, Type, TypeKind, TypeNameKind,
    },
    resolve::{Def, DefId, DefKind},
    source::SourceMap,
    span::{FileId, Span},
    thir::{self, TExpr, TExprKind, TForInit, TPattern, TStmt, TStmtKind, ThirVisitor},
    types::Ty,
};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use super::completion_facts::CompletionFacts;

const TOK_NAMESPACE: u32 = 0;
const TOK_TYPE: u32 = 1;
const TOK_STRUCT: u32 = 2;
const TOK_ENUM: u32 = 3;
const TOK_INTERFACE: u32 = 4;
const TOK_TYPE_PARAMETER: u32 = 5;
const TOK_PARAMETER: u32 = 6;
const TOK_VARIABLE: u32 = 7;
const TOK_PROPERTY: u32 = 8;
const TOK_ENUM_MEMBER: u32 = 9;
const TOK_FUNCTION: u32 = 10;
const TOK_KEYWORD: u32 = 11;
const TOK_COMMENT: u32 = 12;
const TOK_STRING: u32 = 13;
const TOK_NUMBER: u32 = 14;
const TOK_OPERATOR: u32 = 15;

const MOD_DECLARATION: u32 = 1 << 0;
const MOD_DEFINITION: u32 = 1 << 1;
const MOD_READONLY: u32 = 1 << 2;
const MOD_ASYNC: u32 = 1 << 3;
const MOD_DEFAULT_LIBRARY: u32 = 1 << 4;
const MOD_MUTABLE: u32 = 1 << 6;

pub fn run_stdio() -> Result<(), Box<dyn Error + Send + Sync>> {
    let (connection, io_threads) = Connection::stdio();
    let initialize_params = connection.initialize(serde_json::to_value(server_capabilities())?)?;
    let initialize_params: InitializeParams = serde_json::from_value(initialize_params)?;
    let mut state = LspState::new(initialize_params);

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    break;
                }
                state.handle_request(&connection, request)?;
            }
            Message::Notification(notification) => {
                state.handle_notification(&connection, notification)?;
            }
            Message::Response(_) => {}
        }
    }

    io_threads.join()?;
    Ok(())
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string(), ">".to_string(), ":".to_string()]),
            resolve_provider: Some(false),
            ..CompletionOptions::default()
        }),
        definition_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: Default::default(),
                legend: semantic_tokens_legend(),
                range: None,
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
        inlay_hint_provider: Some(OneOf::Left(true)),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: Some(vec![",".to_string()]),
            work_done_progress_options: Default::default(),
        }),
        ..ServerCapabilities::default()
    }
}

fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::NAMESPACE,
            SemanticTokenType::TYPE,
            SemanticTokenType::STRUCT,
            SemanticTokenType::ENUM,
            SemanticTokenType::INTERFACE,
            SemanticTokenType::TYPE_PARAMETER,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::ENUM_MEMBER,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::KEYWORD,
            SemanticTokenType::COMMENT,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::OPERATOR,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::DEFINITION,
            SemanticTokenModifier::READONLY,
            SemanticTokenModifier::ASYNC,
            SemanticTokenModifier::DEFAULT_LIBRARY,
            SemanticTokenModifier::MODIFICATION,
            SemanticTokenModifier::new("mutable"),
        ],
    }
}

struct LspState {
    workspace_root: Option<PathBuf>,
    open_documents: HashMap<Url, String>,
    revision: u64,
    analysis_cache: HashMap<Url, CachedFacts>,
}

impl LspState {
    fn new(params: InitializeParams) -> Self {
        let workspace_root = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.uri.to_file_path().ok())
            .or_else(|| {
                #[allow(deprecated)]
                {
                    params.root_uri.and_then(|uri| uri.to_file_path().ok())
                }
            });
        Self {
            workspace_root,
            open_documents: HashMap::new(),
            revision: 0,
            analysis_cache: HashMap::new(),
        }
    }

    fn handle_request(
        &mut self,
        connection: &Connection,
        request: Request,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        match request.method.as_str() {
            SemanticTokensFullRequest::METHOD => {
                let id = request.id;
                let params: SemanticTokensParams = serde_json::from_value(request.params)?;
                let result = self
                    .analyze_uri(&params.text_document.uri)
                    .map(|facts| facts.semantic_tokens(&params.text_document.uri))
                    .unwrap_or_else(|| SemanticTokens {
                        result_id: None,
                        data: Vec::new(),
                    });
                send_ok(connection, id, result)?;
            }
            HoverRequest::METHOD => {
                let id = request.id;
                let params: HoverParams = serde_json::from_value(request.params)?;
                let uri = &params.text_document_position_params.text_document.uri;
                let position = params.text_document_position_params.position;
                let result = self
                    .analyze_uri(uri)
                    .and_then(|facts| facts.hover(uri, position));
                send_ok(connection, id, result)?;
            }
            Completion::METHOD => {
                let id = request.id;
                let params: CompletionParams = serde_json::from_value(request.params)?;
                let uri = &params.text_document_position.text_document.uri;
                let position = params.text_document_position.position;
                let result = self
                    .analyze_uri(uri)
                    .and_then(|facts| facts.completion(uri, position))
                    .unwrap_or_else(|| CompletionResponse::Array(Vec::new()));
                send_ok(connection, id, result)?;
            }
            GotoDefinition::METHOD => {
                let id = request.id;
                let params: GotoDefinitionParams = serde_json::from_value(request.params)?;
                let uri = &params.text_document_position_params.text_document.uri;
                let position = params.text_document_position_params.position;
                let result = self
                    .analyze_uri(uri)
                    .and_then(|facts| facts.definition(uri, position));
                send_ok(connection, id, result)?;
            }
            InlayHintRequest::METHOD => {
                let id = request.id;
                let params: InlayHintParams = serde_json::from_value(request.params)?;
                let result = self
                    .analyze_uri(&params.text_document.uri)
                    .map(|facts| facts.inlay_hints(&params.text_document.uri, params.range))
                    .unwrap_or_default();
                send_ok(connection, id, result)?;
            }
            SignatureHelpRequest::METHOD => {
                let id = request.id;
                let params: SignatureHelpParams = serde_json::from_value(request.params)?;
                let uri = &params.text_document_position_params.text_document.uri;
                let position = params.text_document_position_params.position;
                let result = self
                    .analyze_uri(uri)
                    .and_then(|facts| facts.signature_help(uri, position));
                send_ok(connection, id, result)?;
            }
            _ => {
                connection.sender.send(Message::Response(Response::new_err(
                    request.id,
                    lsp_server::ErrorCode::MethodNotFound as i32,
                    format!("unsupported request `{}`", request.method),
                )))?;
            }
        }
        Ok(())
    }

    fn handle_notification(
        &mut self,
        connection: &Connection,
        notification: lsp_server::Notification,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        match notification.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let params: DidOpenTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                let uri = params.text_document.uri;
                self.open_documents
                    .insert(uri.clone(), params.text_document.text);
                self.invalidate_analysis_cache();
                self.publish_diagnostics(connection, &uri)?;
            }
            DidChangeTextDocument::METHOD => {
                let params: DidChangeTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                if let Some(change) = params.content_changes.into_iter().last() {
                    let uri = params.text_document.uri;
                    self.open_documents.insert(uri.clone(), change.text);
                    self.invalidate_analysis_cache();
                    self.publish_diagnostics(connection, &uri)?;
                }
            }
            DidSaveTextDocument::METHOD => {
                let params: DidSaveTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                if let Some(text) = params.text {
                    self.open_documents
                        .insert(params.text_document.uri.clone(), text);
                }
                self.invalidate_analysis_cache();
                self.publish_diagnostics(connection, &params.text_document.uri)?;
            }
            DidCloseTextDocument::METHOD => {
                let params: DidCloseTextDocumentParams =
                    serde_json::from_value(notification.params)?;
                self.open_documents.remove(&params.text_document.uri);
                self.invalidate_analysis_cache();
                send_notification(
                    connection,
                    "textDocument/publishDiagnostics",
                    PublishDiagnosticsParams {
                        uri: params.text_document.uri,
                        diagnostics: Vec::new(),
                        version: None,
                    },
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    fn publish_diagnostics(
        &mut self,
        connection: &Connection,
        uri: &Url,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let diagnostics = self
            .analyze_uri(uri)
            .map(|facts| facts.diagnostics(uri))
            .unwrap_or_default();
        send_notification(
            connection,
            "textDocument/publishDiagnostics",
            PublishDiagnosticsParams {
                uri: uri.clone(),
                diagnostics,
                version: None,
            },
        )
    }

    fn analyze_uri(&mut self, uri: &Url) -> Option<&DocumentFacts> {
        let cache_is_current = self
            .analysis_cache
            .get(uri)
            .is_some_and(|cached| cached.revision == self.revision);
        if !cache_is_current {
            let facts = self.compute_uri_facts(uri)?;
            self.analysis_cache.insert(
                uri.clone(),
                CachedFacts {
                    revision: self.revision,
                    facts,
                },
            );
        }
        self.analysis_cache.get(uri).map(|cached| &cached.facts)
    }

    fn compute_uri_facts(&self, uri: &Url) -> Option<DocumentFacts> {
        let path = uri.to_file_path().ok()?;
        let mut options = CompileOptions::new(path);
        if let Some(root) = &self.workspace_root {
            options = options.with_std_path(root.clone());
        }
        for (uri, text) in &self.open_documents {
            if let Ok(path) = uri.to_file_path() {
                options = options.with_source_override(path, text.clone());
            }
        }
        analyze_frontend_lossy(options)
            .ok()
            .map(DocumentFacts::from_analysis)
    }

    fn invalidate_analysis_cache(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.analysis_cache.clear();
    }
}

struct CachedFacts {
    revision: u64,
    facts: DocumentFacts,
}

fn send_ok<T: Serialize>(
    connection: &Connection,
    id: RequestId,
    result: T,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    connection
        .sender
        .send(Message::Response(Response::new_ok(id, result)))?;
    Ok(())
}

fn send_notification<T: Serialize>(
    connection: &Connection,
    method: &str,
    params: T,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    connection
        .sender
        .send(Message::Notification(lsp_server::Notification::new(
            method.to_string(),
            params,
        )))?;
    Ok(())
}

#[derive(Debug)]
pub(super) struct DocumentFacts {
    pub(super) source_map: SourceMap,
    line_indices: HashMap<FileId, LineIndex>,
    diagnostics: Vec<CielDiagnostic>,
    symbols: Vec<SymbolFact>,
    hovers: Vec<HoverFact>,
    tokens: Vec<TokenFact>,
    inlay_hints: Vec<InlayHintFact>,
    signatures: Vec<SignatureFact>,
    pub(super) completion_facts: CompletionFacts,
}

impl DocumentFacts {
    fn from_analysis(analysis: FrontendAnalysis) -> Self {
        let completion_facts = CompletionFacts::from_checked(&analysis.checked);
        let mut builder = FactsBuilder::new(&analysis.checked);
        builder.collect();
        let mut tokens = lexical_tokens(&analysis.source_map);
        tokens.extend(builder.tokens);
        let line_indices = analysis
            .source_map
            .files()
            .iter()
            .map(|file| (file.id, LineIndex::new(&file.text)))
            .collect();
        Self {
            source_map: analysis.source_map,
            line_indices,
            diagnostics: analysis.diagnostics,
            symbols: builder.symbols,
            hovers: builder.hovers,
            tokens,
            inlay_hints: builder.inlay_hints,
            signatures: builder.signatures,
            completion_facts,
        }
    }

    fn semantic_tokens(&self, uri: &Url) -> SemanticTokens {
        let Some(file_id) = self.file_id_for_uri(uri) else {
            return SemanticTokens {
                result_id: None,
                data: Vec::new(),
            };
        };
        let mut by_span = BTreeMap::<(usize, usize), TokenFact>::new();
        for token in self
            .tokens
            .iter()
            .filter(|token| token.span.file == file_id)
        {
            by_span.insert((token.span.start, token.span.end), *token);
        }
        let mut tokens = by_span
            .values()
            .filter_map(|token| self.semantic_token(token))
            .collect::<Vec<_>>();
        tokens.sort_by_key(|token| (token.line, token.start, token.length, token.token_type));
        tokens.dedup_by_key(|token| {
            (
                token.line,
                token.start,
                token.length,
                token.token_type,
                token.token_modifiers_bitset,
            )
        });

        let mut data = Vec::with_capacity(tokens.len());
        let mut prev_line = 0;
        let mut prev_start = 0;
        for raw in tokens {
            let delta_line = raw.line - prev_line;
            let delta_start = if delta_line == 0 {
                raw.start - prev_start
            } else {
                raw.start
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length: raw.length,
                token_type: raw.token_type,
                token_modifiers_bitset: raw.token_modifiers_bitset,
            });
            prev_line = raw.line;
            prev_start = raw.start;
        }
        SemanticTokens {
            result_id: None,
            data,
        }
    }

    fn hover(&self, uri: &Url, position: Position) -> Option<Hover> {
        let (file_id, offset) = self.offset_for_position(uri, position)?;
        let symbol = self
            .symbols
            .iter()
            .filter(|symbol| span_contains(symbol.span, file_id, offset))
            .min_by_key(|symbol| symbol.span.end.saturating_sub(symbol.span.start));
        if let Some(symbol) = symbol {
            return Some(Hover {
                contents: markdown_hover(&symbol.hover),
                range: Some(self.range_for_span(symbol.span)?),
            });
        }

        self.hovers
            .iter()
            .filter(|hover| span_contains(hover.span, file_id, offset))
            .min_by_key(|hover| hover.span.end.saturating_sub(hover.span.start))
            .and_then(|hover| {
                Some(Hover {
                    contents: markdown_hover(&hover.text),
                    range: Some(self.range_for_span(hover.span)?),
                })
            })
    }

    fn definition(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<lsp_types::GotoDefinitionResponse> {
        let (file_id, offset) = self.offset_for_position(uri, position)?;
        let symbol = self
            .symbols
            .iter()
            .filter(|symbol| span_contains(symbol.span, file_id, offset))
            .filter_map(|symbol| symbol.definition)
            .min_by_key(|span| span.end.saturating_sub(span.start))?;
        let uri = self.uri_for_file(symbol.file)?;
        let range = self.range_for_span(symbol)?;
        Some(lsp_types::GotoDefinitionResponse::Scalar(Location {
            uri,
            range,
        }))
    }

    fn inlay_hints(&self, uri: &Url, range: Range) -> Vec<InlayHint> {
        let Some(file_id) = self.file_id_for_uri(uri) else {
            return Vec::new();
        };
        let Some(start) = self.offset_for_file_position(file_id, range.start) else {
            return Vec::new();
        };
        let Some(end) = self.offset_for_file_position(file_id, range.end) else {
            return Vec::new();
        };
        self.inlay_hints
            .iter()
            .filter(|hint| hint.position.file == file_id)
            .filter(|hint| hint.position.start >= start && hint.position.start <= end)
            .filter_map(|hint| {
                let position = self.position_for_offset(hint.position.file, hint.position.end)?;
                Some(InlayHint {
                    position,
                    label: InlayHintLabel::String(hint.label.clone()),
                    kind: Some(hint.kind),
                    text_edits: None,
                    tooltip: hint.tooltip.clone().map(Into::into),
                    padding_left: Some(hint.padding_left),
                    padding_right: Some(hint.padding_right),
                    data: None,
                })
            })
            .collect()
    }

    fn signature_help(&self, uri: &Url, position: Position) -> Option<SignatureHelp> {
        let (file_id, offset) = self.offset_for_position(uri, position)?;
        let signature = self
            .signatures
            .iter()
            .filter(|signature| span_contains(signature.call_span, file_id, offset))
            .min_by_key(|signature| {
                signature
                    .call_span
                    .end
                    .saturating_sub(signature.call_span.start)
            })?;
        let active_parameter = signature
            .arg_spans
            .iter()
            .find(|arg| span_contains(arg.span, file_id, offset))
            .map(|arg| arg.parameter_index)
            .or_else(|| {
                signature
                    .arg_spans
                    .iter()
                    .take_while(|arg| arg.span.start <= offset)
                    .last()
                    .map(|arg| arg.parameter_index)
            })
            .or_else(|| signature.arg_spans.first().map(|arg| arg.parameter_index))
            .unwrap_or(0usize)
            .min(signature.parameters.len().saturating_sub(1));
        let parameters = signature
            .parameters
            .iter()
            .map(|parameter| ParameterInformation {
                label: ParameterLabel::Simple(parameter.label.clone()),
                documentation: Some(lsp_types::Documentation::String(parameter.ty.clone())),
            })
            .collect();
        Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: signature.label.clone(),
                documentation: signature
                    .documentation
                    .clone()
                    .map(lsp_types::Documentation::String),
                parameters: Some(parameters),
                active_parameter: Some(active_parameter as u32),
            }],
            active_signature: Some(0),
            active_parameter: Some(active_parameter as u32),
        })
    }

    fn diagnostics(&self, uri: &Url) -> Vec<lsp_types::Diagnostic> {
        let file_id = self.file_id_for_uri(uri);
        self.diagnostics
            .iter()
            .filter_map(|diagnostic| self.lsp_diagnostic(uri, file_id, diagnostic))
            .collect()
    }

    fn lsp_diagnostic(
        &self,
        uri: &Url,
        file_id: Option<FileId>,
        diagnostic: &CielDiagnostic,
    ) -> Option<lsp_types::Diagnostic> {
        let range = match diagnostic.span {
            Some(span) => {
                if Some(span.file) != file_id {
                    return None;
                }
                self.range_for_span(span)?
            }
            None => {
                uri.to_file_path().ok()?;
                Range {
                    start: Position::new(0, 0),
                    end: Position::new(0, 0),
                }
            }
        };
        let mut message = diagnostic.message.clone();
        for note in &diagnostic.notes {
            message.push_str("\nnote: ");
            message.push_str(note);
        }
        Some(lsp_types::Diagnostic {
            range,
            severity: Some(lsp_types::DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("cielc".to_string()),
            message,
            related_information: None,
            tags: None,
            data: None,
        })
    }

    fn semantic_token(&self, token: &TokenFact) -> Option<RawSemanticToken> {
        let start = self.position_for_offset(token.span.file, token.span.start)?;
        let end = self.position_for_offset(token.span.file, token.span.end)?;
        if start.line != end.line || end.character <= start.character {
            return None;
        }
        Some(RawSemanticToken {
            line: start.line,
            start: start.character,
            length: end.character - start.character,
            token_type: token.token_type,
            token_modifiers_bitset: token.modifiers,
        })
    }

    fn file_id_for_uri(&self, uri: &Url) -> Option<FileId> {
        let path = uri.to_file_path().ok()?;
        self.source_map
            .files()
            .iter()
            .find(|file| paths_equal(&file.path, &path))
            .map(|file| file.id)
    }

    fn uri_for_file(&self, file_id: FileId) -> Option<Url> {
        Url::from_file_path(&self.source_map.get(file_id).path).ok()
    }

    pub(super) fn offset_for_position(
        &self,
        uri: &Url,
        position: Position,
    ) -> Option<(FileId, usize)> {
        let file_id = self.file_id_for_uri(uri)?;
        let offset = self.offset_for_file_position(file_id, position)?;
        Some((file_id, offset))
    }

    fn offset_for_file_position(&self, file_id: FileId, position: Position) -> Option<usize> {
        let file = self.source_map.get(file_id);
        self.line_indices
            .get(&file_id)?
            .offset(&file.text, position)
    }

    fn position_for_offset(&self, file_id: FileId, offset: usize) -> Option<Position> {
        let file = self.source_map.get(file_id);
        self.line_indices
            .get(&file_id)?
            .position(&file.text, offset)
    }

    fn range_for_span(&self, span: Span) -> Option<Range> {
        Some(Range {
            start: self.position_for_offset(span.file, span.start)?,
            end: self.position_for_offset(span.file, span.end)?,
        })
    }

    pub(super) fn completion_prefix_range(&self, file_id: FileId, offset: usize) -> Option<Range> {
        let file = self.source_map.get(file_id);
        if offset > file.text.len() || !file.text.is_char_boundary(offset) {
            return None;
        }
        let mut start = offset;
        for (idx, ch) in file.text.get(..offset)?.char_indices().rev() {
            if ch == '_' || ch.is_ascii_alphanumeric() {
                start = idx;
            } else {
                break;
            }
        }
        Some(Range {
            start: self.position_for_offset(file_id, start)?,
            end: self.position_for_offset(file_id, offset)?,
        })
    }
}

#[derive(Clone, Debug)]
struct SymbolFact {
    span: Span,
    definition: Option<Span>,
    hover: String,
}

#[derive(Clone, Debug)]
struct HoverFact {
    span: Span,
    text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TokenFact {
    span: Span,
    token_type: u32,
    modifiers: u32,
}

#[derive(Clone, Debug)]
struct InlayHintFact {
    position: Span,
    label: String,
    kind: InlayHintKind,
    tooltip: Option<String>,
    padding_left: bool,
    padding_right: bool,
}

#[derive(Clone, Debug)]
struct SignatureFact {
    call_span: Span,
    label: String,
    documentation: Option<String>,
    parameters: Vec<SignatureParameterFact>,
    arg_spans: Vec<SignatureArgFact>,
}

#[derive(Clone, Debug)]
struct SignatureParameterFact {
    label: String,
    ty: String,
}

#[derive(Clone, Debug)]
struct SignatureArgFact {
    span: Span,
    parameter_index: usize,
}

#[derive(Clone, Copy, Debug)]
struct RawSemanticToken {
    line: u32,
    start: u32,
    length: u32,
    token_type: u32,
    token_modifiers_bitset: u32,
}

struct FactsBuilder<'a> {
    checked: &'a CheckedProgram,
    local_defs: HashMap<hir::LocalId, Span>,
    local_tys: HashMap<hir::LocalId, Ty>,
    local_mutabilities: HashMap<hir::LocalId, BindingMutability>,
    parameter_locals: HashSet<hir::LocalId>,
    function_infos: HashMap<DefId, FunctionInfo>,
    symbols: Vec<SymbolFact>,
    hovers: Vec<HoverFact>,
    tokens: Vec<TokenFact>,
    inlay_hints: Vec<InlayHintFact>,
    signatures: Vec<SignatureFact>,
}

impl<'a> FactsBuilder<'a> {
    fn new(checked: &'a CheckedProgram) -> Self {
        let local_defs = checked
            .hir_locals
            .iter()
            .map(|local| (local.id, local.span))
            .collect();
        Self {
            checked,
            local_defs,
            local_tys: HashMap::new(),
            local_mutabilities: HashMap::new(),
            parameter_locals: HashSet::new(),
            function_infos: function_infos(checked),
            symbols: Vec::new(),
            hovers: Vec::new(),
            tokens: Vec::new(),
            inlay_hints: Vec::new(),
            signatures: Vec::new(),
        }
    }

    fn collect(&mut self) {
        self.collect_checked();
        self.collect_resolved_defs();
        for module in &self.checked.hir_modules {
            self.collect_module(module);
        }
        for hole in &self.checked.inferred_type_holes {
            self.hovers.push(HoverFact {
                span: hole.span,
                text: format!("inferred type for `{}`: `{}`", hole.local_name, hole.ty),
            });
            self.inlay_hints.push(InlayHintFact {
                position: Span {
                    file: hole.span.file,
                    start: hole.span.end,
                    end: hole.span.end,
                },
                label: format!(": {}", hole.ty),
                kind: InlayHintKind::TYPE,
                tooltip: Some(format!("inferred type for `{}`", hole.local_name)),
                padding_left: true,
                padding_right: false,
            });
        }
    }

    fn collect_checked(&mut self) {
        for function in &self.checked.functions {
            for (local_id, _, ty, mutability) in &function.params {
                if let Some(local_id) = local_id {
                    self.local_tys.insert(*local_id, ty.clone());
                    self.local_mutabilities.insert(*local_id, *mutability);
                    self.parameter_locals.insert(*local_id);
                }
            }
            if let Some(body) = &function.body {
                let mut collector = ThirFactsCollector { builder: self };
                collector.visit_block(body);
            }
        }
    }

    fn collect_resolved_defs(&mut self) {
        for def in &self.checked.resolved.defs {
            let (token_type, mut modifiers) = token_for_def_kind(&def.kind);
            modifiers |= MOD_DECLARATION | MOD_DEFINITION;
            if self.is_default_library_span(def.span) {
                modifiers |= MOD_DEFAULT_LIBRARY;
            }
            if def.kind == DefKind::Function
                && self
                    .function_infos
                    .get(&def.id)
                    .is_some_and(|info| info.is_async)
            {
                modifiers |= MOD_ASYNC;
            }
            self.tokens.push(TokenFact {
                span: def.span,
                token_type,
                modifiers,
            });
            self.symbols.push(SymbolFact {
                span: def.span,
                definition: Some(def.span),
                hover: self.hover_for_def(def),
            });
        }
    }

    fn collect_module(&mut self, module: &hir::Module) {
        for item in &module.items {
            self.collect_item(item);
        }
    }

    fn collect_item(&mut self, item: &hir::Item) {
        match &item.kind {
            hir::ItemKind::Import(_) | hir::ItemKind::CInclude(_) => {}
            hir::ItemKind::TypeAlias(decl) => {
                self.collect_generics(&decl.generics);
                match &decl.target {
                    hir::TypeAliasTarget::Type(ty) => self.collect_type(ty),
                    hir::TypeAliasTarget::CSpelling { .. } => {}
                }
            }
            hir::ItemKind::Struct(decl) => {
                self.collect_generics(&decl.generics);
                for field in &decl.fields {
                    self.collect_type(&field.ty);
                    self.tokens.push(TokenFact {
                        span: field.name.span,
                        token_type: TOK_PROPERTY,
                        modifiers: MOD_DECLARATION | MOD_DEFINITION,
                    });
                    self.symbols.push(SymbolFact {
                        span: field.name.span,
                        definition: Some(field.name.span),
                        hover: field_hover(&field.name.name, &Ty::from_ast_name_lossy(&field.ty)),
                    });
                }
            }
            hir::ItemKind::Enum(decl) => {
                self.collect_generics(&decl.generics);
                for variant in &decl.variants {
                    for ty in &variant.payload {
                        self.collect_type(ty);
                    }
                }
            }
            hir::ItemKind::Interface(decl) => {
                self.collect_generics(&decl.generics);
                self.collect_signature(&decl.signature);
            }
            hir::ItemKind::InterfaceAlias(decl) => {
                self.collect_generics(&decl.generics);
                self.collect_interface_expr(&decl.expr);
            }
            hir::ItemKind::Impl(decl) => self.collect_impl_decl(decl),
            hir::ItemKind::DerivableImpl(decl) => self.collect_impl_decl(&decl.impl_decl),
            hir::ItemKind::Derive(decl) => {
                self.collect_generics(&decl.generics);
                self.collect_name_ref(&decl.name);
                for arg in &decl.args {
                    self.collect_type(arg);
                }
            }
            hir::ItemKind::Function(decl) => {
                self.collect_signature(&decl.signature);
                if let Some(body) = &decl.body {
                    self.collect_block(body);
                }
            }
            hir::ItemKind::ExternBlock(block) => {
                for item in &block.items {
                    match item {
                        hir::ExternItem::OpaqueStruct(_) => {}
                        hir::ExternItem::Function { signature, .. } => {
                            self.collect_signature(signature);
                        }
                        hir::ExternItem::TypeAlias(alias) => {
                            self.collect_generics(&alias.generics);
                            match &alias.target {
                                hir::TypeAliasTarget::Type(ty) => self.collect_type(ty),
                                hir::TypeAliasTarget::CSpelling { .. } => {}
                            }
                        }
                    }
                }
            }
        }
    }

    fn collect_impl_decl(&mut self, decl: &hir::ImplDecl) {
        self.collect_generics(&decl.generics);
        self.collect_name_ref(&decl.name);
        for arg in &decl.args {
            self.collect_type(arg);
        }
        for param in &decl.params {
            self.collect_type(&param.ty);
            if let Some(local_id) = param.local_id {
                self.collect_local_decl(
                    local_id,
                    &param.name.name,
                    param.name.span,
                    param.mutability,
                    true,
                );
            }
        }
        self.collect_block(&decl.body);
    }

    fn collect_signature(&mut self, signature: &hir::FunctionSignature) {
        self.collect_generics(&signature.generics);
        match &signature.ret {
            FunctionReturnType::Type(ty) => self.collect_type(ty),
            FunctionReturnType::OpaqueConstraint { constraint, .. } => {
                self.collect_constraint_expr(constraint);
            }
        }
        for param in &signature.params {
            self.collect_type(&param.ty);
            if let Some(local_id) = param.local_id {
                self.collect_local_decl(
                    local_id,
                    &param.name.name,
                    param.name.span,
                    param.mutability,
                    true,
                );
            }
        }
        if let Some(selector) = &signature.receiver_selector {
            if let Some(receiver) = &selector.receiver_param {
                self.tokens.push(TokenFact {
                    span: receiver.span,
                    token_type: TOK_PARAMETER,
                    modifiers: MOD_DECLARATION | MOD_DEFINITION,
                });
            }
            self.tokens.push(TokenFact {
                span: selector.name.span,
                token_type: TOK_FUNCTION,
                modifiers: MOD_DECLARATION | MOD_DEFINITION,
            });
        }
    }

    fn collect_generics(&mut self, generics: &[hir::GenericParam]) {
        for generic in generics {
            self.tokens.push(TokenFact {
                span: generic.name.span,
                token_type: TOK_TYPE_PARAMETER,
                modifiers: MOD_DECLARATION | MOD_DEFINITION,
            });
            self.symbols.push(SymbolFact {
                span: generic.name.span,
                definition: Some(generic.name.span),
                hover: format!("type parameter `{}`", generic.name.name),
            });
            if let Some(constraint) = &generic.constraint {
                self.collect_constraint_expr(constraint);
            }
        }
    }

    fn collect_constraint_expr(&mut self, constraint: &ConstraintExpr) {
        for term in &constraint.terms {
            self.collect_name_ref(&term.name);
            for arg in &term.args {
                match arg {
                    ConstraintArg::Type(ty) => self.collect_type(ty),
                    ConstraintArg::Binding {
                        name, constraint, ..
                    } => {
                        self.tokens.push(TokenFact {
                            span: name.span,
                            token_type: TOK_TYPE_PARAMETER,
                            modifiers: MOD_DECLARATION | MOD_DEFINITION,
                        });
                        if let Some(constraint) = constraint {
                            self.collect_constraint_expr(constraint);
                        }
                    }
                }
            }
        }
    }

    fn collect_interface_expr(&mut self, expr: &hir::InterfaceExpr) {
        self.collect_interface_term(&expr.first);
        for (_, term) in &expr.rest {
            self.collect_interface_term(term);
        }
    }

    fn collect_interface_term(&mut self, term: &hir::InterfaceTerm) {
        self.collect_name_ref(&term.name);
        for arg in &term.args {
            self.collect_type(arg);
        }
    }

    fn collect_type(&mut self, ty: &Type) {
        match &ty.kind {
            TypeKind::Hole | TypeKind::Never | TypeKind::Void | TypeKind::Primitive(_) => {}
            TypeKind::Named(name, args) => {
                match &name.kind {
                    TypeNameKind::Def(def_id) => {
                        self.collect_def_ref(name.name_span, *def_id);
                    }
                    TypeNameKind::Generic(generic) => {
                        self.tokens.push(TokenFact {
                            span: name.name_span,
                            token_type: TOK_TYPE_PARAMETER,
                            modifiers: 0,
                        });
                        self.symbols.push(SymbolFact {
                            span: name.name_span,
                            definition: None,
                            hover: format!("type parameter `{generic}`"),
                        });
                    }
                    TypeNameKind::Error => {}
                }
                for arg in args {
                    self.collect_type(arg);
                }
            }
            TypeKind::Pointer { inner, .. }
            | TypeKind::Array { elem: inner, .. }
            | TypeKind::Slice { elem: inner, .. } => self.collect_type(inner),
            TypeKind::Function { ret, params, .. } => {
                self.collect_type(ret);
                for param in params {
                    self.collect_type(param);
                }
            }
            TypeKind::Closure {
                ret,
                params,
                constraint,
            } => {
                self.collect_type(ret);
                for param in params {
                    self.collect_type(param);
                }
                if let Some(constraint) = constraint {
                    self.collect_constraint_expr(constraint);
                }
            }
        }
    }

    fn collect_block(&mut self, block: &hir::Block) {
        for stmt in &block.statements {
            self.collect_stmt(stmt);
        }
    }

    fn collect_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Block(block) => self.collect_block(block),
            StmtKind::VarDecl {
                ty,
                name,
                mutability,
                local_id,
                init,
            } => {
                self.collect_type(ty);
                self.collect_local_decl(*local_id, &name.name, name.span, *mutability, false);
                if let Some(init) = init {
                    self.collect_expr(init);
                }
            }
            StmtKind::Assign { target, value } => {
                self.collect_expr(target);
                self.collect_expr(value);
            }
            StmtKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.collect_expr(cond);
                self.collect_block(then_block);
                if let Some(else_branch) = else_branch {
                    self.collect_stmt(else_branch);
                }
            }
            StmtKind::While { cond, body } => {
                self.collect_expr(cond);
                self.collect_block(body);
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.collect_for_init(init);
                }
                if let Some(cond) = cond {
                    self.collect_expr(cond);
                }
                if let Some(step) = step {
                    self.collect_for_init(step);
                }
                self.collect_block(body);
            }
            StmtKind::Switch {
                expr,
                cases,
                default,
                ..
            } => {
                self.collect_expr(expr);
                for case in cases {
                    self.collect_pattern(&case.pattern);
                    for stmt in &case.statements {
                        self.collect_stmt(stmt);
                    }
                }
                for stmt in default {
                    self.collect_stmt(stmt);
                }
            }
            StmtKind::Defer(expr) | StmtKind::Return(Some(expr)) | StmtKind::Expr(expr) => {
                self.collect_expr(expr);
            }
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => {}
        }
    }

    fn collect_for_init(&mut self, init: &ForInit) {
        match init {
            ForInit::VarDecl {
                ty,
                name,
                mutability,
                local_id,
                init,
            } => {
                self.collect_type(ty);
                self.collect_local_decl(*local_id, &name.name, name.span, *mutability, false);
                if let Some(init) = init {
                    self.collect_expr(init);
                }
            }
            ForInit::Assign { target, value } => {
                self.collect_expr(target);
                self.collect_expr(value);
            }
            ForInit::Expr(expr) => self.collect_expr(expr),
        }
    }

    fn collect_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Name(name) => self.collect_name_ref(name),
            ExprKind::Literal(_) => {}
            ExprKind::StructLiteral(fields) => {
                for field in fields {
                    self.tokens.push(TokenFact {
                        span: field.name.span,
                        token_type: TOK_PROPERTY,
                        modifiers: 0,
                    });
                    self.collect_expr(&field.expr);
                }
            }
            ExprKind::ArrayLiteral(elements) => {
                for element in elements {
                    self.collect_expr(element);
                }
            }
            ExprKind::ArrayRepeat { element, .. } => self.collect_expr(element),
            ExprKind::Closure { params, body, .. } => {
                for param in params {
                    if let Some(ty) = &param.ty {
                        self.collect_type(ty);
                    }
                    self.collect_local_decl(
                        param.local_id,
                        &param.name.name,
                        param.name.span,
                        param.mutability,
                        true,
                    );
                }
                match body {
                    hir::ClosureBody::Expr(expr) => self.collect_expr(expr),
                    hir::ClosureBody::Block(block) => self.collect_block(block),
                }
            }
            ExprKind::Unary { expr, .. } | ExprKind::Try(expr) | ExprKind::Await(expr) => {
                self.collect_expr(expr);
            }
            ExprKind::Cast { expr, ty } => {
                self.collect_expr(expr);
                self.collect_type(ty);
            }
            ExprKind::GenericValue { callee, type_args } => {
                self.collect_expr(callee);
                for arg in type_args {
                    self.collect_type(arg);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.collect_expr(left);
                self.collect_expr(right);
            }
            ExprKind::UnsafeBlock(block) => {
                for stmt in &block.statements {
                    self.collect_stmt(stmt);
                }
                if let Some(value) = &block.value {
                    self.collect_expr(value);
                }
            }
            ExprKind::Call {
                callee,
                type_args,
                args,
            } => {
                self.collect_expr(callee);
                for arg in type_args {
                    self.collect_type(arg);
                }
                for arg in args {
                    self.collect_expr(arg);
                }
            }
            ExprKind::Field { base, field } | ExprKind::Arrow { base, field } => {
                self.collect_expr(base);
                self.tokens.push(TokenFact {
                    span: field.span,
                    token_type: TOK_PROPERTY,
                    modifiers: 0,
                });
            }
            ExprKind::ReceiverSelector { base, selector } => {
                self.collect_expr(base);
                if let Some(name) = selector.last() {
                    self.tokens.push(TokenFact {
                        span: name.span,
                        token_type: TOK_FUNCTION,
                        modifiers: 0,
                    });
                }
            }
            ExprKind::Index { base, index } => {
                self.collect_expr(base);
                self.collect_expr(index);
            }
            ExprKind::Slice { base, start, end } => {
                self.collect_expr(base);
                if let Some(start) = start {
                    self.collect_expr(start);
                }
                if let Some(end) = end {
                    self.collect_expr(end);
                }
            }
            ExprKind::Select { arms, .. } => {
                for arm in arms {
                    self.collect_local_decl(
                        arm.binding_local,
                        &arm.binding.name,
                        arm.binding.span,
                        BindingMutability::Immutable,
                        false,
                    );
                    self.collect_expr(&arm.future);
                    self.collect_expr(&arm.body);
                }
            }
        }
    }

    fn collect_pattern(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::Variant(name, payload) => {
                match &name.kind {
                    PatternNameKind::Variant(def_id) => {
                        self.collect_def_ref(name.name_span, *def_id);
                        self.collect_enum_parent_ref_from_path(&name.path, *def_id);
                    }
                    PatternNameKind::VariantCandidates(candidates) => {
                        self.tokens.push(TokenFact {
                            span: name.name_span,
                            token_type: TOK_ENUM_MEMBER,
                            modifiers: 0,
                        });
                        if let Some(def_id) = candidates.first() {
                            let def = self.checked.resolved.def(*def_id);
                            self.symbols.push(SymbolFact {
                                span: name.name_span,
                                definition: Some(def.span),
                                hover: "enum variant pattern".to_string(),
                            });
                            self.collect_enum_parent_ref_from_path(&name.path, *def_id);
                        }
                    }
                    PatternNameKind::Binding {
                        local_id,
                        mutability,
                    } => {
                        let name_text = name
                            .path
                            .last()
                            .map(|name| name.name.as_str())
                            .unwrap_or("_");
                        self.collect_local_decl(
                            *local_id,
                            name_text,
                            name.name_span,
                            *mutability,
                            false,
                        );
                    }
                    PatternNameKind::Error => {}
                }
                for pattern in payload {
                    self.collect_pattern(pattern);
                }
            }
            Pattern::Wildcard(_) => {}
        }
    }

    fn collect_name_ref(&mut self, name: &NameRef) {
        match &name.kind {
            NameRefKind::Local(local_id) => {
                let token_type = if self.parameter_locals.contains(local_id) {
                    TOK_PARAMETER
                } else {
                    TOK_VARIABLE
                };
                let mutability = self
                    .local_mutabilities
                    .get(local_id)
                    .copied()
                    .unwrap_or(BindingMutability::Immutable);
                self.tokens.push(TokenFact {
                    span: name.name_span,
                    token_type,
                    modifiers: binding_reference_mutability_modifiers(mutability),
                });
                let ty = self
                    .local_tys
                    .get(local_id)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_string());
                self.symbols.push(SymbolFact {
                    span: name.name_span,
                    definition: self.local_defs.get(local_id).copied(),
                    hover: binding_hover(&name.display, &ty, mutability),
                });
            }
            NameRefKind::Def(def_id) => {
                self.collect_def_ref(name.name_span, *def_id);
                self.collect_enum_parent_ref_from_path(&name.path, *def_id);
            }
            NameRefKind::VariantCandidates(candidates) => {
                self.tokens.push(TokenFact {
                    span: name.name_span,
                    token_type: TOK_ENUM_MEMBER,
                    modifiers: 0,
                });
                if let Some(def_id) = candidates.first() {
                    let def = self.checked.resolved.def(*def_id);
                    self.symbols.push(SymbolFact {
                        span: name.name_span,
                        definition: Some(def.span),
                        hover: "enum variant".to_string(),
                    });
                    self.collect_enum_parent_ref_from_path(&name.path, *def_id);
                }
            }
            NameRefKind::Error => {}
        }
    }

    fn collect_def_ref(&mut self, span: Span, def_id: DefId) {
        let def = self.checked.resolved.def(def_id);
        let (token_type, mut modifiers) = token_for_def_kind(&def.kind);
        if self.is_default_library_span(def.span) {
            modifiers |= MOD_DEFAULT_LIBRARY;
        }
        if def.kind == DefKind::Function
            && self
                .function_infos
                .get(&def.id)
                .is_some_and(|info| info.is_async)
        {
            modifiers |= MOD_ASYNC;
        }
        self.tokens.push(TokenFact {
            span,
            token_type,
            modifiers,
        });
        self.symbols.push(SymbolFact {
            span,
            definition: Some(def.span),
            hover: self.hover_for_def(def),
        });
    }

    fn collect_enum_parent_ref_from_path(&mut self, path: &[ast::Ident], def_id: DefId) {
        let def = self.checked.resolved.def(def_id);
        if def.kind != DefKind::EnumVariant {
            return;
        }
        let Some(parent_id) = def.parent else {
            return;
        };
        let parent = self.checked.resolved.def(parent_id);
        let Some(parent_segment) = path
            .iter()
            .rev()
            .skip(1)
            .find(|segment| segment.name == parent.name)
        else {
            return;
        };
        self.collect_def_ref(parent_segment.span, parent_id);
    }

    fn collect_local_decl(
        &mut self,
        local_id: hir::LocalId,
        name: &str,
        span: Span,
        mutability: BindingMutability,
        parameter: bool,
    ) {
        let token_type = if parameter {
            self.parameter_locals.insert(local_id);
            TOK_PARAMETER
        } else {
            TOK_VARIABLE
        };
        let modifiers =
            MOD_DECLARATION | MOD_DEFINITION | binding_declaration_mutability_modifiers(mutability);
        self.tokens.push(TokenFact {
            span,
            token_type,
            modifiers,
        });
        self.local_mutabilities.insert(local_id, mutability);
        let ty = self
            .local_tys
            .get(&local_id)
            .map(ToString::to_string)
            .unwrap_or_else(|| "unknown".to_string());
        self.symbols.push(SymbolFact {
            span,
            definition: Some(span),
            hover: binding_hover(name, &ty, mutability),
        });
    }

    fn hover_for_def(&self, def: &Def) -> String {
        if let Some(info) = self.function_infos.get(&def.id) {
            return format!("```ciel\n{}\n```", info.label);
        }
        let kind = match def.kind {
            DefKind::TypeAlias => "type alias",
            DefKind::Struct => "struct",
            DefKind::Enum => "enum",
            DefKind::EnumVariant => "enum variant",
            DefKind::Interface => "interface",
            DefKind::InterfaceAlias => "interface alias",
            DefKind::Function => "function",
            DefKind::ExternFunction => "extern function",
            DefKind::OpaqueStruct => "opaque struct",
        };
        format!("{kind} `{}`", def.name)
    }

    fn is_default_library_span(&self, span: Span) -> bool {
        let path = self
            .checked
            .resolved
            .modules
            .iter()
            .find(|module| {
                module
                    .ast
                    .items
                    .iter()
                    .any(|item| item.span.file == span.file)
            })
            .map(|module| module.path.as_path());
        path.is_some_and(|path| {
            path.components()
                .any(|component| component.as_os_str() == "std")
        })
    }
}

#[derive(Clone, Debug)]
struct FunctionInfo {
    name: String,
    label: String,
    parameters: Vec<FunctionParameterInfo>,
    is_async: bool,
}

#[derive(Clone, Debug)]
struct FunctionParameterInfo {
    name: String,
    ty: Ty,
    mutability: BindingMutability,
}

fn function_infos(checked: &CheckedProgram) -> HashMap<DefId, FunctionInfo> {
    let mut infos = HashMap::new();
    for function in &checked.functions {
        let parameters = function
            .params
            .iter()
            .map(|(_, name, ty, mutability)| FunctionParameterInfo {
                name: name.clone(),
                ty: ty.clone(),
                mutability: *mutability,
            })
            .collect::<Vec<_>>();
        infos.insert(
            function.def_id,
            FunctionInfo {
                name: function.name.clone(),
                label: function_label(
                    function.is_async,
                    &function.ret,
                    &function.name,
                    &parameters,
                ),
                parameters,
                is_async: function.is_async,
            },
        );
    }
    for function in &checked.generic_functions {
        let parameters = function
            .function
            .signature
            .params
            .iter()
            .zip(function.params.iter())
            .map(|(param, ty)| FunctionParameterInfo {
                name: param.name.name.clone(),
                ty: ty.clone(),
                mutability: param.mutability,
            })
            .collect::<Vec<_>>();
        infos.insert(
            function.def_id,
            FunctionInfo {
                name: function.name.clone(),
                label: function_label(
                    function.is_async,
                    &function.ret,
                    &function.name,
                    &parameters,
                ),
                parameters,
                is_async: function.is_async,
            },
        );
    }
    infos
}

fn function_label(
    is_async: bool,
    ret: &Ty,
    name: &str,
    parameters: &[FunctionParameterInfo],
) -> String {
    format_function_signature(
        is_async,
        ret,
        name,
        parameters.iter().map(parameter_label).collect::<Vec<_>>(),
    )
}

fn parameter_label(param: &FunctionParameterInfo) -> String {
    format_typed_binding(&param.ty, &param.name, param.mutability)
}

fn binding_hover(name: &str, ty: &str, mutability: BindingMutability) -> String {
    format!(
        "```ciel\n{}\n```",
        format_typed_binding(&ty, name, mutability)
    )
}

fn field_hover(name: &str, ty: &str) -> String {
    format!("field\n\n```ciel\n{} {};\n```", ty, name)
}

struct ThirFactsCollector<'a, 'b> {
    builder: &'a mut FactsBuilder<'b>,
}

impl ThirVisitor for ThirFactsCollector<'_, '_> {
    fn visit_stmt(&mut self, stmt: &TStmt) {
        match &stmt.kind {
            TStmtKind::VarDecl { ty, local_id, .. } => {
                self.builder.local_tys.insert(*local_id, ty.clone());
            }
            TStmtKind::For { init, .. } => {
                if let Some(init) = init {
                    collect_for_init_local_types(self.builder, init);
                }
            }
            _ => {}
        }
        thir::walk_stmt(self, stmt);
    }

    fn visit_pattern(&mut self, pattern: &TPattern) {
        collect_pattern_local_types(self.builder, pattern);
        thir::walk_pattern(self, pattern);
    }

    fn visit_expr(&mut self, expr: &TExpr) {
        if !matches!(expr.ty, Ty::Void | Ty::Unknown) {
            self.builder.hovers.push(HoverFact {
                span: expr.span,
                text: format!("type: `{}`", expr.ty),
            });
        }
        if let TExprKind::Closure { params, .. } = &expr.kind {
            for (local_id, _, ty) in params {
                self.builder.local_tys.insert(*local_id, ty.clone());
                self.builder.parameter_locals.insert(*local_id);
            }
        }
        if let TExprKind::Call { callee, args } = &expr.kind
            && let Some(def_id) = callee_def_id(callee)
        {
            if let Some(info) = self.builder.function_infos.get(&def_id).cloned() {
                self.collect_call_facts(expr.span, callee.span, args, &info);
            }
        }
        thir::walk_expr(self, expr);
    }
}

impl ThirFactsCollector<'_, '_> {
    fn collect_call_facts(
        &mut self,
        call_span: Span,
        callee_span: Span,
        args: &[TExpr],
        info: &FunctionInfo,
    ) {
        let call_args = call_arguments(callee_span, args, &info.parameters);
        let selector_receiver = call_args.iter().find(|arg| arg.is_selector_receiver);

        if let Some(receiver) = selector_receiver {
            self.builder.hovers.push(HoverFact {
                span: callee_span,
                text: format!(
                    "selector call\n\n```ciel\n{}\n```\n\nreceiver parameter `{}`",
                    info.label,
                    parameter_label(receiver.param)
                ),
            });
        }

        for arg in call_args.iter().filter(|arg| !arg.is_selector_receiver) {
            self.builder.inlay_hints.push(InlayHintFact {
                position: Span {
                    file: arg.span.file,
                    start: arg.span.start,
                    end: arg.span.start,
                },
                label: format!("{}:", arg.param.name),
                kind: InlayHintKind::PARAMETER,
                tooltip: Some(format!("parameter `{}`", parameter_label(arg.param))),
                padding_left: false,
                padding_right: true,
            });
        }
        if !call_args.is_empty() {
            let mut parameters = info
                .parameters
                .iter()
                .map(|param| SignatureParameterFact {
                    label: param.name.clone(),
                    ty: parameter_label(param),
                })
                .collect::<Vec<_>>();
            if let Some(receiver) = selector_receiver
                && let Some(parameter) = parameters.get_mut(receiver.parameter_index)
            {
                parameter.ty = format!(
                    "{}\nselector receiver supplied by the expression before `.{}`",
                    parameter_label(receiver.param),
                    info.name
                );
            }
            let label = if selector_receiver.is_some() {
                format!("{} [selector]", info.label)
            } else {
                info.label.clone()
            };
            let documentation = selector_receiver.map(|receiver| {
                format!(
                    "selector call; receiver parameter `{}` is supplied by the expression before `.{}`",
                    parameter_label(receiver.param),
                    info.name
                )
            });
            self.builder.signatures.push(SignatureFact {
                call_span,
                label,
                documentation,
                parameters,
                arg_spans: call_args
                    .iter()
                    .filter(|arg| !arg.is_selector_receiver)
                    .map(|arg| SignatureArgFact {
                        span: arg.span,
                        parameter_index: arg.parameter_index,
                    })
                    .collect(),
            });
        }
    }
}

struct CallArgument<'a> {
    span: Span,
    param: &'a FunctionParameterInfo,
    parameter_index: usize,
    is_selector_receiver: bool,
}

fn call_arguments<'a>(
    callee_span: Span,
    args: &'a [TExpr],
    params: &'a [FunctionParameterInfo],
) -> Vec<CallArgument<'a>> {
    args.iter()
        .enumerate()
        .zip(params.iter())
        .map(|((parameter_index, arg), param)| {
            let is_selector_receiver =
                arg.span.file == callee_span.file && arg.span.end <= callee_span.start;
            CallArgument {
                span: arg.span,
                param,
                parameter_index,
                is_selector_receiver,
            }
        })
        .collect()
}

fn collect_for_init_local_types(builder: &mut FactsBuilder<'_>, init: &TForInit) {
    match init {
        TForInit::VarDecl { ty, local_id, .. } => {
            builder.local_tys.insert(*local_id, ty.clone());
        }
        TForInit::Assign { .. } | TForInit::Expr(_) => {}
    }
}

fn collect_pattern_local_types(builder: &mut FactsBuilder<'_>, pattern: &TPattern) {
    match pattern {
        TPattern::Binding { local_id, ty, .. } => {
            builder.local_tys.insert(*local_id, ty.clone());
        }
        TPattern::Variant { payload, .. } => {
            for pattern in payload {
                collect_pattern_local_types(builder, pattern);
            }
        }
        TPattern::Wildcard { .. } => {}
    }
}

fn callee_def_id(expr: &TExpr) -> Option<DefId> {
    match &expr.kind {
        TExprKind::Function(def_id, _) => Some(*def_id),
        TExprKind::GenericFunction { def_id, .. } => Some(*def_id),
        TExprKind::FunctionToClosure(inner) | TExprKind::Move(inner) => callee_def_id(inner),
        _ => None,
    }
}

fn token_for_def_kind(kind: &DefKind) -> (u32, u32) {
    match kind {
        DefKind::TypeAlias => (TOK_TYPE, 0),
        DefKind::Struct | DefKind::OpaqueStruct => (TOK_STRUCT, 0),
        DefKind::Enum => (TOK_ENUM, 0),
        DefKind::EnumVariant => (TOK_ENUM_MEMBER, 0),
        DefKind::Interface | DefKind::InterfaceAlias => (TOK_INTERFACE, 0),
        DefKind::Function | DefKind::ExternFunction => (TOK_FUNCTION, 0),
    }
}

fn lexical_tokens(source_map: &SourceMap) -> Vec<TokenFact> {
    let Ok(mut config) = HighlightConfiguration::new(
        crate::tree_sitter_ciel::language(),
        "ciel",
        crate::tree_sitter_ciel::HIGHLIGHTS_QUERY,
        "",
        "",
    ) else {
        return Vec::new();
    };
    config.configure(crate::tree_sitter_ciel::HIGHLIGHT_NAMES);

    let mut highlighter = Highlighter::new();
    let mut out = Vec::new();
    for file in source_map.files() {
        let Ok(events) = highlighter.highlight(&config, file.text.as_bytes(), None, |_| None)
        else {
            continue;
        };
        let mut active = Vec::new();
        for event in events {
            match event {
                Ok(HighlightEvent::HighlightStart(highlight)) => active.push(highlight.0),
                Ok(HighlightEvent::HighlightEnd) => {
                    active.pop();
                }
                Ok(HighlightEvent::Source { start, end }) => {
                    if start == end {
                        continue;
                    }
                    let Some(highlight_index) = active.last().copied() else {
                        continue;
                    };
                    let Some(name) = crate::tree_sitter_ciel::HIGHLIGHT_NAMES.get(highlight_index)
                    else {
                        continue;
                    };
                    let Some((token_type, modifiers)) = tree_sitter_highlight_token(name) else {
                        continue;
                    };
                    out.push(TokenFact {
                        span: Span::new(file.id, start, end),
                        token_type,
                        modifiers,
                    });
                }
                Err(_) => break,
            }
        }
    }
    out
}

fn tree_sitter_highlight_token(name: &str) -> Option<(u32, u32)> {
    match name {
        "keyword" => Some((TOK_KEYWORD, 0)),
        "type.builtin" => Some((TOK_TYPE, MOD_DEFAULT_LIBRARY)),
        "boolean" | "constant.builtin" => Some((TOK_KEYWORD, 0)),
        "number" | "number.float" => Some((TOK_NUMBER, 0)),
        "string" | "string.special" => Some((TOK_STRING, 0)),
        "comment" => Some((TOK_COMMENT, 0)),
        "function" | "function.call" => Some((TOK_FUNCTION, 0)),
        "type" => Some((TOK_TYPE, 0)),
        "type.definition" => Some((TOK_TYPE, MOD_DECLARATION | MOD_DEFINITION)),
        "type.parameter" => Some((TOK_TYPE_PARAMETER, 0)),
        "property.definition" => Some((TOK_PROPERTY, MOD_DECLARATION | MOD_DEFINITION)),
        "property" => Some((TOK_PROPERTY, 0)),
        "constant" => Some((TOK_ENUM_MEMBER, 0)),
        "variable.parameter" => Some((TOK_PARAMETER, 0)),
        "variable" => Some((TOK_VARIABLE, 0)),
        "namespace" => Some((TOK_NAMESPACE, 0)),
        "operator" => Some((TOK_OPERATOR, 0)),
        _ => None,
    }
}

fn binding_declaration_mutability_modifiers(mutability: BindingMutability) -> u32 {
    match mutability {
        BindingMutability::Immutable => MOD_READONLY,
        BindingMutability::Mutable => MOD_MUTABLE,
    }
}

fn binding_reference_mutability_modifiers(mutability: BindingMutability) -> u32 {
    match mutability {
        BindingMutability::Immutable => 0,
        BindingMutability::Mutable => MOD_MUTABLE,
    }
}

fn span_contains(span: Span, file_id: FileId, offset: usize) -> bool {
    span.file == file_id && span.start <= offset && offset <= span.end
}

fn markdown_hover(text: &str) -> HoverContents {
    HoverContents::Scalar(MarkedString::String(text.to_string()))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn normalize_path(path: &Path) -> PathBuf {
    path.components().collect()
}

#[derive(Debug)]
struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (idx, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(idx + 1);
            }
        }
        Self { line_starts }
    }

    fn position(&self, text: &str, byte: usize) -> Option<Position> {
        if byte > text.len() {
            return None;
        }
        let line = match self.line_starts.binary_search(&byte) {
            Ok(line) => line,
            Err(0) => 0,
            Err(line) => line - 1,
        };
        let line_start = self.line_starts[line];
        let slice = text.get(line_start..byte)?;
        let character = slice.encode_utf16().count() as u32;
        Some(Position::new(line as u32, character))
    }

    fn offset(&self, text: &str, position: Position) -> Option<usize> {
        let line_start = *self.line_starts.get(position.line as usize)?;
        let line_end = self
            .line_starts
            .get(position.line as usize + 1)
            .copied()
            .unwrap_or(text.len());
        let line_text = text.get(line_start..line_end)?;
        let mut utf16 = 0u32;
        for (idx, ch) in line_text.char_indices() {
            if utf16 >= position.character {
                return Some(line_start + idx);
            }
            utf16 += ch.len_utf16() as u32;
        }
        Some(line_end)
    }
}

trait TypeDisplayLossy {
    fn from_ast_name_lossy(ty: &Type) -> String;
}

impl TypeDisplayLossy for Ty {
    fn from_ast_name_lossy(ty: &Type) -> String {
        match &ty.kind {
            TypeKind::Named(name, _) => name.display.clone(),
            _ => Ty::from_hir_lossy(ty).to_string(),
        }
    }
}

trait HirTypeToTy {
    fn from_hir_lossy(ty: &Type) -> Ty;
}

impl HirTypeToTy for Ty {
    fn from_hir_lossy(ty: &Type) -> Ty {
        match &ty.kind {
            TypeKind::Hole => Ty::Unknown,
            TypeKind::Never => Ty::Never,
            TypeKind::Void => Ty::Void,
            TypeKind::Primitive(primitive) => match primitive {
                hir::PrimitiveType::Bool => Ty::Bool,
                hir::PrimitiveType::Char => Ty::Char,
                hir::PrimitiveType::I8 => Ty::I8,
                hir::PrimitiveType::I16 => Ty::I16,
                hir::PrimitiveType::I32 => Ty::I32,
                hir::PrimitiveType::I64 => Ty::I64,
                hir::PrimitiveType::U8 => Ty::U8,
                hir::PrimitiveType::U16 => Ty::U16,
                hir::PrimitiveType::U32 => Ty::U32,
                hir::PrimitiveType::U64 => Ty::U64,
                hir::PrimitiveType::Usize => Ty::Usize,
                hir::PrimitiveType::F32 => Ty::F32,
                hir::PrimitiveType::F64 => Ty::F64,
            },
            TypeKind::Named(name, args) => Ty::Named {
                name: name.display.clone(),
                args: args.iter().map(Ty::from_hir_lossy).collect(),
            },
            TypeKind::Pointer {
                nullable,
                mutability,
                inner,
            } => Ty::Pointer {
                nullable: *nullable,
                mutability: *mutability,
                inner: Box::new(Ty::from_hir_lossy(inner)),
            },
            TypeKind::Array { len, elem } => Ty::Array {
                len: *len,
                elem: Box::new(Ty::from_hir_lossy(elem)),
            },
            TypeKind::Slice { mutability, elem } => Ty::Slice {
                mutability: *mutability,
                elem: Box::new(Ty::from_hir_lossy(elem)),
            },
            TypeKind::Function {
                is_unsafe,
                abi,
                ret,
                params,
            } => Ty::Function {
                is_unsafe: *is_unsafe,
                abi: abi.clone(),
                ret: Box::new(Ty::from_hir_lossy(ret)),
                params: params.iter().map(Ty::from_hir_lossy).collect(),
            },
            TypeKind::Closure { ret, params, .. } => Ty::Closure {
                ret: Box::new(Ty::from_hir_lossy(ret)),
                params: params.iter().map(Ty::from_hir_lossy).collect(),
                constraints: Default::default(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_facts_include_type_hole_and_parameter_hints() {
        let path = PathBuf::from("/tmp/ciel_lsp_hints.ciel");
        let source = r#"
            i64 add(i64 lhs, i64 rhs) {
                return lhs + rhs;
            }

            i64 main() {
                _ value = add(1, 2);
                return value;
            }
        "#;
        let options = CompileOptions::new(&path).with_source_override(&path, source);
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let facts = DocumentFacts::from_analysis(analysis);

        assert!(
            facts
                .inlay_hints
                .iter()
                .any(|hint| hint.label == ": i64" && hint.kind == InlayHintKind::TYPE)
        );
        assert!(
            facts
                .inlay_hints
                .iter()
                .any(|hint| hint.label == "lhs:" && hint.kind == InlayHintKind::PARAMETER)
        );
        assert!(
            facts
                .symbols
                .iter()
                .any(|symbol| symbol.hover.contains("i64 add(i64 lhs, i64 rhs)"))
        );
        let uri = Url::from_file_path(&path).expect("file uri");
        let file_id = facts.file_id_for_uri(&uri).expect("file id");
        assert!(facts.tokens.iter().any(|token| {
            token.span.file == file_id
                && token.token_type == TOK_KEYWORD
                && &source[token.span.start..token.span.end] == "return"
        }));

        let lhs_use = source.find("lhs + rhs").expect("lhs use");
        let lhs_position = facts
            .position_for_offset(file_id, lhs_use)
            .expect("lhs position");
        let hover = facts.hover(&uri, lhs_position).expect("lhs hover");
        assert!(format!("{:?}", hover.contents).contains("i64 lhs"));
        let definition = facts
            .definition(&uri, lhs_position)
            .expect("lhs definition");
        assert!(matches!(
            definition,
            lsp_types::GotoDefinitionResponse::Scalar(_)
        ));

        let call_arg = source.find("1, 2").expect("call arg");
        let call_position = facts
            .position_for_offset(file_id, call_arg)
            .expect("call position");
        let signature = facts
            .signature_help(&uri, call_position)
            .expect("signature help");
        assert_eq!(signature.signatures[0].label, "i64 add(i64 lhs, i64 rhs)");
    }

    #[test]
    fn local_binding_hover_shows_mutability() {
        let path = PathBuf::from("/tmp/ciel_lsp_binding_mutability.ciel");
        let source = r#"
            i64 bump(i64 value, i64 @state) {
                i64 total = value;
                i64 @count = state;
                count = count + total;
                return count;
            }

            i64 main() {
                return bump(1, 2);
            }
        "#;
        let options = CompileOptions::new(&path).with_source_override(&path, source);
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let facts = DocumentFacts::from_analysis(analysis);
        let uri = Url::from_file_path(&path).expect("file uri");
        let file_id = facts.file_id_for_uri(&uri).expect("file id");

        let has_token_modifier = |offset: usize, token_type: u32, modifier: u32| {
            facts.tokens.iter().any(|token| {
                token.span.file == file_id
                    && token.span.start == offset
                    && token.token_type == token_type
                    && token.modifiers & modifier != 0
            })
        };
        let lacks_token_modifier = |offset: usize, token_type: u32, modifier: u32| {
            facts.tokens.iter().all(|token| {
                token.span.file != file_id
                    || token.span.start != offset
                    || token.token_type != token_type
                    || token.modifiers & modifier == 0
            })
        };

        let value_decl = source.find("value").expect("value declaration");
        assert!(has_token_modifier(value_decl, TOK_PARAMETER, MOD_READONLY));
        assert!(lacks_token_modifier(value_decl, TOK_PARAMETER, MOD_MUTABLE));

        let state_decl = source.find("@state").expect("state declaration") + 1;
        assert!(has_token_modifier(state_decl, TOK_PARAMETER, MOD_MUTABLE));
        assert!(lacks_token_modifier(
            state_decl,
            TOK_PARAMETER,
            MOD_READONLY
        ));

        let count_decl = source.find("@count").expect("count declaration") + 1;
        assert!(has_token_modifier(count_decl, TOK_VARIABLE, MOD_MUTABLE));
        assert!(lacks_token_modifier(count_decl, TOK_VARIABLE, MOD_READONLY));

        let value_use = source.find("value;").expect("value use");
        let value_position = facts
            .position_for_offset(file_id, value_use)
            .expect("value position");
        let value_hover = facts.hover(&uri, value_position).expect("value hover");
        assert!(format!("{:?}", value_hover.contents).contains("i64 value"));

        let state_use = source.find("state;").expect("state use");
        assert!(has_token_modifier(state_use, TOK_PARAMETER, MOD_MUTABLE));
        let state_position = facts
            .position_for_offset(file_id, state_use)
            .expect("state position");
        let state_hover = facts.hover(&uri, state_position).expect("state hover");
        assert!(format!("{:?}", state_hover.contents).contains("i64 @state"));

        let count_use = source.rfind("count;").expect("count use");
        assert!(has_token_modifier(count_use, TOK_VARIABLE, MOD_MUTABLE));
        let count_position = facts
            .position_for_offset(file_id, count_use)
            .expect("count position");
        let count_hover = facts.hover(&uri, count_position).expect("count hover");
        assert!(format!("{:?}", count_hover.contents).contains("i64 @count"));

        assert!(
            facts
                .symbols
                .iter()
                .any(|symbol| symbol.hover.contains("i64 bump(i64 value, i64 @state)"))
        );

        let call_arg = source.find("1, 2").expect("call arg");
        let call_position = facts
            .position_for_offset(file_id, call_arg)
            .expect("call position");
        let signature = facts
            .signature_help(&uri, call_position)
            .expect("signature help");
        assert_eq!(
            signature.signatures[0].label,
            "i64 bump(i64 value, i64 @state)"
        );
        let parameters = signature.signatures[0]
            .parameters
            .as_ref()
            .expect("signature parameters");
        assert!(
            format!("{:?}", parameters[1].documentation).contains("i64 @state"),
            "mutable parameter documentation should use Ciel binding syntax"
        );
    }

    #[test]
    fn async_function_hover_and_signature_show_async() {
        let path = PathBuf::from("/tmp/ciel_lsp_async_signature.ciel");
        let source = r#"
            async i64 work(i64 @state) {
                return state;
            }

            async i64 main() {
                return await work(1);
            }
        "#;
        let options = CompileOptions::new(&path).with_source_override(&path, source);
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let facts = DocumentFacts::from_analysis(analysis);
        let uri = Url::from_file_path(&path).expect("file uri");
        let file_id = facts.file_id_for_uri(&uri).expect("file id");

        assert!(
            facts
                .symbols
                .iter()
                .any(|symbol| symbol.hover.contains("async i64 work(i64 @state)"))
        );

        let work_use = source.rfind("work(1)").expect("work call");
        let work_position = facts
            .position_for_offset(file_id, work_use)
            .expect("work position");
        let work_hover = facts.hover(&uri, work_position).expect("work hover");
        assert!(format!("{:?}", work_hover.contents).contains("async i64 work(i64 @state)"));

        let arg_position = facts
            .position_for_offset(file_id, source.find("1);").expect("call arg"))
            .expect("arg position");
        let signature = facts
            .signature_help(&uri, arg_position)
            .expect("signature help");
        assert_eq!(signature.signatures[0].label, "async i64 work(i64 @state)");
    }

    #[test]
    fn semantic_tokens_include_contextual_keywords() {
        let path = PathBuf::from("/tmp/ciel_lsp_contextual_keywords.ciel");
        let source = r#"
            async void main() {
                await sleeper();
            }

            async void sleeper() {
                return;
            }
        "#;
        let options = CompileOptions::new(&path).with_source_override(&path, source);
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let facts = DocumentFacts::from_analysis(analysis);
        let uri = Url::from_file_path(&path).expect("file uri");
        let file_id = facts.file_id_for_uri(&uri).expect("file id");

        assert!(facts.tokens.iter().any(|token| {
            token.span.file == file_id
                && token.token_type == TOK_KEYWORD
                && &source[token.span.start..token.span.end] == "async"
        }));
        assert!(facts.tokens.iter().any(|token| {
            token.span.file == file_id
                && token.token_type == TOK_KEYWORD
                && &source[token.span.start..token.span.end] == "await"
        }));
        assert!(facts.tokens.iter().any(|token| {
            token.span.file == file_id
                && token.token_type == TOK_FUNCTION
                && &source[token.span.start..token.span.end] == "sleeper"
        }));
    }

    #[test]
    fn completion_returns_bare_member_and_qualified_candidates() {
        let path = PathBuf::from("/tmp/ciel_lsp_completion.ciel");
        let source = r#"
            enum Status {
                Success,
                Failure,
            }

            struct Packet {
                i64 value;
            }

            i64 load(*const Packet packet) = .load {
                return packet->value;
            }

            i64 add(i64 lhs, i64 rhs) {
                return lhs + rhs;
            }

            i64 main() {
                Packet packet = { value: 1 };
                i64 actor = 2;
                return actor;
            }
        "#;
        let options = CompileOptions::new(&path).with_source_override(&path, source);
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let facts = DocumentFacts::from_analysis(analysis);
        let uri = Url::from_file_path(&path).expect("file uri");
        let file_id = facts.file_id_for_uri(&uri).expect("file id");

        let bare_offset = source.find("return actor").expect("bare completion") + "return ".len();
        let bare_position = facts
            .position_for_offset(file_id, bare_offset)
            .expect("bare position");
        let bare = facts
            .completion(&uri, bare_position)
            .expect("bare completion");
        assert_completion_contains(&bare, "actor");
        assert_completion_contains(&bare, "add");

        let member_offset = source.find("packet->value").expect("member") + "packet->".len();
        let member_position = facts
            .position_for_offset(file_id, member_offset)
            .expect("member position");
        let member = facts
            .completion(&uri, member_position)
            .expect("member completion");
        assert_completion_contains(&member, "value");
        assert_completion_contains(&member, "load");

        let qualified_source = format!("{source}\nStatus::");
        let qualified_path = PathBuf::from("/tmp/ciel_lsp_completion_qualified.ciel");
        let qualified_options = CompileOptions::new(&qualified_path)
            .with_source_override(&qualified_path, qualified_source.clone());
        let qualified_analysis =
            analyze_frontend_lossy(qualified_options).expect("qualified frontend analysis");
        let qualified_facts = DocumentFacts::from_analysis(qualified_analysis);
        let qualified_uri = Url::from_file_path(&qualified_path).expect("qualified uri");
        let qualified_file_id = qualified_facts
            .file_id_for_uri(&qualified_uri)
            .expect("qualified file id");
        let offset = qualified_source
            .rfind("Status::")
            .expect("qualified completion")
            + "Status::".len();
        let position = qualified_facts
            .position_for_offset(qualified_file_id, offset)
            .expect("qualified position");
        let qualified = qualified_facts
            .completion(&qualified_uri, position)
            .expect("qualified completion");
        assert_completion_contains(&qualified, "Success");
        assert_completion_contains(&qualified, "Failure");
    }

    #[test]
    fn goto_definition_covers_aliased_import_function_calls() {
        let main_path = PathBuf::from("/tmp/ciel_lsp_alias_main.ciel");
        let dep_path = PathBuf::from("/tmp/dep.ciel");
        let main_source = r#"
            import ./dep as dep;

            i64 main() {
                return dep::answer();
            }
        "#;
        let dep_source = r#"
            export i64 answer() {
                return 42;
            }
        "#;
        let options = CompileOptions::new(&main_path)
            .with_source_override(&main_path, main_source)
            .with_source_override(&dep_path, dep_source);
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let facts = DocumentFacts::from_analysis(analysis);
        let main_uri = Url::from_file_path(&main_path).expect("main file uri");
        let dep_uri = Url::from_file_path(&dep_path).expect("dep file uri");
        let main_file_id = facts.file_id_for_uri(&main_uri).expect("main file id");
        let dep_file_id = facts.file_id_for_uri(&dep_uri).expect("dep file id");

        let answer_use = main_source.find("answer").expect("answer use");
        let answer_position = facts
            .position_for_offset(main_file_id, answer_use)
            .expect("answer use position");
        let definition = facts
            .definition(&main_uri, answer_position)
            .expect("answer definition");

        let lsp_types::GotoDefinitionResponse::Scalar(location) = definition else {
            panic!("expected scalar definition response");
        };
        assert_eq!(location.uri, dep_uri);

        let answer_def = dep_source.find("answer").expect("answer definition");
        let expected_position = facts
            .position_for_offset(dep_file_id, answer_def)
            .expect("answer definition position");
        assert_eq!(location.range.start, expected_position);
    }

    #[test]
    fn goto_definition_covers_aliased_import_types_and_enum_variants() {
        let main_path = PathBuf::from("/tmp/ciel_lsp_alias_symbols_main.ciel");
        let dep_path = PathBuf::from("/tmp/dep.ciel");
        let main_source = r#"
            import ./dep as dep;

            dep::Count read_count(dep::PaintBox box) {
                dep::Color color = dep::Red;
                switch (color) {
                    case dep::Color::Red:
                        return box.value;
                    case dep::Color::Blue:
                        return 0;
                }
            }
        "#;
        let dep_source = r#"
            export type Count = i64;

            export struct PaintBox {
                i64 value;
            }

            export enum Color {
                Red,
                Blue,
            }
        "#;
        let options = CompileOptions::new(&main_path)
            .with_source_override(&main_path, main_source)
            .with_source_override(&dep_path, dep_source);
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let facts = DocumentFacts::from_analysis(analysis);
        let main_uri = Url::from_file_path(&main_path).expect("main file uri");
        let dep_uri = Url::from_file_path(&dep_path).expect("dep file uri");
        let main_file_id = facts.file_id_for_uri(&main_uri).expect("main file id");
        let dep_file_id = facts.file_id_for_uri(&dep_uri).expect("dep file id");

        assert_definition_start(
            &facts,
            &main_uri,
            main_file_id,
            main_source.find("Count read_count").expect("Count use"),
            &dep_uri,
            dep_file_id,
            dep_source.find("Count").expect("Count definition"),
        );
        assert_definition_start(
            &facts,
            &main_uri,
            main_file_id,
            main_source.find("PaintBox box").expect("PaintBox use"),
            &dep_uri,
            dep_file_id,
            dep_source.find("PaintBox").expect("PaintBox definition"),
        );
        assert_definition_start(
            &facts,
            &main_uri,
            main_file_id,
            main_source.find("Color color").expect("Color type use"),
            &dep_uri,
            dep_file_id,
            dep_source.find("Color").expect("Color definition"),
        );
        assert_definition_start(
            &facts,
            &main_uri,
            main_file_id,
            main_source.find("Red;").expect("bare qualified Red use"),
            &dep_uri,
            dep_file_id,
            dep_source.find("Red").expect("Red definition"),
        );

        let qualified_variant = main_source
            .find("Color::Red")
            .expect("qualified enum variant use");
        assert_definition_start(
            &facts,
            &main_uri,
            main_file_id,
            qualified_variant,
            &dep_uri,
            dep_file_id,
            dep_source.find("Color").expect("Color definition"),
        );
        assert_definition_start(
            &facts,
            &main_uri,
            main_file_id,
            qualified_variant + "Color::".len(),
            &dep_uri,
            dep_file_id,
            dep_source.find("Red").expect("Red definition"),
        );
    }

    fn assert_definition_start(
        facts: &DocumentFacts,
        source_uri: &Url,
        source_file_id: FileId,
        source_offset: usize,
        expected_uri: &Url,
        expected_file_id: FileId,
        expected_offset: usize,
    ) {
        let source_position = facts
            .position_for_offset(source_file_id, source_offset)
            .expect("source position");
        let definition = facts
            .definition(source_uri, source_position)
            .expect("definition");
        let lsp_types::GotoDefinitionResponse::Scalar(location) = definition else {
            panic!("expected scalar definition response");
        };
        assert_eq!(location.uri, *expected_uri);

        let expected_position = facts
            .position_for_offset(expected_file_id, expected_offset)
            .expect("expected definition position");
        assert_eq!(location.range.start, expected_position);
    }

    fn assert_completion_contains(completion: &CompletionResponse, label: &str) {
        let CompletionResponse::Array(items) = completion else {
            panic!("expected completion array");
        };
        assert!(
            items.iter().any(|item| item.label == label),
            "completion should contain `{label}`; got {:?}",
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn receiver_selector_facts_mark_selector_receiver_argument() {
        let path = PathBuf::from("/tmp/ciel_lsp_receiver_selector_hints.ciel");
        let source = r#"
            struct Box {
                i64 value;
            }

            i64 add(i64 lhs, *Box box, i64 rhs) = box.add {
                return box->value + lhs + rhs;
            }

            i64 main() {
                Box box = { value: 3 };
                return box.add(4, 5);
            }
        "#;
        let options = CompileOptions::new(&path).with_source_override(&path, source);
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let facts = DocumentFacts::from_analysis(analysis);
        let uri = Url::from_file_path(&path).expect("file uri");
        let file_id = facts.file_id_for_uri(&uri).expect("file id");
        let call = source.find("box.add(4, 5)").expect("selector call");
        let call_end = call + "box.add(4, 5)".len();

        let hint_labels = facts
            .inlay_hints
            .iter()
            .filter(|hint| {
                hint.position.file == file_id
                    && hint.position.start >= call
                    && hint.position.start <= call_end
                    && hint.kind == InlayHintKind::PARAMETER
            })
            .map(|hint| hint.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(hint_labels, vec!["lhs:", "rhs:"]);

        let selector_position = facts
            .position_for_offset(file_id, call + "box.".len())
            .expect("selector position");
        let hover = facts
            .hover(&uri, selector_position)
            .expect("selector hover");
        let hover_text = format!("{:?}", hover.contents);
        assert!(hover_text.contains("selector call"));
        assert!(hover_text.contains("receiver parameter `*Box box`"));

        let arg_position = facts
            .position_for_offset(file_id, source.find("4, 5").expect("first explicit arg"))
            .expect("arg position");
        let signature = facts
            .signature_help(&uri, arg_position)
            .expect("signature help");
        assert_eq!(
            signature.signatures[0].label,
            "i64 add(i64 lhs, *Box box, i64 rhs) [selector]"
        );
        assert_eq!(signature.active_parameter, Some(0));
        let parameters = signature.signatures[0]
            .parameters
            .as_ref()
            .expect("signature parameters");
        assert_eq!(parameters.len(), 3);
        assert!(matches!(
            parameters[1].label,
            ParameterLabel::Simple(ref label) if label == "box"
        ));
        assert!(
            format!("{:?}", parameters[1].documentation).contains("*Box box")
                && format!("{:?}", parameters[1].documentation).contains("selector receiver"),
            "receiver parameter documentation should mark selector receiver"
        );

        let second_arg_position = facts
            .position_for_offset(file_id, source.find("5);").expect("second explicit arg"))
            .expect("second arg position");
        let second_signature = facts
            .signature_help(&uri, second_arg_position)
            .expect("second signature help");
        assert_eq!(second_signature.active_parameter, Some(2));
    }

    #[test]
    fn receiver_selector_tokens_only_mark_selector_name_as_function() {
        let main_path = PathBuf::from("/tmp/ciel_lsp_receiver_selector_tokens.ciel");
        let dep_path = PathBuf::from("/tmp/selector_symbols.ciel");
        let main_source = r#"
            import ./selector_symbols as symbols;

            i64 main() {
                symbols::Counter @counter = { value: 3 };
                counter.symbols::add(4);
                return counter.symbols::get();
            }
        "#;
        let dep_source = r#"
            export struct Counter {
                i64 value;
            }

            export void counter_add(*Counter counter, i64 amount) = .add {
                counter->value = counter->value + amount;
            }

            export i64 counter_get(*const Counter counter) = .get {
                return counter->value;
            }
        "#;
        let options = CompileOptions::new(&main_path)
            .with_source_override(&main_path, main_source)
            .with_source_override(&dep_path, dep_source);
        let analysis = analyze_frontend_lossy(options).expect("frontend analysis should run");
        let facts = DocumentFacts::from_analysis(analysis);
        let uri = Url::from_file_path(&main_path).expect("file uri");
        let file_id = facts.file_id_for_uri(&uri).expect("file id");
        let call = main_source
            .find("counter.symbols::add(4)")
            .expect("qualified selector call");
        let namespace_start = call + "counter.".len();
        let namespace_end = namespace_start + "symbols".len();
        let selector_start = namespace_end + "::".len();
        let selector_end = selector_start + "add".len();

        assert!(!facts.tokens.iter().any(|token| {
            token.span.file == file_id
                && token.token_type == TOK_FUNCTION
                && token.span.start == namespace_start
                && token.span.end == namespace_end
        }));
        assert!(facts.tokens.iter().any(|token| {
            token.span.file == file_id
                && token.token_type == TOK_FUNCTION
                && token.span.start == selector_start
                && token.span.end == selector_end
        }));
    }
}
