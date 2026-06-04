# Owned Error Box Proposal

This proposal defines an application-level error model for Ciel that keeps
precise error enums usable in libraries while making `?` ergonomic across
different error types at application boundaries.

The design is intentionally close to the shape of Rust's `anyhow` crate, but it
uses Ciel's existing interface and dynamic interface machinery instead of method
syntax, trait objects, or a global error-code namespace.

## Proposal Order

```text
metaprogramming :> error-box[derived format_error]
error-box || metaprogramming[owned error erasure and ? propagation]
```

This proposal owns the standard erased error type, the `ErrorTrait` formatting
capability, context helpers, and the targeted `?` propagation rule into
`/std/error.Error`.

Automatic `format_error` generation for structs or enums is a structural
capability-derivation problem and belongs to metaprogramming. Until that exists,
concrete error types implement `format_error` explicitly.

## Problem

Today `?` requires the inner and outer `Result` error types to be exactly equal:

```ciel
Result<void, AppError> init() {
    open("config.json")?; // IoError
    parse_json()?;        // JsonError
    connect_db()?;        // DbError
    return Ok;
}
```

This is too strict for application code. The outer function often only wants to
report the failure with useful text and context. It does not need to recover
from every concrete error variant.

Adding generic implicit conversions is not a good fit. A conversion graph such
as `IoError -> AppError -> FatalError` creates questions about transitive search,
cycles, ambiguity, import order, and hidden semantic rewrites.

Global error codes are also not enough. They make `?` easy, but they lose payload
data such as paths, parser locations, errno values, or backend-specific failure
details.

## Goals

1. Keep precise error types for library APIs and recoverable control flow.
2. Allow application code to return one standard erased error type.
3. Make `?` work from any formattable concrete error into that standard type.
4. Reuse existing dynamic interface conversion and GC heap storage.
5. Keep context attachment as ordinary function calls, not method syntax.

## Non-Goals

1. No global error-code namespace.
2. No automatic multi-step conversion search.
3. No automatic conversion between arbitrary error enums.
4. No downcast support in the first version.
5. No method-call syntax such as `result.context("...")`.

## Standard Error Interface

`/std/error` should define one formatting capability:

```ciel
export interface<T> []char format_error(*T error);
export interface ErrorTrait = format_error;
```

Any concrete error type that implements `format_error` can be converted into the
standard erased error type.

Example:

```ciel
enum IoError {
    MissingFile([]char),
    PermissionDenied([]char),
}

impl format_error(*IoError error) {
    switch (*error) {
        case MissingFile(path):
            return path;
        case PermissionDenied(path):
            return path;
    }
}
```

The exact formatting helpers can improve later. The important semantic property
is that `format_error(&err)` produces a diagnostic string for a concrete error
value.

## Owned Error Type

`Error` should be a normal standard-library struct that owns an erased error
value through a dynamic interface field:

```ciel
export struct Error {
    ErrorTrait value;
}
```

`ErrorTrait` is a dynamic interface value. It stores:

```text
data pointer + vtable pointer
```

When a concrete non-pointer value is passed where `ErrorTrait` is expected, the
compiler already knows how to allocate a GC-owned copy and construct the dynamic
interface value. Therefore the minimal `error_box` helper can be ordinary Ciel:

```ciel
export Error error_box(ErrorTrait error) {
    return { value: error };
}
```

`Error` itself implements `format_error`, so boxed errors can be formatted
through the same capability:

```ciel
impl format_error(*Error error) {
    return format_error(error->value);
}
```

`Error` is not an opaque C interop type. It is a standard-library-owned wrapper
around an existing dynamic interface value.

## `?` Propagation Rule

The existing exact-match rule remains valid:

```text
Result<T, E> ? inside Result<U, E>
```

This proposal adds one special standard-library rule:

```text
Result<T, E> ? inside Result<U, Error>

Allowed when E implements ErrorTrait.
On Err(e), return Err(error_box(e)).
```

Conceptual lowering:

```ciel
Result<T, E> temp = expr;
switch (temp) {
    case Ok(value):
        value
    case Err(error):
        return Err(error_box(error));
}
```

The call to `error_box(error)` is type checked like an ordinary function call.
Its parameter expects `ErrorTrait`, so the existing dynamic interface conversion
performs the owned erasure from `E` to `ErrorTrait`.

Example:

```ciel
Result<void, Error> init() {
    open("config.json")?;
    parse_json()?;
    connect_db()?;
    return Ok;
}
```

If `open` returns `Result<Fd, IoError>`, this is accepted only when `IoError`
implements `format_error`.

## Context

Ciel does not need method syntax to support context. Context can be an ordinary
standard-library function.

Minimal API:

```ciel
export struct Error {
    ErrorTrait value;
    ?*Error source;
    []char context;
}

export Error error_box(ErrorTrait error);
export Error error_with_context(Error source, []char context);

export Result<T, Error> error_context<T, E: ErrorTrait>(
    Result<T, E> result,
    []char context,
);

export Result<void, Error> error_context_void<E: ErrorTrait>(
    Result<void, E> result,
    []char context,
);
```

The `void` helper exists because the current standard library already needs
separate `must_void` and `expect_void` functions for `Result<void, E>`.

Usage:

```ciel
Result<void, Error> init() {
    error_context(open("config.json"), "open config")?;
    error_context(parse_json(), "parse config")?;
    error_context(connect_db(), "connect database")?;
    return Ok;
}
```

Conceptual implementation:

```ciel
export Result<T, Error> error_context<T, E: ErrorTrait>(
    Result<T, E> result,
    []char context,
) {
    switch (result) {
        case Ok(value):
            return Ok(value);
        case Err(error):
            return Err(error_with_context(error_box(error), context));
    }
}
```

`error_with_context` can keep a source chain. A possible representation is:

```ciel
export struct Error {
    ErrorTrait value;
    []char context;
    ?*Error source;
}
```

For a root boxed error, `value` is the concrete error erased to `ErrorTrait`,
`context` is empty, and `source` is `null`. For a context wrapper, `context`
stores the new message and `source` points at an owned or rooted copy of the
previous `Error`.

The exact representation may change. The required behavior is that the returned
`Error` owns or roots every value needed to format the full diagnostic chain. The
proposal does not require user code to construct this layout by hand.

## Library and Application Split

Libraries should keep precise errors when callers may recover:

```ciel
enum ParseError {
    EmptyInput,
    InvalidNumber([]char),
}

Result<Config, ParseError> parse_config([]char text);
```

Applications and task boundaries can erase:

```ciel
Result<void, Error> run() {
    Config config = parse_config(load_config_text()?)?;
    start_service(config)?;
    return Ok;
}
```

This avoids forcing every library to agree on one large application enum. It
also avoids hiding recoverable errors too early.

## Interaction With Dynamic Interfaces

The proposal depends on one existing rule: when a dynamic interface type is
expected, a concrete receiver value can be coerced to that dynamic interface if
the concrete type implements the required interface view.

For non-pointer values, the dynamic interface conversion must own or root the
concrete value long enough for the erased interface to remain valid. The current
C backend already lowers such conversions by allocating a copy for the dynamic
interface data pointer.

Therefore `error_box(ErrorTrait error)` can accept an already erased value and
store it. It does not need to compute `sizeof(T)`, allocate `T`, or manually
build a vtable inside ordinary Ciel code.

## Type Checking

The type checker should recognize the standard `Error` type exported by
`/std/error`, just as `?` already recognizes the standard `Result` type exported
by `/std/result`.

When checking `inner?`:

1. Require `inner` to be `/std/result.Result<T, E>`.
2. If the enclosing function returns `/std/result.Result<U, E>`, keep current
   behavior.
3. Otherwise, if the enclosing function returns `/std/result.Result<U, Error>`,
   require `E: ErrorTrait`.
4. Otherwise, report the existing error type mismatch.

This is a targeted rule for `?`. It does not add assignment compatibility from
`E` to `Error`, and it does not change ordinary argument passing except for the
explicit `error_box` call inserted by `?` lowering.

## Lowering

The generated code for `?` currently returns the inner `Err` payload directly
when the inner and outer error layouts match.

For the erased-error rule, lowering should instead construct the outer `Error`
through `error_box`.

Conceptual Ciel lowering:

```ciel
Result<T, E> temp = inner;
switch (temp) {
    case Ok(value):
        value
    case Err(error):
        return Err(error_box(error));
}
```

The compiler can implement this either by:

1. representing the inserted `error_box(error)` as a typed expression in THIR,
   then reusing normal call lowering; or
2. emitting a specialized C helper call after monomorphization.

The first option is preferable because it keeps dynamic interface coercion,
generic instantiation, and source diagnostics on the ordinary expression path.

## Formatting A Chain

The first version only needs a stable formatting operation:

```ciel
export []char error_message(*Error error);
```

This may format:

```text
connect database: timeout
```

or, for a chain:

```text
connect database
caused by: network timeout
```

The exact text is a standard-library policy. The language only needs the owned
erasure and `?` propagation rule.

## Open Questions

1. Should `Error` store context as `[]char`, an owned string type, or a later
   standard `String`?
2. Should `Error` expose `source` in the public struct, or keep it behind helper
   functions once Ciel has better opaque standard-library types?
3. Should `format_error(*Error)` return only the top-level message, while
   `error_message(*Error)` formats the whole chain?
4. Should erased errors be allowed in actor messages? The safe default is to
   require an explicit `Message` implementation for `Error`.

Automatic `format_error` for simple enums is intentionally not an open question
in this proposal. It is deferred to metaprogramming.

## Implementation Plan

1. Extend `/std/error` with `format_error`, `ErrorTrait`, `Error`, `error_box`,
   and basic formatting helpers.
2. Add `format_error` implementations for the current standard error values.
3. Teach `?` type checking to accept `E: ErrorTrait` when the enclosing return
   error type is `/std/error.Error`.
4. Represent the inserted `error_box(error)` in typed IR or add a dedicated
   lowering path that still reuses dynamic interface conversion.
5. Add fixtures for:
   - exact error propagation still working;
   - concrete error propagation into `Error`;
   - rejection when `E` lacks `format_error`;
   - `Result<void, E>` propagation into `Result<void, Error>`;
   - `error_context` and `error_context_void`.
6. Update `design.md` only after the proposal is accepted.
