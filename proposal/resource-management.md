# Owner-Based Resource Management Proposal

This proposal adds deterministic management for non-memory resources while
preserving Ciel's garbage-collected memory model.

Memory remains GC-managed. Source programs do not choose stack or heap
placement, and escape analysis remains an optimization only. Non-memory
resources such as file descriptors, sockets, database connections, prepared
statements, file locks, timers, and async operation tokens are managed through
explicit resource owners. A resource owner tracks the real host resource in a
runtime table and closes it when the owner is closed. User-visible resource
values are revocable handle tokens, not the owner of the underlying resource.

The model is RAII-like at the resource-owner boundary, not at every ordinary
value. It provides structured deterministic release for resources without
adding manual memory management, general object destructors, or a full affine
type system.

## Proposal Order

```text
pure-library-message <= resource-management[resource capability boundaries]
dispatch-actor-io-runtime <= resource-management[task and actor resource owners]
async-await <= resource-management[async task cleanup and cancellation]
actor-owned-state <= resource-management[actor-owned resource state]
generic-growable-storage || resource-management[GC storage is not resource ownership]
```

`pure-library-message` owns the existing `Message`, `ShareHandle`, and
`ThreadLocal` capability lattice. This proposal reuses that lattice for
cross-task and cross-actor boundaries, but it does not make ordinary resource
handles `Message` by default.

`dispatch-actor-io-runtime` and `async-await` own the runtime execution model
that supplies task and actor lifetimes. Resource owners are attached to those
lifetimes. `actor-owned-state` remains the path for long-lived actor-local
state that contains resources.

`generic-growable-storage` is independent. GC-backed storage is still memory,
not a deterministic external resource.

## Problem

Ciel is garbage-collected. That solves memory lifetime, but it does not solve
resource lifetime. A wrapper value can become unreachable long before the GC
runs, and finalization is not a scheduling guarantee. A live wrapper can also be
copied into ordinary heap data while the underlying file descriptor or socket is
still open.

The current standard library uses three partial patterns:

1. Scoped helpers such as `/std/io::with_open` open a resource, pass a private
   token to a callback, and close on callback return.
2. Descriptor slot tables with generation checks prevent stale copied tokens
   from touching a reused host descriptor.
3. APIs expose explicit `close` operations and rely on callers to remember
   `defer close(...)` or to choose a scoped helper.

These patterns are useful, but they do not form a complete resource safety
story:

1. Direct open APIs can return handles that are never closed.
2. Requiring every public API to accept an explicit scope parameter would
   pollute ordinary application signatures.
3. Treating escape analysis as a semantic proof is unacceptable. Ciel source
   semantics should not depend on whether a value was stack-allocated,
   promoted, or conservatively treated as escaping.
4. Ordinary `defer` is not enough. It is an explicit call registration
   mechanism, not a resource ownership proof.
5. Panic is process termination in the current model. It does not unwind and
   does not run resource cleanup.

## Goals

1. Deterministically release non-memory resources on normal owner closure.
2. Preserve GC-managed memory and keep escape analysis as an optimization only.
3. Avoid requiring every API that may open a resource to accept an explicit
   scope parameter.
4. Allow resource handle tokens to be passed to functions and stored in heap
   data without losing track of the real resource.
5. Let callers create short resource scopes when they need bounded lifetime.
6. Let long-lived resources belong to task, actor, or process owners.
7. Prevent stale tokens from touching reused host resources.
8. Keep cross-task and cross-actor resource movement explicit.
9. Integrate with async cancellation and actor shutdown where the runtime
   continues executing cleanup.
10. Keep unsafe C interop as an explicit escape hatch.

## Non-Goals

1. Manual memory allocation or manual memory free.
2. C++-style destructors for every ordinary value.
3. GC finalizers as part of the resource safety contract.
4. A full affine or linear type system in the first version.
5. Treating compiler escape analysis as a source-language guarantee.
6. Running Ciel cleanup after panic, `exit`, host process kill, or abort.
7. Proving that stale resource tokens are impossible in all heap-aliasing
   scenarios.
8. Inferring the shortest possible resource lifetime from last use.
9. Making actor-local or task-local resources generally `Message`.
10. Hiding explicit lifetime extension. Moving a resource to a longer-lived
    owner remains explicit.

## Resource Owners

A resource owner is a runtime object that owns zero or more resource table
entries. Each entry contains:

1. a resource id;
2. a generation;
3. a resource kind;
4. the raw host resource or runtime resource pointer;
5. a close function;
6. a state flag such as open, closed, closing, or poisoned;
7. optional owner-specific metadata.

Owners form a structured hierarchy:

1. lexical resource scopes;
2. async task owners;
3. actor owners;
4. process owners.

Closing an owner closes every live resource entry owned by that owner, then
closes child owners according to a defined order. The order should be
deterministic, with last-acquired-first-closed as the default for resources in
one owner.

The owner table is the source of truth. The compiler and runtime do not need to
find every copied handle token in the heap before closing a resource.

## Ambient Current Owner

Opening a resource uses the current ambient resource owner by default. The
ambient owner is task-local or actor-local runtime state, with lexical resource
scopes temporarily installing a child owner.

Application code can therefore write ordinary APIs:

```ciel
Result<Bytes, Error> load_config([]const char path) {
    File file = io::open_read(path)?;
    return io::read_all(file);
}
```

`io::open_read` registers the real file descriptor in the current owner and
returns a handle token. `load_config` does not need a scope parameter.

Callers that need a bounded lifetime create a local resource scope:

```ciel
Result<Bytes, Error> read_config([]const char path) {
    return resource::scoped<Bytes>(|| {
        return load_config(path);
    });
}
```

When the scoped body returns, the child owner closes all resources opened inside
the body that were not explicitly transferred to another owner.

If no lexical resource scope is active, the current task or actor owner is used.
Top-level synchronous programs run inside a process-root owner. A process-root
resource is closed during normal program shutdown when possible; panic and
direct process exit are outside the Ciel cleanup guarantee.

## Handle Tokens

Resource values visible to Ciel code are revocable handle tokens:

```ciel
export struct File {
    resource::Handle handle;
}
```

The exact representation is private, but conceptually a handle stores:

```text
owner id, resource id, generation
```

Copying a handle token does not copy or own the underlying resource. Storing a
handle token in a struct, enum, array, closure environment, or GC-backed
container does not change the registered owner.

Operations validate a handle before touching the host resource:

1. the owner exists and is accessible from the current execution domain;
2. the resource id exists in the owner table;
3. the generation matches;
4. the entry is open and not poisoned;
5. the operation is allowed for the resource kind.

If validation fails, the operation returns a stable resource error. It must not
operate on a reused host descriptor or dereference stale runtime state.

Closing a resource entry revokes all existing token copies. Reusing an internal
resource id increments the generation.

## Heap Storage

Safe Ciel may store resource handle tokens in ordinary heap values. This is not
an escape-analysis exception. It is part of the model:

```ciel
struct ServerState {
    File log;
}

Result<void, Error> write_log(ServerState state, []const char text) {
    return io::write_text(state.log, text);
}
```

The heap object does not own the real file descriptor. The owner table owns it.
When the owner closes, `state.log` becomes a stale token. Future operations on
that token return a resource error.

If a program wants the resource to outlive the current owner, it must explicitly
transfer the resource entry to a longer-lived owner before the current owner
closes. Merely storing the token in a longer-lived heap object does not extend
the resource lifetime.

## Scoped Blocks

`resource::scoped` creates a child owner, installs it as the current owner for
the body, and closes it after the body returns:

```ciel
export Result<R, Error> scoped<R: Message>(Result<R, Error> |()| body);
```

The result type is constrained so a scoped resource token cannot be returned
through the ordinary result path. A later implementation may use a more direct
`NonResource` or `ResourceFree` capability instead of `Message`, but the first
shape should reuse existing capability machinery if practical.

The body may call arbitrarily deep functions. Every resource opened inside
those functions registers with the current scoped owner unless a function
explicitly opens in or transfers to another owner.

`resource::scoped` is not a memory arena. Values allocated inside the body
remain GC-managed values. Closing the resource scope releases only registered
non-memory resources.

## Explicit Transfer

Extending a resource lifetime is explicit. The core transfer operations are:

```ciel
export Result<File, Error> transfer_to_parent(File file);
export Result<File, Error> transfer_to_task(File file, async::TaskOwner owner);
export Result<File, Error> transfer_to_actor(File file, actor::ActorOwner owner);
```

The exact API names are placeholders. The contract is:

1. transfer removes the resource entry from the source owner or marks it moved;
2. transfer installs a new entry in the destination owner;
3. transfer returns a fresh handle token;
4. old token copies are revoked, usually by advancing generation;
5. transfer fails if the destination owner is closed or incompatible.

This makes lifetime extension visible without requiring all normal APIs to pass
scope parameters.

Examples:

```ciel
Result<void, Error> cache_file(Cache cache, []const char path) {
    File file = io::open_read(path)?;
    File cached = resource::transfer_to_parent(file)?;
    cache.insert(path, cached)?;
    return Ok;
}
```

The stored token is valid because the real resource was transferred to an owner
that outlives the local scoped owner.

## Early Close

Explicit close remains useful:

```ciel
export Result<void, Error> close(File file);
```

`close` closes the owner-table entry and revokes all token copies. It does not
need to find those copies. Repeated close returns a stable error or a documented
idempotent success, depending on the resource type.

Implicit owner close cannot reliably report close errors to the source program.
APIs where close errors matter should expose an explicit close or flush
operation. Owner cleanup still performs best-effort release for any resource
that remains open.

## Async and Task Owners

Each async task has a resource owner. `resource::scoped` inside an async body
creates a child owner stored in the task state. Awaiting does not close the
scope. Normal return, `Err` propagation, task cancellation, and runtime abort of
the task close active owners before the task is considered finished.

Panic is not part of this guarantee. If panic is immediate process termination,
task cleanup does not run.

Resource handles are not generally `Message`. A spawned task cannot capture a
task-local resource handle through the ordinary `Message` crossing path. Safe
cross-task use requires one of:

1. constructing the resource inside the spawned task;
2. transferring the resource to the spawned task owner;
3. using a resource type that is explicitly a synchronized `ShareHandle`;
4. communicating with an actor that owns the resource.

Async operation tokens are resources too. A pending operation belongs to an
owner and has a cleanup path that cancels, finishes, or poisons the operation
according to its `CancelSafe` and `Abortable` contracts.

## Actor Owners

Each actor has a resource owner. Actor-owned state may contain resource handle
tokens that point to entries owned by that actor. Stopping or joining the actor
closes the actor owner after accepted work has drained according to actor
runtime policy.

This keeps actor-local resources out of `Message`:

```ciel
struct ServerState {
    TcpListener listener;
    HashMap<u32, TcpStream> streams;
}
```

The state object is memory and remains GC-managed. The listener and stream
entries are non-memory resources owned by the actor owner.

Message payloads still use the existing `Message` policy. Sending a resource
handle through a mailbox is rejected unless the handle type explicitly supports
message cloning or transfer through a separate move-only channel facility.

## Capability Policy

Resource handle types should have a standard marker, tentatively:

```ciel
unsafe interface<T> bool resource_handle_marker(*const T value);

interface ResourceHandle = resource_handle_marker + !MessageInternal;
```

The exact alias must fit the accepted `/std/message` lattice. The important
policy is:

1. resource handles are not `Message` by default;
2. resource handles are not `ShareHandle` by default;
3. a wrapper can implement `ShareHandle` only when its operations are internally
   synchronized or immutable and its cleanup model is well-defined;
4. a wrapper can implement `Message` only by constructing an independent
   receiver-owned resource or by using an explicit synchronized shared handle
   policy;
5. `meta::Repr<T>` must not bypass resource-local policy.

`ThreadLocal` remains useful for resources that are tied to one actor or task.
Some resources may be process-shareable but still require owner registration.
The proposal should not force every resource handle to be `ThreadLocal`; it
should force every resource handle to have an explicit boundary policy.

## Runtime Model

The runtime provides:

1. owner creation and close;
2. current-owner push/pop for scoped blocks;
3. task-local and actor-local current owner lookup;
4. owner-table registration;
5. handle validation;
6. resource close and transfer;
7. generation advancement;
8. diagnostic resource errors;
9. optional leak diagnostics at process shutdown.

The runtime must not depend on GC finalizers for correctness. A finalizer may
report a leaked owner or perform emergency cleanup as a debugging aid, but the
language guarantee comes from owner close.

## Standard Library Shape

Resource modules should expose normal APIs that use the current owner:

```ciel
export Result<File, Error> open_read([]const char path);
export Result<File, Error> create([]const char path);
export Result<usize, Error> read(File file, []u8 out);
export Result<void, Error> close(File file);
```

Explicit-owner variants are for framework code and unusual lifetime control:

```ciel
export Result<File, Error> open_read_in(resource::Owner owner, []const char path);
```

Scoped helpers remain useful as convenience wrappers:

```ciel
export Result<R, Error> with_open_read<R: Message>(
    []const char path,
    Result<R, Error> |(File)| body
);
```

They can be implemented using `resource::scoped` plus the current-owner
open operation.

Low-level raw descriptor APIs remain `unsafe` and outside the resource owner
guarantee unless wrapped back into an owner entry.

## Panic and Process Exit

Panic is immediate process termination in the current language model. It does
not unwind and does not run `defer` or resource owner cleanup.

The resource guarantee applies when the runtime continues to execute:

1. normal return;
2. `Err` propagation through `?`;
3. break and continue out of scoped blocks;
4. task cancellation;
5. task abort paths that are documented as cleanup-capable;
6. actor stop and join;
7. normal process shutdown.

Host process kill, abort, direct `exit`, and panic rely on operating-system
process cleanup. Programs that need transaction rollback, flush reporting, or
application-level shutdown must use explicit operations before panic-capable
paths.

## Migration Plan

1. Add a runtime resource owner table and handle representation.
2. Add a `/std/resource` module with owner, scoped block, close, and transfer
   primitives.
3. Rework `/std/io` to register files in the current owner while preserving
   scoped helpers.
4. Rework blocking `/std/net` listener and stream handles to use owner entries.
5. Rework SQLite connection and statement wrappers to use owner entries.
6. Classify async fd, async TCP stream, and async operation-token wrappers as
   resources or explicit share handles.
7. Attach owners to async task and actor runtime state.
8. Add stale-token and generation-reuse regression tests.
9. Add diagnostics or lints for opening many resources in a long-lived owner
   without an inner scope.
10. Keep raw fd adoption unsafe until it registers the fd with a resource owner.

## Test Plan

1. A resource opened inside `resource::scoped` is closed when the body returns.
2. A deep function can open a resource without accepting an explicit scope
   parameter, and the enclosing `resource::scoped` owner closes it.
3. A copied handle token stored in a struct becomes stale after owner close and
   operations return a stable error.
4. A token stored in a GC container does not prevent owner close.
5. Transferring a resource to a parent owner keeps the returned fresh token
   usable after the child scope closes.
6. Old token copies fail after transfer.
7. Actor state can store resource handles owned by the actor owner.
8. Sending a non-shareable resource handle through a channel or actor mailbox
   fails under the existing capability rules.
9. Task cancellation closes task-owned resources and pending operation tokens.
10. Panic tests document that cleanup does not run.

## Open Questions

1. Should the first public constraint for `resource::scoped` be `R: Message`, or
   should the language add a narrower `ResourceFree` capability?
2. Should stale-token operations return a shared `resource_closed_error()`, a
   module-specific error, or preserve existing generation-error behavior?
3. Should repeated explicit close be idempotent for all standard resources or
   resource-specific?
4. What transfer APIs are needed for task and actor owners without exposing
   unstable runtime internals?
5. Should `ShareHandle` resources use owner entries, reference-counted runtime
   state, or both?
6. How should owner close report multiple close errors during best-effort
   cleanup?
7. Should the compiler enforce that resource handles do not implement `Message`
   accidentally, or is the existing capability lattice sufficient?
8. Should `resource::scoped` be ordinary library syntax, special lowering, or
   later surface syntax such as `resource { ... }`?
