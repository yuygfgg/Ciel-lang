use std::{error::Error, fmt};

use pretty::RcDoc;
use tree_sitter::{Node, Parser, Tree};

use crate::tree_sitter_ciel;

const DEFAULT_LINE_WIDTH: usize = 80;
const DEFAULT_INDENT_WIDTH: usize = 4;
const DEFAULT_CHAIN_CALL_BREAK_THRESHOLD: usize = 3;

#[derive(Clone, Copy, Debug)]
pub struct FormatOptions {
    pub line_width: usize,
    pub indent_width: usize,
    pub chain_call_break_threshold: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            line_width: DEFAULT_LINE_WIDTH,
            indent_width: DEFAULT_INDENT_WIDTH,
            chain_call_break_threshold: DEFAULT_CHAIN_CALL_BREAK_THRESHOLD,
        }
    }
}

#[derive(Debug)]
pub enum FormatError {
    ParserUnavailable,
    ParseError(ParseProblem),
    InvalidTokenRange { start: usize, end: usize },
    Render(std::io::Error),
    InvalidUtf8(std::string::FromUtf8Error),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::ParserUnavailable => write!(f, "Tree-sitter parser did not return a tree"),
            FormatError::ParseError(problem) => write!(
                f,
                "cannot format source with Tree-sitter parse problem `{}` at {}:{}",
                problem.kind,
                problem.row + 1,
                problem.column + 1
            ),
            FormatError::InvalidTokenRange { start, end } => {
                write!(
                    f,
                    "Tree-sitter produced invalid token byte range {start}..{end}"
                )
            }
            FormatError::Render(error) => write!(f, "failed to render formatted document: {error}"),
            FormatError::InvalidUtf8(error) => {
                write!(f, "formatted document was not valid UTF-8: {error}")
            }
        }
    }
}

impl Error for FormatError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            FormatError::Render(error) => Some(error),
            FormatError::InvalidUtf8(error) => Some(error),
            FormatError::ParserUnavailable
            | FormatError::ParseError(_)
            | FormatError::InvalidTokenRange { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParseProblem {
    pub kind: String,
    pub row: usize,
    pub column: usize,
}

#[derive(Clone, Debug)]
struct Token {
    text: String,
    kind: String,
    parent_kind: Option<String>,
    ancestors: Vec<Ancestor>,
    member_call_chain_len: usize,
    matching_index: Option<usize>,
    start_byte: usize,
    end_byte: usize,
    line_breaks_before: usize,
    start_row: usize,
    end_row: usize,
}

#[derive(Clone, Debug)]
struct Ancestor {
    kind: String,
    flat_width: usize,
    head_width: Option<usize>,
    list_content_width: Option<usize>,
    direct_list_multiline: bool,
    nested_list_multiline: bool,
    direct_block_multiline: bool,
    start_byte: usize,
    end_byte: usize,
    binary_precedence: Option<u8>,
    contains_comma: bool,
}

impl Token {
    fn is_comment(&self) -> bool {
        self.kind == "comment"
    }

    fn is_line_comment(&self) -> bool {
        self.text.starts_with("//")
    }

    fn parent_is(&self, kind: &str) -> bool {
        self.parent_kind.as_deref() == Some(kind)
    }

    fn has_ancestor(&self, kind: &str) -> bool {
        self.ancestors.iter().any(|ancestor| ancestor.kind == kind)
    }

    fn ancestor_flat_exceeds_width(&self, kind: &str, line_width: usize) -> bool {
        self.ancestors
            .iter()
            .any(|ancestor| ancestor.kind == kind && ancestor.flat_width > line_width)
    }

    fn nearest_ancestor_flat_width(&self, kind: &str) -> Option<usize> {
        self.ancestors
            .iter()
            .find_map(|ancestor| (ancestor.kind == kind).then_some(ancestor.flat_width))
    }

    fn nearest_ancestor_range(&self, kind: &str) -> Option<(usize, usize)> {
        self.ancestors.iter().find_map(|ancestor| {
            (ancestor.kind == kind).then_some((ancestor.start_byte, ancestor.end_byte))
        })
    }

    fn nearest_ancestor_contains_comma(&self, kind: &str) -> Option<bool> {
        self.ancestors
            .iter()
            .find_map(|ancestor| (ancestor.kind == kind).then_some(ancestor.contains_comma))
    }

    fn nearest_ancestor_head_width(&self, kind: &str) -> Option<usize> {
        self.ancestors.iter().find_map(|ancestor| {
            (ancestor.kind == kind)
                .then_some(ancestor.head_width)
                .flatten()
        })
    }

    fn nearest_ancestor_list_content_width(&self, kind: &str) -> Option<usize> {
        self.ancestors.iter().find_map(|ancestor| {
            (ancestor.kind == kind)
                .then_some(ancestor.list_content_width)
                .flatten()
        })
    }

    fn nearest_ancestor_direct_list_multiline(&self, kind: &str) -> Option<bool> {
        self.ancestors
            .iter()
            .find_map(|ancestor| (ancestor.kind == kind).then_some(ancestor.direct_list_multiline))
    }

    fn nearest_ancestor_nested_list_multiline(&self, kind: &str) -> bool {
        self.ancestors
            .iter()
            .any(|ancestor| ancestor.kind == kind && ancestor.nested_list_multiline)
    }

    fn nearest_ancestor_direct_block_multiline(&self, kind: &str) -> Option<bool> {
        self.ancestors
            .iter()
            .find_map(|ancestor| (ancestor.kind == kind).then_some(ancestor.direct_block_multiline))
    }

    fn count_ancestor(&self, kind: &str) -> usize {
        self.ancestors
            .iter()
            .filter(|ancestor| ancestor.kind == kind)
            .count()
    }

    fn text_is(&self, text: &str) -> bool {
        self.text == text
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Separator {
    None,
    Space,
    Line(usize),
    BlankLine(usize),
}

#[derive(Clone, Debug)]
struct FormatState {
    indent: usize,
    brace_indents: Vec<BraceIndent>,
    list_indents: Vec<ListIndent>,
    current_line_indent: usize,
    current_line_len: usize,
    indent_width: usize,
    line_width: usize,
    chain_call_break_threshold: usize,
}

#[derive(Clone, Debug)]
struct BraceIndent {
    previous_indent: usize,
    closing_indent: usize,
}

#[derive(Clone, Debug)]
struct ListIndent {
    item_indent: usize,
    closing_indent: usize,
    block_indent: usize,
    closing_token_idx: Option<usize>,
}

#[derive(Clone, Debug)]
struct DisabledRange {
    start_idx: usize,
    end_idx: usize,
    raw_start: usize,
    raw_end: usize,
}

impl FormatState {
    fn effective_indent(&self) -> usize {
        self.list_indents.last().map_or(self.indent, |list| {
            list.item_indent + self.indent.saturating_sub(list.block_indent)
        })
    }

    fn available_width(&self) -> usize {
        self.line_width
            .saturating_sub(self.effective_indent() * self.indent_width)
    }

    fn current_available_width(&self) -> usize {
        self.line_width
            .saturating_sub(self.current_line_indent * self.indent_width)
    }

    fn remaining_line_width(&self) -> usize {
        self.line_width.saturating_sub(self.current_line_len)
    }

    fn list_available_width(&self) -> usize {
        self.available_width().saturating_sub(4)
    }

    fn closing_list_indent(&self) -> Option<usize> {
        self.list_indents
            .last()
            .map(|list| list.closing_indent + self.indent.saturating_sub(list.block_indent))
    }

    fn closing_brace_indent(&self) -> Option<usize> {
        self.brace_indents.last().map(|brace| brace.closing_indent)
    }

    fn open_brace(&mut self, align_to_current_line: bool) {
        let previous_indent = self.indent;
        let closing_indent = if align_to_current_line || !self.list_indents.is_empty() {
            self.current_line_indent
        } else {
            previous_indent
        };
        self.brace_indents.push(BraceIndent {
            previous_indent,
            closing_indent,
        });
        self.indent = if align_to_current_line {
            (previous_indent + 1).max(closing_indent + 1)
        } else {
            previous_indent + 1
        };
    }

    fn close_brace(&mut self) {
        if let Some(brace) = self.brace_indents.pop() {
            self.indent = brace.previous_indent;
        } else if self.indent > 0 {
            self.indent -= 1;
        }
    }

    fn open_list(&mut self, closing_token_idx: Option<usize>) {
        self.list_indents.push(ListIndent {
            item_indent: self.current_line_indent + 1,
            closing_indent: self.current_line_indent,
            block_indent: self.indent,
            closing_token_idx,
        });
    }

    fn close_list(&mut self) {
        self.list_indents.pop();
    }

    fn apply_separator(&mut self, separator: Separator, base_indent: usize) {
        match separator {
            Separator::None => {}
            Separator::Space => {
                self.current_line_len += 1;
            }
            Separator::Line(extra) | Separator::BlankLine(extra) => {
                self.current_line_indent = base_indent + extra;
                self.current_line_len = self.current_line_indent * self.indent_width;
            }
        }
    }

    fn push_text(&mut self, text: &str) {
        if let Some(newline) = text.rfind('\n') {
            let trailing = &text[newline + 1..];
            self.current_line_indent =
                trailing.chars().take_while(|ch| *ch == ' ').count() / self.indent_width;
            self.current_line_len = trailing.chars().count();
        } else {
            self.current_line_len += text.chars().count();
        }
    }

    fn line_would_exceed(&self, appended_width: usize) -> bool {
        self.current_line_len + 1 + appended_width > self.line_width
    }
}

pub fn format_source(source: &str, options: FormatOptions) -> Result<String, FormatError> {
    let tree = parse_source(source)?;
    if let Some(problem) = first_parse_problem(tree.root_node()) {
        return Err(FormatError::ParseError(problem));
    }

    let mut tokens = Vec::new();
    collect_tokens(tree.root_node(), source, &mut tokens)?;
    attach_original_line_breaks(source, &mut tokens);
    attach_matching_delimiters(&mut tokens);
    let raw_ranges = raw_format_ranges(source, &tokens);

    let mut state = FormatState {
        indent: 0,
        brace_indents: Vec::new(),
        list_indents: Vec::new(),
        current_line_indent: 0,
        current_line_len: 0,
        indent_width: options.indent_width,
        line_width: options.line_width,
        chain_call_break_threshold: options.chain_call_break_threshold,
    };
    let mut docs = Vec::new();
    let mut previous: Option<&Token> = None;
    let mut next_disabled_range = 0;
    let mut idx = 0;

    while idx < tokens.len() {
        if raw_ranges
            .get(next_disabled_range)
            .is_some_and(|range| range.start_idx == idx)
        {
            let range = &raw_ranges[next_disabled_range];
            let mut raw = String::new();
            if let Some(prev) = previous {
                if prev.end_byte < range.raw_start {
                    raw.push_str(&source[prev.end_byte..range.raw_start]);
                }
            } else if range.raw_start > 0 {
                raw.push_str(&source[..range.raw_start]);
            }
            raw.push_str(&source[range.raw_start..range.raw_end]);
            update_current_line_indent_from_raw(&mut state, &raw);
            docs.push(RcDoc::text(raw));
            for disabled_idx in range.start_idx..=range.end_idx {
                apply_token_state(disabled_idx, &tokens[disabled_idx], &mut state);
            }
            previous = tokens.get(range.end_idx);
            idx = range.end_idx + 1;
            next_disabled_range += 1;
            continue;
        }

        let token = &tokens[idx];
        let closes_list = closes_list_indent(idx, &state);
        let closing_list_indent = if closes_list {
            state.closing_list_indent()
        } else {
            None
        };
        let closing_brace_indent = if token.text_is("}") && closes_indent_context(token, &state) {
            state.closing_brace_indent()
        } else {
            None
        };
        close_token_state(token, closes_list, &mut state);

        if let Some(prev) = previous {
            let separator = preserve_original_blank_lines(
                separator_before(prev, token, &tokens, idx, closes_list, &state),
                token,
            );
            let base_indent = closing_list_indent
                .or(closing_brace_indent)
                .unwrap_or_else(|| separator_base_indent(separator, prev, token, &state));
            docs.push(separator_doc(separator, base_indent, state.indent_width));
            state.apply_separator(separator, base_indent);
        }

        docs.push(RcDoc::text(token.text.clone()));
        state.push_text(&token.text);

        open_token_state(token, &mut state);

        previous = Some(token);
        idx += 1;
    }

    let mut bytes = Vec::new();
    let doc = concat_balanced(docs);
    doc.render(options.line_width, &mut bytes)
        .map_err(FormatError::Render)?;
    let mut formatted = String::from_utf8(bytes).map_err(FormatError::InvalidUtf8)?;
    if !formatted.is_empty() && !formatted.ends_with('\n') {
        formatted.push('\n');
    }

    let formatted_tree = parse_source(&formatted)?;
    if let Some(problem) = first_parse_problem(formatted_tree.root_node()) {
        return Err(FormatError::ParseError(problem));
    }

    Ok(formatted)
}

fn concat_balanced(mut docs: Vec<RcDoc<'static, ()>>) -> RcDoc<'static, ()> {
    if docs.is_empty() {
        return RcDoc::nil();
    }
    while docs.len() > 1 {
        let mut next = Vec::with_capacity(docs.len().div_ceil(2));
        let mut iter = docs.into_iter();
        while let Some(left) = iter.next() {
            if let Some(right) = iter.next() {
                next.push(left.append(right));
            } else {
                next.push(left);
            }
        }
        docs = next;
    }
    docs.pop().unwrap()
}

fn parse_source(source: &str) -> Result<Tree, FormatError> {
    let mut parser = Parser::new();
    let language = tree_sitter_ciel::language();
    parser
        .set_language(&language)
        .expect("Ciel Tree-sitter language should load");
    parser
        .parse(source.as_bytes(), None)
        .ok_or(FormatError::ParserUnavailable)
}

fn collect_tokens(node: Node<'_>, source: &str, out: &mut Vec<Token>) -> Result<(), FormatError> {
    if node.child_count() == 0 {
        let range = node.byte_range();
        if range.start == range.end {
            return Ok(());
        }
        let text = source
            .get(range.clone())
            .ok_or(FormatError::InvalidTokenRange {
                start: range.start,
                end: range.end,
            })?
            .to_string();
        out.push(Token {
            text,
            kind: node.kind().to_string(),
            parent_kind: node.parent().map(|parent| parent.kind().to_string()),
            ancestors: ancestor_kinds(node, source),
            member_call_chain_len: enclosing_member_call_chain_len(node),
            matching_index: None,
            start_byte: range.start,
            end_byte: range.end,
            line_breaks_before: 0,
            start_row: node.start_position().row,
            end_row: node.end_position().row,
        });
        return Ok(());
    }

    let range = node.byte_range();
    let mut last_end = range.start;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_gap_tokens(node, source, last_end, child.start_byte(), out)?;
        collect_tokens(child, source, out)?;
        last_end = child.end_byte();
    }
    collect_gap_tokens(node, source, last_end, range.end, out)?;
    Ok(())
}

fn collect_gap_tokens(
    parent: Node<'_>,
    source: &str,
    start: usize,
    end: usize,
    out: &mut Vec<Token>,
) -> Result<(), FormatError> {
    if start >= end {
        return Ok(());
    }
    let gap = source
        .get(start..end)
        .ok_or(FormatError::InvalidTokenRange { start, end })?;
    let mut token_start = None;
    for (offset, ch) in gap.char_indices() {
        let byte = start + offset;
        if ch.is_whitespace() {
            if let Some(run_start) = token_start.take() {
                push_gap_token(parent, source, run_start, byte, out)?;
            }
        } else if token_start.is_none() {
            token_start = Some(byte);
        }
    }
    if let Some(run_start) = token_start {
        push_gap_token(parent, source, run_start, end, out)?;
    }
    Ok(())
}

fn push_gap_token(
    parent: Node<'_>,
    source: &str,
    start: usize,
    end: usize,
    out: &mut Vec<Token>,
) -> Result<(), FormatError> {
    let text = source
        .get(start..end)
        .ok_or(FormatError::InvalidTokenRange { start, end })?
        .to_string();
    let start_row = byte_row(source, start);
    let end_row = byte_row(source, end);
    out.push(Token {
        text,
        kind: "raw_token".to_string(),
        parent_kind: Some(parent.kind().to_string()),
        ancestors: ancestor_kinds_from(Some(parent), source),
        member_call_chain_len: enclosing_member_call_chain_len(parent),
        matching_index: None,
        start_byte: start,
        end_byte: end,
        line_breaks_before: 0,
        start_row,
        end_row,
    });
    Ok(())
}

fn byte_row(source: &str, byte: usize) -> usize {
    source[..byte].bytes().filter(|byte| *byte == b'\n').count()
}

fn attach_original_line_breaks(source: &str, tokens: &mut [Token]) {
    for idx in 1..tokens.len() {
        let prev_end = tokens[idx - 1].end_byte;
        let start = tokens[idx].start_byte;
        tokens[idx].line_breaks_before = source[prev_end..start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
    }
}

fn disabled_format_ranges(source: &str, tokens: &[Token]) -> Vec<DisabledRange> {
    let mut ranges = Vec::new();
    let mut open = None::<(usize, usize)>;

    for (idx, token) in tokens.iter().enumerate() {
        match format_directive(token) {
            Some(FormatDirective::Off) if open.is_none() => {
                open = Some((idx, protected_range_start(source, token.start_byte)));
            }
            Some(FormatDirective::On) => {
                if let Some((start_idx, raw_start)) = open.take() {
                    ranges.push(DisabledRange {
                        start_idx,
                        end_idx: idx,
                        raw_start,
                        raw_end: token.end_byte,
                    });
                }
            }
            _ => {}
        }
    }

    if let Some((start_idx, raw_start)) = open {
        if let Some(last_idx) = tokens.len().checked_sub(1) {
            ranges.push(DisabledRange {
                start_idx,
                end_idx: last_idx,
                raw_start,
                raw_end: source.len(),
            });
        }
    }

    ranges
}

fn raw_format_ranges(source: &str, tokens: &[Token]) -> Vec<DisabledRange> {
    let mut ranges = disabled_format_ranges(source, tokens);
    ranges.extend(config_format_ranges(source, tokens));
    ranges.sort_by_key(|range| range.start_idx);
    let mut merged = Vec::<DisabledRange>::new();
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            if range.start_idx <= last.end_idx {
                last.end_idx = last.end_idx.max(range.end_idx);
                last.raw_start = last.raw_start.min(range.raw_start);
                last.raw_end = last.raw_end.max(range.raw_end);
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

fn config_format_ranges(source: &str, tokens: &[Token]) -> Vec<DisabledRange> {
    let mut ranges = Vec::new();
    let mut start = None::<(usize, usize)>;
    let mut last_config_idx = None::<usize>;

    for (idx, token) in tokens.iter().enumerate() {
        if token.has_ancestor("config_item") {
            if start.is_none() {
                start = Some((idx, line_start(source, token.start_byte)));
            }
            last_config_idx = Some(idx);
        } else if let (Some((start_idx, raw_start)), Some(end_idx)) =
            (start.take(), last_config_idx.take())
        {
            ranges.push(DisabledRange {
                start_idx,
                end_idx,
                raw_start,
                raw_end: tokens[end_idx].end_byte,
            });
        }
    }

    if let (Some((start_idx, raw_start)), Some(end_idx)) = (start, last_config_idx) {
        ranges.push(DisabledRange {
            start_idx,
            end_idx,
            raw_start,
            raw_end: tokens[end_idx].end_byte,
        });
    }

    ranges
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormatDirective {
    Off,
    On,
}

fn format_directive(token: &Token) -> Option<FormatDirective> {
    if !token.is_comment() {
        return None;
    }
    let text = token.text.to_ascii_lowercase();
    if text.contains("ciel-format off") {
        Some(FormatDirective::Off)
    } else if text.contains("ciel-format on") {
        Some(FormatDirective::On)
    } else {
        None
    }
}

fn protected_range_start(source: &str, byte: usize) -> usize {
    let line_start = line_start(source, byte);
    if source[line_start..byte].chars().all(char::is_whitespace) {
        line_start
    } else {
        byte
    }
}

fn line_start(source: &str, byte: usize) -> usize {
    source[..byte]
        .rfind('\n')
        .map(|newline| newline + 1)
        .unwrap_or(0)
}

fn update_current_line_indent_from_raw(state: &mut FormatState, raw: &str) {
    let Some(last_newline) = raw.rfind('\n') else {
        state.current_line_len += raw.chars().count();
        return;
    };
    let trailing = &raw[last_newline + 1..];
    let spaces = raw[last_newline + 1..]
        .chars()
        .take_while(|ch| *ch == ' ')
        .count();
    state.current_line_indent = spaces / state.indent_width;
    state.current_line_len = trailing.chars().count();
}

fn apply_token_state(idx: usize, token: &Token, state: &mut FormatState) {
    let closes_list = closes_list_indent(idx, state);
    close_token_state(token, closes_list, state);
    open_token_state(token, state);
}

fn close_token_state(token: &Token, closes_list: bool, state: &mut FormatState) {
    if token.text_is("}") && closes_indent_context(token, state) {
        state.close_brace();
    }
    if closes_list {
        state.close_list();
    }
}

fn open_token_state(token: &Token, state: &mut FormatState) {
    if token.text_is("{") && opens_indent_context(token, state) {
        state.open_brace(
            token.parent_is("select_expression")
                || closure_block_on_continuation(token, state)
                || unsafe_block_on_continuation(token, state),
        );
    }
    if opens_list_indent(token, state) {
        state.open_list(token.matching_index);
    }
}

fn attach_matching_delimiters(tokens: &mut [Token]) {
    let mut stack = Vec::<(usize, &'static str)>::new();
    for idx in 0..tokens.len() {
        let text = tokens[idx].text.clone();
        match text.as_str() {
            "(" => stack.push((idx, ")")),
            "[" => stack.push((idx, "]")),
            "{" => stack.push((idx, "}")),
            ")" | "]" | "}" => {
                if let Some((open_idx, _)) = stack
                    .iter()
                    .rposition(|(_, close)| *close == text)
                    .map(|stack_idx| stack.remove(stack_idx))
                {
                    tokens[open_idx].matching_index = Some(idx);
                    tokens[idx].matching_index = Some(open_idx);
                }
            }
            _ => {}
        }
    }
}

fn ancestor_kinds(node: Node<'_>, source: &str) -> Vec<Ancestor> {
    ancestor_kinds_from(node.parent(), source)
}

fn ancestor_kinds_from(mut current: Option<Node<'_>>, source: &str) -> Vec<Ancestor> {
    let mut out = Vec::new();
    while let Some(parent) = current {
        let range = parent.byte_range();
        let text = source.get(range.clone()).unwrap_or("");
        let kind = parent.kind().to_string();
        let binary_precedence = (kind == "binary_expression")
            .then(|| direct_binary_operator(parent, source))
            .flatten()
            .and_then(binary_operator_precedence);
        let flat_width = if kind == "binary_expression" {
            flat_width(text) + binary_operator_spacing_padding(text)
        } else {
            flat_width(text)
        };
        let head_width = ancestor_head_width(parent, source);
        out.push(Ancestor {
            kind,
            flat_width,
            head_width,
            list_content_width: list_content_width(parent.kind(), text),
            direct_list_multiline: direct_list_multiline(parent.kind(), text),
            nested_list_multiline: nested_list_multiline(parent.kind(), text),
            direct_block_multiline: direct_block_multiline(parent.kind(), text),
            start_byte: range.start,
            end_byte: range.end,
            binary_precedence,
            contains_comma: text.contains(','),
        });
        current = parent.parent();
    }
    out
}

fn direct_list_multiline(kind: &str, text: &str) -> bool {
    if !matches!(
        kind,
        "argument_list" | "parameter_list" | "array_literal" | "struct_literal"
    ) {
        return false;
    }

    let mut depth = 0usize;
    let mut newline_after_top_level_marker = false;
    for ch in text.chars() {
        match ch {
            '(' | '[' | '{' => {
                if depth == 0 {
                    newline_after_top_level_marker = true;
                }
                depth += 1;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                newline_after_top_level_marker = false;
            }
            ',' | ';' if depth == 1 => {
                newline_after_top_level_marker = true;
            }
            '\n' if depth == 1 && newline_after_top_level_marker => {
                return true;
            }
            ch if depth == 1 && !ch.is_whitespace() => {
                newline_after_top_level_marker = false;
            }
            _ => {}
        }
    }
    false
}

fn list_content_width(kind: &str, text: &str) -> Option<usize> {
    if !matches!(
        kind,
        "argument_list" | "parameter_list" | "array_literal" | "struct_literal"
    ) {
        return None;
    }

    let mut chars = text.char_indices();
    let (_, open) = chars.next()?;
    if !matches!(open, '(' | '[' | '{') {
        return None;
    }
    let (close_idx, close) = text.char_indices().next_back()?;
    if !matches!(close, ')' | ']' | '}') || close_idx == 0 {
        return None;
    }
    let content_start = open.len_utf8();
    text.get(content_start..close_idx)
        .map(str::trim)
        .map(flat_width)
}

fn nested_list_multiline(kind: &str, text: &str) -> bool {
    if !matches!(
        kind,
        "argument_list" | "parameter_list" | "array_literal" | "struct_literal"
    ) {
        return false;
    }

    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '\n' if depth > 1 => return true,
            _ => {}
        }
    }
    false
}

fn direct_block_multiline(kind: &str, text: &str) -> bool {
    if kind != "unsafe_block_expression" {
        return false;
    }

    let mut depth = 0usize;
    let mut newline_after_block_open = false;
    for ch in text.chars() {
        match ch {
            '(' | '[' | '{' => {
                if ch == '{' && depth == 0 {
                    newline_after_block_open = true;
                }
                depth += 1;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                newline_after_block_open = false;
            }
            '\n' if depth == 1 && newline_after_block_open => {
                return true;
            }
            ch if depth == 1 && !ch.is_whitespace() => {
                newline_after_block_open = false;
            }
            _ => {}
        }
    }
    false
}

fn ancestor_head_width(node: Node<'_>, source: &str) -> Option<usize> {
    match node.kind() {
        "call_expression" => {
            let arguments = node.child_by_field_name("arguments")?;
            source
                .get(node.start_byte()..arguments.start_byte())
                .map(|head| flat_width(head) + "(".len())
        }
        "unsafe_block_expression" => Some("unsafe {".len()),
        "select_expression" => node
            .children(&mut node.walk())
            .find(|child| child.kind() == "{")
            .and_then(|brace| source.get(node.start_byte()..brace.start_byte()))
            .map(|head| flat_width(head) + "{".len()),
        "array_literal" | "struct_literal" => Some(1),
        _ => None,
    }
}

fn direct_binary_operator<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() {
            continue;
        }
        let text = source.get(child.byte_range()).unwrap_or("");
        if binary_operator_precedence(text).is_some() {
            return Some(text);
        }
    }
    None
}

fn flat_width(text: &str) -> usize {
    let mut width = 0;
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = width > 0;
        } else {
            if pending_space {
                width += 1;
                pending_space = false;
            }
            width += 1;
            if ch == ',' {
                pending_space = true;
            }
        }
    }
    width
}

fn binary_operator_spacing_padding(text: &str) -> usize {
    let mut padding = 0;
    let bytes = text.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'"' | b'\'' => {
                let quote = bytes[idx];
                idx += 1;
                while idx < bytes.len() {
                    if bytes[idx] == b'\\' {
                        idx = (idx + 2).min(bytes.len());
                    } else if bytes[idx] == quote {
                        idx += 1;
                        break;
                    } else {
                        idx += 1;
                    }
                }
            }
            b'|' | b'&' | b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'^' => {
                padding += 2;
                idx += if idx + 1 < bytes.len()
                    && matches!(
                        &bytes[idx..idx + 2],
                        b"||" | b"&&" | b"==" | b"!=" | b"<=" | b">=" | b"<<" | b">>"
                    ) {
                    2
                } else {
                    1
                };
            }
            _ => {
                idx += 1;
            }
        }
    }
    padding
}

fn enclosing_member_call_chain_len(node: Node<'_>) -> usize {
    let mut best = 0;
    let mut current = Some(node);
    while let Some(candidate) = current {
        if member_chain_node(candidate.kind()) {
            let root = outer_member_chain_root(candidate);
            best = best.max(member_call_chain_len(root));
        }
        current = candidate.parent();
    }
    best
}

fn outer_member_chain_root(mut node: Node<'_>) -> Node<'_> {
    while let Some(parent) = node.parent() {
        let should_climb = match parent.kind() {
            kind if transparent_expression_node(kind) => only_named_child_matches(parent, node),
            "call_expression" => field_child_matches(parent, "function", node),
            "field_expression"
            | "receiver_selector_expression"
            | "arrow_expression"
            | "index_expression"
            | "slice_expression" => field_child_matches(parent, "object", node),
            "try_expression" => field_child_matches(parent, "value", node),
            _ => false,
        };
        if !should_climb {
            break;
        }
        node = parent;
    }
    node
}

fn member_call_chain_len(node: Node<'_>) -> usize {
    let node = unwrap_transparent_node(node);
    match node.kind() {
        "call_expression" => member_call_chain_len_for_call(node),
        kind if member_access_node(kind) => field_member_call_chain_len(node, "object"),
        "index_expression" | "slice_expression" => field_member_call_chain_len(node, "object"),
        "try_expression" => field_member_call_chain_len(node, "value"),
        _ => 0,
    }
}

fn member_call_chain_len_for_call(node: Node<'_>) -> usize {
    let Some(function) = node.child_by_field_name("function") else {
        return 0;
    };
    let function = unwrap_transparent_node(function);
    if member_access_node(function.kind()) {
        1 + field_member_call_chain_len(function, "object")
    } else {
        member_call_chain_len(function)
    }
}

fn field_member_call_chain_len(node: Node<'_>, field: &str) -> usize {
    node.child_by_field_name(field)
        .map_or(0, member_call_chain_len)
}

fn member_chain_node(kind: &str) -> bool {
    member_access_node(kind)
        || matches!(
            kind,
            "index_expression" | "slice_expression" | "call_expression" | "try_expression"
        )
        || transparent_expression_node(kind)
}

fn member_access_node(kind: &str) -> bool {
    matches!(
        kind,
        "field_expression" | "receiver_selector_expression" | "arrow_expression"
    )
}

fn transparent_expression_node(kind: &str) -> bool {
    matches!(kind, "expression" | "statement_expression")
}

fn unwrap_transparent_node(mut node: Node<'_>) -> Node<'_> {
    while transparent_expression_node(node.kind()) && node.named_child_count() == 1 {
        let Some(child) = node.named_child(0) else {
            break;
        };
        node = child;
    }
    node
}

fn field_child_matches(parent: Node<'_>, field: &str, child: Node<'_>) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|field_child| same_node(field_child, child))
}

fn only_named_child_matches(parent: Node<'_>, child: Node<'_>) -> bool {
    parent.named_child_count() == 1
        && parent
            .named_child(0)
            .is_some_and(|field_child| same_node(field_child, child))
}

fn same_node(left: Node<'_>, right: Node<'_>) -> bool {
    left.kind() == right.kind() && left.byte_range() == right.byte_range()
}

fn first_parse_problem(node: Node<'_>) -> Option<ParseProblem> {
    if node.is_error() || node.is_missing() {
        let position = node.start_position();
        return Some(ParseProblem {
            kind: node.kind().to_string(),
            row: position.row,
            column: position.column,
        });
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(problem) = first_parse_problem(child) {
            return Some(problem);
        }
    }
    None
}

fn separator_doc(separator: Separator, indent: usize, indent_width: usize) -> RcDoc<'static, ()> {
    match separator {
        Separator::None => RcDoc::nil(),
        Separator::Space => RcDoc::space(),
        Separator::Line(extra) => line_doc(indent + extra, indent_width),
        Separator::BlankLine(extra) => lines_doc(2, indent + extra, indent_width),
    }
}

fn line_doc(indent: usize, indent_width: usize) -> RcDoc<'static, ()> {
    lines_doc(1, indent, indent_width)
}

fn lines_doc(count: usize, indent: usize, indent_width: usize) -> RcDoc<'static, ()> {
    let mut doc = RcDoc::nil();
    for _ in 0..count {
        doc = doc.append(RcDoc::hardline());
    }
    doc.append(RcDoc::text(" ".repeat(indent * indent_width)))
}

fn preserve_original_blank_lines(separator: Separator, current: &Token) -> Separator {
    if current.line_breaks_before < 2 {
        return separator;
    }
    Separator::BlankLine(separator_extra(separator))
}

fn separator_extra(separator: Separator) -> usize {
    match separator {
        Separator::Line(extra) | Separator::BlankLine(extra) => extra,
        Separator::None | Separator::Space => 0,
    }
}

fn opens_indent_context(token: &Token, state: &FormatState) -> bool {
    block_like_brace_context(token)
        || unsafe_block_context(token, state)
        || multiline_literal_brace_context(token, state.available_width())
}

fn closes_indent_context(token: &Token, state: &FormatState) -> bool {
    block_like_brace_context(token)
        || unsafe_block_context(token, state)
        || multiline_literal_brace_context(token, state.available_width())
}

fn opens_list_indent(token: &Token, state: &FormatState) -> bool {
    (token.text_is("(")
        && ((token.parent_is("argument_list")
            && multiline_open_list_context(token, "argument_list", state.remaining_line_width()))
            || (token.parent_is("parameter_list")
                && multiline_open_list_context(
                    token,
                    "parameter_list",
                    state.remaining_line_width(),
                ))))
        || (token.text_is("[")
            && token.parent_is("array_literal")
            && multiline_list_context(token, "array_literal", state.list_available_width()))
}

fn closes_list_indent(current_idx: usize, state: &FormatState) -> bool {
    state
        .list_indents
        .last()
        .is_some_and(|list| list.closing_token_idx == Some(current_idx))
}

fn block_like_brace_context(token: &Token) -> bool {
    token.parent_is("block")
        || token.parent_is("struct_body")
        || token.parent_is("enum_body")
        || token.parent_is("extern_block")
        || token.parent_is("switch_statement")
        || token.parent_is("select_expression")
}

fn closure_block_on_continuation(token: &Token, state: &FormatState) -> bool {
    token.has_ancestor("closure_expression")
        && state.list_indents.is_empty()
        && state.current_line_indent > state.indent
}

fn unsafe_block_on_continuation(token: &Token, state: &FormatState) -> bool {
    token.parent_is("unsafe_block_expression")
        && state.list_indents.is_empty()
        && state.current_line_indent > state.indent
}

fn unsafe_block_context(token: &Token, state: &FormatState) -> bool {
    let width_limit = state.available_width().saturating_sub("unsafe { ".len());
    token.parent_is("unsafe_block_expression")
        && (token.nearest_ancestor_direct_block_multiline("unsafe_block_expression") == Some(true)
            || token
                .nearest_ancestor_flat_width("unsafe_block_expression")
                .is_some_and(|width| width > width_limit)
            || (token.nearest_ancestor_contains_comma("unsafe_block_expression") == Some(true)
                && token
                    .nearest_ancestor_flat_width("unsafe_block_expression")
                    .is_some_and(|width| width > state.line_width / 2)))
}

fn multiline_literal_brace_context(token: &Token, line_width: usize) -> bool {
    token.parent_is("struct_literal") && multiline_list_context(token, "struct_literal", line_width)
}

fn inline_brace_context(token: &Token, state: &FormatState) -> bool {
    (token.parent_is("struct_literal")
        && !multiline_list_context(token, "struct_literal", state.available_width()))
        || (token.parent_is("unsafe_block_expression") && !unsafe_block_context(token, state))
}

fn separator_before(
    prev: &Token,
    current: &Token,
    tokens: &[Token],
    current_idx: usize,
    closes_list: bool,
    state: &FormatState,
) -> Separator {
    if current.is_comment() {
        return if prev.is_comment() || prev.text_is("{") || prev.text_is(";") || prev.text_is("}") {
            Separator::Line(0)
        } else {
            Separator::Space
        };
    }
    if prev.is_line_comment() {
        return Separator::Line(0);
    }
    if prev.is_comment() {
        return if prev.end_row < current.start_row {
            Separator::Line(0)
        } else {
            Separator::Space
        };
    }

    if different_top_level_items(prev, current) {
        return line_separator_for(current);
    }

    if prev.text_is("}") && current.text_is("else") {
        return Separator::Space;
    }
    if prev.text_is("}") && current.text_is(";") {
        return Separator::None;
    }
    if prev.text_is("}")
        && prev.parent_is("block")
        && prev.has_ancestor("closure_expression")
        && matches!(current.text.as_str(), ")" | "," | "]")
        && !closes_list
    {
        return Separator::None;
    }
    if prev.text_is("}")
        && prev.parent_is("unsafe_block_expression")
        && !current.text_is(";")
        && closing_or_separator(current)
        && current_line_has_only_token(state, prev)
        && closes_list
    {
        return Separator::Line(0);
    }
    if current.text_is("}") && unsafe_block_context(current, state) {
        return if prev.text_is("{") {
            Separator::None
        } else {
            Separator::Line(0)
        };
    }
    if prev.text_is("}") && inline_brace_context(prev, state) {
        return if current.text_is("}") {
            Separator::Space
        } else if closing_or_separator(current) || current.text_is(")") {
            Separator::None
        } else {
            Separator::Space
        };
    }
    if current.text_is("}") {
        return if prev.text_is("{") {
            Separator::None
        } else if inline_brace_context(current, state) {
            Separator::Space
        } else if case_context_depth(current) > 0 {
            Separator::Line(case_body_extra(current))
        } else {
            Separator::Line(0)
        };
    }
    if prev.text_is("{") {
        if inline_brace_context(prev, state) {
            return Separator::Space;
        }
        if current.text_is("case") || current.text_is("default") {
            return Separator::Line(case_label_extra(current));
        }
        if case_context_depth(current) > 0 {
            return Separator::Line(case_body_extra(current));
        }
        return if block_like_brace_context(prev)
            || unsafe_block_context(prev, state)
            || multiline_literal_brace_context(prev, state.available_width())
        {
            Separator::Line(0)
        } else {
            Separator::Space
        };
    }
    if prev.text_is("}") {
        return if state.indent == 0 {
            Separator::BlankLine(0)
        } else {
            line_separator_for(current)
        };
    }

    if opening_list_should_break(prev, current, state) {
        return Separator::Line(0);
    }
    if closing_list_should_break(prev, current, closes_list, state) {
        return Separator::Line(0);
    }
    if opening_condition_should_break(prev, tokens, current_idx, state) {
        return Separator::Line(1);
    }
    if closing_condition_should_break(current, tokens, current_idx, state) {
        return Separator::Line(0);
    }

    if prev.text_is(";") {
        return if prev.parent_is("array_literal") {
            if current.text_is("]") {
                Separator::None
            } else {
                Separator::Space
            }
        } else if prev.parent_is("for_statement") && !current.text_is(")") {
            Separator::Space
        } else {
            line_separator_for(current)
        };
    }

    if prev.text_is(":") {
        return if prev.parent_is("case_clause")
            || prev.parent_is("default_clause")
            || prev.parent_is("select_arm")
        {
            Separator::Line(case_body_extra(current))
        } else {
            Separator::Space
        };
    }

    if current.text_is(":") || current.text_is(",") || current.text_is(";") {
        return Separator::None;
    }
    if prev.text_is(",") {
        return if comma_should_break(prev, current, state) {
            Separator::Line(0)
        } else {
            Separator::Space
        };
    }
    if binary_operator_should_break(prev, current, tokens, current_idx, state) {
        return if logical_operator_condition_context(prev) {
            Separator::Line(condition_logical_extra(state))
        } else {
            Separator::Line(binary_operator_extra(state))
        };
    }
    if return_expression_should_break(prev, current, state) {
        return Separator::Line(1);
    }

    if current.text_is(".") && should_break_chain(current, state) {
        return Separator::Line(2);
    }
    if prev.text_is("=")
        && (starts_long_chain(current, state) || assignment_rhs_should_break(prev, current, state))
    {
        return Separator::Line(1);
    }

    if no_space_around(prev, current, tokens, current_idx) {
        return Separator::None;
    }

    if needs_operator_space(current) || needs_operator_space(prev) {
        return Separator::Space;
    }
    if current.text_is("as") || prev.text_is("as") {
        return Separator::Space;
    }
    if determined_parameter_separator(current) || determined_parameter_separator(prev) {
        return Separator::Space;
    }

    if keyword_wants_space_after(prev) && !closing_or_separator(current) {
        return Separator::Space;
    }
    if keyword_wants_space_before(current) && !opening_or_accessor(prev) {
        return Separator::Space;
    }

    Separator::Space
}

fn line_separator_for(current: &Token) -> Separator {
    if current.text_is("case") || current.text_is("default") {
        Separator::Line(case_label_extra(current))
    } else if case_context_depth(current) > 0 {
        Separator::Line(case_body_extra(current))
    } else {
        Separator::Line(0)
    }
}

fn different_top_level_items(prev: &Token, current: &Token) -> bool {
    !prev.text_is("}")
        && matches!(
            (
                prev.nearest_ancestor_range("top_level_item"),
                current.nearest_ancestor_range("top_level_item"),
            ),
            (Some(left), Some(right)) if left != right
        )
}

fn separator_base_indent(
    separator: Separator,
    prev: &Token,
    current: &Token,
    state: &FormatState,
) -> usize {
    if matches!(separator, Separator::Line(_) | Separator::BlankLine(_))
        && current.text_is(")")
        && condition_paren_context(current)
    {
        return state.current_line_indent.saturating_sub(1);
    }
    if continuation_separator(separator, prev, current) {
        state.current_line_indent
    } else {
        state.effective_indent()
    }
}

fn continuation_separator(separator: Separator, prev: &Token, current: &Token) -> bool {
    matches!(separator, Separator::Line(_) | Separator::BlankLine(_))
        && (prev.text_is("=")
            || prev.text_is("return")
            || (prev.text_is(",") && !matches!(current.text.as_str(), ")" | "]" | "}"))
            || binary_operator_token(prev)
            || (prev.text_is("(") && condition_paren_context(prev)))
}

fn case_context_depth(token: &Token) -> usize {
    token.count_ancestor("case_clause")
        + token.count_ancestor("default_clause")
        + token.count_ancestor("select_arm")
}

fn case_label_extra(token: &Token) -> usize {
    case_context_depth(token).saturating_sub(1)
}

fn case_body_extra(token: &Token) -> usize {
    case_context_depth(token).max(1)
}

fn opening_list_should_break(prev: &Token, current: &Token, state: &FormatState) -> bool {
    !closing_or_separator(current)
        && ((prev.text_is("(")
            && prev.parent_is("argument_list")
            && multiline_open_list_context(prev, "argument_list", state.remaining_line_width()))
            || (prev.text_is("(")
                && prev.parent_is("parameter_list")
                && multiline_open_list_context(
                    prev,
                    "parameter_list",
                    state.remaining_line_width(),
                ))
            || (prev.text_is("[")
                && prev.parent_is("array_literal")
                && multiline_list_context(prev, "array_literal", state.available_width())))
}

fn closing_list_should_break(
    prev: &Token,
    current: &Token,
    closes_list: bool,
    state: &FormatState,
) -> bool {
    !opening_or_accessor(prev)
        && (closes_list
            || (current.text_is(")")
                && current.parent_is("argument_list")
                && multiline_list_context(current, "argument_list", state.line_width))
            || (current.text_is(")")
                && current.parent_is("parameter_list")
                && multiline_list_context(current, "parameter_list", state.line_width))
            || (current.text_is("]")
                && current.parent_is("array_literal")
                && multiline_list_context(current, "array_literal", state.line_width)))
}

fn comma_should_break(prev: &Token, current: &Token, state: &FormatState) -> bool {
    if current.text_is("}") || current.text_is(")") || current.text_is("]") {
        return false;
    }
    if prev.parent_is("enum_body") {
        return true;
    }
    if prev.parent_is("struct_literal")
        && list_separator_should_break(prev, current, "struct_literal", state.line_width)
    {
        return true;
    }
    if (prev.parent_is("argument_list")
        && list_separator_should_break(prev, current, "argument_list", state.line_width))
        || (prev.parent_is("parameter_list")
            && list_separator_should_break(prev, current, "parameter_list", state.line_width))
        || (prev.parent_is("array_literal")
            && list_separator_should_break(prev, current, "array_literal", state.line_width))
    {
        return true;
    }
    false
}

fn multiline_list_context(token: &Token, kind: &str, line_width: usize) -> bool {
    preserve_existing_multiline_list(token, kind)
        || (list_flat_width_exceeds(token, kind, line_width)
            && !token.nearest_ancestor_nested_list_multiline(kind))
        || (kind == "argument_list"
            && !chain_context(token)
            && token
                .nearest_ancestor_flat_width("call_expression")
                .is_some_and(|width| width > line_width))
        || (kind == "parameter_list"
            && (token
                .nearest_ancestor_flat_width("function_signature")
                .is_some_and(|width| width > line_width)
                || token
                    .nearest_ancestor_flat_width("interface_signature")
                    .is_some_and(|width| width > line_width)))
}

fn multiline_open_list_context(token: &Token, kind: &str, line_width: usize) -> bool {
    preserve_existing_multiline_list(token, kind)
        || token
            .nearest_ancestor_flat_width(kind)
            .is_some_and(|width| width > line_width)
        || (kind == "parameter_list"
            && (token
                .nearest_ancestor_flat_width("function_signature")
                .is_some_and(|width| width > line_width)
                || token
                    .nearest_ancestor_flat_width("interface_signature")
                    .is_some_and(|width| width > line_width)))
}

fn preserve_existing_multiline_list(token: &Token, kind: &str) -> bool {
    token
        .nearest_ancestor_direct_list_multiline(kind)
        .is_some_and(|multiline| multiline)
}

fn list_separator_should_break(
    prev: &Token,
    current: &Token,
    kind: &str,
    line_width: usize,
) -> bool {
    current.line_breaks_before > 0
        || (list_flat_width_exceeds(prev, kind, line_width)
            && !prev.nearest_ancestor_nested_list_multiline(kind))
}

fn list_flat_width_exceeds(token: &Token, kind: &str, line_width: usize) -> bool {
    token
        .nearest_ancestor_list_content_width(kind)
        .or_else(|| token.nearest_ancestor_flat_width(kind))
        .is_some_and(|width| width > line_width)
}

fn opening_condition_should_break(
    prev: &Token,
    tokens: &[Token],
    current_idx: usize,
    state: &FormatState,
) -> bool {
    prev.text_is("(")
        && condition_paren_context(prev)
        && tokens
            .get(current_idx)
            .is_some_and(|current| state.line_would_exceed(expression_head_width(current)))
}

fn closing_condition_should_break(
    current: &Token,
    tokens: &[Token],
    current_idx: usize,
    state: &FormatState,
) -> bool {
    current.text_is(")")
        && condition_paren_context(current)
        && matching_condition_open(tokens, current_idx).is_some_and(|open_idx| {
            condition_head_exceeds_width(tokens, open_idx, condition_available_width(state))
        })
}

fn condition_available_width(state: &FormatState) -> usize {
    state.available_width().saturating_sub(6)
}

fn condition_paren_context(token: &Token) -> bool {
    token.parent_is("if_statement")
        || token.parent_is("while_statement")
        || token.parent_is("switch_statement")
}

fn binary_operator_should_break(
    prev: &Token,
    current: &Token,
    tokens: &[Token],
    current_idx: usize,
    state: &FormatState,
) -> bool {
    prev.parent_is("binary_expression")
        && binary_operator_token(prev)
        && !matches!(prev.text.as_str(), "<<" | ">>")
        && !has_lower_precedence_binary_ancestor(prev)
        && (prev.ancestor_flat_exceeds_width("binary_expression", state.current_available_width())
            || current
                .ancestor_flat_exceeds_width("binary_expression", state.current_available_width()))
        && !should_defer_binary_break(prev, tokens, current_idx, state)
}

fn logical_operator_condition_context(token: &Token) -> bool {
    token.has_ancestor("if_statement")
        || token.has_ancestor("while_statement")
        || token.has_ancestor("switch_statement")
}

fn condition_logical_extra(state: &FormatState) -> usize {
    if state.current_line_indent <= state.indent {
        1
    } else {
        0
    }
}

fn binary_operator_extra(state: &FormatState) -> usize {
    if state.current_line_indent <= state.indent {
        1
    } else {
        0
    }
}

fn return_expression_should_break(prev: &Token, current: &Token, state: &FormatState) -> bool {
    prev.text_is("return") && state.line_would_exceed(expression_head_width(current))
}

fn expression_head_width(token: &Token) -> usize {
    const HEAD_EXPR_KINDS: &[&str] = &[
        "call_expression",
        "unsafe_block_expression",
        "select_expression",
        "array_literal",
        "struct_literal",
    ];

    HEAD_EXPR_KINDS
        .iter()
        .find_map(|kind| token.nearest_ancestor_head_width(kind))
        .unwrap_or_else(|| token.text.chars().count())
}

fn condition_head_exceeds_width(tokens: &[Token], open_idx: usize, line_width: usize) -> bool {
    tokens
        .get(open_idx + 1)
        .is_some_and(|token| expression_head_width(token) > line_width)
}

fn has_lower_precedence_binary_ancestor(token: &Token) -> bool {
    let Some(precedence) = binary_operator_precedence(token.text.as_str()) else {
        return false;
    };
    token.ancestors.iter().skip(1).any(|ancestor| {
        ancestor.kind == "binary_expression"
            && ancestor
                .binary_precedence
                .is_some_and(|ancestor_precedence| ancestor_precedence < precedence)
    })
}

fn should_defer_binary_break(
    operator: &Token,
    tokens: &[Token],
    current_idx: usize,
    state: &FormatState,
) -> bool {
    if operator.end_row < tokens[current_idx].start_row {
        return false;
    }
    let Some(operator_idx) = current_idx.checked_sub(1) else {
        return false;
    };
    let Some(precedence) = binary_operator_precedence(operator.text.as_str()) else {
        return false;
    };
    let Some(group) = binary_break_group_range(operator, precedence) else {
        return false;
    };
    if inline_suffix_fits(tokens, current_idx, group, state) {
        return true;
    }
    let group_mid = group.0 + (group.1 - group.0) / 2;
    let current_distance = byte_distance(operator.start_byte, group_mid);
    let mut inline_width = 0usize;

    for idx in current_idx..tokens.len() {
        let token = &tokens[idx];
        if token.start_byte >= group.1 {
            break;
        }
        inline_width += inline_token_width(token);
        if binary_operator_precedence(token.text.as_str()) == Some(precedence)
            && same_binary_break_group(token, group, precedence)
        {
            let next_distance = byte_distance(token.start_byte, group_mid);
            let reaches_next = state.current_line_len + 1 + inline_width <= state.line_width;
            if reaches_next && next_distance < current_distance {
                return true;
            }
            break;
        }
        if idx > operator_idx && idx + 1 < tokens.len() {
            inline_width += inline_separator_width(token, &tokens[idx + 1]);
        }
    }

    false
}

fn inline_suffix_fits(
    tokens: &[Token],
    start_idx: usize,
    group: (usize, usize),
    state: &FormatState,
) -> bool {
    let mut width = 0usize;
    for idx in start_idx..tokens.len() {
        let token = &tokens[idx];
        if token.start_byte >= group.1 {
            break;
        }
        width += inline_token_width(token);
        if idx + 1 < tokens.len() && tokens[idx + 1].start_byte < group.1 {
            width += inline_separator_width(token, &tokens[idx + 1]);
        }
    }
    state.current_line_len + 1 + width <= state.line_width
}

fn binary_break_group_range(token: &Token, precedence: u8) -> Option<(usize, usize)> {
    let mut seen_binary = false;
    let mut group = None;
    for ancestor in &token.ancestors {
        if ancestor.kind == "binary_expression" {
            if ancestor.binary_precedence == Some(precedence) {
                seen_binary = true;
                group = Some((ancestor.start_byte, ancestor.end_byte));
                continue;
            }
            if seen_binary {
                break;
            }
        } else if seen_binary && !transparent_expression_node(&ancestor.kind) {
            break;
        }
    }
    group
}

fn same_binary_break_group(token: &Token, group: (usize, usize), precedence: u8) -> bool {
    binary_break_group_range(token, precedence) == Some(group)
}

fn byte_distance(left: usize, right: usize) -> usize {
    left.max(right) - left.min(right)
}

fn inline_token_width(token: &Token) -> usize {
    token.text.chars().count()
}

fn inline_separator_width(prev: &Token, current: &Token) -> usize {
    if current.text_is(",") || current.text_is(";") || current.text_is(":") {
        0
    } else if prev.text_is(",") || needs_operator_space(prev) || needs_operator_space(current) {
        1
    } else if no_inline_space_around(prev, current) {
        0
    } else {
        1
    }
}

fn no_inline_space_around(prev: &Token, current: &Token) -> bool {
    current.text_is(")")
        || current.text_is("]")
        || current.text_is("?")
        || prev.text_is("(")
        || prev.text_is("[")
        || prev.text_is(".")
        || prev.text_is("::")
        || prev.text_is("->")
        || current.text_is(".")
        || current.text_is("::")
        || current.text_is("->")
        || current.text_is("..")
        || prev.text_is("..")
        || deref_operator_token(prev)
        || (prev.parent_is("unary_expression")
            && matches!(prev.text.as_str(), "!" | "~" | "-" | "&"))
}

fn binary_operator_precedence(operator: &str) -> Option<u8> {
    match operator {
        "||" => Some(1),
        "&&" => Some(2),
        "|" => Some(3),
        "^" => Some(4),
        "&" => Some(5),
        "==" | "!=" => Some(6),
        "<" | "<=" | ">" | ">=" => Some(7),
        "<<" | ">>" => Some(8),
        "+" | "-" => Some(9),
        "*" | "/" | "%" => Some(10),
        _ => None,
    }
}

fn binary_operator_token(token: &Token) -> bool {
    binary_operator_precedence(token.text.as_str()).is_some()
}

fn matching_condition_open(tokens: &[Token], close_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    for idx in (0..=close_idx).rev() {
        let token = &tokens[idx];
        if token.text_is(")") {
            depth += 1;
        } else if token.text_is("(") {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(idx);
            }
        }
    }
    None
}

fn should_break_chain(current: &Token, state: &FormatState) -> bool {
    chain_context(current) && current.member_call_chain_len >= state.chain_call_break_threshold
}

fn starts_long_chain(token: &Token, state: &FormatState) -> bool {
    token.member_call_chain_len >= state.chain_call_break_threshold
}

fn assignment_rhs_should_break(prev: &Token, current: &Token, state: &FormatState) -> bool {
    if current.text_is("[") || current.text_is("{") {
        return false;
    }

    const ASSIGNMENT_KINDS: &[&str] = &[
        "var_declaration_clause",
        "pointer_var_declaration_clause",
        "assignment_statement",
        "deref_assignment_statement",
    ];

    ASSIGNMENT_KINDS.iter().any(|kind| {
        prev.has_ancestor(kind) && state.line_would_exceed(expression_head_width(current))
    })
}

fn chain_context(token: &Token) -> bool {
    token.has_ancestor("receiver_selector_expression") || token.has_ancestor("field_expression")
}

fn current_line_has_only_token(state: &FormatState, token: &Token) -> bool {
    state.current_line_len
        == state.current_line_indent * state.indent_width + token.text.chars().count()
}

fn no_space_around(prev: &Token, current: &Token, tokens: &[Token], current_idx: usize) -> bool {
    if generic_call_angle_spacing(prev, current, tokens, current_idx) {
        return true;
    }
    if determined_parameter_separator(prev) || determined_parameter_separator(current) {
        return false;
    }
    if prev.text_is("import") && current.has_ancestor("module_path") {
        return false;
    }
    if prev.has_ancestor("module_path") && current.has_ancestor("module_path") {
        return true;
    }
    if prev.has_ancestor("qualified_name") && current.has_ancestor("qualified_name") {
        return true;
    }
    if prev.has_ancestor("binding_name") && current.has_ancestor("binding_name") {
        return true;
    }
    if prev.text_is("|(") || current.text_is(")|") {
        return true;
    }
    if pointer_constructor_token(prev) {
        return !prev.text.contains("const");
    }
    if prev.text_is("|")
        && current.text_is("|")
        && prev.has_ancestor("closure_suffix")
        && current.has_ancestor("closure_suffix")
    {
        return false;
    }
    if pointer_constructor_context(prev) {
        if prev.text_is("as") {
            return false;
        }
        return !(prev.text.contains("const") && !pointer_constructor_context(current));
    }
    if angle_list_boundary(prev, current) {
        return true;
    }
    if prev.has_ancestor("array_type") && current.has_ancestor("array_type") {
        return prev.text_is("[") || current.text_is("]") || prev.text_is("]");
    }
    if prev.has_ancestor("slice_type") && current.has_ancestor("slice_type") {
        return prev.text_is("[") || current.text_is("]") || prev.text_is("]");
    }
    if prev.parent_is("closure_intro") && prev.text_is("|") && current.has_ancestor("closure_body")
    {
        return false;
    }
    if prev.has_ancestor("closure_intro")
        && current.has_ancestor("closure_intro")
        && (prev.text_is("|") || current.text_is("|"))
    {
        return true;
    }
    if prev.has_ancestor("closure_suffix")
        && current.has_ancestor("closure_suffix")
        && (prev.text_is("|") || current.text_is("|"))
    {
        return true;
    }
    if deref_operator_token(prev) {
        return true;
    }

    if current.text_is(")") || current.text_is("]") || current.text_is("?") {
        return true;
    }
    if current.text_is(".") && current.has_ancestor("module_path") {
        return false;
    }
    if (current.text_is(".") || current.text_is("::") || current.text_is("->"))
        && !needs_operator_space(prev)
    {
        return true;
    }
    if prev.text_is("(")
        || prev.text_is("[")
        || prev.text_is(".")
        || prev.text_is("::")
        || prev.text_is("->")
        || prev.text_is("@")
    {
        return true;
    }
    if current.text_is("..") || prev.text_is("..") {
        return true;
    }
    if prev.parent_is("unary_expression") && matches!(prev.text.as_str(), "!" | "~" | "-" | "&") {
        return true;
    }
    if prev.text_is("!") && (prev.parent_is("interface_term") || prev.parent_is("constraint_term"))
    {
        return true;
    }
    if current.text_is("(")
        && !(prev.text_is("if")
            || prev.text_is("while")
            || prev.text_is("for")
            || prev.text_is("switch"))
        && !prev.text_is("return")
        && !needs_operator_space(prev)
    {
        return true;
    }
    if current.text_is("[") && !keyword_wants_space_after(prev) && !needs_operator_space(prev) {
        return true;
    }
    if current.text_is("<") && !current.parent_is("binary_expression") {
        return true;
    }
    if current.text_is(">") && !current.parent_is("binary_expression") {
        return true;
    }

    false
}

fn angle_list_boundary(prev: &Token, current: &Token) -> bool {
    const LIST_KINDS: &[&str] = &[
        "type_argument_list",
        "generic_item_type_argument_list",
        "generic_parameter_list",
        "interface_generic_parameter_list",
        "constraint_argument_list",
        "constraint_binding",
    ];

    let in_angle_list = LIST_KINDS
        .iter()
        .any(|kind| prev.has_ancestor(kind) || current.has_ancestor(kind));
    in_angle_list
        && (prev.text_is("<")
            || current.text_is("<")
            || current.text_is(">")
            || (prev.text_is(">")
                && (current.text_is(">")
                    || current.text_is("(")
                    || current.text_is(")")
                    || current.text_is(",")
                    || current.text_is(";")
                    || current.text_is(".")
                    || current.text_is("::")
                    || current.text_is("->")))
            || current.text_is(","))
}

fn pointer_constructor_text(token: &Token) -> bool {
    matches!(token.text.as_str(), "*" | "*const" | "?*" | "?*const")
}

fn pointer_constructor_token(token: &Token) -> bool {
    pointer_constructor_text(token) && pointer_constructor_context(token)
}

fn pointer_constructor_context(token: &Token) -> bool {
    token.kind == "pointer_constructor"
        || token.parent_is("pointer_constructor")
        || token.has_ancestor("pointer_constructor")
}

fn deref_operator_token(token: &Token) -> bool {
    token.text_is("*")
        && (token.parent_is("deref_expression")
            || token.parent_is("deref_assignment_target")
            || token.has_ancestor("deref_assignment_target"))
}

fn determined_parameter_separator(token: &Token) -> bool {
    token.text_is("->") && token.parent_is("determined_parameter_separator")
}

fn generic_call_angle_spacing(
    prev: &Token,
    current: &Token,
    tokens: &[Token],
    current_idx: usize,
) -> bool {
    if current.text_is("<") {
        return angle_open_is_generic_call(tokens, current_idx);
    }
    if prev.text_is("<") {
        return angle_open_is_generic_call(tokens, current_idx - 1);
    }
    if current.text_is(">") {
        return angle_close_is_generic_call(tokens, current_idx)
            || tokens
                .get(current_idx + 1)
                .is_some_and(|next| next.text_is(">"));
    }
    if prev.text_is(">") && current.text_is("(") {
        return angle_close_is_generic_call(tokens, current_idx - 1);
    }
    false
}

fn angle_open_is_generic_call(tokens: &[Token], open_idx: usize) -> bool {
    if !tokens.get(open_idx).is_some_and(|token| token.text_is("<")) {
        return false;
    }
    matching_angle_close(tokens, open_idx)
        .is_some_and(|idx| tokens.get(idx + 1).is_some_and(|token| token.text_is("(")))
}

fn matching_angle_close(tokens: &[Token], open_idx: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (idx, token) in tokens.iter().enumerate().skip(open_idx) {
        if token.text_is("<") {
            depth += 1;
        } else if token.text_is(">") {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(idx);
            }
        } else if token.text_is(";") || token.text_is("{") || token.text_is("}") {
            return None;
        }
    }
    None
}

fn angle_close_is_generic_call(tokens: &[Token], close_idx: usize) -> bool {
    if !tokens
        .get(close_idx)
        .is_some_and(|token| token.text_is(">"))
        || !tokens
            .get(close_idx + 1)
            .is_some_and(|token| token.text_is("("))
    {
        return false;
    }

    let mut depth = 0usize;
    for idx in (0..=close_idx).rev() {
        let token = &tokens[idx];
        if token.text_is(">") {
            depth += 1;
        } else if token.text_is("<") {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return idx > 0 && generic_callee_token(&tokens[idx - 1]);
            }
        } else if token.text_is(";") || token.text_is("{") || token.text_is("}") {
            return false;
        }
    }
    false
}

fn generic_callee_token(token: &Token) -> bool {
    token.kind == "regular_identifier"
        || token.text_is(")")
        || token.text_is("]")
        || token.has_ancestor("qualified_name")
}

fn needs_operator_space(token: &Token) -> bool {
    if token.text_is("=") {
        return true;
    }
    if token.parent_is("binary_expression") {
        return matches!(
            token.text.as_str(),
            "||" | "&&"
                | "|"
                | "^"
                | "&"
                | "=="
                | "!="
                | "<"
                | "<="
                | ">"
                | ">="
                | "<<"
                | ">>"
                | "+"
                | "-"
                | "*"
                | "/"
                | "%"
        );
    }
    false
}

fn keyword_wants_space_after(token: &Token) -> bool {
    matches!(
        token.text.as_str(),
        "import"
            | "as"
            | "export"
            | "resource"
            | "unsafe"
            | "extern"
            | "noescape"
            | "opaque"
            | "struct"
            | "enum"
            | "interface"
            | "impl"
            | "derivable"
            | "derive"
            | "async"
            | "type"
            | "return"
            | "defer"
            | "if"
            | "else"
            | "while"
            | "for"
            | "switch"
            | "case"
            | "select"
            | "biased"
            | "#if"
            | "#elif"
            | "#c_include"
    )
}

fn keyword_wants_space_before(token: &Token) -> bool {
    matches!(token.text.as_str(), "else" | "as" | "fn")
}

fn closing_or_separator(token: &Token) -> bool {
    matches!(token.text.as_str(), ")" | "]" | "}" | "," | ";" | ":" | "?")
}

fn opening_or_accessor(token: &Token) -> bool {
    matches!(token.text.as_str(), "(" | "[" | "." | "::" | "->" | "@")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_simple_function_demo() {
        let input = "i64 add(i64 a,i64 b){return a+b;}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(formatted, "i64 add(i64 a, i64 b) {\n    return a + b;\n}\n");
    }

    #[test]
    fn formats_if_else_demo() {
        let input = "i64 pick(bool flag){if(flag){return 1;}else{return 2;}}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "i64 pick(bool flag) {\n    if (flag) {\n        return 1;\n    } else {\n        return 2;\n    }\n}\n"
        );
    }

    #[test]
    fn preserves_multiline_logical_condition() {
        let input = "void f(){if(first_really_long_condition_name_with_suffix||second_really_long_condition_name_with_suffix){return;}}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    if (first_really_long_condition_name_with_suffix ||\n        second_really_long_condition_name_with_suffix) {\n        return;\n    }\n}\n"
        );
    }

    #[test]
    fn keeps_short_logical_condition_inline() {
        let input = "void f(){if(\nwinner<1||\nwinner>4\n){return;}}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    if (winner < 1 || winner > 4) {\n        return;\n    }\n}\n"
        );
    }

    #[test]
    fn formatting_is_idempotent_for_demo_case() {
        let input = "struct Box{[20]char name;[]const u8 bytes;}\n";
        let once = format_source(input, FormatOptions::default()).unwrap();
        let twice = format_source(&once, FormatOptions::default()).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn formats_receiver_chain_on_continuation_lines() {
        let input =
            "void f(){Result<Vec<i64>, Error> r=raw[..].iter().filter(even).map(add).collect();}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    Result<Vec<i64>, Error> r =\n        raw[..]\n            .iter()\n            .filter(even)\n            .map(add)\n            .collect();\n}\n"
        );
    }

    #[test]
    fn preserves_multiline_assignment_rhs_break() {
        let input = "void f(){Result<Vec<i64>,VecError> r=\ncollect<Vec<i64>>(map(filter(slice_iter(raw[..]),even),add));}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    Result<Vec<i64>, VecError> r = collect<Vec<i64>>(\n        map(filter(slice_iter(raw[..]), even), add)\n    );\n}\n"
        );
        let second = format_source(&formatted, FormatOptions::default()).unwrap();
        assert_eq!(formatted, second);
    }

    #[test]
    fn keeps_short_member_accesses_inside_calls_inline() {
        let input = "void f(){x(selector_collected.slice().len);y(&local.read);}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    x(selector_collected.slice().len);\n    y(&local.read);\n}\n"
        );
    }

    #[test]
    fn formats_chained_call_multiline_arguments_from_chain_indent() {
        let input = "void f(){i64 v=range(0,6).filter(even).map(add).take(2).fold(\n0,\n|i64 acc,i64 value|{return acc+value;}\n);}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    i64 v =\n        range(0, 6)\n            .filter(even)\n            .map(add)\n            .take(2)\n            .fold(\n                0,\n                |i64 acc, i64 value| {\n                    return acc + value;\n                }\n            );\n}\n"
        );
    }

    #[test]
    fn formats_only_direct_multiline_argument_lists() {
        let input = "void f(){x(\na,\nfoo(1,2)\n);}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    x(\n        a,\n        foo(1, 2)\n    );\n}\n"
        );
    }

    #[test]
    fn line_width_breaks_long_argument_lists() {
        let input = "void f(){x(alpha,beta,gamma,delta);}";
        let formatted = format_source(
            input,
            FormatOptions {
                line_width: 24,
                ..FormatOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    x(\n        alpha,\n        beta,\n        gamma,\n        delta\n    );\n}\n"
        );
    }

    #[test]
    fn formats_multiline_array_literal_from_opening_indent() {
        let input = "void f(){[4]u8 data=[\n1 as u8,\n2 as u8\n];}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    [4]u8 data = [\n        1 as u8,\n        2 as u8\n    ];\n}\n"
        );
    }

    #[test]
    fn formats_generic_slice_element_type_without_angle_spaces() {
        let input = "void f(){[]const vec::Vec<i64> outer=decoded.vec::slice();}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    []const vec::Vec<i64> outer = decoded.vec::slice();\n}\n"
        );
    }

    #[test]
    fn formats_closure_suffix_and_pointer_cast_spacing() {
        let input = "void f(\nFuture<Result<R,E>> |(TaskGroup<T,E>)| body\n){raw as*CielAsyncOp;?* CielAsyncOp ptr;}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f(\n    Future<Result<R, E>> |(TaskGroup<T, E>)| body\n) {\n    raw as *CielAsyncOp;\n    ?*CielAsyncOp ptr;\n}\n"
        );
    }

    #[test]
    fn keeps_space_before_deref_expression() {
        let input = "bool f(*const i64 ptr){i64 value=*ptr;*ptr=*ptr;return*ptr==*ptr;}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "bool f(*const i64 ptr) {\n    i64 value = *ptr;\n    *ptr = *ptr;\n    return *ptr == *ptr;\n}\n"
        );
    }

    #[test]
    fn keeps_space_before_pointer_type_alias_target() {
        let input = "type c_string=*char; type const_c_string=*const char;";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "type c_string = *char;\ntype const_c_string = *const char;\n"
        );
    }

    #[test]
    fn keeps_space_between_adjacent_closure_suffixes() {
        let input = "struct Holder<T>{T value;} T |()| |(Holder<T>)| make_factory<T>();";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "struct Holder<T> {\n    T value;\n}\n\nT |()| |(Holder<T>)| make_factory<T>();\n"
        );
    }

    #[test]
    fn keeps_short_closure_return_inline() {
        let input = "i64 |()| make_reader(*const i64 ptr){return\n||{return *ptr;};}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "i64 |()| make_reader(*const i64 ptr) {\n    return || {\n        return *ptr;\n    };\n}\n"
        );
    }

    #[test]
    fn keeps_return_call_head_inline_when_arguments_break() {
        let input = "Result<void,E> f(){return Err(error_at(wire::InvalidValue,offset,\"unicode codepoint out of range\"));}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "Result<void, E> f() {\n    return Err(\n        error_at(wire::InvalidValue, offset, \"unicode codepoint out of range\")\n    );\n}\n"
        );
        let second = format_source(&formatted, FormatOptions::default()).unwrap();
        assert_eq!(formatted, second);
    }

    #[test]
    fn separates_closing_call_after_multiline_unsafe_argument() {
        let input = "Result<void,E> f(){return check(unsafe{ciel_sqlite_exec(raw_connection(connection)?,sql.ptr,sql.len)});}";
        let formatted = format_source(
            input,
            FormatOptions {
                line_width: 48,
                ..FormatOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            formatted,
            "Result<void, E> f() {\n    return check(\n        unsafe {\n            ciel_sqlite_exec(\n                raw_connection(connection)?, sql.ptr, sql.len\n            )\n        }\n    );\n}\n"
        );
        let second = format_source(
            &formatted,
            FormatOptions {
                line_width: 48,
                ..FormatOptions::default()
            },
        )
        .unwrap();
        assert_eq!(formatted, second);
    }

    #[test]
    fn keeps_short_try_assignment_inline() {
        let input = "Result<i64,Error> five(); i64 f(){i64 value=\nfive()?;return value;}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "Result<i64, Error> five();\ni64 f() {\n    i64 value = five()?;\n    return value;\n}\n"
        );
    }

    #[test]
    fn keeps_short_closure_assignment_header_inline() {
        let input = "i64 main(){Result<i64,Error> |()| callback=||{return Ok(5);};return 0;}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "i64 main() {\n    Result<i64, Error> |()| callback = || {\n        return Ok(5);\n    };\n    return 0;\n}\n"
        );
    }

    #[test]
    fn keeps_inline_closure_call_argument_closing_stable() {
        let input = "async::Task<usize,Error> f(){return must(async::spawn<usize,Error>(async ||{return Ok(0);}));}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "async::Task<usize, Error> f() {\n    return must(async::spawn<usize, Error>(async || {\n        return Ok(0);\n    }));\n}\n"
        );
        let second = format_source(&formatted, FormatOptions::default()).unwrap();
        assert_eq!(formatted, second);
    }

    #[test]
    fn keeps_boundary_width_multiline_parameter_list_stable() {
        let input = "Result<i64,Error> visit(Slot<readable_seekable> slot,Result<i64,Error> |(readable_seekable)| callback){return callback(slot);}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "Result<i64, Error> visit(\n    Slot<readable_seekable> slot, Result<i64, Error> |(readable_seekable)| callback\n) {\n    return callback(slot);\n}\n"
        );
        let second = format_source(&formatted, FormatOptions::default()).unwrap();
        assert_eq!(formatted, second);
    }

    #[test]
    fn keeps_multiline_unsafe_assignment_header_inline() {
        let input = "void f(){c::c_int rc=unsafe{ciel_sqlite_open(\npath.ptr,\npath.len,\nopen_mode_code(mode),\n&owner_id,\n&resource_id,\n&generation\n)};}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    c::c_int rc = unsafe {\n        ciel_sqlite_open(\n            path.ptr,\n            path.len,\n            open_mode_code(mode),\n            &owner_id,\n            &resource_id,\n            &generation\n        )\n    };\n}\n"
        );
    }

    #[test]
    fn keeps_struct_literal_with_nested_multiline_value_stable() {
        let input = "Item f(){return unsafe{{inner: read_exact_buffered_inner(\nread_exact_buffered_async(reader,len)\n)}};}";
        let formatted = format_source(
            input,
            FormatOptions {
                line_width: 64,
                ..FormatOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            formatted,
            "Item f() {\n    return unsafe {\n        { inner: read_exact_buffered_inner(\n            read_exact_buffered_async(reader, len)\n        ) }\n    };\n}\n"
        );
        let second = format_source(
            &formatted,
            FormatOptions {
                line_width: 64,
                ..FormatOptions::default()
            },
        )
        .unwrap();
        assert_eq!(formatted, second);
    }

    #[test]
    fn indents_broken_unsafe_assignment_body_from_continuation() {
        let input = "void f(){ExtremelyLongResultTypeNameForFormatting value=unsafe{do_work(\nalpha,\nbeta\n)};}";
        let formatted = format_source(
            input,
            FormatOptions {
                line_width: 56,
                ..FormatOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    ExtremelyLongResultTypeNameForFormatting value =\n        unsafe {\n            do_work(\n                alpha,\n                beta\n            )\n        };\n}\n"
        );
    }

    #[test]
    fn keeps_space_between_closure_intro_and_expression_body() {
        let input = "i64 f(){return visit(slot,|erased| (value(erased)+weight(erased))*scale);}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "i64 f() {\n    return visit(slot, |erased| (value(erased) + weight(erased)) * scale);\n}\n"
        );
    }

    #[test]
    fn does_not_split_short_signature_for_long_body() {
        let input = "bool is_digit(char ch){return ch=='0'||ch=='1'||ch=='2'||ch=='3'||ch=='4'||ch=='5'||ch=='6';}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "bool is_digit(char ch) {\n    return ch == '0' || ch == '1' || ch == '2' || ch == '3' ||\n        ch == '4' || ch == '5' || ch == '6';\n}\n"
        );
        let second = format_source(&formatted, FormatOptions::default()).unwrap();
        assert_eq!(formatted, second);
    }

    #[test]
    fn prefers_lower_precedence_binary_breaks() {
        let input = "i64 f(){return alpha_long_name*beta_long_name+gamma_long_name*delta_long_name+epsilon_long_name*zeta_long_name;}";
        let formatted = format_source(
            input,
            FormatOptions {
                line_width: 72,
                ..FormatOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            formatted,
            "i64 f() {\n    return alpha_long_name * beta_long_name +\n        gamma_long_name * delta_long_name +\n        epsilon_long_name * zeta_long_name;\n}\n"
        );
    }

    #[test]
    fn preserves_format_disabled_region_verbatim() {
        let disabled = "    // ciel-format off\n  if(flag){return 1;}else{return 2;}\n      i64    x=foo( 1,2);\n    // ciel-format on";
        let input = format!("i64 f(bool flag){{i64 a=0;\n{disabled}\ni64 b=3;return a+b;}}");
        let formatted = format_source(&input, FormatOptions::default()).unwrap();
        assert!(formatted.contains(disabled));
        assert_eq!(
            formatted,
            "i64 f(bool flag) {\n    i64 a = 0;\n    // ciel-format off\n  if(flag){return 1;}else{return 2;}\n      i64    x=foo( 1,2);\n    // ciel-format on\n    i64 b = 3;\n    return a + b;\n}\n"
        );
    }

    #[test]
    fn preserves_format_disabled_region_until_eof() {
        let input = "void f(){\n// ciel-format off\n  i64    x=foo( 1,2);}\n";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n// ciel-format off\n  i64    x=foo( 1,2);}\n"
        );
    }

    #[test]
    fn preserves_existing_blank_lines() {
        let input = "import /std/result;\n\nstruct Box{i64 value;}\n\n\ni64 f(){return 1;}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "import /std/result;\n\nstruct Box {\n    i64 value;\n}\n\ni64 f() {\n    return 1;\n}\n"
        );
    }

    #[test]
    fn inserts_single_top_level_blank_line() {
        let input = "struct A{i64 value;}\nstruct B{i64 value;}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "struct A {\n    i64 value;\n}\n\nstruct B {\n    i64 value;\n}\n"
        );
    }

    #[test]
    fn keeps_space_after_import_before_relative_module_path() {
        let input = "import ./cli as cli;\nimport ../protocol/auth as auth;";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "import ./cli as cli;\nimport ../protocol/auth as auth;\n"
        );
    }

    #[test]
    fn separates_semicolonless_c_includes() {
        let input = "#c_include \"stddef.h\" #c_include \"stdint.h\"";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "#c_include \"stddef.h\"\n#c_include \"stdint.h\"\n"
        );
    }

    #[test]
    fn keeps_spaces_around_determined_generic_separator() {
        let input = "interface<I->Item> Next<Item> next(*I iter)=.next;";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "interface<I -> Item> Next<Item> next(*I iter) = .next;\n"
        );
    }

    #[test]
    fn keeps_negative_interface_terms_tight() {
        let input = "interface reader=measure+! seek;i64 f<T:read+! seek>(T value);";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "interface reader = measure + !seek;\ni64 f<T: read + !seek>(T value);\n"
        );
    }

    #[test]
    fn aligns_case_bodies_assignment_rhs_and_nested_cases() {
        let input = "void f(){switch(a){case Ok(x):Value v=\nwrap(\nx\n);switch(x){case A:if(flag){return;}case B:return;}case Err(e):return;}}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    switch (a) {\n        case Ok(x):\n            Value v = wrap(\n                x\n            );\n            switch (x) {\n                case A:\n                    if (flag) {\n                        return;\n                    }\n                case B:\n                    return;\n            }\n        case Err(e):\n            return;\n    }\n}\n"
        );
    }

    #[test]
    fn preserves_deref_expression_and_enum_closing_indent() {
        let input = "enum E{A,}\nvoid f(*const E error){switch(*error){case A:return;}}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "enum E {\n    A,\n}\n\nvoid f(*const E error) {\n    switch (*error) {\n        case A:\n            return;\n    }\n}\n"
        );
    }

    #[test]
    fn indents_select_arms_from_select_line() {
        let input = "async Result<usize,Error> f(){Result<usize,Error> picked=await select{case left=ready_value(1):left;case right=ready_value(2):right;};return picked;}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "async Result<usize, Error> f() {\n    Result<usize, Error> picked = await select {\n        case left = ready_value(1):\n            left;\n        case right = ready_value(2):\n            right;\n    };\n    return picked;\n}\n"
        );
        let second = format_source(&formatted, FormatOptions::default()).unwrap();
        assert_eq!(formatted, second);
    }

    #[test]
    fn keeps_multiline_return_logical_expression_stable() {
        let input = "bool f(){return left==right&&\nflag==other;}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "bool f() {\n    return left == right && flag == other;\n}\n"
        );
        let second = format_source(&formatted, FormatOptions::default()).unwrap();
        assert_eq!(formatted, second);
    }

    #[test]
    fn aligns_multiline_struct_literals_and_conditions() {
        let input = "void f(){Item item={\na:1,\nb:2,\nc:3\n};if(a||b||c){return;}}";
        let formatted = format_source(input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            "void f() {\n    Item item = {\n        a: 1,\n        b: 2,\n        c: 3\n    };\n    if (a || b || c) {\n        return;\n    }\n}\n"
        );
    }

    #[test]
    fn preserves_config_blocks_verbatim() {
        let config = "#if has_feature(\"fast\")  i64 selected() {     return 10;\n}\n\n#else  i64 selected() {     return 30;\n}\n\n#endif";
        let input = format!("{config}\n\ni64 main(){{return selected();}}");
        let formatted = format_source(&input, FormatOptions::default()).unwrap();
        assert_eq!(
            formatted,
            format!("{config}\n\ni64 main() {{\n    return selected();\n}}\n")
        );
    }
}
