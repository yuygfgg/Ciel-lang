# Owner-Based Resource Management Proposal

## Historical Status

The implemented model is stricter than several early capability sketches below.
`Message` clones must be freely discardable and therefore cannot construct or
own a file, socket, permit, native lease, or manually released reference.
Deterministically cleaned values are resource-affine and move through resource
transfer APIs; a `ShareHandle` alias is valid only when discarding the alias has
no semantic cleanup effect. `design.md` is normative.

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
lifetimes. The resource registry proposed here must be a generalization of the
existing async operation-token routing and generation checks, not a second
parallel runtime substrate. `actor-owned-state` remains the path for long-lived
actor-local state that contains resources.

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
6. The async runtime already has operation tokens, task routing, callback
   generation checks, and cleanup paths. A resource proposal that adds a
   separate but similar table would duplicate the hardest runtime machinery.

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
11. Reuse and generalize the existing async operation-token substrate instead
    of adding a competing slot/generation system.
12. Keep high-level async APIs simple while making low-level async resource
    tokens explicit and resource-backed.

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
11. A second runtime registry that is independent from async operation tokens.
12. Unlimited per-task, per-actor, or process-wide resource growth.
13. Adding an explicit resource scope parameter to every high-level async I/O
    API.
14. Removing low-level async operation-token APIs that are still needed for
    runtime integration tests and custom adapters.

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

## Unification With Async Runtime

The current async runtime already uses the essential ingredients of this
proposal:

1. runtime operation tokens;
2. task-owned wakeup routing;
3. operation id and generation validation;
4. stale callback rejection;
5. cancellation and abort cleanup hooks.

Resource management should not introduce an unrelated second mechanism for the
same shape of problem. The resource registry is the shared substrate for both
ordinary resources and async operation tokens.

Conceptually, an async operation token is a resource entry whose close action is
operation-specific cleanup:

1. a sleep token cancels or finishes a timer;
2. an async read token cancels, finishes, or poisons the read operation;
3. an accept/connect token cancels or finishes the socket operation;
4. a buffered read token releases the runtime-owned operation state.

The accepted async generation checks remain the model for external callbacks.
Callbacks carry enough routing identity to find the target task and operation
entry, validate generation, and discard stale completions without touching
released task frames. This proposal extends that table discipline to blocking
file descriptors, blocking sockets, SQLite handles, statement handles, and
other non-memory resources.

The implementation may still use specialized structs internally for fast paths,
but there is one semantic model:

```text
owner -> registry entry -> kind-specific close/cancel/finish policy
handle token -> entry id + generation
```

Standard modules must not grow independent slot tables once the common registry
exists. Existing `/std/io` and `/std/net` generation-checked tables are
migration sources, not permanent parallel ownership systems.

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

## Identifier and Generation Exhaustion

Resource ids, owner ids, and generations must have defined exhaustion behavior.
The safety story must not rely on "this counter never overflows" as an
unstated assumption.

The registry should use 64-bit ids and generations. Overflow remains a checked
runtime condition:

1. allocating a new owner id fails with a stable `Error` classified as resource
   id exhaustion if no fresh owner id can be produced;
2. allocating a new resource id fails with a stable `Error` classified as
   resource id exhaustion if no fresh entry id can be produced;
3. advancing a generation at the maximum value permanently retires that entry
   slot instead of wrapping;
4. if no usable slot remains after retirements, registration fails with a
   stable resource id exhaustion error;
5. transfer that would require a fresh generation or entry and cannot allocate
   one fails without moving the resource.

Retired slots are never reused in a way that can make an old token valid again.
An implementation may compact or rebuild an owner table only if all live handle
tokens for retired entries remain invalid. It is acceptable for compaction to
allocate fresh entry ids and generations.

These errors are ordinary resource errors, not undefined behavior. They are
expected to be practically unreachable under normal 64-bit counters, but they
are still part of the contract.

## Owner Quotas and Resource Pressure

Each owner enforces resource limits. A task, actor, lexical scope, or process
owner must not be able to grow its registry without bound merely because safe
code forgot to create a shorter scope.

At minimum, an owner tracks:

1. live resource count;
2. child owner count;
3. pending async operation count;
4. optional kind-specific counts such as descriptors, timers, and database
   handles;
5. optional approximate native memory or kernel-resource cost.

Registration fails with a stable `Error` classified as resource limit
exhaustion when adding an entry would exceed the effective limit. Effective
limits are inherited from the parent owner unless an owner is created with an
explicit override. An override may raise or lower a child owner's own limits,
subject to any configured process-wide or ancestor aggregate caps.

The default limits should be conservative enough to catch accidental leaks in
long-lived tasks, while still high enough for normal servers. Programs that
need many concurrent descriptors should raise limits explicitly near the
service boundary instead of relying on unbounded task-local ownership:

```ciel
export struct Limits {
    usize max_resources;
    usize max_child_owners;
    usize max_pending_ops;
    usize max_descriptors;
}

export Result<R, Error> scoped_with_limits<R: ResourceFree>(
    Limits limits,
    Result<R, Error> |()| body
);

resource::scoped_with_limits<void>(resource::Limits {
    max_resources: 4096,
    max_child_owners: 1024,
    max_pending_ops: 8192,
    max_descriptors: 4096,
}, || {
    return serve_many_connections();
});
```

Async code has the matching form:

```ciel
export async Result<R, Error> scoped_async_with_limits<R: ResourceFree>(
    Limits limits,
    async_core::Future<Result<R, Error>> |()| body
);
```

Task, actor, and group construction also need limit-bearing variants so quota
changes can be made at ownership boundaries instead of inside arbitrary helper
functions:

```ciel
export Result<Task<Out>, Error> spawn_with_limits<Out: Message>(
    resource::Limits limits,
    async_core::Future<Result<Out, Error>> |()| body
);

export async Result<R, Error> with_task_group_with_limits<
    T: Message,
    R: ResourceFree
>(
    resource::Limits limits,
    async_core::Future<Result<R, Error>> |(*const TaskGroup<T>)| body
);

export Result<ActorHandle<A>, Error> spawn_actor_with_limits<A: Actor>(
    resource::Limits limits,
    A initial_state
);
```

The runtime also exposes process or runtime-default configuration for embedding
and service setup. The exact configuration transport is implementation-defined,
but safe Ciel must have a way to install stricter or looser owner defaults at a
clear boundary before the affected owners are created.

Owner cleanup releases quota as entries close. Transfer releases quota from the
source owner and consumes quota in the destination owner atomically; if the
destination has no capacity, transfer fails and the source resource remains
owned by the source owner.

Quotas are not a substitute for application-level backpressure. They are the
runtime safety floor that prevents one task or actor from silently accumulating
unbounded resources.

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
export Result<R, Error> scoped<R: ResourceFree>(Result<R, Error> |()| body);
```

The result type is constrained so a scoped resource token cannot be returned
through the ordinary result path. This uses `ResourceFree`, not `Message`.
`Message` means a value can cross task or actor ownership; resource scopes only
need to prove that the returned value does not contain resource handles owned by
the closing scope.

The body may call arbitrarily deep functions. Every resource opened inside
those functions registers with the current scoped owner unless a function
explicitly opens in or transfers to another owner.

`resource::scoped` is not a memory arena. Values allocated inside the body
remain GC-managed values. Closing the resource scope releases only registered
non-memory resources.

## ResourceFree

`ResourceFree` is the standard capability for values that do not contain a
resource handle. It is recursive and structural for visible Ciel value shapes.
It is not the same as `Message`.

Tentative surface:

```ciel
unsafe interface<T> bool resource_free_marker(*const T value);
unsafe interface<T> bool resource_handle_marker(*const T value);

interface ResourceFreeInternal = resource_free_marker;
interface ResourceHandleInternal = resource_handle_marker;
interface ResourceFree = ResourceFreeInternal + !ResourceHandleInternal;
```

The compiler and `/std/meta` must make this capability transitive:

1. primitive scalars, `void`, `never` values that never return, ordinary
   strings, owned bytes, and GC memory handles are `ResourceFree` unless their
   wrapper declares a resource marker;
2. a resource handle type is not `ResourceFree`;
3. a visible struct is `ResourceFree` when every field is `ResourceFree`;
4. a visible enum is `ResourceFree` when every payload field of every variant is
   `ResourceFree`;
5. a fixed-size array is `ResourceFree` when its element type is
   `ResourceFree`;
6. a concrete closure value is `ResourceFree` when every captured field is
   `ResourceFree`;
7. generic code must carry `T: ResourceFree` when it returns or stores an
   unconstrained `T` across a closing resource scope;
8. erased dynamic interface values need an explicit retained `ResourceFree`
   witness when the erased value crosses a resource-scope boundary.

The compiler recognizes the canonical `/std/resource` capability names for
generic constraint checking and for structural expansion over visible Ciel
shapes. `/std/meta` owns the ordinary structural impls over representation nodes
such as `HNil`, `HCons`, `Field<T>`, `Variant<T>`, and bounded array chunks.
This keeps recursive policy explicit in the standard library while still giving
ordinary visible structs, enums, arrays, and concrete closures useful
`ResourceFree` behavior.

`meta::Repr<T>` does not bypass resource policy. If any owned leaf in the
representation is a resource handle, the representation is not `ResourceFree`.
Likewise, `meta::RefRepr<T>` is a borrowed view and does not make a resource
free to return from a closing scope.

## Explicit Transfer

Extending a resource lifetime is explicit. The core transfer operations are:

```ciel
export Result<File, Error> transfer_to_parent(File file);
```

The first public surface should avoid exposing raw `TaskOwner` or `ActorOwner`
objects. Transfer to a task or actor is expressed through construction helpers
that create the destination owner and move selected resources into it:

```ciel
export Result<Task<Out>, Error> spawn_with_resource<Out: Message>(
    File file,
    async_core::Future<Result<Out, Error>> |(File)| body
);

export Result<ActorHandle<A>, Error> spawn_actor_with_resource<A: Actor>(
    File file,
    A |(File)| make_initial_state
);
```

These signatures are placeholders. The owner-internal primitive is still a
generic move from one registry owner to another, but it is runtime or framework
API, not the normal application API. The contract is:

1. transfer removes the resource entry from the source owner or marks it moved;
2. transfer installs a new entry in the destination owner;
3. transfer returns a fresh handle token;
4. old token copies are revoked, usually by advancing generation;
5. transfer fails if the destination owner is closed, incompatible, or over
   quota.

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
need to find those copies. Repeated close on the same entry and generation is
idempotent. Stale, transferred, mismatched, or retired tokens return a stable
error.

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

## Async Surface API

High-level async APIs continue to use the current owner implicitly. Users should
not pass a resource scope to every async read, write, connect, or sleep:

```ciel
AsyncTcpStream stream = await async_net::connect(addr)?;
Bytes data = await async_net::read(stream, 4096)?;
await async_time::sleep_ms(10)?;
```

Those operations register stream handles, operation tokens, and timers with
the current task owner or the innermost active resource scope.

Resource scopes need an async form:

```ciel
export async Result<R, Error> scoped_async<R: ResourceFree>(
    async_core::Future<Result<R, Error>> |()| body
);
```

The API is written in the current closure surface: an `async || { ... }`
closure has a generated future return value that matches the standard
`Future<Result<R, Error>> |()|` callable shape. The semantics are:

1. create a child owner;
2. install it as the current owner for the async body;
3. preserve that owner across `await`;
4. close the owner on normal return or `Err` return;
5. close the owner during task cancellation or cleanup-capable abort;
6. leave panic and process exit outside the cleanup guarantee.

Async resource helpers should mirror blocking scoped helpers:

```ciel
export async Result<R, Error> with_connect<R: ResourceFree>(
    SocketAddr addr,
    async_core::Future<Result<R, Error>> |(AsyncTcpStream)| body
);

export async Result<R, Error> with_open_read<R: ResourceFree>(
    []const char path,
    async_core::Future<Result<R, Error>> |(AsyncFd)| body
);
```

These helpers are convenience wrappers over `scoped_async`, open/connect, and
owner close. They do not require callers to thread scope parameters through
deep helper functions.

Low-level operation-token APIs remain available, but their names and namespace
should make their resource nature explicit. Normal application code should call
high-level awaitable functions:

```ciel
Bytes data = await async_io::read_bytes(file, 4096)?;
usize written = await async_net::write(stream, bytes)?;
```

Low-level integration code may use operation-token names such as:

```ciel
AsyncRead op = async_io::start_read(file, 4096)?;
Bytes data = async_io::finish_read(op)?;
```

or keep the current `*_async` naming if the distinction remains clear. These
tokens are resource handles backed by the common registry. `finish` and
`cancel` close or consume the registry entry; stale token copies fail through
ordinary generation validation.

`TaskGroup` should be a natural async resource scope:

```ciel
export async Result<R, Error> with_task_group<
    T: Message,
    R: ResourceFree
>(
    async_core::Future<Result<R, Error>> |(*const TaskGroup<T>)| body
);
```

Closing a task group cancels unfinished tasks, closes the group owner, and
releases group-owned resources. The existing explicit `group_close` remains the
manual form. `T: Message` is the task result boundary; `R: ResourceFree` is the
value returned after the group owner closes.

`async::spawn` remains a task-ownership boundary. It does not capture ordinary
resource handles through `Message`. A resource needed by the spawned task must
be constructed inside the spawned task, transferred to the child owner, or
wrapped in an explicitly synchronized `ShareHandle`.

Future surface work may add an explicit transfer helper:

```ciel
Task<void> task = async::spawn_with_resource(file, async |owned_file| {
    ...
});
```

Such an API is a move/transfer boundary, not `Message` cloning.

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

Resource capability policy uses two canonical markers:

```ciel
unsafe interface<T> bool resource_free_marker(*const T value);
unsafe interface<T> bool resource_handle_marker(*const T value);

interface ResourceFreeInternal = resource_free_marker;
interface ResourceHandleInternal = resource_handle_marker;
interface ResourceFree = ResourceFreeInternal + !ResourceHandleInternal;
```

The exact alias must fit the accepted `/std/message` lattice. The important
policy is:

1. resource handles are not `ResourceFree`;
2. resource handles are not `Message` by default;
3. resource handles are not `ShareHandle` by default;
4. a wrapper can implement `ShareHandle` only when its operations are internally
   synchronized or immutable and its cleanup model is well-defined;
5. a wrapper can implement `Message` only by constructing an independent
   receiver-owned resource or by using an explicit synchronized shared handle
   policy;
6. `meta::Repr<T>` must not bypass resource-local policy.

`ThreadLocal` remains useful for resources that are tied to one actor or task.
Some resources may be process-shareable but still require owner registration.
The proposal should not force every resource handle to be `ThreadLocal`; it
should force every resource handle to have an explicit boundary policy.

## Runtime Model

The runtime provides a single registry substrate shared by resource owners and
async operation tokens:

1. owner creation and close;
2. current-owner push/pop for scoped blocks;
3. task execution-context and actor-local current owner lookup;
4. owner-table registration;
5. handle validation;
6. resource close and transfer;
7. generation advancement;
8. checked id allocation and slot retirement;
9. per-owner quotas and registration failure;
10. async callback routing through the same entry/generation validation model;
11. async owner preservation across suspension and resumption;
12. active scoped-owner cleanup from generated async cleanup functions;
13. `block_on` and task entry owner installation;
14. diagnostic resource errors;
15. optional leak diagnostics at process shutdown.

The runtime must not depend on GC finalizers for correctness. A finalizer may
report a leaked owner or perform emergency cleanup as a debugging aid, but the
language guarantee comes from owner close.

Async runtime implementation details may keep specialized internal types for
performance, but the observable semantics for stale events, stale handles,
entry generation, and cleanup ownership must be shared.

The ambient owner for async code is task-context state, not only OS-thread TLS.
If a task resumes on a different thread, current-owner lookup still resolves to
that task's owner stack.

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
export Result<R, Error> with_open_read<R: ResourceFree>(
    []const char path,
    Result<R, Error> |(File)| body
);
```

They can be implemented using `resource::scoped` plus the current-owner
open operation.

Low-level raw descriptor APIs remain `unsafe` and outside the resource owner
guarantee unless wrapped back into an owner entry.

Resource failures use the existing `Error` and error-box mechanism. This
proposal does not require a new global family of resource error constructors.
Modules may return module-specific errors, wrap a runtime resource error in an
error box, or attach context to an existing source error. The required contract
is semantic rather than constructor-based:

1. stale handles report a stable error instead of touching host resources;
2. quota exhaustion reports a stable error instead of registering the resource;
3. id or generation exhaustion reports a stable error instead of wrapping;
4. tests can identify these cases through the existing error inspection surface.

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

1. Inventory the existing async operation-token registry, task routing,
   generation checks, and cleanup hooks.
2. Extract or generalize that machinery into the common resource registry
   substrate.
3. Add a `/std/resource` module with owner, scoped block, close, transfer,
   `ResourceFree`, and limit primitives.
4. Add checked id/generation exhaustion behavior and per-owner quota failures.
5. Rework `/std/io` to register files in the current owner while preserving
   scoped helpers.
6. Rework blocking `/std/net` listener and stream handles to use owner entries.
7. Rework SQLite connection and statement wrappers to use owner entries.
8. Classify async fd, async TCP stream, and async operation-token wrappers as
   resources or explicit share handles backed by the common registry.
9. Attach owners to async task and actor runtime state.
10. Add `resource::scoped_async` and async scoped helper APIs.
11. Normalize low-level async operation-token naming and namespace placement.
12. Align `TaskGroup` close/cancel semantics with resource-owner cleanup.
13. Add stale-token, generation-retirement, and quota regression tests.
14. Add diagnostics or lints for opening many resources in a long-lived owner
   without an inner scope.
15. Keep raw fd adoption unsafe until it registers the fd with a resource owner.

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
11. Async stale callback tests and ordinary stale handle tests use the same
    generation-validation semantics.
12. A generation-at-maximum test retires the slot instead of wrapping.
13. Resource creation fails with a stable `Error` when an owner quota is
    reached.
14. Transfer to an owner without remaining quota fails without moving the
    source resource.
15. `scoped_async` closes resources after awaits on normal and error returns.
16. Task cancellation closes an active `scoped_async` owner.
17. High-level async I/O APIs use the current owner without explicit scope
    parameters.
18. Low-level operation-token `finish` and `cancel` revoke stale token copies.
19. `TaskGroup` scoped helpers cancel unfinished tasks and close group-owned
    resources.

## Resolved Policy Decisions

1. `resource::scoped`, `resource::scoped_with_limits`,
   `resource::scoped_async`, async `with_*` helpers, and task-group scoped
   helpers use `R: ResourceFree`, not `R: Message`.
2. `ResourceFree` is recursive and transitive over visible structs, enums,
   arrays, concrete closure captures, and `/std/meta` owned representation
   nodes. `meta::Repr<T>` does not bypass resource policy.
3. Resource failures use the existing `Error` and error-box mechanism. The
   proposal specifies stable failure behavior, not a mandatory global
   constructor family.
4. Repeated explicit close is idempotent for the same entry and generation.
   Stale, transferred, mismatched, or retired tokens report a stable error.
5. V1 exposes minimal transfer helpers such as `transfer_to_parent` and
   high-level task transfer helpers. It does not expose unstable `TaskOwner` or
   `ActorOwner` internals as ordinary application API.
6. `ShareHandle` resources use both owner entries and synchronized or
   reference-counted runtime state. The owner gives deterministic release; the
   shared state gives safe concurrent access.
7. Owner close performs best-effort cleanup of every entry. If the body already
   returned an error, cleanup errors are attached as context or diagnostics
   rather than replacing the primary error.
8. The compiler enforces a coherence guard: a resource handle cannot implement
   `Message` or `ShareHandle` accidentally. Any exception requires an explicit
   unsafe sharing, cloning, or transfer policy.
9. The first surface is library-shaped and compiler-recognized:
   `resource::scoped` and `resource::scoped_async`. Dedicated syntax such as
   `resource { ... }` can come later if the pattern becomes common enough.
10. Quotas are configurable. The runtime supplies defaults, but applications
    can override limits at scoped, async scoped, task-group, actor, process, or
    embedding boundaries before the affected owners are created.
11. The common registry owns id/generation validation, owner attachment,
    quota accounting, close/cancel/finish dispatch, and stale callback
    rejection. Backend-specific dispatch I/O state, buffered reader state,
    channel queues, and generated future frame layout stay specialized.
12. Low-level async operation-token APIs should move toward explicit `start_*`,
    `finish_*`, and `cancel_*` naming or an internal adapter namespace. High
    level `await read/connect/sleep` APIs remain the normal user path.
13. Async closure type spelling for `scoped_async` and async `with_*` helpers
    uses the current `Future<Result<R, Error>> |(...)|` callable shape. An
    `async || { ... }` closure is accepted through the standard `Future`
    compatibility path. This proposal does not introduce a full `AsyncFnOnce`
    interface hierarchy.
14. `TaskGroup<T>` does not expose raw owner controls. Owner limits are supplied
    through scoped helper construction or surrounding resource scopes.
