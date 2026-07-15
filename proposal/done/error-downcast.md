# Typed Error Downcast And Report Separation Proposal

This proposal adds exact runtime type recovery to `/std/error.Error` and
separates downcastable erased errors from message-safe diagnostic reports.

`Error` remains an owned, type-erased error value. It gains exact read-only
downcast, keeps context, and no longer implements `Message`. A new `Report`
type owns diagnostic text, implements `Message`, and cannot recover the
original concrete error.

## Proposal Order

```text
error-box < error-downcast
pure-library-message :> error-downcast[Error and Report message policy]
typed-task-errors <= error-downcast[erased task errors]
backend-modernization-roadmap <= error-downcast[type descriptor reuse]
```

`error-box` owns the standard erased error and the targeted `?` conversion
into it. This proposal extends that representation with runtime type identity.

`pure-library-message` owns cross-owner transfer policy. This proposal removes
the unconditional `Message` implementation from `Error` and gives that policy
to `Report`.

`typed-task-errors` remains the model for preserving concrete task errors.
When a task intentionally uses an erased message-safe error, its error type is
`Report`, not `Error`.

The backend roadmap may later reuse its generated type descriptors as type
identity tokens. Downcast does not depend on precise GC or multi-translation-
unit code generation.

## Problem

The standard `Error` stores a concrete value behind the `ErrorTrait` dynamic
interface. The concrete type is unavailable after erasure:

```ciel
Result<void, Error> run() {
    load_config()?;
    connect()?;
    return Ok;
}
```

Callers can format the returned error but cannot distinguish a recoverable
`ConfigError` from a retryable `NetError`. Applications that need occasional
recovery must either keep a large concrete carrier enum or erase the value and
lose all concrete access.

The current dynamic interface representation does not contain a stable type
identity. It contains a data pointer and a vtable pointer. Vtable address
comparison is insufficient:

1. One concrete type may be erased to several interface views.
2. Dynamic interface re-erasure may allocate a new vtable that copies a subset
   of the original entries.
3. Backend changes may split or merge generated vtable definitions without
   changing source-level type identity.

`meta::Type<T>` also cannot serve as a runtime identity. It is a phantom
compile-time witness with one shared physical representation.

Downcast also changes the safety assumptions behind `Error: Message`. An
erased value that was safe to format is not necessarily safe to expose in a
different actor. A non-affine error may still contain raw pointers, slices, or
thread-local handles. The current shallow `clone_message(*const Error)` hides
those fields. A general downcast would reveal them after transfer.

## Goals

1. Recover an exact concrete error type from a local `Error` without exposing
   mutable access.
2. Preserve downcast through ordinary local copies and context attachment.
3. Give each concrete monomorphized type an unforgeable in-process identity.
4. Keep user error implementations limited to `format_error`.
5. Prevent downcast from bypassing `Message`, actor isolation, or resource
   ownership.
6. Provide a message-safe erased diagnostic type for tasks, actors, channels,
   and messageable error carriers.
7. Keep custom concrete error enums as the preferred recoverable API.

## Non-Goals

1. No general `Any` type or universal runtime reflection facility.
2. No downcast syntax for arbitrary dynamic interface values.
3. No mutable downcast.
4. No owning extraction from `Error` in the first version.
5. No structural or interface-based downcast. Matching is exact nominal type
   identity after transparent alias normalization.
6. No stable type identifiers across processes, builds, serialized data, or
   independently versioned plugin ABIs.
7. No automatic search through custom error conversion paths.
8. No recovery of a concrete value after conversion to `Report`.

## Resolved Design

The accepted model uses two types:

```text
Error   = owned typed erasure, exact downcast, not Message
Report  = owned diagnostic representation, no source-value downcast, Message
```

Code chooses the boundary in its return type. `Result<T, Error>` preserves the
erased concrete value within one ownership domain. `Result<T, Report>` commits
to a transferable diagnostic representation.

`Error` never contains a report state, and `clone_message` never changes an
`Error` into another representation. Conversion to `Report` is a distinct
target-directed operation that produces a different type.

Concrete errors remain preferable when callers are expected to branch on
variants or when task results must retain structured recovery across owners.

## Runtime Type Identity

`/std/meta` adds an opaque runtime identity token:

```ciel
export unsafe struct TypeId {
    *const void token;
}

export TypeId type_id<T>();
export bool type_id_eq(TypeId left, TypeId right);
```

Safe code cannot construct or inspect `TypeId` because `TypeId` is an
`unsafe struct`. It may obtain tokens through `type_id<T>()`, copy them, and
compare them through `type_id_eq`.

The compiler emits one canonical token object for each concrete semantic type
used with `type_id<T>()`. `TypeId` stores the address of that object. Equality
is pointer equality between canonical token addresses.

### Identity Rules

The following rules are normative:

1. Transparent type aliases have the identity of their normalized target.
2. Distinct nominal structs and enums have distinct identities even when their
   layouts are equal.
3. Generic arguments participate in identity. `Box<i64>` and `Box<u8>` are
   different types.
4. C spelling types retain their nominal Ciel declaration identity instead of
   collapsing by width.
5. Dynamic interface erasure uses the concrete interface receiver type. A `T`
   value and a `*T` receiver view erased through the same capability report
   the identity of `T`.
6. Opaque return types preserve the hidden concrete identity selected at their
   construction site, but callers cannot name an inaccessible concrete type.
7. Resource-affine types may have a `TypeId`, but they remain ineligible for
   standard error boxing.

The token is process-local. Source code cannot convert it to a stable integer,
type name, package key, or serialization tag.

### Generated Symbols

The current single-translation-unit backend may emit one static token object
per type. A multi-unit backend must emit one definition and shared declarations
for the same canonical type key. Duplicate per-unit token objects would break
identity.

The token may point at a future `CielTypeDesc` when the backend roadmap adds
canonical descriptors. The language requires identity, not a particular
descriptor layout. Type identity must continue to work in both BDWGC and
precise-GC modes.

## Hidden Erased Error Witness

`ErrorTrait` needs access to both the concrete TypeId and the concrete receiver
storage. This is supplied by a compiler-owned capability in `/std/error`:

```ciel
unsafe struct ErasedErrorRef {
    meta::TypeId type_id;
    *const void data;
}

unsafe interface<T> ErasedErrorRef erased_error_ref(
    *const T error
);
```

The compiler provides the equivalent of this implementation for every
concrete receiver type:

```ciel
unsafe impl<T> erased_error_ref(*const T error) {
    return unsafe {
        {
            type_id: meta::type_id<T>(),
            data: error as *const void
        }
    };
}
```

User code cannot implement or override `erased_error_ref`. It is a
compiler-owned witness, like other canonical metadata capabilities.

`ErrorTrait` becomes an alias containing the public formatting policy and the
hidden witness:

```ciel
export interface ErrorTrait = format_error + erased_error_ref;
```

Concrete error authors still implement only `format_error`. The hidden witness
is satisfied automatically.

The dynamic `ErrorTrait` vtable contains the witness entry. Re-erasing a
dynamic interface to another view that retains `ErrorTrait` copies this entry,
so the original TypeId and data pointer remain available. This avoids any
dependency on vtable address identity.

## Standard Error Representation

`Error` remains an owned dynamic error with context and a source chain:

```ciel
export unsafe struct Error {
    ErrorTrait value;
    []const char context;
    ?*const Error source;
}
```

The exact field layout remains a standard-library detail. `Error` is an
`unsafe struct` so safe code cannot forge source cycles, replace its dynamic
payload, or project internal pointers. Safe access uses functions from
`/std/error`.

`error_box` keeps the existing ownership behavior. Erasing a non-pointer value
allocates rooted storage for the value. Erasing a pointer receiver keeps the
pointed-to receiver storage alive through escape analysis. Resource-affine
values remain rejected because erasure would hide cleanup and move
requirements.

`Error` implements `format_error` but does not implement `Message`.

## Downcast API

`/std/error` adds:

```ciel
export option::Option<*const T> error_downcast_ref<T: ErrorTrait>(
    *const Error error
) = .downcast_ref;

export bool error_is<T: ErrorTrait>(
    *const Error error
) = .is;

export option::Option<*const Error> error_source(
    *const Error error
) = .source;
```

`error_downcast_ref<T>` obtains the hidden `ErasedErrorRef`, compares its token
with `meta::type_id<T>()`, and converts the data pointer to `*const T` only when
the tokens match.

Conceptual implementation:

```ciel
export option::Option<*const T> error_downcast_ref<T: ErrorTrait>(
    *const Error error
) {
    ErasedErrorRef erased = erased_error_ref(error->value);
    if (!meta::type_id_eq(erased.type_id, meta::type_id<T>())) {
        return option::None;
    }
    return option::Some(unsafe { erased.data as *const T });
}
```

The actual implementation may be compiler-lowered to avoid exposing internal
fields. Its semantics must match the code above.

### Match Semantics

Downcast uses exact type identity:

```ciel
type LocalIoError = io::IoError;

error_downcast_ref<LocalIoError>(&error); // same as io::IoError
error_downcast_ref<net::NetError>(&error); // distinct nominal type
```

The operation does not:

- inspect enum variants;
- search `source` recursively;
- invoke custom error conversion capabilities;
- match any type that implements a requested interface;
- unwrap an `Error` that was explicitly boxed inside another `Error`.

Callers may traverse `error_source` and attempt a downcast at each node when a
chain contains distinct source values.

### Read-Only Result

The result is `*const T`. `Error` is copyable, and several local values may
refer to the same erased payload. Returning `*T` would create mutable access
without unique ownership.

An owning `error_downcast<T>(Error)` is deferred. Consuming one copy of an
`Error` does not prove that no other copy refers to the same payload. A later
owning API would need a distinct unique box representation or a copy/clone
constraint on `T`.

### Context

`error_with_context` must preserve the underlying typed payload. Attaching
context does not change TypeId, and `error_downcast_ref<T>` continues to find
the same value.

Context strings in `Error` may remain ordinary rooted slices because `Error`
does not cross a safe `Message` boundary. Conversion to `Report` copies every
diagnostic string into immutable owned storage.

## Message-Safe Report

`Report` is the standard transferable erased diagnostic:

```ciel
export unsafe struct Report {
    DiagnosticText message;
    ?*const Report source;
}
```

`DiagnosticText` is an internal immutable owned character buffer. Its safe
constructors copy the input bytes into GC-managed storage. Safe code cannot
forge it from an arbitrary borrowed slice.

`Report` provides:

```ciel
export Report error_report(*const Error error) = .report;
export Report report_box(ErrorTrait error);
export Report report_with_context(
    Report source,
    []const char context
) = .with_context;
export []const char report_message(*const Report report) = .message;
```

`error_report` copies the complete diagnostic chain while the original typed
values are still accessible in their ownership domain. Each message and
context string becomes `DiagnosticText`. The conversion does not retain the
original dynamic payload or its TypeId.

`report_box` formats a concrete or erased `ErrorTrait` value and immediately
stores an owned diagnostic. It is the direct report boundary when no local
downcastable `Error` is needed.

When the supplied concrete value is `Error`, report conversion uses
`error_report` and preserves the complete context and source chain. It does not
reduce an `Error` to the single slice returned by its top-level `format_error`
implementation.

`Report` implements `format_error`. It has no downcast API.

`Report` implements `Message` through an explicit unsafe standard-library
policy. Its text buffers and source nodes are immutable and GC-owned, so
`clone_message(*const Report)` may share that immutable graph.

## Error Propagation Into Report

`Report` is a second canonical target for `ErrorTrait` propagation:

```text
Result<T, E> ? inside Result<U, Report>

Allowed when E implements ErrorTrait.
On Err(e), return Err(report_box(e)).
```

The target return type makes the boundary visible:

```ciel
Result<Config, Error> load_local() {
    return parse_config(read_config()?)?;
}

async Result<void, Report> run_task() {
    Config config = read_config()?;
    start(config)?;
    return Ok;
}
```

The first function preserves exact erased values for local inspection. The
second commits every propagated error to a transferable report.

Expected-type conversion follows the same target-directed rule already used
for `Error`:

```ciel
Report report = io::IoError::WriteZero;
return Err(io::IoError::WriteZero); // expected Result<_, Report>
```

The compiler represents this conversion separately from typed `Error` boxing.
`TryPropagation` conceptually gains `ReportBox` alongside `Exact` and
`ErrorBox`.

No conversion from `Report` recovers the original error. Converting a `Report`
to `Error` boxes the `Report` value itself, so downcast can recover `Report`,
not the value that was formatted to create it.

## Concurrency And Message Policy

This proposal removes:

```ciel
unsafe impl clone_message(*const Error value);
```

and adds:

```ciel
unsafe impl clone_message(*const Report value);
```

This makes transfer legality visible to ordinary capability checking. A struct
or enum containing `Error` cannot derive `Message`. It must carry `Report`, a
concrete messageable error, or a module-specific message representation.

Task APIs follow the same rule:

```ciel
Task<T, ConcreteError> // structured and messageable
Task<T, Report>        // erased transferable diagnostic
Task<T, Error>         // rejected: Error does not implement Message
```

The task runtime carrier checks should recognize `Report` wherever they
currently recognize `Error` as an erased carrier for runtime and message-clone
failures.

Standard messageable error enums that currently contain `Error`, including
`MessageClone(Error)` variants, move to `MessageClone(Report)` when the payload
crosses an ownership boundary. The clone failure is converted to `Report` in
the source owner before the enclosing value is transferred.

Functions such as `clone_message` may continue to return local `Error` because
the failure is observed at the call site. Only storage or transfer of that
failure requires conversion to `Report`.

## Interaction With Concrete Error Carriers

Downcast does not replace nominal error enums. A public API that expects callers
to recover should still return a concrete type:

```ciel
enum ServiceError {
    Config(config::ConfigError),
    Network(net::NetError),
    Internal(Error),
}
```

`Internal(Error)` makes `ServiceError` local-only unless the application
provides a custom `Message` policy that converts the internal value to a report.
A transferable carrier should instead use:

```ciel
enum ServiceTaskError {
    Config(config::ConfigError),
    Network(net::NetError),
    Internal(Report),
}
```

Direct custom error conversion and variant-derived wrapping are independent of
downcast. Compile-time conversion remains preferable when the source type is
part of the API contract. Downcast serves boundaries where the set of possible
concrete errors is intentionally open.

## Type Checking

The type checker recognizes the canonical `/std/meta.TypeId`,
`/std/error.Error`, and `/std/error.Report` definitions by standard-library
identity rather than source spelling.

It enforces:

1. `type_id<T>()` requires a concrete runtime type after monomorphization.
2. User impls of the hidden erased-error witness are rejected.
3. Resource-affine values cannot be boxed into `Error` or `Report`.
4. `E -> Error` requires `E: ErrorTrait` and creates typed erasure.
5. `E -> Report` requires `E: ErrorTrait` and creates an owned diagnostic.
6. `error_downcast_ref<T>` requires `T: ErrorTrait`.
7. `Error` has no compiler-provided or standard-library `Message` fact.
8. `Report` uses its explicit standard-library `Message` implementation.

Generic functions preserve the existing obligation model:

```ciel
Result<T, Error> erase_local<T, E: ErrorTrait>(Result<T, E> value);
Result<T, Report> erase_report<T, E: ErrorTrait>(Result<T, E> value);
```

No capability search is performed from `Error` or `Report` back to a concrete
type.

## Lowering

### TypeId

`meta::type_id<T>()` lowers to a `TypeId` containing the address of the
canonical token for `T`. The token object does not need runtime initialization.

### Erased Witness

The hidden witness lowers like an ordinary dynamic interface method. The shim
returns the canonical token and the receiver data pointer already passed to the
dynamic call.

The backend may optimize the witness into a type descriptor field in the
dynamic vtable. That optimization must preserve identity through interface
re-erasure and across generated C units.

### Downcast

Downcast performs one TypeId comparison and one pointer cast. The cast is
emitted only on the equal branch. No allocation is required.

The returned pointer is a GC root like any other typed pointer. In a future
precise-GC backend, the root and boxed payload descriptor must remain visible at
safepoints. The returned pointer addresses the start of the concrete receiver
storage, not an arbitrary interior field.

### Report Conversion

Converting to `Report` calls `format_error` while the typed value is available,
copies the returned characters into `DiagnosticText`, and copies each context
or source message required by the report chain. Allocation failure follows the
runtime's existing GC allocation policy.

## Runtime And ABI

No runtime registry is required. Type identity uses linker-visible canonical
tokens or descriptors generated with the program.

`TypeId` is a Ciel internal ABI value. It is not exported as a stable C ABI
contract. Native C code may receive it only through an explicit Ciel wrapper
that treats it as opaque.

The backend must prevent duplicate token definitions for one canonical type in
multi-unit builds. A generated descriptor unit, COMDAT/link-once definition, or
central registration unit may provide the single identity. The choice belongs
to backend implementation as long as pointer equality is stable within the
linked program.

Report text storage must be immutable and exactly traceable by the future
precise collector. The descriptor plan should classify `DiagnosticText` and
`Report` as ordinary GC-managed values.

## Diagnostics

When a program tries to send or spawn with `Error`, diagnostics should name the
replacement choices:

```text
`Error` does not implement `Message` because it may contain a local erased value
use a concrete messageable error type or convert the value to `Report`
```

When a message derivation reaches an `Error` field, the diagnostic should
retain the structural path:

```text
Message derivation blocked at variant `Internal` (`Error`):
downcastable erased errors are local-only; use `Report` for transfer
```

Downcast mismatch is not a diagnostic. It returns `None`.

An attempt to box a resource-affine error keeps the existing diagnostic about
hidden ownership and cleanup requirements.

## Migration

1. Add canonical TypeId emission and equality.
2. Add the compiler-owned erased-error witness to `ErrorTrait`.
3. Make `Error` an unsafe standard-library value with safe access helpers.
4. Add `error_downcast_ref`, `error_is`, and `error_source`.
5. Add immutable `DiagnosticText`, `Report`, and report conversion helpers.
6. Add expected-type and `?` conversion into `Report`.
7. Implement `Message` for `Report` and remove the implementation for `Error`.
8. Replace `Error` payloads in messageable standard error enums with `Report`
   where the payload crosses owners.
9. Update task carrier checks from erased `Error` to erased `Report`.
10. Migrate `Task<T, Error>` uses to concrete error types or `Task<T, Report>`.
11. Update `design.md` after the implementation and standard-library migration
    land together.

## Testing Strategy

Add fixtures for:

1. exact downcast success and mismatch;
2. transparent aliases preserving identity;
3. distinct nominal types with equal layouts remaining distinct;
4. generic instances receiving distinct TypeIds;
5. context attachment preserving local downcast;
6. dynamic interface re-erasure preserving the original TypeId;
7. erasure from a value and from a pointer receiver producing the same receiver
   identity;
8. resource-affine error boxing remaining rejected;
9. `Error` failing `Message` constraints;
10. `Report` crossing actors, channels, and task boundaries;
11. `Report` preserving owned messages after the source error is unreachable;
12. propagation from a concrete error into `Report` with `?`;
13. conversion from `Error` to `Report` preserving the diagnostic chain;
14. `Task<T, Error>` rejection with a targeted replacement diagnostic;
15. `Task<T, Report>` and concrete typed task errors succeeding;
16. generated C using one canonical TypeId token per concrete type;
17. multi-unit builds preserving TypeId equality when that backend lands;
18. precise-GC stress coverage for downcast pointers and Report chains when
    precise GC becomes available.

## Consequences

Downcastable erased errors become local values. Code that needs structured
recovery across tasks or actors keeps a concrete messageable error type.

Transferable erased errors become reports. Their API promises formatting and
context, not recovery of an arbitrary hidden value.

The standard library gains one runtime type identity primitive, but the
language does not gain a universal cast operator. Exact recovery remains scoped
to the standard erased error abstraction.

The split changes existing `Task<T, Error>` and messageable carrier signatures.
That cost removes an unsafe shallow-erasure assumption and makes ownership
boundaries visible in types.

## Acceptance Criteria

The proposal is complete when:

1. local `Error` values support exact read-only downcast;
2. TypeId remains stable through dynamic interface re-erasure;
3. safe code cannot forge the TypeId and data-pointer association;
4. `Error` no longer implements `Message`;
5. `Report` owns every diagnostic byte it exposes and implements `Message`;
6. concrete errors propagate into either `Error` or `Report` according to the
   enclosing result type;
7. actor and task boundaries reject `Error` and accept `Report`;
8. standard messageable error carriers no longer hide local erased values;
9. the current generated backend emits one canonical identity per concrete
   type, and any future multi-unit backend preserves that identity across
   units;
10. `design.md` documents the accepted type identity, downcast, Error, and
    Report semantics.
