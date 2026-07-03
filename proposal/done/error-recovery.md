# Compiler Error Recovery Proposal

This proposal strengthens the compiler frontend so malformed source can still
produce useful diagnostics and partial compiler data. It is not an LSP proposal,
but it establishes the recovery and analysis shape that a future editor or LSP
server would need.

The strict compile pipeline should keep rejecting invalid programs. The new
behavior is for the frontend and semantic analysis to recover enough structure
to report nearby errors, discover imports, and build partial symbol information
without pretending that the program is valid.

## Proposal Order

```text
error-recovery < lsp
error-recovery || monomorphized-c-callbacks[syntax]
```

`lsp` is a reserved future proposal anchor. This proposal owns parser and
frontend recovery behavior, but it does not define protocol messages, editor
features, incremental parsing, or workspace indexing.

Syntax proposals can proceed independently. When they add grammar, they should
add recovery rules for their new delimiters and list forms through this
proposal's parser infrastructure.

## Problem

The current compiler is optimized for batch compilation. The frontend returns
either a complete value or diagnostics:

1. Lexing collects invalid-token diagnostics but returns no token stream when
   any lexical error exists.
2. Parsing recovers only at top-level item boundaries.
3. A syntax error inside a function body, type list, expression, or declaration
   normally aborts the whole containing top-level item.
4. The AST has no explicit error or missing nodes, so successfully parsed
   structure around a syntax error cannot be preserved.
5. Later stages collect many semantic diagnostics, but they only run after the
   previous stage fully succeeds, and they return diagnostics instead of
   exposing partial analysis products.

This is acceptable for a CLI compiler, but it makes diagnostics brittle during
normal editing. A missing semicolon, missing delimiter, or stray token can hide
unrelated declarations, imports, and semantic errors elsewhere in the file.

## Goals

1. Preserve as much syntactic structure as practical after lexical and parse
   errors.
2. Keep strict compilation behavior unchanged: any frontend diagnostic still
   prevents code generation.
3. Make parser recovery local to the smallest reasonable syntactic construct.
4. Avoid cascades by suppressing follow-on diagnostics derived only from an
   earlier syntax hole.
5. Allow import discovery and top-level symbol collection from partially parsed
   files.
6. Let semantic phases skip or tolerate explicit error nodes where that is
   straightforward.
7. Keep the recovery model testable with fixture expectations.
8. Provide APIs that return both partial values and diagnostics.
9. Prefer preserving recoverable structure over dropping malformed regions.
   When the parser can choose between discarding source and representing it with
   an error node, missing marker, or unknown type, it should choose the
   representation that exposes more valid surrounding information.

## Non-Goals

1. Implementing an LSP server.
2. Implementing incremental parsing.
3. Accepting invalid programs.
4. Running monomorphization or C code generation after frontend errors.
5. Making every semantic phase fully tolerant of every malformed AST in the
   first implementation.
6. Changing language syntax.
7. Replacing the existing strict compile APIs used by the CLI.

## Recovery API

Add lossy frontend APIs alongside the strict ones:

```rust
pub struct WithDiagnostics<T> {
    pub value: T,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lex_lossy(file: FileId, source: &str) -> WithDiagnostics<Vec<Token>>;
pub fn parse_file_lossy(tokens: Vec<Token>) -> WithDiagnostics<AstFile>;
```

The existing `lex` and `parse_file` remain strict wrappers:

1. call the lossy API;
2. return `Ok(value)` when diagnostics are empty;
3. return `Err(diagnostics)` otherwise.

This preserves current CLI behavior while giving future tooling and tests access
to partial products.

Later semantic stages may use the same shape:

```rust
pub fn resolve_modules_lossy(modules: Vec<ParsedModule>)
    -> WithDiagnostics<ResolvedProgram>;
```

The strict `resolve_modules`, `lower_to_hir`, and `type_check` wrappers should
continue to reject any diagnostics unless a caller explicitly chooses the lossy
analysis path.

## Lexical Recovery

Lexing should always return a token stream ending in `Eof`. Invalid source
spans produce diagnostics and public `TokenKind::Error` tokens that preserve
the invalid span and lexeme.

`TokenKind::Error` is visible in the token API. The parser may consume it into
an AST error node, include it in a skipped recovery span, or ignore it when a
more specific parse diagnostic already explains the same source. Exposing it is
intentional: source coverage and later recovery are more valuable than hiding
invalid bytes.

Rules:

1. A lexical error must not prevent later valid tokens from being emitted.
2. Adjacent invalid bytes may be coalesced into one diagnostic when that reduces
   noise.
3. Unterminated string and character literals should produce one diagnostic at
   the start of the literal and then resume at a line boundary when possible.
4. Unterminated block comments should produce one diagnostic and resume at EOF.
5. The strict `lex` API still returns `Err` when any lexical diagnostics exist.

## AST Error Nodes

Add explicit error placeholders to syntax categories that can contain malformed
subtrees:

```rust
pub enum ItemKind {
    ...
    Error,
}

pub enum StmtKind {
    ...
    Error,
}

pub enum ExprKind {
    ...
    Error,
}

pub enum TypeKind {
    ...
    Error,
}

pub enum Pattern {
    ...
    Error(Span),
}
```

Each error node carries a span through the surrounding node. Error nodes are
syntax placeholders, not valid language constructs. They exist so later passes
can keep walking the tree without inventing a real value or type.

Use zero-width missing spans only for missing delimiters or separators. Missing
delimiters and separators are represented by parser-internal zero-width
synthetic tokens with the expected token kind. These synthetic tokens are not
emitted by `lex_lossy`, but the parser may use them to continue building the
surrounding AST node and to attach precise diagnostics.

Use the offending token span for unexpected tokens. When multiple skipped
tokens form one bad construct, merge their spans.

## Parser Recovery

Parser functions should stop returning immediately to the nearest top-level item
for ordinary local syntax errors. Instead, recovery should be scoped to the
construct being parsed.

### Top-Level Items

Top-level recovery should stop at:

1. a semicolon after a declaration-like item;
2. a balanced closing brace for braced items;
3. the next likely item starter, including `export`, `resource`, `unsafe`,
   `import`, `type`, `struct`, `enum`, `interface`, `impl`, `extern`,
   `derive`, and `derivable`;
4. EOF.

Stopping before the next likely item starter is important. The current strategy
can skip a valid following item when the previous item is missing a semicolon.

### Blocks And Statements

Block parsing should recover per statement:

```text
parse_block:
    while not "}" or EOF:
        parse_statement_recovering()
```

If statement parsing fails, emit a `StmtKind::Error`, synchronize to `;`, `}`,
`case`, `default`, or a statement starter, and then continue. A bad statement
inside a function should not discard the function body or following functions.

Statement starters include `{`, `if`, `while`, `for`, `switch`, `defer`,
`return`, `break`, `continue`, and likely declaration type starters.

### Expressions

Expression recovery should preserve surrounding expressions when one operand or
postfix part is malformed.

Examples:

```ciel
return left + ;        // binary expression with Error right operand
value = call(1, , 3);  // argument list with an Error argument
items[index;           // index expression recovers at `;`
```

The parser does not need to build perfect trees for bad expressions. It should
prefer a small `ExprKind::Error` over skipping the entire containing statement.

### Types And Lists

Type, parameter, generic argument, enum payload, struct field, and call argument
lists should recover element-by-element. Synchronization points are `,`, the
list closing delimiter, `;`, and EOF.

For a missing separator, emit one diagnostic and continue at the next likely
element when doing so is unambiguous.

For a missing closing delimiter, emit one diagnostic at the current token and
let the caller decide whether to continue.

### Delimiter Awareness

Recovery should track delimiter nesting enough to avoid synchronizing on a
semicolon or comma inside nested parentheses, brackets, or braces. This does not
require an incremental parser; a simple recovery helper can skip balanced nested
groups while looking for synchronization tokens.

## Semantic Recovery

The first semantic goal is not to type-check invalid code deeply. It is to keep
useful declarations and imports alive.

Rules:

1. Resolver skips `ItemKind::Error` but continues collecting valid definitions.
2. Duplicate definition diagnostics should still be emitted for valid items.
3. HIR lowering must be total over recovered AST. It lowers error items,
   statements, expressions, and types to matching HIR error nodes unless the
   parent construct can safely skip the child while preserving more useful
   surrounding information.
4. Type checking must have a `Ty::Error` or equivalent unknown sentinel.
   Operations involving that type should avoid cascaded diagnostics and should
   keep checking nearby valid expressions, locals, and declarations.
5. Lossy semantic analysis should run through HIR lowering and type checking
   before any future editor-facing analysis layer is attempted. That path does
   not need to produce codegen-ready output, but it must expose top-level
   symbols, imports, local declarations, and any expression/type information
   that can be computed without depending on an error node.
6. Codegen and monomorphization must reject any program that contains frontend
   or semantic diagnostics.

The implementation may land in slices, but the target recovery model is
semantic, not only syntactic: the compiler should recover as much valid symbol
and type information as it can without accepting the program.

## Diagnostic Policy

Recovery diagnostics should be specific and local:

```text
expected `;` after local declaration
expected expression before `;`
expected `)` after argument list
skipping invalid statement after previous parse error
```

Avoid diagnostics that are only consequences of a known syntax hole:

```ciel
i64 value = ;
return value;
```

The parser should report the missing initializer expression. Type checking
should not also report unrelated type errors caused solely by the error
expression.

When the compiler skips a large region, emit at most one recovery diagnostic for
that region. Prefer the original syntax error over a generic "skipping tokens"
message unless the skipped region is surprising.

Diagnostics should carry enough classification for callers and tests to tell
ordinary language errors from recovery artifacts. Extend `Diagnostic` with a
phase/category field or an equivalent structured tag. At minimum, distinguish
lexical, parse, recovery, resolve, and type-check diagnostics. Diagnostics
emitted only to explain synthetic tokens or skipped regions are recovery
diagnostics.

## Module Loading

Module loading should have a lossy mode that:

1. reads the requested source file;
2. lexes and parses it lossily;
3. records diagnostics;
4. discovers imports from successfully parsed import items;
5. attempts to load imported modules even if the current module has unrelated
   syntax diagnostics.

Strict compilation still fails as soon as diagnostics exist. Lossy loading is
for analysis surfaces that need a best-effort module graph.

If an import declaration itself is malformed, only that import is skipped.

## Source Ranges

Spans remain byte offsets internally. Recovery should preserve byte spans for
all error nodes and diagnostics.

A future editor-facing layer will need conversions from byte offsets to LSP
UTF-16 ranges. That conversion is out of scope for this proposal, but recovery
must not discard the source text or collapse spans in ways that make range
conversion impossible.

## Testing Strategy

Add focused fixtures for recovery behavior:

1. Lexical error followed by a valid function still reports the lexical error
   and allows parsing of the valid function through `lex_lossy`.
2. Missing semicolon in one top-level declaration does not hide the following
   top-level declaration.
3. Bad statement inside a function does not discard later statements in the same
   block.
4. Bad function body does not discard the next function.
5. Bad expression in a call argument list recovers at the next comma.
6. Missing closing delimiter emits one diagnostic and recovers at the enclosing
   construct.
7. Malformed import is skipped, but later valid imports are still discovered.
8. Multiple independent syntax errors in one file produce multiple diagnostics
   without an excessive cascade.
9. Existing strict CLI behavior still fails on any recovery diagnostic.

The fixture runner may need a new metadata kind for frontend recovery tests if
the existing compile/error modes cannot observe partial AST or lossy module
loading.

## Implementation Plan

1. Add `WithDiagnostics<T>` and lossy lexer/parser APIs.
2. Add `TokenKind::Error` and make lexing produce tokens plus diagnostics.
3. Add AST error variants for items, statements, expressions, types, and
   patterns.
4. Refactor parser `expect` helpers so callers can either fail strictly or
   record a diagnostic and insert a missing token placeholder.
5. Add delimiter-aware synchronization helpers.
6. Convert block and statement parsing to statement-level recovery.
7. Convert list parsing to element-level recovery.
8. Improve top-level synchronization to stop before likely item starters.
9. Add strict wrappers so existing compiler entry points keep current behavior.
10. Add diagnostic phase/category tagging.
11. Add lossy module loading for import discovery.
12. Teach resolver to skip error items while keeping valid definitions.
13. Add HIR error nodes and a type-checking error sentinel.
14. Keep lossy semantic analysis running through type checking while preventing
   monomorphization and codegen after any diagnostic.
15. Add recovery fixtures and keep existing compile fixtures unchanged.

## First Slice

The first useful slice should include:

1. lossy lexer and parser APIs;
2. AST error nodes;
3. top-level, block, statement, and list recovery;
4. strict wrappers preserving current CLI behavior;
5. diagnostic phase/category tags;
6. lossy resolve that preserves imports and valid top-level definitions;
7. tests proving that independent syntax errors are reported independently.

That slice is enough to make the compiler frontend robust without committing to
LSP work.

## Fixed Decisions

The recovery design uses these fixed decisions:

1. `TokenKind::Error` is part of the public token stream returned by lossy
   lexing. Invalid source should remain visible to downstream recovery.
2. Missing delimiters and separators are represented by parser-internal
   zero-width synthetic tokens, plus diagnostics. The lexer never invents these
   tokens.
3. The recovery target extends through lossy HIR lowering and type checking,
   using HIR error nodes and `Ty::Error` or an equivalent sentinel. Future
   editor-facing work should not start from parser-only recovery.
4. Recovery diagnostics are structurally tagged separately from ordinary
   lexical, parse, resolve, and type-check diagnostics.
5. When in doubt, prefer preserving more recoverable program structure and
   suppressing cascaded diagnostics over dropping malformed regions early.
