# Affine Resource System Proposal

This proposal replaces copyable revocable resource tokens as the source-level
resource safety model with affine `resource` types. Runtime owners and the
resource registry remain as the implementation substrate and as the runtime
authority for host-resource identity, descriptor reuse defense, stale async
completion rejection, and unsafe or C interop boundary validation.

Memory remains GC-managed. The affine rules apply only to non-memory resources
and to values that contain them.

## Proposal Order

```text
resource-management <= affine-resource-system[runtime registry substrate]
binding-mutability <= affine-resource-system[local state tracking]
async-await <= affine-resource-system[async frame cleanup and cancellation]
pure-library-message <= affine-resource-system[cross-task and cross-actor policy]
unsafe <= affine-resource-system[trusted resource declarations and FFI escape]
```

`resource-management` supplies the runtime registry, owners, stale-token
validation, and owner cleanup. This proposal changes the source-language proof
on top of that runtime. It does not remove the registry.

`binding-mutability` supplies the local binding model that affine state extends.
`async-await` owns frame lowering, cancellation, abort, and task cleanup.
`pure-library-message` owns the clone-based `Message` capability lattice; live
resources do not become `Message` merely because they can move within one task.
`unsafe` owns declarations that bind compiler resource tracking to actual host
cleanup behavior.

## Problem

The current resource design stores real resources in runtime owners and exposes
copyable handle tokens. Runtime validation makes stale token use fail safely,
and a public marker bound prevents scoped callbacks from returning types that
syntactically contain resource handles.

That model is runtime-sound, but it is not the right source-language model:

1. A resource handle can be copied freely, so use-after-close and double close
   are ordinary runtime errors instead of compile-time errors.
2. The marker bound is too coarse. It rejects any result type that contains a
   resource, even when ordinary return-by-value would move that resource to the
   caller.
3. The marker bound is also too public. Resource safety should be a type-system
   rule, not a user-implemented marker capability.
4. Scoped helpers are awkward because the type system cannot express that
   ordinary return-by-value moves the actual resource out of the closing scope.
5. Relying on escape analysis would be unsound as a source rule. Escape
   analysis is conservative and decides storage placement only; an
   implementation that promotes every local value must still be valid.

The desired model is closer to RAII for non-memory resources, but without
making ordinary memory values owned, borrowed, or lifetime-checked.

## Goals

1. Reject use-after-close, double close, and double finish/cancel in safe code
   at compile time.
2. Automatically release resources on all normal cleanup paths.
3. Preserve explicit `close` operations for early release and error reporting.
4. Keep ordinary memory values, GC objects, slices, and structs without
   resources on value-copy semantics.
5. Make resource-ness a declaration-level type property, not a public marker
   interface.
6. Let scoped callbacks return resources by ordinary return-value move.
7. Keep cross-task and cross-actor resource movement explicit.
8. Keep the runtime registry for FFI, unsafe code, descriptor reuse defense,
   async stale callback rejection, cancellation, abort, quotas, and owner
   teardown.
9. Keep the first implementation small enough to fit the existing type checker,
   definite-assignment analysis, and async cleanup lowering.

## Non-Goals

1. Manual memory management.
2. A general Rust-like borrow checker for all values.
3. General linear types where every value must be consumed exactly once.
4. User-defined destructors for ordinary non-resource values.
5. Inferring resource safety from escape analysis.
6. Running cleanup after process abort, host `exit`, signal kill, or panic
   termination that does not unwind.
7. Making live resources cloneable `Message` values.
8. General user-defined `drop` hooks for arbitrary Ciel values.
9. Supporting partial field moves from resource aggregates in the first
   implementation.

## Source Syntax

Add `resource` as a struct declaration modifier:

```ebnf
StructDecl ::= [ "resource" ] [ "unsafe" ] "struct" Identifier
               [ GenericParamList ] StructBody
```

Examples:

```ciel
export resource struct File {
    resource::Handle handle;
}

export resource struct AsyncTcpStream {
    resource::Handle handle;
}
```

`resource` is a safe type property of the nominal struct. It is stricter than
an ordinary struct: values of that type cannot be copied, and live values are
automatically cleaned up. The declaration does not require `unsafe` merely
because it is a resource.

`unsafe struct` keeps its existing meaning: the representation has invariants
that safe code may not construct or project directly. A type may be both
`resource` and `unsafe` only when its representation has extra unsafe
invariants. Standard resource wrappers should prefer plain `resource struct`
with an internal `resource::Handle` field.

The modifier is intentionally not called `linear`. Ciel resources are affine:
they may be explicitly consumed at most once, and if they are not consumed, the
compiler inserts cleanup. Strict linear values would require exactly one
explicit consume operation and would make ordinary resource use more verbose.

## Derived Affine Types

The compiler derives an internal affine property structurally:

1. A `resource struct` is affine.
2. A concrete instantiation of a generic struct with any affine field is
   affine.
3. A concrete instantiation of a generic enum with any affine payload is
   affine.
4. A fixed-size array whose element type is affine is affine.
5. A concrete closure that captures an affine value is affine.
6. A compiler-generated future whose frame contains affine state is affine.

Only compiler/runtime built-ins such as `resource::Handle` are leaf resources.
Ordinary `resource struct` declarations are nominal resource wrappers that
compose affine fields. Aggregates containing resources are cleaned up by
recursively cleaning their live fields.

Plain pointers and slices to resources are borrowed views. They do not own the
resource and are not affine merely because the pointed-to type is affine.

Concrete, non-generic structs and enums must declare `resource` when they store
an affine field directly:

```ciel
struct BadSession {
    File file; // error: non-resource struct stores a resource field
}

resource struct Session {
    File file;
    Text name;
}
```

This keeps resource ownership visible at nominal API boundaries. Generic
containers are different: they may be ordinary declarations, and their concrete
instantiations become affine when their type arguments make a field affine:

```ciel
struct Box<T> {
    T value;
}

Box<i64> number_box; // ordinary value
Box<File> file_box;  // affine concrete type
```

## Structural Cleanup

Do not add a public user-defined destructor interface in the first version.
Cleanup is structural and bottoms out at compiler-known runtime resource
handles.

`/std/resource::Handle` is the runtime resource key. Its representation is
private to the standard library and runtime, and it is a compiler-known affine
leaf. The close function for the actual host resource is registered in the
runtime registry when the handle is created:

```ciel
// /std/resource
export resource unsafe struct Handle {
    *void token;
}
```

`Handle` itself remains an unsafe representation type: ordinary safe code
should not manufacture or inspect raw handles. High-level resource wrappers are
safe resource structs that contain one or more handles or other affine resource
fields:

```ciel
export resource struct File {
    resource::Handle handle;
}
```

A `resource struct` must contain at least one owning affine resource field after
layout substitution, unless it is a compiler/runtime built-in leaf such as
`resource::Handle`. Marking an ordinary GC-only data structure as `resource` is
therefore a compile-time error:

```ciel
resource struct BadBox<T> {
    T value; // error unless T is known affine for every valid instantiation
}
```

This prevents `resource` from becoming a general RAII destructor mechanism for
GC-managed memory.

`unsafe` does not relax this rule. A `resource unsafe struct` is allowed when
the wrapper representation has unsafe invariants, but it must still
transitively contain an owning affine resource field unless it is a
compiler/runtime built-in leaf:

```ciel
resource unsafe struct NativeSocket {
    resource::Handle handle; // ok
    i32 domain;
}

resource unsafe struct FakeResource {
    *void ptr; // error: no owning affine resource field
}
```

When generated cleanup runs for an affine value:

1. if the value is a `resource::Handle`, the runtime registry closes that
   entry;
2. if the value is a struct, the compiler cleans up affine fields in reverse
   field order;
3. if the value is an enum, the compiler cleans up the active variant payload;
4. if the value is an array, the compiler cleans up initialized elements in
   reverse index order;
5. non-affine fields are ignored by cleanup and remain GC-managed.

Explicit close remains an ordinary consuming operation:

```ciel
export Result<void, Error> close(File file) = .close;
```

Automatic structural cleanup guarantees release when users do not write
`close`. `close` exists for early release and for reporting close errors. It
consumes the resource and internally performs the same registry close that
automatic cleanup would eventually perform.

## Boundary With GC

`resource` is not a smaller GC or an opt-out from GC. It exists only for
non-memory capabilities whose lifetime must be detached from object
reachability: file descriptors, sockets, locks, native library handles, timers,
async operation tokens, and similar host resources.

The split from GC is kept narrow by these rules:

1. Ordinary Ciel allocations never need `resource`.
2. GC-backed buffers, strings, maps, arrays, and object graphs remain ordinary
   values even when they are stored inside a resource wrapper.
3. A `resource struct` must transitively contain an owning runtime resource
   handle. Without such a handle, it cannot be declared `resource`.
4. Cleanup is not arbitrary user code. It is structural cleanup of affine
   fields and registry close of `resource::Handle` leaves.
5. Registering a native resource handle is an unsafe standard-library/runtime
   operation. Safe code can use the returned resource wrapper, but it cannot
   turn an arbitrary GC object into a resource with a custom finalizer.
6. If a library wants an owned memory container, it should use GC-backed
   storage such as `Bytes`, `Text`, `RawStorage<T>`, or a future `Vec<T>`, not
   `resource`.

The mental model is therefore one unified ownership rule:

```text
GC owns memory reachability.
resource owns non-memory capabilities registered in the runtime.
ordinary structs compose both by containing ordinary fields and resource fields.
```

There is no general destructor layer between them.

## Affine Local State

Extend definite-assignment state for affine locals and affine fields stored in
compiler-managed slots:

```text
Uninit
Live
Moved
MaybeLive
```

The rules are:

1. A local initialized with an affine value becomes `Live`.
2. Moving a `Live` affine value by value changes the source state to `Moved`.
3. Reading, borrowing, assigning through, or returning a `Moved` affine value is
   a compile-time error.
4. A mutable binding may be assigned a new affine value after it is `Moved`.
5. An immutable binding may not be reinitialized after a move.
6. Branch merges follow definite-assignment shape:
   `Live + Live => Live`, `Moved + Moved => Moved`,
   `Live + Moved => MaybeLive`, and `MaybeLive + anything => MaybeLive`.
7. On any cleanup edge, the compiler emits conditional cleanup for `Live` or
   `MaybeLive` affine slots.
8. Moving an affine local declared outside a loop body from inside that loop
   body is rejected in the first implementation. This includes explicit close,
   by-value calls, assignment moves, and moves into nested closures or futures.
   The only exception is a move into a `return` expression, because it exits the
   resource's owning scope and no later loop iteration can observe the moved
   local.

Affine rvalues are always materialized into compiler temporaries when needed.
The temporary has ordinary cleanup state:

1. if it is immediately passed to a by-value parameter, it is moved into the
   callee;
2. if it is immediately returned, it is moved to the caller;
3. if it is borrowed for a noescape call, it lives until that call completes and
   is then cleaned up unless the call moved it through another by-value path;
4. if it is an unused expression statement, it is cleaned up at the end of that
   statement;
5. if a borrow of the temporary would escape, the program is rejected.

This gives chained calls such as `use(open_file(path)?)` deterministic
semantics instead of depending on an implementation choice.

## Move Semantics

By-value use of an affine expression is a move:

```ciel
File file = io::open_read(path)?;
File other = file;      // move
io::read(&file, buf)?;  // error: file was moved
```

These operations move affine values:

1. assignment into a local or field;
2. passing to a by-value parameter;
3. returning from a function;
4. constructing a struct field or enum payload;
5. array element initialization;
6. closure capture by value;
7. storing into an async frame.

These operations do not move:

1. `&value` and receiver auto-borrow;
2. reading through `*const T` or `*T` without moving out;
3. passing to `*const T` or `*T` parameters;
4. field access used only to borrow.

Copying an affine value is never implicit. If a resource type wants a separate
logical duplicate, it must expose an explicit API such as `try_clone_file` or
`dup_stream`, and that API creates a fresh resource value backed by a distinct
runtime entry or a documented shared handle.

## Borrowing Resource Values

Resource borrowing is intentionally much smaller than Rust borrowing. A borrow
of an affine value is a noescape view:

```ciel
usize n = io::read(&file, out)?;
```

Safe code may create `*const T` or `*T` borrows of affine values only in
contexts the compiler can prove do not store the pointer:

1. direct call arguments;
2. receiver selector auto-borrow;
3. direct calls to Ciel functions whose signature and body are known to be
   noescape for that parameter;
4. imported `extern "C"` parameters explicitly marked `noescape`, treated as a
   trusted unsafe contract.

Safe code cannot store, return, capture, or send a pointer or slice borrowed
from a resource value. This rule is syntactic and type-checker-owned. It is not
derived from escape analysis.

Moving out through a resource borrow is rejected in safe code. A function that
wants to consume a resource must take it by value.

## Aggregates

Aggregates containing affine values are affine. The first implementation should
avoid partial-move complexity:

1. Moving an affine aggregate moves the entire aggregate.
2. Borrowing a field of an affine aggregate is allowed when the base aggregate
   remains live.
3. Moving a single resource field out of an aggregate is rejected in safe code.
4. Replacing a single resource field of an affine aggregate is rejected in safe
   code.
5. Replacing the whole affine aggregate is allowed. The old whole value is
   cleaned up before the new whole value is stored.

This keeps cleanup simple: every affine aggregate slot has one initialized
state and generated cleanup recursively walks its fields.

Later versions may add field-level move state if it becomes important.

## Function Boundaries

Function parameters of affine type are owned by the callee:

```ciel
Result<void, Error> close(File file);
```

When the call starts, the caller has moved the resource. If the callee returns
without moving the parameter elsewhere, the callee's cleanup drops it.

Returning an affine value transfers ownership to the caller:

```ciel
Result<File, Error> open_read([]const char path);
```

On `Ok(file)`, the caller owns the returned resource. On `Err`, any temporaries
created before returning the error are cleaned up by the callee.

## Resource-Qualified Type Parameters

Resource-only generic APIs use a built-in type-parameter modifier, not an
interface-style bound:

```ebnf
GenericParam ::= [ "resource" ] Identifier [ ":" ConstraintExpr ]
```

```ciel
Result<void, Error> send_move<resource T>(MoveSender<T> sender, T value);
```

`resource T` is a compiler-known type-parameter property. It is not an
interface, it has no impls, and user code cannot satisfy it by declaring a
method. The compiler derives it from `resource struct` declarations and from
aggregates that structurally contain resources.

The modifier is written at the generic binder. After the parameter is bound,
ordinary type positions keep using `T`, not `resource T`. `resource T` means
"this type argument must be resource-affine", not "wrap `T` in a new type".

A bare type parameter such as `<T>` is an ordinary generic parameter. It does
not announce that the API is resource-only. It may still be instantiated with a
resource-affine type when the generic body is affine-correct. Public APIs that
specifically consume, store, or route resources across non-lexical ownership
boundaries should write `<resource T>` so the resource requirement is visible
in the signature.

Generic data types such as `Result<T, E>` do not need to write `resource T`.
They can be instantiated with resource types, and the resulting concrete
`Result<File, Error>` becomes affine structurally. The modifier is needed at
operation boundaries, not for every generic container definition.

Generic functions that copy values must still be checked against affine
instantiations. In the first implementation, generic code that duplicates `T`
is rejected when instantiated with a resource-affine type unless a future
copy-only type-parameter modifier is added.

## Scoped Resource Owners

Scoped helpers do not need an explicit transfer API. Ordinary return-by-value
move is enough.

`resource::scoped` creates a child owner and checks the body under that owner.
When the body returns, every resource still live in the child scope is cleaned
up. A resource that is returned from the body has been moved out of the child
scope, so it is not cleaned up there:

```ciel
Result<File, Error> open_for_caller([]const char path) {
    return resource::scoped<File>(|| {
        File file = io::open_read(path)?;
        return file; // move to caller
    });
}
```

The compiler lowers the moved-out return value so ownership is reattached to
the caller's current owner before the child owner closes. This reattachment is a
compiler/runtime operation, not a source-level transfer call.

Resources that are not moved out are cleaned up at the end of the scoped body:

```ciel
Result<void, Error> use_temp([]const char path) {
    return resource::scoped<void>(|| {
        File file = io::open_read(path)?;
        io::read(&file, scratch)?;
        return Ok(void); // file remains live and is closed here
    });
}
```

This rule is stronger and simpler than a type-level "result contains no
resource" bound: the checker tracks whether the actual value was moved out.

## Non-Lexical Ownership Boundaries

Lexical moves and returns cover ordinary function calls and scoped helpers.
Explicit move APIs are still needed at non-lexical ownership boundaries where a
resource leaves the current task, actor, or Ciel ownership domain.

For cross-task and cross-actor transfer, use a separate affine transfer-only
boundary. The current standard library keeps the raw registry token as a
resource-internal fallback; it does not expose per-resource message tokens such
as an async TCP stream transfer wrapper:

```ciel
export resource unsafe struct TransferToken {
    Handle handle;
}
```

The token itself is affine. It can be moved through a transfer-only boundary,
but it is not cloneable `Message`. Ordinary task and channel APIs remain
clone-based, so live resources are not sent through them.

FFI extraction and adoption are also explicit non-lexical boundaries:

```ciel
export unsafe RawFd into_raw_fd(File file);
export unsafe Result<File, Error> from_raw_fd(RawFd fd);
```

`into_raw_fd` consumes the resource and removes it from Ciel's resource
tracking. `from_raw_fd` adopts a host resource into the current Ciel owner.

## Message And Cross-Task Policy

`Message` is clone-based. Live resources are move-based. Therefore:

1. A `resource struct` cannot implement `Message`.
2. Any aggregate containing a live resource cannot implement `Message`.
3. Concrete closures capturing resources cannot satisfy `Message`.
4. Spawned tasks cannot capture resources through ordinary `async::spawn`
   unless the spawn API is explicitly a move-transfer boundary.
5. Async channels cannot send resources through ordinary clone-message channels.

The standard library may add explicit move-only surfaces:

```ciel
MoveChannel<T> move_channel<resource T>();
Result<void, Error> send_move<resource T>(MoveSender<T> sender, T value);
async Result<T, Error> recv_move<resource T>(MoveReceiver<T> receiver);
```

The first implementation may omit general move channels and keep only
domain-specific transfer tokens, such as TCP stream transfer.

## Async Frames And Futures

Async functions may own resources across `await` if the resource type is
documented frame-safe. This is separate from `Message`; values inside one task
do not cross task ownership merely by living in the task frame.

Resource borrows do not live across `await` in the first implementation. An
async operation that must keep a resource while suspended takes that resource by
value and returns it, or returns a wrapper that contains it:

```ciel
resource struct ReadResult {
    AsyncFd file;
    Bytes bytes;
}

async Result<ReadResult, Error> read_bytes(AsyncFd file, usize max_len);

ReadResult result = await async_io::read_bytes(file, 4096)?;
file = result.file;
Bytes bytes = result.bytes;
```

This is more explicit than a resource loan, but it avoids introducing a
general borrow checker for resources across suspension points.

The async lowering must store affine state in the generated frame:

1. every affine local that is live across an await has a frame slot and an
   initialized-state bit;
2. moving the value clears that bit;
3. normal return, `Err` return through `?`, cancellation, and abort run cleanup
   for every live affine slot in program-counter order;
4. generated future values are themselves affine when their frame owns affine
   state.

Dropping an unpolled or incomplete generated future must run cleanup for every
initialized resource in the future frame. Pending operation cleanup consumes
the operation token exactly once, but the operation performed depends on the
cleanup context:

1. If a pending future is dropped while the current task continues, cleanup uses
   the cancellation path and requires `CancelSafe`.
2. If the owning task is ending through cancellation, panic, or runtime
   teardown, cleanup uses the abort path and requires `Abortable`.

Affine tracking proves that the operation token is consumed once. `CancelSafe`
and `Abortable` prove that the chosen cleanup action is semantically valid for
the async protocol.

Low-level async operation tokens are `resource struct`s. `finish` and `cancel`
consume the token. A second finish or cancel is a compile-time move error in
safe code, with runtime registry validation remaining for unsafe stale copies and
callback races.

## Select And Cancellation

`select` creates multiple candidate futures. If an arm future owns a resource
or operation token, losing the race must either:

1. cancel the losing future through its `CancelSafe` path, consuming its
   operation token; or
2. reject the select expression because cancellation could hide user-visible
   state.

This matches the current `SelectableFuture = Awaitable + CancelSafe +
Abortable` policy. The affine change strengthens it by making operation token
single-consumer behavior statically visible.

## Runtime Registry Role

The runtime registry remains mandatory, but it is not a substitute for affine
checking. The type checker proves ownership for safe Ciel source, including
resources stored in async frames and operation tokens consumed by `finish`,
`cancel`, cleanup, or abort. The registry still provides:

1. registry validation against descriptor reuse;
2. stale async callback rejection;
3. owner hierarchy and quota enforcement;
4. task and actor owner teardown;
5. fallback cleanup for unsafe or FFI-created handles;
6. poisoning and revocation after abort;
7. diagnostics for cleanup failures;
8. implementation support for owner reattachment on move-out boundaries.

Source-level affine rules prevent ordinary safe mistakes, including async
use-after-close, double finish, double cancel, and cleanup leaks. Runtime checks
defend host-resource identity and external events that static checking
intentionally does not try to prove, such as stale kernel/runtime callbacks and
unsafe duplicated raw handles.

## C Interop And Unsafe

Raw descriptors and raw pointers remain unsafe escape hatches:

```ciel
export unsafe RawFd into_raw_fd(File file);
export unsafe Result<File, Error> from_raw_fd(RawFd fd);
```

`into_raw_fd` consumes the `File`. After extraction, Ciel no longer owns the
resource unless the raw descriptor is adopted back through `from_raw_fd`.

Imported C functions may receive resource borrows only through `noescape`
pointer parameters. Passing an owned resource by value to C is invalid unless a
wrapper explicitly consumes it and translates it to the C ABI representation.

Unsafe code may still create stale or duplicated raw handles if it violates the
contract. The runtime registry must continue to validate all standard-library
resource operations before touching host resources.

## Diagnostics

Diagnostics should report ownership events, not escape-analysis guesses:

```ciel
File file = io::open_read(path)?;
io::close(file)?;
io::read(&file, buf)?;
```

Expected diagnostic:

```text
error: use of moved resource `file`
note: `file` was moved here by call to `close`
```

For a non-resource concrete aggregate:

```text
error: non-resource struct `Session` cannot store resource field `file`
note: declare `resource struct Session` or store a borrowed view instead
```

For resource copies:

```text
error: resource type `File` cannot be copied
note: pass `&file` to borrow it or move `file` to transfer ownership
```

For loop-carried moves:

```text
error: resource `file` is declared outside this loop and cannot be moved inside it
note: declare the resource inside the loop body or return it from the enclosing function
```

Diagnostics must not say "this value escapes" as the reason for rejecting
resource code.

## Standard Library Migration

The migration should happen in layers:

1. Add parser and type representation for `resource struct`.
2. Make `/std/resource::Handle` a compiler-known affine leaf whose cleanup is
   registry close.
3. Keep `/std/resource::Handle` an unsafe implementation detail, not the public
   source-level resource property.
4. Mark `File`, `TcpStream`, `TcpListener`, async fd types, async TCP stream
   types, split halves, operation tokens, transfer-token wrappers, SQLite
   handles, and similar wrappers as `resource struct`.
5. Change non-consuming operations to borrow:

   ```ciel
   Result<usize, Error> read(*const File file, []u8 out);
   Result<usize, Error> write(*const File file, []const u8 data);
   ```

6. Change consuming operations to by-value:

   ```ciel
   Result<void, Error> close(File file);
   Result<AsyncTcpSplit, Error> split(AsyncTcpStream stream);
   Result<Bytes, Error> finish_read(AsyncTcpRead op);
   Result<void, Error> cancel_read(AsyncTcpRead op);
   ```

7. Remove public scoped-result marker bounds; scoped helpers rely on ordinary
   return-value move and generated cleanup state.
8. Update async operation adapters so operation tokens are consumed by finish
   or cancel.
9. Change high-level async resource APIs to take resources by value and return
   the resource, or a resource wrapper containing it, when the resource remains
   usable after `await`.
10. Update tests from "copied stale token fails at runtime" toward "copy or
   second use is rejected at compile time", while retaining runtime stale tests
   under unsafe construction.

## Type Checker Implementation Plan

The first implementation can be conservative:

1. Store `is_resource_decl` on struct definitions.
2. Parse `resource T` in generic parameter lists and store the resource-only
   property on the type parameter.
3. Add an `is_affine_type(Ty)` query that derives through aggregates and
   concrete closures.
4. Add a `resource_cleanup_shape(Ty)` query that bottoms out at
   `resource::Handle` leaves and recursively walks affine aggregate fields.
5. Reject instantiating a `resource T` parameter with a non-affine type.
6. Extend local state from definite assignment with affine liveness.
7. During expression checking, mark by-value affine expression use as a move.
8. During block checking, record cleanup actions for affine locals.
9. During branch merge, merge affine liveness states.
10. Reject loop-body moves of affine locals declared outside the loop, except
    moves into `return`.
11. Materialize affine rvalues into compiler temporaries with fixed cleanup
    state.
12. Reject partial moves and field replacement for affine aggregates.
13. During call checking, distinguish by-value parameters from pointer
    parameters and receiver auto-borrows.
14. During return checking, move returned affine values into the caller result
    and suppress local cleanup for those moved slots.
15. During async lowering, materialize frame cleanup bits for affine slots.
16. Replace scoped helper marker constraints in std helper signatures with
    ordinary return-value move plus generated cleanup state.

This should be implemented before adding broad move-only channels or general
field-level partial moves.

## Soundness Argument

Within safe Ciel:

1. Every owning resource value is represented by exactly one live affine slot at
   a time.
2. By-value use, including return, moves ownership and invalidates the source
   slot.
3. Borrowed views cannot escape, so they cannot outlive the owning slot or be
   used after the owner is moved.
4. Every normal control-flow edge out of a scope cleans up live affine slots.
5. Explicit consuming operations clear the source slot, preventing double
   close, double finish, and use-after-close.
6. Resources returned from a closing scoped owner are moved out before scoped
   cleanup runs, so they are not dropped with the child scope.
7. Cross-task clone boundaries reject live resources because resources are not
   `Message`.
8. Async cancellation and abort run the same cleanup state used by normal
   returns. Pending operation cleanup consumes the operation token once, using
   the cancellation path only when `CancelSafe` holds and the abort path when
   task teardown requires `Abortable`.
9. Unsafe and FFI can violate source-level uniqueness, but every standard
   resource operation validates the runtime registry token before touching the
   host resource.

Therefore safe Ciel code cannot use, close, finish, or cancel the same
non-memory resource twice through ordinary source operations, and cannot leak a
resource on normal cleanup paths merely by forgetting to call `close`.

## Settled First-Version Choices

1. `resource struct` is a safe stricter declaration, not an unsafe one.
2. `unsafe struct` remains available only for representation invariants.
3. The first version has no public user-defined drop hook.
4. Explicit `close(File)` consumes the file even if the OS close reports an
   error, because ownership of the descriptor cannot remain well-defined after
   an attempted close.
5. Resource borrows do not live across `await` in the first implementation.
6. Partial moves out of resource aggregates are rejected in the first
   implementation.
7. Resource-only generic APIs use `<resource T>`, not an interface-style bound.
8. Ordinary lexical moves and returns do not need explicit transfer functions.
   Explicit move APIs are reserved for non-lexical ownership boundaries.
9. Affine rvalues are materialized into compiler temporaries with fixed cleanup
   state.
10. Loop-body moves of affine locals declared outside the loop are rejected,
    except for moves into `return`.
11. Async operations that need resources across `await` own those resources by
    value and return them when they remain usable.

## Open Questions

1. Should a future `copy T` or `value T` modifier exist so copy-oriented
   generic functions can reject resources earlier?
2. When should the language add general move-only channels instead of
   domain-specific transfer tokens?
