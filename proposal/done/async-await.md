# Async/Await Proposal

## Historical Status

This document records the original async/await design and its pre-generalized
task API. `typed-task-errors` later replaced `Task<T>` and `TaskGroup<T>` with
`Task<T, E>` and `TaskGroup<T, E>`. `error-downcast` then made `Error` a local,
downcastable erased value that does not implement `Message`; an erased error
that crosses a task boundary is now `Report`, while concrete messageable error
types remain preferred. References below to `Task<T>`, `TaskGroup<T>`, or
cross-task `Error` results are historical sketches, not the current API.
Local async functions may still return `Result<T, Error>` when their errors do
not cross an ownership boundary. `design.md` is normative.

Current channel endpoint aliases are freely discardable and close only through
explicit `close` or `close_receiver` calls. `SendPermit<T>` is now an affine
resource lease whose cleanup returns unused capacity. The refcounted endpoint
lifetime and ordinary-struct permit sketches below are historical.

This proposal adds stackless async/await to Ciel. The implementation is
actor-backed, but the normal user-facing API should look like mainstream
async/await in languages such as Python and Rust:

```ciel
async Result<void, Error> echo(AsyncTcpStream stream) {
    while (true) {
        Bytes bytes = await net::read(stream, 16384)?;
        if (bytes_len(bytes) == 0) {
            return Ok;
        }
        await net::write_all(stream, bytes)?;
    }
}

Task<void> task = async::spawn(async || echo(stream))?;
await task?;
```

Users should not need to learn the low-level actor mailbox API for ordinary
async I/O. Actors remain the runtime isolation mechanism and a low-level
compatibility API, not the primary programming model.

Compiled safe code must be concurrency-safe by construction. The only exception
is trusted unsafe policy code, such as a manually written `unsafe impl
clone_message` that lies about a type.

## Proposal Order

```text
dispatch-actor-io-runtime <= async-await[async operation backend]
actor-owned-state <= async-await[actor-owned async frame storage]
pure-library-message <= async-await[cross-task payload policy]
binding-mutability <= async-await[locals live across await]
async-await :> async-task-lowering[task spawn and await lowering]
```

`dispatch-actor-io-runtime` provides the nonblocking operation backend.
`actor-owned-state` provides the runtime storage boundary for async frames.
`pure-library-message` owns values that cross between tasks through channels,
task results, spawn captures, or low-level actor mailboxes. `binding-mutability`
supplies the local binding rules used by live-local analysis.

`monomorphized-c-callbacks` is not on this path. It remains a separate FFI
proposal for generic C callback function items.

## Current Implementation Baseline

This proposal starts from an actor-oriented async implementation, not from a
blank runtime.

Compiler status:

1. There is no language-level `async`, `await`, or `select` syntax yet. The
   lexer currently treats `async`, `await`, and `select` as ordinary
   identifiers, which is important because `/std/async` is an existing module
   path. The implementation should therefore introduce these as contextual
   keywords or explicitly preserve module-path compatibility.
2. The compiler already has ordinary closures, retained closure capability
   checks, `?` propagation through `Result<T, Error>`, `defer` cleanup,
   interface aliases, generic capability constraints, structural `Message`
   policy through `meta::Repr<T>`, `ThreadLocal`/share-handle marker policy,
   actor lowering, and actor-owned state through `spawn_actor_state`.
3. The compiler does not yet have first-class generated future frame types,
   liveness across suspension points, async-frame-safety analysis, task lowering,
   hidden resume events, `Task<T>` result routing, or async cleanup state.

Runtime status:

1. Actors are backed by serial libdispatch queues. Actor messages are delivered
   by `ciel_actor_send`, and the runtime prevents concurrent execution of one
   actor's handler.
2. File, TCP, accept, connect, and timer operations are represented by
   `CielAsyncOp` tokens. Completions can enqueue a user-provided actor message
   and are finished through `finish_*` functions.
3. The current operation tokens support cancellation and own their libdispatch
   callback state, but they do not yet carry task id, operation id, generation,
   hidden resume-event routing, task result waiters, select-set registration, or
   deterministic task-frame cleanup.

Standard library status:

1. `/std/async` currently exposes the flow API:
   `AsyncRunner<S>`, `AsyncTask<S, Out>`, `Completion<S, Out>`,
   `spawn_runner`, `from_completion`, `then`, `start`, `stop_runner`, and
   `join_runner`.
2. `/std/async/adapter` contains the operation-token adapter interfaces
   `notify_done` and `finish`.
3. `/std/async_io`, `/std/async_net`, and `/std/async_time` expose low-level
   operation tokens, completion adapters, and `*_task` helpers over the flow
   API. Their fixtures already cover async file I/O, TCP accept/connect/read/
   write, timers, cancellation, and facade re-exports.
4. `/std/channel` is a synchronous actor/message-friendly channel. It is not the
   bounded async task channel described by this proposal.
5. Tutorial chapter 9 is intentionally written against the current flow API.
   It is the compatibility baseline until task async/await has equivalent
   coverage.

## Motivation

The current actor-friendly async APIs expose operation tokens and completion
messages. Users must manually encode:

1. start operation;
2. register completion message;
3. return from the actor handler;
4. receive completion;
5. call `finish_*`;
6. update state and schedule the next operation.

That is safe, but it makes ordinary async I/O look like a hand-written compiler
state machine. The main API should instead let users write sequential async
control flow and let the compiler generate the state machine.

## Goals

1. Make user code look like mainstream async/await.
2. Keep actors as the implementation model, not the ordinary user model.
3. Use ordinary `async` functions, not `actor async` functions.
4. Make async calls produce first-class `Future<T>` values that can be stored,
   passed to combinators, and awaited.
5. Provide `Task<T>` handles that can be awaited.
6. Provide async channels for communication between tasks.
7. Keep async function declarations ordinary: an `async Result<T, Error>`
   function declares the value produced by `await`, while the call expression
   has an opaque future type implementing `Future<Result<T, Error>>`.
8. Allow non-`Message` task-local values to live across await when they are safe
   to store in an actor-owned async frame.
9. Require `Message` only for values that cross task ownership, plus explicit
   low-level actor mailbox payloads.
10. Derive structural `Message` witnesses through `meta::Repr<T>` when possible.
11. Reject safe code that would keep borrowed stack values, raw pointers,
    thread-local handles, or borrowed views live across await.
12. Provide ergonomic multi-way `select` semantics. The user-facing path should
    not require `select2`, `select3`, and so on for ordinary code.
13. Keep select-set machinery as a compiler/stdlib lowering detail, not as a
    second public async API.
14. Separate operation cancellation from task abort so selectable operations
    preserve protocol state, while task termination can still release stuck I/O.
15. Delete the public flow API (`AsyncRunner`, `AsyncTask`, `Completion`,
    `then`, and `start`) after async/await has equivalent coverage.

## Non-Goals

1. Manual polling in ordinary application code.
2. Stackful coroutines or suspended native stacks.
3. Borrow checking or lifetime inference.
4. Parallel execution inside one async task.
5. Making task-local values `Message`.
6. Asking users to classify locals as actor state.
7. Requiring ordinary users to name `Actor<M>` or `Mailbox<M>`.
8. Making arbitrary raw pointers safe across suspension.
9. Stabilizing the memory layout of compiler-generated future frame types.

## User-Facing Model

### Async Functions

An async function is declared with `async`:

```ciel
async Result<Bytes, Error> read_frame(AsyncTcpStream stream) {
    Bytes header = await net::read_exact(stream, 8)?;
    usize len = decode_len(header)?;
    return await net::read_exact(stream, len);
}
```

Calling an async function creates a first-class future. `await` is required only
when the caller wants to drive that future to completion and observe its output:

```ciel
Future<Result<Bytes, Error>> future = read_frame(stream);
Bytes frame = await read_frame(stream)?;
```

The concrete type of `future` is opaque and compiler-generated, but it
implements the standard `Future<Result<Bytes, Error>>` capability. Users can
store it in locals, pass it to `select`, pass it to `async::spawn`, or await it
once. They do not write or inspect the generated frame type.

Dropping a future before it starts is allowed. Dropping a future after it has
registered an operation is cancellation and is permitted only through a
`CancelSafe` path, or through task abort when the task is terminating.

`await future` consumes or drives the future until it produces its declared
output type. If the output is `Result<T, Error>`, ordinary `?` propagation works
after the await:

```ciel
Bytes frame = await future?;
```

The runtime remains actor-backed. A first-class future is a safe handle to a
private continuation frame or operation token, not a pointer into another
task's mutable state.

### Spawning Tasks

The high-level spawn API is task-shaped:

```ciel
export struct Task<T> {
    *void handle;
}

export Result<Task<T>, Error> spawn<T, F: Future<Result<T, Error>> + Abortable>(
    F body
);

export Result<void, Error> cancel<T>(*Task<T> task);
export Result<bool, Error> is_finished<T>(*Task<T> task);
```

The source API intentionally does not ask users to write `Message` constraints
on `spawn`. The compiler treats `spawn` as a cross-task boundary and inserts the
required hidden obligations for the task result and captured values.
An async closure passed directly to `spawn` is not first converted to an
ordinary retained closure type such as `R |(...): Message|`; the compiler
analyzes its captures directly and reports boundary errors on the captured
values. If an already-created future value is passed to `spawn`, that future
crosses into the spawned task; compiler-generated futures are checked through
their captures, while handwritten future values need an explicit safe crossing
policy.

`Task<T>` is awaitable:

```ciel
Task<usize> task = async::spawn(async || compute_size(path))?;
usize size = await task?;
```

`Task<T>` implements `Future<Result<T, Error>>`. The task result crosses from
the spawned task to the awaiting task, so the compiler must prove that `T` is
messageable. In ordinary structs and enums, this proof should be derived or
resolved structurally.

If a task does not need to return a value, use `Task<void>`.

### Task Groups

Static `select` covers a fixed set of futures known at compile time. Dynamic
concurrency should use task groups instead of exposing a second public dynamic
select API:

```ciel
export struct TaskGroup<T> {
    *void handle;
}

export Result<TaskGroup<T>, Error> task_group<T>();
export Result<void, Error> group_add<T>(*const TaskGroup<T> group, Task<T> task);
export async Result<T, Error> group_next<T>(*const TaskGroup<T> group);
export Result<void, Error> group_cancel_all<T>(*const TaskGroup<T> group);
export Result<void, Error> group_close<T>(*const TaskGroup<T> group);
```

`group_next` waits for the next task in the group to finish. It does not cancel
the other tasks, so it does not require loser futures to be `CancelSafe`.
Cancelling or dropping the group aborts the remaining tasks through their task
abort path. This is the scalable model for dynamic sets of connections,
workers, or background jobs: spawn one task per unit of work, add the task to a
group, and await completions with `group_next`.

### Channels

Async tasks communicate through channels, not through low-level actor mailboxes:

```ciel
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

export Result<ChannelPair<T>, Error> channel<T>(usize capacity);
export async Result<void, Error> send<T>(Sender<T> sender, T value);
export Result<void, Error> try_send<T>(Sender<T> sender, T value);
export async Result<SendPermit<T>, Error> reserve<T>(Sender<T> sender);
export Result<void, Error> permit_send<T>(SendPermit<T> permit, T value);
export async Result<T, Error> recv<T>(Receiver<T> receiver);
export Result<void, Error> close<T>(Sender<T> sender);
export Result<void, Error> close_receiver<T>(Receiver<T> receiver);
```

As with tasks, the source API is simple and the compiler enforces the hidden
cross-task payload rule at `send` and `recv` use sites.
The primary channel is bounded. `send` suspends when the channel is full, which
provides backpressure for network-to-database and similar producer/consumer
pipelines. `try_send` is the non-suspending fast path.

`send(value)` is not `CancelSafe` by default because cancelling the future can
drop a value that was moved into the send operation. Code that needs
cancel-safe sending in `select` should first await `reserve(sender)`, which is
`CancelSafe`, and then synchronously commit with `permit_send(permit, value)`.

Example:

```ciel
ChannelPair<Bytes> ch = async::channel<Bytes>(1024)?;
Task<void> writer = async::spawn(async || write_loop(ch.receiver))?;
await send(ch.sender, payload)?;
await writer?;
```

Channels are the normal way to express producer/consumer or control-plane
communication. Low-level actors can still exist for compatibility, but tutorials
should teach async channels.

### Channel Lifecycle

Channels must not rely on user code calling `close` to avoid hung receivers.
The runtime owns endpoint liveness:

1. each channel tracks the number of live sender handles and live receiver
   handles;
2. cloning or moving a sender into another task increments or transfers that
   live-sender ownership according to the handle representation;
3. cloning or moving a receiver into another task increments or transfers that
   live-receiver ownership according to the handle representation; unicast
   channels usually keep this count at one;
4. explicit `close(sender)` closes that sender handle;
5. explicit `close_receiver(receiver)`, receiver drop, or deterministic task
   cleanup closes that receiver handle;
6. when the last sender is closed or destroyed, all pending receivers wake with
   a closed-channel error once the message queue is empty;
7. when the last receiver is closed or destroyed, all pending senders and
   pending `reserve` waiters wake with `channel_closed_error()`;
8. `send(sender, value)`, `try_send(sender, value)`, and `reserve(sender)` fail
   immediately with `channel_closed_error()` if no live receiver remains;
9. task cleanup after normal return, `Err`, panic, cancellation, or abort must
   destroy sender and receiver handles stored in the task frame and update the
   endpoint counts before the task is considered finished;
10. GC finalization may release leaked handles eventually, but it is not a
   scheduling guarantee and cannot be the only channel-close mechanism;
11. `recv(receiver)` waits until a message is available, or until the channel is
   closed and no buffered messages remain.

### Future Capabilities

`Future<T>` is an ordinary capability interface over an opaque future value:

```ciel
export enum Poll<T> {
    Pending,
    Ready(T),
}

export struct FutureContext {
    *void raw;
}

export interface<F, T> Poll<T> poll(*F future, *FutureContext cx);
export interface Future<T> = poll<T>;
```

The exact `Poll` and `FutureContext` spelling can change during implementation,
but the source-level model is fixed:

1. an async function call returns an opaque type implementing `Future<Out>`;
2. `await` accepts a value implementing `Future<Out>` and yields `Out`;
3. async closures and task handles also implement `Future<Out>`;
4. ordinary users do not call `poll` directly;
5. stdlib combinators can be generic over `Future<T>`.

Two additional capabilities describe what the runtime may do to a pending
future:

```ciel
export unsafe interface<F> bool cancel_safe_marker(*const F future);
export interface CancelSafe = cancel_safe_marker;

export unsafe interface<F> Result<void, Error> abort_future(*F future);
export interface Abortable = abort_future;

export interface SelectableFuture<T> = Future<T> + CancelSafe + Abortable;

export Error cancelled_error();
export Error timeout_error();
export Error channel_closed_error();
export Error task_failed_error(Error cause);
export Error aborted_error();
```

`SelectableFuture<T>` requires generic interface aliases. Without aliases, the
same bound can be written out as `Future<T> + CancelSafe + Abortable`.

### Cancel And Abort

This proposal uses two distinct terms:

1. **Cancel** abandons one pending future while the current task continues. This
   is what happens to losing arms in `select` and `timeout`.
   Cancellation must preserve the logical state of every resource that remains
   usable after the combinator returns.
2. **Abort** tears down the currently suspended operation because the owning
   task is terminating. Abort must release runtime resources and wake the task
   cleanup path, but it may close or poison the underlying protocol object
   because user code in that task will not continue using it.

`CancelSafe` means that cancelling a pending future cannot lose user-visible
data, corrupt protocol state, or hide a side effect in a resource that remains
usable. It is required for selectable losing futures.

`Abortable` means that a suspended future can be forcefully unwound by the task
runtime without leaking the task, the actor, or kernel resources. It is required
for futures awaited by cancellable tasks. An `Abortable` implementation may
close a socket, deregister a timer, poison a handle, or otherwise make the
resource unusable as long as cleanup is bounded and later aliases observe a
defined error instead of unsynchronized state.

`Abortable` also requires callback lifetime safety. If a suspended operation is
backed by libdispatch, epoll, kqueue, a timer queue, or any other external
callback source, the C callback must never capture an async frame pointer,
`TaskState` pointer, or pointer into user frame storage. Some backends, notably
libdispatch, cannot cancel a closure that has already entered a queue; that
closure may run after the task has aborted and after `drop_async_frame` has
released the frame to GC.

The required design is actor-mailbox routing by id and generation:

1. starting an external operation allocates a runtime-owned operation token;
2. the token contains only routing data such as actor mailbox id, task id,
   operation id, generation, result storage, and cleanup hooks;
3. any C callback stores its owned result into that token and enqueues a hidden
   completion event to the actor mailbox or runtime completion router;
4. the generated actor resume dispatcher receives the event, looks up the task
   and operation generation, and resumes only if the event still matches the
   current suspended operation;
5. stale events release their operation-owned result storage and are ignored.

The callback-visible result buffers, connection handles, and temporary payloads
must be owned or rooted by the operation token until the routed event is
processed. They must not live only inside the async frame.

Both interfaces are trusted policy interfaces. They are implemented by the
compiler for generated async futures when their bodies satisfy the rules, and
by the stdlib for primitive operation futures. Handwritten unsafe impls are
allowed only at the same trust boundary as other unsafe policy impls.

`CancelSafe` is not closed under ordinary async composition. A future that
awaits only `CancelSafe` operations may still be non-cancel-safe if it consumes
protocol state into local variables and then suspends again. For example, a
frame reader that reads a header, stores the decoded length locally, and then
awaits the body cannot be cancelled after the header has been consumed unless it
can put the header back or otherwise restore protocol state.

When business logic is not `CancelSafe`, the recommended isolation pattern is
to spawn it as an independent `Task<T>` and select or timeout the task handle
instead of selecting the protocol future directly. Cancelling the wait on a task
handle only unregisters the waiter; it does not discard the running task's
internal protocol state. If the timeout policy should terminate the work, the
caller must explicitly cancel the task, which uses the task's `Abortable` path
and may close or poison owned resources such as sockets.

Example:

```ciel
Task<Frame> frame_task = async::spawn(async || read_frame(reader))?;
Result<Frame, Error> task_result = await async::timeout(frame_task, 5000)?;
Frame frame = task_result?;
```

Here `read_frame(reader)` may be non-`CancelSafe` because it consumes protocol
state across multiple awaits. The timeout cancels only the wait on the task
handle. It does not drop the partially progressed frame reader inside the task.

Rules:

1. `time::sleep_ms` is `CancelSafe + Abortable`.
2. awaiting a `Task<T>` handle is `CancelSafe + Abortable`; cancelling the wait
   does not cancel the task itself.
3. `async::recv` is `CancelSafe + Abortable`; sender liveness determines closed
   channel wakeups.
4. `async::reserve` for bounded channels is `CancelSafe + Abortable`; moving
   the value with `send(value)` is not `CancelSafe` by default.
5. `async_net::connect` is `CancelSafe + Abortable`; cancelling or aborting a
   pending connect closes the in-progress socket.
6. `async_net::accept` is `CancelSafe + Abortable`; cancelling deregisters the
   pending accept, and any race with a completed accepted stream is resolved by
   generation ownership so the stream is either returned to the winner or
   closed.
7. Buffered stream reads can be `CancelSafe + Abortable` if cancellation leaves
   already-read bytes in the reader's private buffer.
8. Raw `net::read`, `net::read_into`, `net::write`, and `net::write_all` are
   `Abortable` but not `CancelSafe`: task abort may close the stream, but a
   losing race must not silently discard bytes, lose an owned buffer, or hide
   partial writes while the task continues.
9. `CancelSafe` and `Abortable` are behavioral and trusted; neither should be
   structurally derived through `meta::Repr<T>` the way `Message` can be.
10. The compiler must not infer `CancelSafe` merely because all awaited
   operations are `CancelSafe`.
11. The compiler may infer `CancelSafe` only for transparent wrappers or other
   patterns whose cancellation proof preserves all externally visible protocol
   state. General multi-await protocol code requires an explicit trusted
   stdlib implementation or an unsafe policy impl.
12. A compiler-generated async future implements `Abortable` only when every
   suspension point can be aborted and cleanup cannot block indefinitely on a
   non-`Abortable` operation.
13. Generic async functions carry latent `CancelSafe` and `Abortable`
    obligations for future parameters or calls whose concrete behavior is not
    yet known.
14. Aborting a suspended external operation marks its task/operation generation
    dead and detaches the async frame from the operation token. The runtime may
    then run frame cleanup without waiting for libdispatch-style callbacks to
    drain, because those callbacks can only enqueue mailbox-routed completion
    events and cannot dereference the freed frame.

### Select, Race, And Timeout

`select` must scale to an arbitrary number of arms. `select2` is not an
acceptable primary API because nested binary selection is not fair by default:
`select2(select2(a, b), c)` gives `c` half of the tie probability and gives
`a` and `b` one quarter each.

The user-facing API should therefore include a `select` expression with
multi-way fairness:

```ciel
AsyncTcpSplit split = net::split(stream)?;
BufferedStreamReader reader = net::buffered_reader(split.read, 65536)?;

Event event = await select {
    case result = net::read_buffered(reader, 16384):
        Event::Bytes(result?)

    case result = async::recv(commands):
        Event::Command(result?)

    case result = time::sleep_ms(5000):
        result?;
        Event::Tick
};
```

Rules:

1. The whole `select` expression is awaited; arms contain futures, not nested
   `await` expressions.
2. Each arm must produce a value assignable to the `select` expression result
   type. Heterogeneous arm outputs are handled by explicit arm bodies, usually
   by constructing an enum such as `Event`.
   `?` inside an arm has the same propagation behavior as `?` in the enclosing
   async function.
3. Every arm future must implement `Future<ArmOut> + CancelSafe + Abortable`.
4. The first completed arm wins.
5. If more than one arm is ready, default `select` chooses fairly over all
   ready arms using a per-site rotating start point, random start point, or an
   equivalent starvation-free strategy. Fairness is over the flat arm list, not
   over a nested binary tree.
6. `biased select` is the explicit source-order variant for code that needs
   deterministic priority.
7. Losing futures are cancelled only after the type system has proven
   `CancelSafe`.
8. Stale completions from losing futures are ignored only after the
   `CancelSafe` contract permits dropping the completion.
9. Raw stream reads and writes are rejected by the arm capability checks. Use
   buffered reader APIs, protocol-specific future wrappers that preserve stream
   state, or run the raw operation in a separate task and communicate by
   channel.

The compiler lowers `select` to a stdlib-managed internal `SelectSet<R>` future.
That keeps the runtime mechanism in the library while still giving users a
normal language-level control-flow construct. `SelectSet<R>` is not a public
composition API and should not be re-exported as the normal way to write async
code.

Schematic internal declarations:

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

export async Result<R, Error> select_set_wait<R>(
    SelectSet<R> set,
);
```

These functions live in an internal stdlib namespace used by compiler lowering.
They are listed here to specify the implementation contract, not as tutorial
surface.

`timeout` remains a convenience wrapper implemented through the same select
set machinery:

```ciel
export async Result<A, Error> timeout<A, FA: SelectableFuture<A>>(
    FA future,
    u64 ms,
);
```

## What Requires Message

Users should not decide which locals are "state" and which are "messages". The
compiler decides where values live. Most application code should only encounter
`Message` when a crossing value is genuinely not safe to send to another task,
and the diagnostic should identify the bad field or captured value.

`Message` is required only when a value crosses task ownership, or when code
explicitly opts into the low-level actor API:

1. task results `T` in `Task<T>`;
2. values captured by a spawned task body;
3. channel payloads `T`;
4. low-level actor mailbox payloads, for code that explicitly uses actors.

Task-local values created inside an async function do not need `Message` merely
because they live across await. They are stored in the actor-owned async frame.

## Hidden Boundary Obligations

The task and channel APIs should look like ordinary generic APIs in source, but
the compiler must attach hidden obligations at every cross-task boundary:

1. for concrete crossing types, prove `Message` immediately;
2. for user structs and enums, try structural derivation through `meta::Repr`;
3. for generic crossing types, record a latent `Message` obligation on the
   generic instance and check it at monomorphization or at the call site where
   the type becomes concrete;
4. for exported generic APIs or erased interfaces that can cross tasks, require
   an explicit public capability bound or an equivalent carried witness;
5. for async closures passed directly to `spawn`, inspect captures directly
   instead of requiring users to cast the closure to a retained `: Message`
   function type;
6. if proof fails, report the source boundary and the nested field or capture
   that blocked the proof.

This is similar to Rust surfacing `Send` only at concurrency boundaries, but the
common concrete-struct case should be automatic.

## Compiler-Generated Message Witnesses

When a crossing value must satisfy `Message`, the compiler should generate or
resolve the witness automatically whenever the type is structurally messageable.

For user structs and enums, the generated path is:

1. derive or synthesize `meta::Repr<T>`;
2. prove that the representation contains only `Message` components;
3. prove that no nested component satisfies forbidden `ThreadLocal` policy;
4. generate clone through the representation, or equivalent direct field clone;
5. report diagnostics through the original field path when derivation fails.

This derivation is part of ordinary type checking for crossing values, not
boilerplate users write in async code. Users should see diagnostics such as:

```text
field `config.cache.raw_fd` is not messageable because `RawFd` is thread-local
```

They should not be asked to write `meta::Repr` boilerplate for ordinary async
code.

Unsafe handwritten `Message` impls remain trusted. If such an impl lies, it can
break the guarantee just like any other unsafe policy impl.

## Concurrency Safety Invariant

Safe async code must be data-race-free and ownership-safe by construction:

1. each async frame is owned by exactly one task;
2. the runtime never resumes two continuations of the same task concurrently;
3. task-local frame values are never exposed through task handles, channels, or
   resume events;
4. every value that crosses into another task is cloned, moved, or stored through
   a proven `Message` path;
5. task handles and channel endpoints are opaque handles, not pointers into an
   async frame;
6. hidden resume events are generated by the compiler/runtime and cannot carry
   arbitrary user payloads;
7. unsafe `Message` impls are the only trusted escape hatch in the safe async
   concurrency model.

## Task-Local Frame Safety

The compiler stores locals live across await in an actor-owned async frame. The
safety question for these locals is not "are they `Message`?" but "can they be
stored in a private async frame without a hidden shared borrow or invalid
pointer?"

Allowed across await in safe code:

1. owned scalars, enums, structs, and arrays whose transitive fields are also
   frame-safe;
2. runtime handles documented as shareable or async-frame-safe;
3. values satisfying `Message`;
4. direct local `[]const T` slices with syntactically proven static read-only
   provenance, including string literals;
5. compiler-generated operation keys and discriminants.

Rejected across await in safe code:

1. raw pointers (`*T`, `*const T`);
2. nullable raw pointers (`?*T`);
3. mutable slices (`[]T`);
4. borrowed `[]const T` slices whose owner is a stack local, a field of another
   live local, a temporary, or otherwise not proven static;
5. pointers or references into stack locals;
6. pointers or references into fields of another live local;
7. values satisfying `ThreadLocal`;
8. closure values that capture forbidden locals;
9. structs, enums, arrays, or generic values whose transitive fields may
   contain forbidden fields and lack a proven async-frame-safety policy.

This structural predicate can be private to the compiler at first, for example
`AsyncFrameSafe`. It should not become a concept ordinary users must name.
The predicate is deep and contagious: if any field of a struct or enum payload
contains a rejected pointer, borrowed view, mutable slice, thread-local handle,
or closure capture, the whole outer value is rejected across await.

The predicate is provenance-sensitive for slices: the slice type alone is not
enough to decide safety. A direct local string literal slice is safe because it
points at static read-only storage, while a slice into a local buffer is
rejected because the view would outlive a stack-like owner relationship across
suspension. In the first implementation, the compiler should not try to prove
static provenance through struct fields, enum payloads, arrays of slices, or
generic containers. Those composite values are rejected across await if they
contain any slice or reference-view field, even if a particular constructor
happened to store a string literal. A later proposal can add explicit owned
frame-safe representations or precise static-provenance tracking through
fields.

Example rejected:

```ciel
[]const u8 view = buffer[0..n];
await time::sleep_ms(1)?;
use(view); // rejected: borrowed slice crosses await
```

Example accepted:

```ciel
[]const char msg = "start processing";
await time::sleep_ms(1)?;
print(msg); // accepted: string literal slice has static read-only provenance
```

Example rejected in the first implementation:

```ciel
struct LogLine {
    []const char text;
}

LogLine line = LogLine { .text = "start processing" };
await time::sleep_ms(1)?;
print(line.text); // rejected: a slice field crosses await
```

Example accepted:

```ciel
usize n = buffer_len;
await time::sleep_ms(1)?;
[]const u8 view = buffer[0..n];
use(view); // slice is created after await
```

### Future Frame Promotion

The first implementation should reject borrowed views across await unless they
are direct local slices with syntactically static read-only provenance. It
should not silently heap-lift them.
Heap-lifting the reference value alone is not safe: the referenced object may
still be a stack local, C allocation, moved GC object, or mutable owner with
aliasing constraints.

A later proposal can relax this through provenance-aware frame promotion:

1. if the owner is an owned local in the same async frame, store the owner in the
   frame and represent the view as owner identity plus offset and length;
2. if the owner is not frame-owned, require an explicit owned copy such as
   `Bytes`;
3. if the runtime supports pinned frame storage or interior-pointer-aware GC,
   allow a narrower borrowed-view representation under that runtime contract;
4. keep raw pointers rejected across await unless an unsafe API explicitly owns
   the lifetime and pinning contract.

## Captures

Values captured by `async::spawn` cross into a new task and therefore must be
proven messageable:

```ciel
Config config = parse_config()?; // compiler derives/checks Message if captured

Task<void> task = async::spawn(async || {
    ServerCore core = init_server_core(config)?;
    await run_server(core)?;
    return Ok;
})?;
```

`ServerCore` is task-local. It can be non-`Message` if it is created inside the
task and satisfies frame safety across awaits. `config` is captured from the
spawner, so the compiler proves it messageable at the spawn boundary.

If an application truly needs to move an already-existing non-`Message` value
into a new task, that requires a future move-state proposal or a low-level
unsafe API. The high-level async API deliberately avoids this ownership hole.

## Low-Level Actors

The existing actor APIs remain available as an advanced compatibility layer:

1. `Actor<M>`;
2. `spawn_actor_state`;
3. `spawn_actor_cloned`;
4. low-level actor `send`, `stop`, and `join`.

Normal async tutorials should not start here. Actor mailboxes are the lowering
model behind tasks and channels, not the everyday user-facing abstraction.

For implementation mapping only:

1. generated task initialization corresponds to the low-level actor `init`
   phase: allocate the async frame, move or clone proven captures into it, and
   enter the first resume point;
2. the generated task resume dispatcher corresponds to the low-level actor
   `handle` phase: receive hidden runtime events, validate operation generation,
   switch on the program counter, and continue execution;
3. neither mapping is reflected in user source. Users write async functions,
   `await`, tasks, channels, and future combinators.

## Execution Semantics

Each spawned async task is backed by one actor-owned execution context. A task is
always in one of these states:

1. `Ready`;
2. `Suspended`;
3. `Cancelling`;
4. `Finished`;
5. `Failed`.

Rules:

1. At most one continuation of a task runs at a time.
2. Awaiting I/O suspends only the current task, not the OS thread.
3. Awaiting any `Future<T>` drives that future until it returns `T` or
   suspends the current task on a registered wakeup.
4. Awaiting a `Task<T>` suspends until that task finishes or fails.
5. Awaiting `recv(receiver)` suspends until a message is available, or until the
   channel is closed and no buffered messages remain.
6. Cancelling a losing future inside a combinator requires `CancelSafe` and
   resumes the combinator through the winning branch.
7. Cancelling a task requests task termination. If the task is suspended, the
   runtime invokes the current future's `Abortable` path so cleanup is bounded.
8. Safe cancellable tasks may not suspend on a non-`Abortable` future unless
   they are explicitly placed in a low-level unabortable runtime mode.
9. When aborting a suspended external operation, the runtime marks the current
   task/operation generation dead. External callbacks may still run later, but
   they can only enqueue hidden mailbox completion events through the
   operation token. They cannot inspect task state directly.
10. Task termination must run deterministic frame cleanup before the task is
   considered finished. Cleanup releases channel senders, task handles, select
   sets, buffered readers, and other logical resources; it must not wait for
   BDWGC finalization.
11. If an async function returns `Err`, the task is marked failed.
12. Stale completions are ignored only for operations whose `CancelSafe` or
    `Abortable` contract permits dropping the completion.
13. Internal actor resume events are not user-visible messages.

## Lowering Model

The compiler lowers each async function and async closure to an opaque
stackless future type. A future stores its program counter, live locals, nested
future state, and any operation keys needed by the runtime. Spawning a task
moves one such future into actor-owned task storage; awaiting a future in the
same task stores it as nested frame state.

For each async function instance, generate:

1. a program-counter field;
2. a frame struct containing locals live across await;
3. resume code for each await point;
4. a `Future<Out>` implementation for the generated future type;
5. cleanup code for cancelled, aborted, or failed frames;
6. drop glue for every initialized frame field that owns logical resources;
7. conditional `CancelSafe` and `Abortable` impls when the body proves them;
8. a wrapper that constructs the future from call arguments;
9. a wrapper that resumes it with a completed operation result.

Async frame cleanup should reuse the same conceptual machinery as `defer`
lowering where possible: a cleanup stack, reverse-order execution, and
well-defined paths for early return and failure. The async-specific extension is
that initialized frame fields are tracked across program-counter states, so
abort can run the right non-awaiting release hooks before the frame is handed to
GC for memory reclamation.

For:

```ciel
Bytes bytes = await net::read(stream, 16384)?;
```

lowering performs:

1. evaluate `stream` and `16384`;
2. construct the raw read future;
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
the native C stack. The compiler lowers resumes to a task-local trampoline:

1. a resume step returns `Suspended`, `Ready(next)`, `Finished`, or `Failed` to
   the trampoline;
2. immediately ready awaits schedule another trampoline iteration instead of a
   direct C callback recursion;
3. the trampoline enforces a fairness budget and re-enqueues the task when the
   budget is exhausted;
4. runtime callbacks enqueue actor-mailbox-routed resume events and never
   directly run unbounded user continuation chains or dereference async frames.

## Compiler Requirements

The compiler must implement:

1. Parse `async` function modifiers.
2. Parse `await` expressions.
3. Parse `select` expressions.
4. Type-check async function calls as opaque values implementing
   `Future<Out>`.
5. Reject `await` outside async functions.
6. Reject calls from synchronous functions that try to use async outputs without
   storing or passing the resulting future.
7. Reject passing async functions to ordinary function types unless an explicit
   future-producing function type is introduced later.
8. Type-check async functions with their declared ordinary output type.
9. Type-check `async::spawn` future shape.
10. Type-check `await` by requiring `Future<Out>` and yielding `Out`.
11. Support generic interface aliases such as `SelectableFuture<T> =
    Future<T> + CancelSafe + Abortable`.
12. Attach hidden `Message` obligations to `spawn`, task results, task awaits,
    channel payloads, and low-level actor mailboxes.
13. Analyze directly spawned async closure captures without requiring retained
    closure `: Message` syntax in user code.
14. Generate or resolve structural `Message` witnesses through `meta::Repr<T>`.
15. Propagate latent `Message` obligations through generic async functions and
    check them when the generic instance becomes concrete.
16. Reject non-`Message` crossing values with diagnostics that identify the
    non-messageable field path.
17. Compute live locals at each await point.
18. Enforce async-frame safety for every live local, using a deep structural
    check through structs, enum payloads, arrays, and generic arguments.
19. Track slice provenance well enough to allow direct local static read-only
    slices, such as string literals, while rejecting slices into non-static
    owners.
20. Reject compound values that contain slice or reference-view fields across
    await in the first implementation, unless the compiler has an explicit
    built-in proof that the representation is owned and frame-safe. Do not rely
    on dataflow through fields to prove static provenance in the MVP.
21. Infer hidden frame-safety constraints for generic locals that cross await.
22. Lower async functions to opaque future frame types.
23. Generate deterministic drop glue for every initialized async-frame field
    that owns logical resources, reusing the existing `defer` cleanup model
    where possible.
24. Lower `async::spawn` to actor runtime initialization plus generated
    dispatcher code.
25. Lower awaitable stdlib operations to future construction, poll,
    start/suspend/finish hooks, operation-token registration, and wake
    registration.
26. Ensure external-operation callbacks are routed through actor mailbox
    completion events keyed by task id, operation id, and generation; generated
    code must not expose async frame pointers to C callbacks.
27. Lower task awaiting to wait for task completion.
28. Lower async channel `recv` to a channel receive suspension.
29. Type-check `select` arms as a flat list of futures and a common result
    type.
30. Lower `select` expressions to `SelectSet<R>` construction and a single
    await of the set future.
31. Ensure generated `select` polling checks every arm once before parking.
32. Infer and emit `CancelSafe` impls only for transparent wrappers or other
    compiler-recognized patterns with a real cancellation proof. The compiler
    must not treat "all awaited operations are `CancelSafe`" as sufficient.
33. Infer and emit `Abortable` impls for generated future types only when every
    suspension point can be aborted.
34. Reject stdlib future helper calls when their generic `CancelSafe` or
    `Abortable` bounds are not satisfied.
35. Preserve flat-list fair winner selection in `select` and `SelectSet`
    lowering.
36. Lower generated resumes through a trampoline instead of direct recursive C
    calls.
37. Root async frames, boxed messages, task results, operation results,
    operation tokens, and runtime handles correctly for GC.
38. Preserve source-order evaluation before await.
39. Preserve `?` propagation across await points.
40. Preserve monomorphization of generic async functions.
41. Reject async functions in exported C ABI positions.
42. Reject safe cancellable tasks that can suspend on a future without an
    `Abortable` path.

## Runtime Requirements

The runtime must provide:

1. actor-owned storage for async frames;
2. hidden resume events not expressible in user code;
3. task handles with completion result storage;
4. operation keys with task identity and generation;
5. future wake registration and poll scheduling;
6. `select` polling that checks every arm once before parking on runtime
   wakeups, so user-space buffered readiness is observed without requiring a
   fresh OS event;
7. cancellation of losing futures only according to their `CancelSafe`
   contract;
8. task abort through the suspended future's `Abortable` contract, with bounded
   release of runtime and kernel resources;
9. rejection or explicit low-level isolation for non-`Abortable` futures in
   cancellable tasks;
10. generation-safe actor-mailbox routing for every pending external operation:
    callbacks receive only a runtime operation token containing actor mailbox
    id, task id, operation id, generation, owned result storage, and cleanup
    hooks;
11. libdispatch, epoll, kqueue, timer, and worker callbacks must enqueue hidden
    completion events through that route token and must never dereference async
    frames, task state, or user frame storage directly;
12. abort marks the current operation generation dead and detaches the frame
    from the operation token; the token and callback-visible result storage
    remain live until the callback/event cleanup path releases them;
13. the actor resume dispatcher validates task id, operation id, and generation
    before resuming a task, and stale events run token cleanup without touching
    the task frame;
14. stale-completion filtering only when the operation contract permits dropping
    stale completions;
15. bounded async channel send/receive storage, sender and receiver counts,
    deterministic endpoint cleanup, sender wakeups on capacity availability,
    sender wakeups on last receiver close, and receiver wakeups on last sender
    close;
16. task-group completion queues, remaining-task ownership, and group
    cancellation of unfinished tasks;
17. deterministic task-frame cleanup on return, `Err`, panic, or abort, before
    relying on GC memory reclamation;
18. non-awaiting release hooks for handles stored in frames, including channel
    sender refcount decrements that wake closed receivers immediately when the
    last sender is released and receiver refcount decrements that wake blocked
    senders immediately when the last receiver is released;
19. task cancellation wakeups;
20. task awaiting wakeups;
21. a trampoline for immediate completions and nested async resumes;
22. fair scheduling budgets so immediate awaits cannot monopolize the executor;
23. GC rooting for frames, messages, captures, handles, operation tokens, and
    completion results.

The runtime must not resume two continuations of the same task concurrently.

## Standard Library Changes

### `/std/async`

Replace the flow API with task/channel async support:

```ciel
export enum Poll<T> {
    Pending,
    Ready(T),
}

export struct FutureContext {
    *void raw;
}

export interface<F, T> Poll<T> poll(*F future, *FutureContext cx);
export interface Future<T> = poll<T>;

export unsafe interface<F> bool cancel_safe_marker(*const F future);
export interface CancelSafe = cancel_safe_marker;

export unsafe interface<F> Result<void, Error> abort_future(*F future);
export interface Abortable = abort_future;

export interface SelectableFuture<T> = Future<T> + CancelSafe + Abortable;

export struct Task<T> {
    *void handle;
}

export struct TaskGroup<T> {
    *void handle;
}

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

export Result<Task<T>, Error> spawn<T, F: Future<Result<T, Error>> + Abortable>(
    F body
);

export Result<T, Error> block_on<T, F: Future<Result<T, Error>> + Abortable>(
    F body
);

export Result<void, Error> cancel<T>(*Task<T> task);
export Result<TaskGroup<T>, Error> task_group<T>();
export Result<void, Error> group_add<T>(*const TaskGroup<T> group, Task<T> task);
export async Result<T, Error> group_next<T>(*const TaskGroup<T> group);
export Result<void, Error> group_cancel_all<T>(*const TaskGroup<T> group);
export Result<void, Error> group_close<T>(*const TaskGroup<T> group);

export Result<ChannelPair<T>, Error> channel<T>(usize capacity);
export async Result<void, Error> send<T>(Sender<T> sender, T value);
export Result<void, Error> try_send<T>(Sender<T> sender, T value);
export async Result<SendPermit<T>, Error> reserve<T>(Sender<T> sender);
export Result<void, Error> permit_send<T>(SendPermit<T> permit, T value);
export async Result<T, Error> recv<T>(Receiver<T> receiver);
export Result<void, Error> close<T>(Sender<T> sender);
export Result<void, Error> close_receiver<T>(Receiver<T> receiver);

export async Result<A, Error> timeout<A, FA: SelectableFuture<A>>(
    FA future,
    u64 ms,
);
```

The internal select-lowering support should live under an internal namespace,
for example `/std/async/internal`, and should not be re-exported as ordinary
user API:

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

export async Result<R, Error> select_set_wait<R>(
    SelectSet<R> set,
);
```

The `*void` fields above are schematic. The real stdlib representation must be
opaque to safe code: users can copy or pass task and channel handles
through the exported API, but cannot forge handles, dereference runtime state,
or reach into another task's async frame.

The visible declarations stay close to ordinary generic APIs. The compiler
attaches hidden `Message` obligations at cross-task boundaries and reports them
as boundary diagnostics, not as actor-state programming requirements.

`block_on` is the synchronous bridge for CLI entry points, tests, and embedding
hosts. It starts one future on the task runtime and blocks the calling thread
until the future returns. Ordinary async code should use `await` instead of
calling `block_on` from inside async bodies.

The stdlib must provide trusted handle implementations for `Task<T>`,
`TaskGroup<T>`, `Sender<T>`, `Receiver<T>`, and `SendPermit<T>` based on runtime
reference counting or equivalent synchronization. Those implementations are
safe only because the handles do not expose frame pointers or unsynchronized
payload storage to user code.

The primary channel constructor is bounded. The proposal intentionally does not
add `unbounded*` channel APIs in the first pass; avoiding unbounded queues keeps
backpressure in the default design and avoids extra migration work.

Async-specific failures use ordinary `Error` values constructed by
`cancelled_error`, `timeout_error`, `channel_closed_error`,
`task_failed_error`, and `aborted_error`. This keeps `?` propagation uniform
across task await, channel receive, timeout, and cancellation paths.

`FutureContext`, `poll`, `cancel_safe_marker`, and `abort_future` are for the
compiler, stdlib, and advanced generic abstractions. Ordinary async code should
get inferred diagnostics such as "raw TCP read is not cancellation safe in
timeout" instead of being asked to write marker boilerplate.

Delete the public flow API:

1. `AsyncRunner<S>`;
2. `AsyncTask<S, Out>`;
3. `Completion<S, Out>`;
4. `spawn_runner`;
5. `then`;
6. `start`;
7. `stop_runner`;
8. `join_runner`;
9. `from_completion`;
10. public `/std/async/adapter`.

The operation-token adapter layer may remain internal for implementation tests,
but users should not compose async code through it.

### Bytes

Move or re-export `Bytes` as a general owned byte buffer, preferably
`/std/bytes`. It is not conceptually tied to the old flow API.

### `/std/async_io`

Expose awaitable operations:

```ciel
export async Result<Bytes, Error> read_bytes(AsyncFd fd, usize max_len);
export async Result<usize, Error> write_bytes(AsyncFd fd, Bytes bytes);
```

Raw fd reads and writes are `Abortable` but not `CancelSafe` by default because
cancellation may otherwise hide offset changes or partial writes. Offset-stable
or buffered fd APIs can add `CancelSafe` later if their contracts preserve
state.

### `/std/async_net`

Expose awaitable TCP operations:

```ciel
export async Result<AsyncTcpStream, Error> accept(AsyncTcpListener listener);
export async Result<AsyncTcpStream, Error> connect(net::SocketAddr addr);
export async Result<Bytes, Error> read(AsyncTcpStream stream, usize max_len);
export async Result<(Bytes, usize), Error> read_into(
    AsyncTcpStream stream,
    Bytes buffer,
);
export async Result<usize, Error> write(AsyncTcpStream stream, Bytes bytes);
export async Result<void, Error> write_all(AsyncTcpStream stream, Bytes bytes);
```

`read` returns zero-length `Bytes` for EOF and is the convenience allocation
API. `read_into` is the reusable-buffer API: it moves an owned `Bytes` buffer
into the future and returns the same buffer with the number of bytes read. This
lets hot read loops reuse capacity without keeping a mutable `[]T` slice live
across await.

Raw `read`, `read_into`, `write`, and `write_all` are `Abortable` but not
`CancelSafe`; they are rejected by `SelectableFuture` combinator bounds. Task
abort may close the stream to release a stuck read or write, but a losing race
cannot keep using the same stream after possibly discarding bytes, losing an
owned buffer, or observing partial writes.

`accept` and `connect` are `CancelSafe + Abortable`. Timeout and race helpers
therefore work directly:

```ciel
AsyncTcpStream stream = await async::timeout(
    async_net::connect(addr),
    5000,
)??;
```

Selectable stream reads should go through a buffered reader:

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

export Result<AsyncTcpSplit, Error> split(
    AsyncTcpStream stream,
);

export Result<BufferedStreamReader, Error> buffered_reader(
    AsyncTcpReadHalf read_half,
    usize capacity,
);

export Result<AsyncTcpReadHalf, Error> into_read_half(
    BufferedStreamReader reader,
);

export async Result<Bytes, Error> read_buffered(
    BufferedStreamReader reader,
    usize max_len,
);
```

`read_buffered` is `CancelSafe + Abortable` only if cancellation preserves
already-read bytes inside the reader buffer and abort releases the pending read.
`BufferedStreamReader` must serialize or reject overlapping reads on the same
reader so cancellation cannot reorder stream bytes.

`BufferedStreamReader` owns the TCP read half and its private buffer. Splitting
the stream gives an independent write half for full-duplex protocols while
ensuring there is only one owner of read readiness and buffering state. Returning
the read half with `into_read_half` is allowed only after the buffered reader has
no pending read; unread buffered bytes remain owned by the reader and must be
drained or explicitly discarded by a later API before the raw read half is
recovered.

`read_buffered` must poll the user-space buffer before registering interest in
the underlying socket. If buffered bytes are already available, the future
returns `Ready` immediately and must not wait for another OS readability event.
This rule is required for `select`: a previous read may have drained the fd into
the buffered reader while leaving unread bytes in the reader's private buffer.
The `SelectSet` lowering must poll every arm once before parking so readiness
from user-space buffers, channels, completed timers, or already-finished tasks
cannot be missed.

### `/std/async_time`

Expose awaitable timers:

```ciel
export async Result<void, Error> sleep_ms(u64 ms);
```

`sleep_ms` is `CancelSafe + Abortable`. Timeouts should be expressed through
future helpers such as `async::timeout`, not through flow tasks.

### `/std/actor`

Keep `/std/actor` as an advanced compatibility module. It should not be the
primary tutorial path once async/await lands.

## Executable Project Split

The checkable execution checklist lives in
[`proposal/async-await-todo.md`](async-await-todo.md). That TODO is the
day-to-day tracker. The split is intentionally bottom-up and vertical: every
phase should use final-shaped public APIs and runtime routing, not throwaway
helpers that a later phase must replace.

Recommended order:

```text
minimal async/await timer slice
  -> async frames and cleanup
  -> task ownership boundary
  -> awaitable file and TCP I/O
  -> cancellation, abort, and timeout
  -> async communication
  -> select and buffered TCP reads
  -> migration and flow removal
```

1. **Minimal async/await timer slice** adds the final future surface,
   contextual async syntax, one-await lowering, `block_on`, final
   `/std/async_time::sleep_ms`, and final-shaped wake routing in one user-visible
   slice.
2. **Async frames and cleanup** expands the vertical slice to multi-await
   frames, nested future storage, live-local frame safety, deterministic cleanup,
   and trampoline scheduling.
3. **Task ownership boundary** adds `Task<T>`, `spawn`, task awaiting, task
   status/cancellation entry points, and hidden `Message` obligations for task
   results and captures.
4. **Awaitable file and TCP I/O** migrates async file I/O and async TCP
   operations from flow tasks to awaitable futures while preserving old flow
   compatibility.
5. **Cancellation, abort, and timeout** adds generation-routed external
   completions, trusted `CancelSafe`/`Abortable`, task abort cleanup, and
   `async::timeout`.
6. **Async communication** adds bounded async channels, endpoint lifecycle
   cleanup, payload boundary policy, and task groups for dynamic concurrency.
7. **Select and buffered TCP reads** adds compiler-level `select`, internal
   `SelectSet<R>` lowering, selectable-future checks, fair/biased tie handling,
   and cancellation-safe buffered TCP reads.
8. **Migration and flow removal** rewrites tutorial and intranet-tunnel code to
   task async/await, moves operation-token adapters internal, removes the public
   flow API, and updates `design.md` only after the new surface has equivalent
   fixture coverage.

## Resolved Decisions

1. `AsyncFrameSafe` remains compiler-private in the MVP. Ordinary users should
   not name it, and expert unsafe frame-safety policy interfaces are deferred
   until there is a concrete stdlib/runtime handle that cannot be expressed by
   the compiler-private predicate.
2. Operation-token APIs are internal. They may live under `/std/async/internal`
   for compiler lowering and implementation tests, but they are not re-exported
   or taught as a public async programming model.
3. Async-specific failures use ordinary `Error` constructors:
   `cancelled_error`, `timeout_error`, `channel_closed_error`,
   `task_failed_error`, and `aborted_error`.
4. Buffered TCP reads use `split`, `AsyncTcpSplit`, `AsyncTcpReadHalf`,
   `AsyncTcpWriteHalf`, `buffered_reader(read_half, capacity)`,
   `read_buffered`, and `into_read_half`. The buffered reader owns the read half
   and private buffer; the write half remains independent for full-duplex
   protocols.
5. Provenance-aware frame promotion is a later borrowed-view proposal. This
   async proposal only allows direct local static read-only slice values, such
   as string literals, across await and rejects non-static borrowed views.
6. Receiver endpoint liveness is tracked symmetrically with sender liveness.
   Last receiver close wakes blocked senders and reservations instead of
   relying on GC finalization.
7. Abortable futures backed by external callbacks use actor-mailbox routing by
   task id, operation id, and generation. C callbacks never capture async frame
   or task-state pointers, so libdispatch callbacks that run after abort can
   only enqueue stale completion events that the actor dispatcher drops.
8. Compound values containing slice or reference-view fields are rejected
   across await in the MVP unless they have an explicit owned frame-safe
   representation.
