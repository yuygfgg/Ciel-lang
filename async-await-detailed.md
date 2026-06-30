# Async/Await Detailed Notes

This document is a detailed companion to the normative async/await text in
[`design.md`](design.md). It records accepted design detail that is useful for
compiler, runtime, standard library, and diagnostic work, but too operational
for the main language specification.

## User-Facing Model

### Async Functions And Futures

An async function is declared with `async`:

```ciel
async Result<Bytes, Error> read_frame(AsyncTcpStream stream) {
    Bytes header = await async_net::read(stream, 8)?;
    usize len = decode_len(header)?;
    return await async_net::read(stream, len);
}
```

Calling an async function constructs a first-class future value. The call does
not run the body to completion and does not start a concurrent task by itself.
`await` is required when the caller wants to drive the future and observe its
output:

```ciel
_ future = read_frame(stream);
Bytes frame = await read_frame(stream)?;
```

The concrete future type is opaque and compiler-generated. It implements
`Awaitable<Result<Bytes, Error>>`, and may also implement `CancelSafe` or
`Abortable` when its body proves those contracts. Users can store it in locals,
pass it to `select`, pass it to `async::spawn`, pass it to generic future
combinators, or await it. Users cannot name or inspect the generated frame type.

Compiler-generated futures are single-consumer values. Awaiting a completed
generated future again is invalid unless that future type explicitly documents
reusable await behavior.

Dropping a future before it has registered a pending operation is allowed.
Dropping or cancelling a pending future while the current task continues is
allowed only through a `CancelSafe` path. Tearing down a pending future because
its owning task is terminating requires `Abortable`.

`await future` drives the future until it yields its declared output type. If the
output is `Result<T, Error>`, ordinary `?` propagation composes after the await:

```ciel
Bytes frame = await future?;
```

The runtime remains actor-backed. A future is a safe handle to private
continuation state or a runtime operation token, not a pointer into another
task's mutable frame.

### Spawning Tasks

`async::spawn` starts an awaitable body as an independent task:

```ciel
Task<usize> task = async::spawn(async || compute_size(path))?;
usize size = await task?;
```

The body passed to `spawn` must be awaitable with output `Result<T, Error>` and
must be abortable. `Task<T>` is itself awaitable with output `Result<T, Error>`.
Awaiting a task waits for normal completion, failure, or cancellation of that
task.

Cancelling a wait on a task handle does not cancel the running task. It
unregisters the waiter. `async::cancel` requests task termination through the
task's abort path.

Spawning is a task-ownership boundary. Values captured by a directly spawned
async closure and the task result `T` must satisfy hidden `Message` obligations,
because they cross from one task owner to another. The public `spawn` signature
does not ask users to write these bounds at ordinary call sites; the compiler
attaches the obligations at the boundary and reports the failing capture or
result field when proof fails.

An async closure passed directly to `spawn` is checked by inspecting its
captures. It is not first coerced into an ordinary retained closure type such as
`R |(...): Message|`.

If an already-created future value is passed to `spawn`, that value crosses into
the spawned task. Compiler-generated futures are checked through their captures.
Handwritten awaitable values need an explicit safe crossing policy before they
can be moved between task owners.

Values created inside the spawned async body are task-local. They do not need
`Message` merely because they live across `await`; they only need to satisfy the
async-frame safety rules described below.

### Task Groups

Static `select` covers a fixed set of futures known at compile time. Dynamic
concurrency is represented by task groups:

```ciel
TaskGroup<Frame> group = async::task_group<Frame>()?;
async::group_add(&group, async::spawn(async || read_frame(a))?)?;
async::group_add(&group, async::spawn(async || read_frame(b))?)?;

Frame first = await async::group_next(&group)?;
```

`group_next` waits for the next task in the group to finish. It does not cancel
the other tasks, so it does not require loser futures to be `CancelSafe`.

Cancelling or closing a group aborts unfinished tasks through their task abort
paths. This is the scalable model for dynamic sets of connections, workers, or
background jobs: spawn one task per unit of work, add the task to a group, and
await completions with `group_next`.

### Channels

Async tasks communicate through channels, not through low-level actor mailboxes:

```ciel
ChannelPair<Bytes> ch = async::channel<Bytes>(1024)?;
Task<void> writer = async::spawn(async || write_loop(ch.receiver))?;

await async::send(ch.sender, payload)?;
await writer?;
```

The primary channel is bounded. `send` suspends when the channel is full, which
provides backpressure for producer/consumer pipelines. `try_send` is the
non-suspending fast path.

`send(sender, value)` is not cancellation-safe by default because cancelling the
future can drop a value that was moved into the send operation. Code that needs
cancel-safe sending in `select` awaits `reserve(sender)`, which is `CancelSafe`,
then synchronously commits with `permit_send(permit, value)`.

Channel payloads cross task ownership. The compiler attaches hidden `Message`
obligations at send and receive boundaries, just as it does for task captures
and task results.

### Channel Lifecycle

Channels must not rely on user code calling `close` to avoid hung receivers.
Endpoint liveness is part of the runtime contract:

1. each channel tracks live sender handles and live receiver handles;
2. cloning or moving a sender increments or transfers sender ownership according
   to the handle representation;
3. cloning or moving a receiver increments or transfers receiver ownership
   according to the handle representation; unicast channels usually keep this
   count at one;
4. explicit `close(sender)` closes that sender handle;
5. explicit `close_receiver(receiver)`, receiver drop, or deterministic task
   cleanup closes that receiver handle;
6. when the last sender is closed or destroyed, pending receivers wake with a
   closed-channel error after the buffered message queue is empty;
7. when the last receiver is closed or destroyed, pending senders and pending
   `reserve` waiters wake with `channel_closed_error()`;
8. `send(sender, value)`, `try_send(sender, value)`, and `reserve(sender)` fail
   immediately with `channel_closed_error()` when no live receiver remains;
9. task cleanup after normal return, `Err`, panic, cancellation, or abort must
   destroy channel handles stored in the task frame and update endpoint counts
   before the task is considered finished;
10. GC finalization may release leaked handles eventually, but it is not a
    scheduling guarantee and cannot be the only close mechanism;
11. `recv(receiver)` waits until a message is available, or until the channel is
    closed and no buffered messages remain.

## Awaitable Capabilities

`Awaitable<Out>` is the public capability for values that can be awaited. The
standard runtime-backed `Future<T>` handle implements it, and compiler-generated
async functions and closures implement it without exposing their frame type:

```ciel
export struct Future<T> {
    *void handle;
}

export unsafe interface<A, Out> *void awaitable_future(*const A awaitable);
export interface Awaitable<Out> = awaitable_future<Out>;
```

Two additional capabilities describe what the runtime may do to a pending
future:

```ciel
export unsafe interface<F> bool cancel_safe_marker(*const F future);
export interface CancelSafe = cancel_safe_marker;

export unsafe interface<F> Result<void, Error> abort_future(*F future);
export interface Abortable = abort_future;

export interface SelectableFuture<Out> =
    Awaitable<Out> + CancelSafe + Abortable;
```

`SelectableFuture<Out>` is the required bound for `select` arms and the operand
of `async::timeout`.

`CancelSafe` and `Abortable` are trusted behavioral interfaces. They are not
structurally derived through `meta::Repr<T>` the way `Message` can be.
Handwritten unsafe implementations are allowed only at the same trust boundary
as other unsafe policy implementations.

## Cancel And Abort

The design intentionally separates cancel from abort:

1. Cancel abandons one pending future while the current task continues. Losing
   arms in `select` and timed-out waits use cancellation.
2. Abort tears down the currently suspended operation because the owning task is
   terminating. Task cancellation, panic teardown, and runtime shutdown use
   abort.

`CancelSafe` means cancelling a pending future cannot lose user-visible data,
corrupt protocol state, or hide a side effect in a resource that remains usable.
It is required for selectable losing futures.

`Abortable` means the runtime can forcefully unwind a suspended future without
leaking the task, actor, or kernel resources. An abort path may close a socket,
deregister a timer, poison a handle, or otherwise make the resource unusable, as
long as cleanup is bounded and later aliases observe a defined error instead of
unsynchronized state.

`CancelSafe` is not closed under ordinary async composition. A future that
awaits only `CancelSafe` operations can still be non-cancel-safe if it consumes
protocol state into locals and then suspends again. A frame reader that reads a
header, stores the decoded length, and then awaits the body cannot be cancelled
after the header has been consumed unless it can put the header back or restore
protocol state.

When business logic is not `CancelSafe`, spawn it as an independent task and
select or timeout the task handle:

```ciel
Task<Frame> frame_task = async::spawn(async || read_frame(reader))?;
Result<Frame, Error> task_result = await async::timeout(frame_task, 5000)?;
Frame frame = task_result?;
```

The timeout cancels only the wait on the task handle. It does not drop the
partially progressed frame reader inside the task. When policy terminates the
work, the caller explicitly cancels the task, which uses the task's `Abortable`
path and may close or poison owned resources.

### Operation Classification

The stdlib operation policy is:

1. `async_time::sleep_ms` is `CancelSafe + Abortable`;
2. awaiting a `Task<T>` handle is `CancelSafe + Abortable`; cancelling the wait
   does not cancel the task itself;
3. `async::recv` is `CancelSafe + Abortable`; sender liveness determines closed
   channel wakeups;
4. `async::reserve` is `CancelSafe + Abortable`; moving the value with
   `send(value)` is not `CancelSafe` by default;
5. `async_net::connect` is `CancelSafe + Abortable`; cancellation or abort
   closes the in-progress socket if needed;
6. `async_net::accept` is `CancelSafe + Abortable`; races with completed accepts
   are resolved through generation ownership so an accepted stream is either
   returned to the winner or closed;
7. buffered stream reads can be `CancelSafe + Abortable` when cancellation keeps
   already-read bytes in the reader's private buffer;
8. raw TCP reads, reusable-buffer reads, and writes are `Abortable` but not
   `CancelSafe`;
9. raw fd reads and writes are `Abortable` but not `CancelSafe` by default
   because cancellation may hide offset changes or partial writes;
10. compiler-generated async futures implement `Abortable` only when every
    suspension point can be aborted and cleanup cannot block indefinitely;
11. the compiler must not infer `CancelSafe` merely because every awaited
    operation is `CancelSafe`;
12. generic async functions carry latent `CancelSafe` and `Abortable`
    obligations for future parameters or calls whose concrete behavior is not
    known yet.

### External Callback Safety

`Abortable` includes callback lifetime safety. If a suspended operation is
backed by libdispatch, epoll, kqueue, a timer queue, or another external
callback source, the C callback must never capture an async frame pointer,
`TaskState` pointer, or pointer into user frame storage.

The required pattern is generation-routed operation tokens:

1. starting an external operation allocates a runtime-owned operation token;
2. the token contains routing data such as actor mailbox id, task id, operation
   id, generation, result storage, and cleanup hooks;
3. external callbacks store owned results into that token and enqueue a hidden
   completion event to the actor mailbox or runtime completion router;
4. the generated resume dispatcher validates the task, operation id, and
   generation before resuming;
5. stale events release operation-owned result storage and are ignored.

Callback-visible result buffers, connection handles, and temporary payloads must
be owned or rooted by the operation token until the routed event is processed.
They must not live only inside the async frame.

Aborting a suspended external operation marks the task/operation generation dead
and detaches the frame from the operation token. The runtime can then run frame
cleanup without waiting for callback queues to drain, because late callbacks can
only enqueue routed completion events and cannot dereference the released frame.

## Select, Race, And Timeout

`select` races a flat set of future expressions and produces one result:

```ciel
Event event = await select {
    case result = async_net::read_buffered(reader, 16384):
        Event::Bytes(result?)

    case result = async::recv(commands):
        Event::Command(result?)

    case result = async_time::sleep_ms(5000):
        result?;
        Event::Tick
};
```

Rules:

1. The whole `select` expression is awaited; arms contain futures, not nested
   `await` expressions.
2. Each arm binds the completed value and evaluates an arm body assignable to
   the common `select` result type.
3. `?` inside an arm propagates from the enclosing async function.
4. Every arm future must implement `SelectableFuture<ArmOut>`.
5. The first completed arm wins.
6. If more than one arm is ready, default `select` chooses fairly over all ready
   arms using a per-site rotating start point, random start point, or equivalent
   starvation-free strategy.
7. `biased select` is the explicit source-order priority variant.
8. Losing futures are cancelled only after their `CancelSafe` contract permits
   it.
9. Raw stream reads and writes are rejected by selectable-future checks. Use
   buffered readers, protocol-specific future wrappers that preserve stream
   state, or run the raw operation in a separate task and communicate by
   channel.

The compiler lowers `select` to stdlib-managed internal select-set machinery.
The internal future must poll every arm once before parking. That rule ensures
readiness from user-space buffers, channel queues, completed tasks, and expired
timers is observed even when no fresh OS readiness event arrives.

The internal declarations are schematic and remain under an internal namespace:

```ciel
export struct SelectSet<R> {
    *void handle;
}

export Result<SelectSet<R>, Error> select_set<R>();

export Result<void, Error> select_push<R, A, F: SelectableFuture<A>>(
    *SelectSet<R> set,
    F future,
    R |(A value)| map,
);

export async Result<R, Error> select_set_wait<R>(SelectSet<R> set);
```

These helpers are an implementation contract, not a tutorial surface.

`async::timeout(future, ms)` is a convenience wrapper over the same model. On
timeout, it cancels only the waiting future. It does not assume arbitrary
protocol futures can safely discard partial state.

## Message Boundaries

Users do not classify locals as "state" or "messages". The compiler decides
where values live. `Message` is required only when a value crosses task
ownership, or when code explicitly opts into the low-level actor API:

1. values captured by a spawned task body;
2. task result values delivered through `Task<T>`;
3. async channel payloads;
4. task-group result payloads;
5. explicit low-level actor mailbox payloads.

Task-local values created inside an async function do not need `Message` merely
because they live across `await`. They live in the owning task's private async
frame and are checked by the frame-safety predicate instead.

### Hidden Boundary Obligations

The task and channel APIs keep ordinary generic source signatures, while the
compiler attaches hidden obligations at every cross-task boundary:

1. for concrete crossing types, prove `Message` immediately;
2. for user structs and enums, try structural derivation through `meta::Repr`;
3. for generic crossing types, record a latent `Message` obligation on the
   generic instance and check it when the type becomes concrete;
4. for exported generic APIs or erased interfaces that can cross tasks, require
   an explicit public capability bound or equivalent carried witness;
5. for async closures passed directly to `spawn`, inspect captures directly
   instead of requiring retained-closure `: Message` syntax;
6. if proof fails, report the source boundary and the nested field or capture
   that blocked the proof.

When a hidden async boundary uses a structural `meta::Repr<T>` crossing path,
that fact is local to the boundary. It does not make the original nominal type
implement `Message` for explicit actor APIs or for public APIs that spell
`T: Message`.

### Compiler-Generated Message Witnesses

When a crossing value must satisfy `Message`, the compiler generates or resolves
the witness automatically when the type is structurally messageable.

For user structs and enums, the generated path is:

1. derive or synthesize `meta::Repr<T>`;
2. prove that the representation contains only `Message` components;
3. prove that no nested component violates the thread-local policy;
4. generate clone through the representation, or equivalent direct field clone;
5. report diagnostics through the original field path when derivation fails.

Diagnostics identify the original field path, for example:

```text
field `config.cache.raw_fd` is not messageable because `RawFd` is thread-local
```

Ordinary async code does not require users to write `meta::Repr` boilerplate.
Unsafe handwritten `Message` impls remain trusted; if such an impl lies, it can
break the guarantee like any other unsafe policy impl.

## Task-Local Frame Safety

The compiler stores locals live across `await` in an actor-owned async frame.
The relevant question is not "is this value `Message`?" but "can this value be
stored in a private async frame without preserving a hidden shared borrow or
invalid pointer?"

Allowed across `await` in safe code:

1. owned scalars, enums, structs, and arrays whose transitive fields are also
   frame-safe;
2. runtime handles or owned containers accepted by the compiler's async-frame
   safety rule;
3. values whose type has the explicit unsafe async-frame opt-in marker;
4. direct local `[]const char` or `[]const u8` slices with syntactically proven
   static read-only provenance, including string literals;
5. compiler-generated operation keys and discriminants.

Rejected across `await` in safe code:

1. raw pointers (`*T`, `*const T`);
2. nullable raw pointers (`?*T`);
3. mutable slices (`[]T`);
4. borrowed `[]const T` slices whose owner is a stack local, a field of another
   live local, a temporary, or otherwise not proven static;
5. pointers or references into stack locals;
6. pointers or references into fields of another live local;
7. values satisfying `ThreadLocal`;
8. closure values that capture forbidden locals;
9. structs, enums, arrays, or generic values whose transitive fields may contain
   forbidden fields and lack a proven async-frame-safety policy.

The compiler recognizes the canonical
`/std/message.async_frame_opt_in_marker` capability for owned values whose
fields would otherwise look unsafe to the structural frame walk. This is a
manual unsafe opt-in, not a public user-facing predicate. The standard library
provides `unsafe impl<T: ShareHandle> async_frame_opt_in_marker` so immutable
or internally synchronized share handles satisfy the frame rule through
interface composition. Implementing `async_frame_opt_in_marker` asserts that
storing the value in a suspended async frame is valid, but it does not imply
cross-thread shared mutation safety.

The structural fallback is deep and contagious: if any field or enum payload
contains a rejected pointer, borrowed view, mutable slice, thread-local handle,
or closure capture, the outer value is rejected across `await` unless the type
has an explicit unsafe async-frame opt-in marker.

The predicate is provenance-sensitive for slices. A direct local string literal
slice is safe because it points at static read-only storage. A slice into a
local buffer is rejected because the view would outlive a stack-like owner
relationship across suspension.

In the first implementation, the compiler does not prove static provenance
through struct fields, enum payloads, arrays of slices, or generic containers.
Composite values are rejected across `await` if they contain a slice or
reference-view field, unless the compiler has an explicit canonical marker
proof that the representation is owned and frame-safe.

Rejected:

```ciel
[]const u8 view = buffer[0..n];
await async_time::sleep_ms(1)?;
use(view); // error: borrowed slice crosses await
```

Accepted:

```ciel
[]const char msg = "start processing";
await async_time::sleep_ms(1)?;
print(msg); // ok: string-literal storage is static and read-only

[]const u8 magic = "PING";
await async_time::sleep_ms(1)?;
use_bytes(magic); // ok: string-literal storage is static byte storage
```

Rejected in the first implementation:

```ciel
struct LogLine {
    []const char text;
}

LogLine line = LogLine { .text = "start processing" };
await async_time::sleep_ms(1)?;
print(line.text); // error: slice field crosses await
```

Accepted:

```ciel
usize n = buffer_len;
await async_time::sleep_ms(1)?;
[]const u8 view = buffer[0..n];
use(view); // slice is created after await
```

### Possible Future Frame Promotion

The current rule rejects borrowed views across `await` unless they are direct
local slices with syntactically static read-only provenance. It does not
silently heap-lift them. Heap-lifting the reference value alone is not safe:
the referenced object may still be a stack local, C allocation, moved GC object,
or mutable owner with aliasing constraints.

A possible future relaxation is provenance-aware frame promotion:

1. if the owner is an owned local in the same async frame, store the owner in the
   frame and represent the view as owner identity plus offset and length;
2. if the owner is not frame-owned, require an explicit owned copy such as
   `Bytes`;
3. if the runtime supports pinned frame storage or interior-pointer-aware GC,
   allow a narrower borrowed-view representation under that runtime contract;
4. keep raw pointers rejected across `await` unless an unsafe API explicitly
   owns the lifetime and pinning contract.

## Captures And Low-Level Actors

Values captured by `async::spawn` cross into a new task and therefore must be
proven messageable:

```ciel
Config config = parse_config()?;

Task<void> task = async::spawn(async || {
    ServerCore core = init_server_core(config)?;
    await run_server(core)?;
    return Ok;
})?;
```

`ServerCore` is task-local. It can be non-`Message` if it is created inside the
task and satisfies frame safety across awaits. `config` is captured from the
spawner, so the compiler proves it messageable at the spawn boundary.

Moving an already-existing non-`Message` value into a new task is deliberately
not supported by the high-level safe spawn API. Such a transfer requires an
explicit synchronized handle, an owned message representation, or a future
unsafe ownership-transfer facility.

The low-level actor APIs remain available as an advanced compatibility layer:

1. `Actor<M>`;
2. actor spawn APIs;
3. low-level actor `send`, `stop`, and `join`;
4. explicit actor mailbox payloads.

Normal async documentation starts from async functions, `await`, tasks,
channels, and `select`. Actor mailboxes are the lowering model behind tasks,
channels, and operation completions, not the ordinary async user abstraction.

For implementation mapping only:

1. generated task initialization corresponds to the low-level actor init phase:
   allocate the async frame, move or clone proven captures into it, and enter
   the first resume point;
2. the generated task resume dispatcher corresponds to the low-level actor
   handle phase: receive hidden runtime events, validate operation generation,
   switch on the program counter, and continue execution;
3. neither mapping is reflected in user source.

## Concurrency Invariants

Safe async code is data-race-free and ownership-safe by construction:

1. every async frame is owned by exactly one task;
2. the runtime never resumes two continuations of the same task concurrently;
3. task-local frame values are never exposed through task handles, channels, or
   resume events;
4. every value that crosses task ownership is cloned, moved, or stored through a
   proven `Message` path;
5. task handles and channel endpoints are opaque synchronized handles, not
   pointers into async frames;
6. external callbacks route completions through runtime-owned operation tokens;
7. hidden resume events are generated by the compiler/runtime and cannot carry
   arbitrary user payloads;
8. stale completions are discarded only when the relevant `CancelSafe` or
   `Abortable` contract permits dropping them.

## Execution Semantics

Each spawned async task is backed by one actor-owned execution context. A task
is always in one of these states:

1. `Ready`;
2. `Suspended`;
3. `Cancelling`;
4. `Finished`;
5. `Failed`.

Operational rules:

1. at most one continuation of a task runs at a time;
2. awaiting I/O suspends only the current task, not the OS thread;
3. awaiting any `Awaitable<T>` drives that value until it returns `T` or
   suspends the current task on a registered wakeup;
4. awaiting a `Task<T>` suspends until that task finishes or fails;
5. awaiting `recv(receiver)` suspends until a message is available, or until the
   channel is closed and no buffered messages remain;
6. cancelling a losing future inside a combinator requires `CancelSafe` and
   resumes the combinator through the winning branch;
7. cancelling a task requests task termination; if the task is suspended, the
   runtime invokes the current future's `Abortable` path so cleanup is bounded;
8. safe cancellable tasks may not suspend on a non-`Abortable` future unless
   they are explicitly placed in a low-level unabortable runtime mode;
9. task termination must run deterministic frame cleanup before the task is
   considered finished;
10. cleanup releases channel endpoints, task handles, select sets, buffered
    readers, and other logical resources; it must not wait for GC finalization;
11. if an async function returns `Err`, the task is marked failed;
12. stale completions are ignored only for operations whose `CancelSafe` or
    `Abortable` contract permits dropping the completion;
13. internal actor resume events are not user-visible messages.

## Lowering Model

The compiler lowers each async function and async closure to an opaque
stackless future type. A future stores its program counter, live locals, nested
future state, cleanup state, initialized-field state, and operation keys needed
by the runtime. Spawning a task moves one such future into actor-owned task
storage. Awaiting a nested future in the same task stores that nested state in
the same task frame.

For each async function instance, generate:

1. a program-counter field;
2. a frame struct containing locals live across `await`;
3. resume code for each await point;
4. an `Awaitable<Out>` implementation for the generated future type;
5. cleanup code for cancelled, aborted, or failed frames;
6. drop glue for every initialized frame field that owns logical resources;
7. conditional `CancelSafe` and `Abortable` implementations when the body proves
   them;
8. a wrapper that constructs the future from call arguments;
9. a wrapper or dispatcher path that resumes it with a completed operation
   result.

Async frame cleanup is modeled on `defer` lowering: a cleanup stack,
reverse-order execution, and well-defined paths for early return and failure.
The async-specific extension is that initialized frame fields are tracked across
program-counter states, so abort can run the correct non-awaiting release hooks
before the frame is handed to GC for memory reclamation.

For:

```ciel
Bytes bytes = await async_net::read(stream, 16384)?;
```

lowering performs:

1. evaluate `stream` and `16384` in source order;
2. construct the read future;
3. store that future in the current frame;
4. poll it through the generated future trampoline;
5. if it is pending, store the current program counter and live frame-safe
   locals, register the operation key, and return to the runtime;
6. on completion, validate the operation key and generation;
7. poll or finish the read future;
8. bind `bytes` or propagate `Err`;
9. drop the completed nested future state;
10. continue at the next source statement.

Nested async calls use nested frame storage owned by the same task. No native C
stack is suspended.

Immediate completions must not recursively call generated resume functions on
the native C stack. Resumes go through a task-local trampoline:

1. a resume step returns `Suspended`, `Ready(next)`, `Finished`, or `Failed` to
   the trampoline;
2. immediately ready awaits schedule another trampoline iteration instead of a
   direct C callback recursion;
3. the trampoline enforces a fairness budget and re-enqueues the task when the
   budget is exhausted;
4. runtime callbacks enqueue actor-mailbox-routed resume events and never
   directly run unbounded user continuation chains or dereference async frames.

## Compiler Obligations

The accepted design implies these compiler responsibilities:

1. parse contextual `async`, `await`, `select`, and `biased select` syntax;
2. type-check async function calls as opaque values implementing
   `Awaitable<Out>`;
3. reject `await` outside async bodies except for compiler-recognized bridges
   such as `async::block_on`;
4. reject passing async functions to ordinary function types unless an explicit
   future-producing function type is introduced;
5. type-check async functions against their written ordinary output type;
6. type-check `async::spawn` body shape as `Awaitable<Result<T, Error>> +
   Abortable`;
7. type-check `await` by requiring `Awaitable<Out>` and yielding `Out`;
8. attach hidden `Message` obligations to `spawn`, task results, task awaits,
   channel payloads, task groups, and low-level actor mailboxes;
9. inspect directly spawned async closure captures without requiring retained
   closure `: Message` syntax;
10. generate or resolve structural `Message` witnesses through `meta::Repr<T>`;
11. propagate latent `Message` obligations through generic async functions and
    check them when generic instances become concrete;
12. reject non-`Message` crossing values with diagnostics that identify the
    non-messageable field path;
13. compute live locals at each await point;
14. enforce deep async-frame safety for every live local;
15. track slice provenance well enough to allow direct local static read-only
    slices while rejecting slices into non-static owners;
16. reject compound values containing slice or reference-view fields across
    await in the first implementation unless the compiler has a canonical
    async-frame opt-in marker proof that the representation is owned and
    frame-safe;
17. infer hidden frame-safety constraints for generic locals that cross await;
18. lower async functions to opaque future frame types;
19. generate deterministic drop glue for every initialized async-frame field
    that owns logical resources;
20. lower `async::spawn` to actor runtime initialization plus generated
    dispatcher code;
21. lower awaitable stdlib operations to future construction, polling,
    start/suspend/finish hooks, operation-token registration, and wake
    registration;
22. ensure external-operation callbacks route through actor mailbox completion
    events keyed by task id, operation id, and generation;
23. ensure generated code does not expose async frame pointers to C callbacks;
24. lower task awaiting to task-completion waiting;
25. lower channel `recv` to channel receive suspension;
26. type-check `select` arms as a flat list of futures and a common result type;
27. lower `select` to internal `SelectSet<R>` construction and a single await of
    the set future;
28. ensure generated select polling checks every arm once before parking;
29. infer and emit `CancelSafe` only for transparent wrappers or other
    compiler-recognized patterns with a real cancellation proof;
30. infer and emit `Abortable` only when every suspension point can be aborted;
31. reject stdlib future helper calls when their generic `CancelSafe` or
    `Abortable` bounds are not satisfied;
32. preserve flat-list fair winner selection in `select` lowering;
33. lower generated resumes through a trampoline instead of direct recursive C
    calls;
34. root async frames, boxed messages, task results, operation results,
    operation tokens, and runtime handles correctly for GC;
35. preserve source-order evaluation before await;
36. preserve `?` propagation across await points;
37. preserve monomorphization of generic async functions;
38. reject async functions in exported C ABI positions;
39. reject safe cancellable tasks that can suspend on a future without an
    `Abortable` path.

## Runtime Obligations

The runtime must provide:

1. actor-owned storage for async frames;
2. hidden resume events not expressible in user code;
3. task handles with completion result storage;
4. operation keys with task identity and generation;
5. future wake registration and poll scheduling;
6. select polling that checks every arm once before parking on runtime wakeups;
7. cancellation of losing futures only according to their `CancelSafe` contract;
8. task abort through the suspended future's `Abortable` contract, with bounded
   release of runtime and kernel resources;
9. rejection or explicit low-level isolation for non-`Abortable` futures in
   cancellable tasks;
10. generation-safe actor-mailbox routing for every pending external operation;
11. callbacks that receive only runtime operation tokens and never dereference
    async frames, task state, or user frame storage directly;
12. abort logic that marks the current operation generation dead and detaches
    the frame from the operation token;
13. token/result storage that remains live until callback/event cleanup releases
    it;
14. resume dispatch that validates task id, operation id, and generation before
    resuming a task;
15. stale-event cleanup that does not touch the task frame;
16. stale-completion filtering only when the operation contract permits dropping
    stale completions;
17. bounded async channel send/receive storage, sender and receiver counts,
    deterministic endpoint cleanup, and wakeups for capacity and closure;
18. task-group completion queues, remaining-task ownership, and group
    cancellation of unfinished tasks;
19. deterministic task-frame cleanup on return, `Err`, panic, cancellation, or
    abort before relying on GC memory reclamation;
20. non-awaiting release hooks for handles stored in frames, including channel
    endpoint refcount decrements that wake the opposite side immediately;
21. task cancellation wakeups;
22. task awaiting wakeups;
23. a trampoline for immediate completions and nested async resumes;
24. fair scheduling budgets so immediate awaits cannot monopolize the executor;
25. GC rooting for frames, messages, captures, handles, operation tokens, and
    completion results.

The runtime must never resume two continuations of the same task concurrently.

## Standard Library Detail

### `/std/async`

`/std/async` is the user-facing async/await surface:

```ciel
export struct Future<T> {
    *void handle;
}

export unsafe interface<A, Out> *void awaitable_future(*const A awaitable);
export interface Awaitable<Out> = awaitable_future<Out>;

export unsafe interface<F> bool cancel_safe_marker(*const F future);
export interface CancelSafe = cancel_safe_marker;

export unsafe interface<F> Result<void, Error> abort_future(*F future);
export interface Abortable = abort_future;
export interface SelectableFuture<Out> = Awaitable<Out> + CancelSafe + Abortable;

export T block_on<T, A: Awaitable<T> + Abortable>(A future);
export Future<Result<Out, AsyncError>> future_from_op<Op, Out>(Op op);

export AsyncError timeout_error();
export AsyncError channel_closed_error();

export struct Task<T> {
    *void handle;
}

export Result<Task<T>, AsyncError> spawn<T, A: Awaitable<Result<T, Error>> + Abortable>(
    A body
);
export Result<void, AsyncError> cancel<T>(*const Task<T> task);
export Result<bool, AsyncError> is_finished<T>(*const Task<T> task);

export struct Sender<T> {
    *void handle;
}

export struct Receiver<T> {
    *void handle;
}

export struct SendPermit<T> {
    *void handle;
}

export struct ChannelPair<T> {
    Sender<T> sender;
    Receiver<T> receiver;
}

export Result<ChannelPair<T>, AsyncError> channel<T>(usize capacity);
export async Result<void, AsyncError> send<T>(Sender<T> sender, T value);
export Result<void, AsyncError> try_send<T>(Sender<T> sender, T value);
export async Result<SendPermit<T>, AsyncError> reserve<T>(Sender<T> sender);
export Result<void, AsyncError> permit_send<T>(SendPermit<T> permit, T value);
export async Result<T, AsyncError> recv<T>(Receiver<T> receiver);
export Result<void, AsyncError> close<T>(Sender<T> sender);
export Result<void, AsyncError> close_receiver<T>(Receiver<T> receiver);

export struct TaskGroup<T> {
    *void handle;
}

export Result<TaskGroup<T>, AsyncError> task_group<T>();
export Result<void, AsyncError> group_add<T>(*const TaskGroup<T> group, Task<T> task);
export async Result<T, Error> group_next<T>(*const TaskGroup<T> group);
export Result<void, AsyncError> group_cancel_all<T>(*const TaskGroup<T> group);
export Result<void, AsyncError> group_close<T>(*const TaskGroup<T> group);
export enum TaskGroupError<E> {
    TaskGroupAsync(AsyncError),
    TaskGroupBody(E),
    TaskGroupCleanup(AsyncError),
    TaskGroupBodyCleanup(E, AsyncError),
}
export async Result<R, TaskGroupError<E>> with_task_group<T: Message, R, E: ErrorTrait>(
    Future<Result<R, E>> |(*const TaskGroup<T>)| body
);

export async Result<Out, AsyncError> timeout<Out, A: SelectableFuture<Out>>(
    A future,
    u64 ms
);
```

`Future<T>` is a runtime-backed future handle. Compiler-generated async
functions and closures also implement `Awaitable<T>` without exposing their
generated frame type.

`block_on` is the synchronous bridge for `main`, tests, and embedding hosts. It
starts a future on the task runtime and blocks the current thread until the
future returns. Async bodies use `await` instead of nested `block_on`.

Task and channel handles are trusted synchronized handles. Safe code can copy
or pass them through exported APIs, but cannot forge handles, dereference
runtime state, or reach into another task's async frame.

Async-specific primitive failures use `AsyncError` values such as
`timeout_error` and `channel_closed_error`. User task bodies still return
`Result<T, Error>`, so the compiler erases `AsyncError` into an
application-boundary `Error` when `?`, `Err(error)`, or a function argument is
checked against an expected `Error`.

`with_task_group` is generic over the body error type. Group creation and
normal cleanup failures are reported through `TaskGroupAsync` or
`TaskGroupCleanup`; callback failures are reported as `TaskGroupBody(E)`, and a
body failure followed by cleanup failure is represented as
`TaskGroupBodyCleanup(E, AsyncError)`.

`awaitable_future`, `cancel_safe_marker`, and `abort_future` are for the
compiler, stdlib, and advanced generic abstractions. Ordinary async code
receives diagnostics such as "raw TCP read is not cancellation safe in timeout"
instead of marker-boilerplate requirements.

Internal operation-token adapters may remain under `/std/async/internal`, but
they are not the public async composition API.

### Bytes

`Bytes` is the standard immutable owned byte buffer. It is useful for async I/O
because it can cross awaits without preserving borrowed mutable views, and it
implements `ShareHandle` because it exposes only read-only byte views.
`ShareHandle` opts into async-frame storage through the standard-library
`async_frame_opt_in_marker` impl.

Reusable mutable read buffers are represented by `/std/buf.ByteBuf`, not
`Bytes`. `ByteBuf` has an explicit unsafe async-frame opt-in marker so it can
be moved through an async read future, but it is not a `ShareHandle`; mutation
APIs require unique mutable access and do not provide synchronization.

### `/std/async_io`

`/std/async_io` exposes awaitable file-descriptor operations:

```ciel
export async Result<bytes::Bytes, AsyncIoError> read_bytes(*const AsyncFd fd, usize max_len);
export async Result<usize, AsyncIoError> write_bytes(*const AsyncFd fd, bytes::Bytes data);
```

The high-level async functions are the normal API. Low-level `*_async`,
`notify_*`, `finish_*`, and `cancel_*` operation-token functions exist for
direct actor-completion integration and runtime tests.

Raw fd reads and writes are `Abortable` but not `CancelSafe` by default because
cancellation may hide offset changes or partial writes. Offset-stable and
buffered fd APIs are the extension points for `CancelSafe` contracts that
preserve state.

### `/std/async_net`

`/std/async_net` exposes awaitable TCP operations:

```ciel
export async Result<AsyncTcpStream, AsyncNetError> accept(*const AsyncTcpListener listener);
export async Result<AsyncTcpStream, AsyncNetError> connect(net::SocketAddr addr);
export async Result<AsyncTcpStream, AsyncNetError> connect_timeout(
    net::SocketAddr addr,
    u64 ms
);
export async Result<bytes::Bytes, AsyncNetError> read(*const AsyncTcpStream stream, usize max_len);
export async Result<ReadIntoResult, AsyncNetError> read_into(
    *const AsyncTcpStream stream,
    buf::ByteBuf @buffer
);
export async Result<usize, AsyncNetError> write(*const AsyncTcpStream stream, bytes::Bytes data);
export async Result<usize, AsyncNetError> write_half(*const AsyncTcpWriteHalf half, bytes::Bytes data);
export async Result<AsyncTcpStream, AsyncNetError> write_all(AsyncTcpStream @stream, bytes::Bytes data);
export async Result<AsyncTcpWriteHalf, AsyncNetError> write_all_half(
    AsyncTcpWriteHalf @half,
    bytes::Bytes data
);
```

`read` returns zero-length `bytes::Bytes` for EOF. `read_into` moves an owned
`buf::ByteBuf` into the future and returns the same buffer with the number of
bytes read. This lets hot read loops reuse capacity without treating immutable
`Bytes` as a mutable destination or keeping a mutable slice live across await.

Raw TCP `read`, `read_into`, `write`, and `write_all` are `Abortable` but not
`CancelSafe`. Task abort may close or poison the stream to release a stuck
operation, but a losing `select` or timeout cannot continue using the same
stream after possibly discarding bytes, losing an owned buffer, or observing a
partial write.

`accept` and `connect` are `CancelSafe + Abortable`, so they can be used
directly with `select` and `async::timeout`:

```ciel
AsyncTcpStream stream = await async::timeout(
    async_net::connect(addr),
    5000,
)??;
```

Selectable stream reads use a buffered reader:

```ciel
export struct AsyncTcpReadHalf {
    *void handle;
}

export struct AsyncTcpWriteHalf {
    *void handle;
}

export struct AsyncTcpSplit {
    AsyncTcpReadHalf read;
    AsyncTcpWriteHalf write;
}

export struct BufferedStreamReader {
    *void handle;
}

export Result<AsyncTcpSplit, AsyncNetError> split(AsyncTcpStream stream);
export Result<BufferedStreamReader, AsyncNetError> buffered_reader(
    AsyncTcpReadHalf half,
    usize capacity
);
export Result<AsyncTcpReadHalf, AsyncNetError> into_read_half(
    BufferedStreamReader reader
);
export async Result<Bytes, AsyncNetError> read_buffered(
    BufferedStreamReader reader,
    usize max_len
);
export async Result<Bytes, AsyncNetError> read_exact_buffered(
    BufferedStreamReader reader,
    usize len
);
```

`BufferedStreamReader` owns the TCP read half and its private buffer. Splitting
the stream gives an independent write half for full-duplex protocols while
ensuring there is only one owner of read readiness and buffering state.

`read_buffered` is `CancelSafe + Abortable` only if cancellation preserves
already-read bytes inside the reader buffer and abort releases the pending read.
The reader must serialize or reject overlapping reads on the same reader so
cancellation cannot reorder stream bytes.

`read_buffered` must poll the user-space buffer before registering interest in
the underlying socket. If buffered bytes are already available, the future
returns ready immediately and must not wait for another OS readability event.
This is required for `select`: a previous read may have drained the fd into the
buffered reader while leaving unread bytes in the reader's private buffer.

Recovering the read half with `into_read_half` is allowed only when the buffered
reader has no pending read. Unread buffered bytes remain owned by the reader and
must be drained or explicitly discarded by a later API before raw read-half
ownership is recovered.

Low-level `*_async`, `notify_*`, `finish_*`, and `cancel_*` functions are
operation-token hooks for actor completion tests and direct runtime integration.
Normal async application code uses `accept`, `connect`, `read`, `write`, and
the buffered reader helpers.

### `/std/async_time`

`/std/async_time` exposes awaitable timers:

```ciel
export async Result<void, AsyncError> sleep_ms(u64 ms);
```

`sleep_ms` is `CancelSafe + Abortable`. Timeouts are expressed through future
helpers such as `async::timeout`, not through manual operation-token chains.

The low-level `sleep_ms_async`, `notify_sleep_done`, `finish_sleep`, and
`cancel_sleep` functions are hooks for direct actor-completion integration.
Timer policy is deliberately narrow: heartbeat, retry, missed-pong, and
deadline behavior belongs in application code or in helpers such as
`async::timeout`.

### `/std/actor`

`/std/actor` remains an advanced compatibility module. It is not the primary
surface for ordinary async/await programs. New async examples teach async
functions, `await`, tasks, channels, task groups, `select`, and timeout before
exposing operation tokens or explicit actor mailboxes.
