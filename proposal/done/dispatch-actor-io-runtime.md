# Dispatch Actor And Async I/O Runtime Proposal

This proposal replaces the current one-pthread-per-actor runtime with a
libdispatch-backed runtime and adds a small actor-oriented asynchronous file
descriptor facility. The source-level actor API remains unchanged.

The intent is not to import Swift concurrency into Ciel. Ciel actors remain
mailbox actors: messages are cloned through `Message`, actor state is private,
and a handler processes one accepted message at a time.

## Proposal Order

```text
unsafe < dispatch-actor-io-runtime[raw descriptors and C runtime hooks]

dispatch-actor-io-runtime || monomorphized-c-callbacks[runtime ABI]
dispatch-actor-io-runtime || pure-library-message[async operation payloads]

monomorphized-c-callbacks :> actor-stdlib-lowering[dispatch callback]
```

`unsafe` is a hard prerequisite. This proposal uses unsafe source markers for
raw descriptor adoption, imported C runtime hooks, raw pointer casts in runtime
shims, and manual policy impls for runtime-backed handles. The actor runtime,
scoped blocking I/O facade, and async I/O handles should not be implemented as
safe ad hoc exceptions before `unsafe interface`, `unsafe impl`, unsafe C
imports, and unsafe function calls are available.

This proposal owns the actor runtime backend, dispatch integration, GC rooting
rules for dispatch callbacks, generation-checked scoped blocking I/O handles,
and the first asynchronous file descriptor completion surface.

`monomorphized-c-callbacks` owns the future migration that removes
actor-specific compiler builtins from `/std/actor`. This proposal does not wait
for that migration. It keeps the current runtime ABI:

```c
int32_t ciel_actor_spawn(
    CielActor **out,
    void *state,
    void *handler,
    void (*dispatch)(void *state, void *handler, void *message, int32_t *failed)
);
int32_t ciel_actor_send(CielActor *actor, void *message);
int32_t ciel_actor_stop(CielActor *actor);
int32_t ciel_actor_join(CielActor *actor);
```

The callback feature may later let `/std/actor` call the same runtime ABI from
ordinary Ciel code. The runtime ABI should be designed once so both lowering
paths share it.

## External API Facts

The proposal relies only on public libdispatch APIs for its stable surface:

1. Serial queues are created with `dispatch_queue_create` and
   `DISPATCH_QUEUE_SERIAL`, which is the same as a `NULL` queue attribute.
2. Actor jobs use `dispatch_async_f(queue, context, function)`, not Blocks.
3. Actor join uses `dispatch_group_create`, `dispatch_group_enter`,
   `dispatch_group_leave`, and `dispatch_group_wait`.
4. Self-join detection uses queue-specific data through
   `dispatch_queue_set_specific` and `dispatch_get_specific`.
5. Asynchronous file descriptor reads and writes use the public
   `dispatch_io_create`, `dispatch_io_read`, `dispatch_io_write`, and
   `dispatch_io_close` APIs through a C runtime shim.

`dispatch_io` is available as a public Blocks-based channel API through
`dispatch_io_create`, `dispatch_io_read`, `dispatch_io_write`, and
`dispatch_io_close`. The function-pointer variants such as
`dispatch_io_read_f` exist in `private/io_private.h`; that header explicitly
marks those interfaces as internal and unstable. Ciel must not expose or depend
on those private APIs.

`dispatch_io_read` and `dispatch_io_write` may invoke their handler more than
once for one scheduled operation. The final handler invocation has `done = true`;
EOF is represented by final empty data with error `0`. Low-water and high-water
marks control handler granularity, but callers must still tolerate partial
chunks. `dispatch_io_close(channel, DISPATCH_IO_STOP)` closes the channel to new
operations and attempts to interrupt outstanding work; final handlers may still
run and may report partial data plus `ECANCELED`.

The runtime uses a small C shim that calls the public Blocks APIs internally.
Ciel-generated code sees only ordinary C function pointers and opaque runtime
handles. Blocks may capture runtime-owned C state, not unrooted Ciel GC
pointers.

On Linux, swift-corelibs-libdispatch provides a real event backend based on
`epoll`, `eventfd`, `timerfd`, and `signalfd`. Its `dispatch_io` implementation
uses nonblocking stream sources for non-regular descriptors and a disk queue
using `read`, `pread`, `write`, and `pwrite` for regular files. That is enough
for the initial macOS/Linux implementation. The runtime should still keep an
internal scheduler boundary so a future port can swap the backend deliberately,
but this proposal does not preserve the current pthread actor backend.

## Goals

1. Keep `Actor<M>`, `spawn_actor_cloned`, `send`, `stop`, and `join` source-compatible.
2. Remove the runtime policy that each actor owns a dedicated pthread.
3. Preserve FIFO processing for accepted messages.
4. Preserve the current shutdown contract: `stop` and `join` close the mailbox
   to new sends; already accepted messages drain; `join` waits for them.
5. Preserve `Message` as the only safe cross-actor value movement rule.
6. Make dispatch callbacks safe with BDWGC/libgc thread registration and
   explicit rooting.
7. Add async file descriptor completion as actor messages, without introducing
   `async`/`await` syntax.
8. Treat macOS and Linux as the supported design targets for the first
   implementation.

## Non-Goals

1. No Swift actor isolation, executor inheritance, task groups, priorities, or
   structured cancellation.
2. No direct exposure of dispatch queues, sources, groups, data objects, or
   Blocks in Ciel source.
3. No dependency on `private/io_private.h`.
4. No removal of blocking POSIX-backed file operations. This proposal reshapes
   the safe surface around scoped helpers with generation-checked value
   handles instead of exposing raw descriptor values.
5. No asynchronous reads into caller-provided `[]u8`; borrowed slices remain
   actor-local and cannot be held by pending runtime operations.
6. No move-only, affine, borrow-checking, or owned-handle language feature.
   Resource ownership is expressed by the shape and documented policy of the
   standard-library API.
7. No `noescape` closure feature for scoped I/O callbacks. The current escape
   analysis is intentionally conservative, so this proposal does not depend on
   it to prove that callback arguments cannot escape.
8. No promise that Windows dispatch I/O is supported before a Ciel CI target
   verifies the required libdispatch and BlocksRuntime pieces.

## Actor Runtime Backend

Each actor is backed by one serial dispatch queue:

```c
struct CielActor {
    dispatch_queue_t queue;
    dispatch_group_t jobs;
    dispatch_semaphore_t lifecycle_lock;
    void *state;
    void *handler;
    CielActorDispatchFn dispatch;
    int closing;
    int joined;
    int failed;
};
```

The lifecycle semaphore is used as a small mutex protecting lifecycle flags and
the handoff between `send`, `stop`, and `join`. The dispatch queue owns
execution order. The dispatch group tracks all accepted jobs so `join` can wait
without a dedicated worker thread.

`spawn_actor_cloned`:

1. validates the raw arguments;
2. initializes the Ciel runtime and GC;
3. creates the actor object in runtime-owned GC-visible storage;
4. creates a serial dispatch queue;
5. sets queue-specific data to the actor pointer for self-join detection;
6. creates the dispatch group;
7. returns the raw actor handle.

`send` accepts an already cloned and boxed message. It does not clone payloads;
the compiler or `/std/actor` wrapper owns that step.

```c
int32_t ciel_actor_send(CielActor *actor, void *message) {
    lock(actor);
    if (actor->closing) {
        unlock(actor);
        return EPIPE;
    }
    dispatch_group_enter(actor->jobs);
    unlock(actor);

    CielActorJob *job = runtime_job_new(actor, message);
    dispatch_async_f(actor->queue, job, ciel_actor_job_run);
    return 0;
}
```

The order of `dispatch_group_enter` matters: a job is counted while the mailbox
is still known to be open. `join` sets `closing` under the same lock before
waiting, so no accepted job can be missed.

`ciel_actor_job_run`:

1. enters the Ciel runtime callback scope;
2. calls the generated `CielActorDispatchFn`;
3. if the handler reports failure, sets `failed` and closes the actor to new
   sends;
4. leaves the dispatch group;
5. exits the Ciel runtime callback scope.

Already accepted messages still drain after a handler failure. Failure closes
the mailbox to new sends, and the dispatch queue continues running already
accepted jobs in FIFO order. `join` returns `EIO` if any accepted job failed.

`stop` sets `closing` and returns. It does not cancel already accepted jobs.

`join`:

1. rejects `NULL`;
2. returns the previous result if the actor was already joined;
3. returns `EDEADLK` if called from the actor's own queue;
4. sets `closing`;
5. waits for `jobs` with `DISPATCH_TIME_FOREVER`;
6. marks the actor joined;
7. returns `EIO` if the actor failed, otherwise `0`.

Self-join is an error because the current job is part of the group being waited
on and a serial queue cannot make progress while its current job is blocked.

## GC And Callback Scope

Dispatch-managed memory is not a GC root. The runtime must make every Ciel
pointer reachable from dispatch callbacks visible to BDWGC/libgc.

Runtime objects that can be referenced only by libdispatch, such as actors,
jobs, async operations, boxed completion messages, byte buffers, handler boxes,
and state boxes, must use one of these policies:

1. allocate them with GC-visible uncollectable storage; or
2. store Ciel pointers in explicit `CielRoot` handles and release those roots
   after the dispatch callback no longer needs them.

The first implementation should use a simple scanned uncollectable allocation
for runtime-owned actor and job records. That matches the current runtime's
leaky handle policy and avoids finalizer ordering issues. A later ownership
proposal can add deterministic handle close/drop.

Every dispatch callback that may allocate, touch Ciel GC pointers, or call Ciel
generated code enters through a counted runtime callback scope:

```c
int32_t ciel_runtime_enter_callback(void);
void ciel_runtime_leave_callback(void);
```

The scope is implemented with thread-local depth:

1. on depth `0 -> 1`, call `ciel_thread_attach`;
2. record whether this scope actually registered the thread;
3. increment depth for nested entries;
4. decrement on leave;
5. on depth `1 -> 0`, call `ciel_thread_detach` only when this scope performed
   the registration.

This prevents nested dispatch callbacks or host-attached threads from
unregistering a thread too early. `ciel_thread_attach` and
`ciel_thread_detach` remain the host ABI; the counted scope is an internal
runtime helper used by dispatch callbacks. The internal helper must distinguish
`GC_SUCCESS` from `GC_DUPLICATE`: a duplicate registration is safe to use, but
the callback scope must not unregister a thread that was attached by the host or
by an outer runtime entry.

## Scoped Blocking I/O API

The safe blocking `/std/io` API should stop exposing a copyable `Fd { raw }`
value as its main file abstraction. Instead, it should expose scoped helpers
that open a descriptor, pass a private file token by value to a callback, and
close the descriptor before returning:

```rust
// /std/io
import /std/message;

export enum OpenMode {
    Read,
    Write,
    Append,
}

struct File;

export Result<R, Error> with_open<R: Message>(
    []const char path,
    OpenMode mode,
    Result<R, Error> |(File)| body
);

export Result<R, Error> with_open_read<R: Message>(
    []const char path,
    Result<R, Error> |(File)| body
);

export Result<R, Error> with_create<R: Message>(
    []const char path,
    Result<R, Error> |(File)| body
);

export Result<R, Error> with_append<R: Message>(
    []const char path,
    Result<R, Error> |(File)| body
);

export Result<usize, Error> read(File file, []u8 out);
export Result<usize, Error> write(File file, []const u8 data);
export Result<void, Error> write_all(File file, []const u8 data);
export Result<void, Error> write_text(File file, []const char text);
```

`File` is not exported and does not implement `Message`. Importers cannot name
it in fields, globals, function signatures, or actor state. The callback result
type is constrained with `R: Message`, so a callback cannot return the private
file token or a closure that captured it through the ordinary safe result path.

Ordinary file use becomes:

```rust
Result<usize, Error> count_header([]const char path) {
    return with_open_read(path, |file| {
        [4096]u8 @buf = [0;];
        usize n = read(file, buf[..])?;
        return Ok(n);
    });
}
```

The callback parameter type is supplied by the expected closure type of
`with_open_read`. The user does not write the private `File` name.

This is not a full static non-escape proof. Ciel does not yet have noescape
closures, and this proposal deliberately avoids adding them. The API still
removes the main source of descriptor reuse bugs: safe user code does not
receive a copyable raw descriptor value and cannot directly close or transfer
the descriptor outside the scoped helper. The standard-library implementation
uses `defer` to close the descriptor after the callback returns.

Because the scoped callback is not statically noescape, the runtime must still
validate every `File` operation. That validation must not rely on a stack
address or a per-open GC object that may dangle or accumulate. `File` is a
small private value handle, and the runtime stores the real descriptor state in
a slot table:

```c
typedef enum {
    CIEL_FILE_OPEN,
    CIEL_FILE_CLOSED,
    CIEL_FILE_TRANSFERRED,
} CielFileState;

typedef struct {
    uint32_t slot;
    uint32_t generation;
} CielFile;

struct CielFileSlot {
    int fd;
    uint32_t generation;
    CielFileState state;
};
```

`with_open_*` allocates a slot, opens the descriptor, writes the fd plus current
generation into that slot, constructs a `File` value from `(slot, generation)`,
calls the callback, then closes the descriptor, marks the slot `CLOSED`,
increments the slot generation, and returns the slot to the free list. `read`
and `write` first resolve the handle by checking that:

1. `slot` names a live slot;
2. `generation` matches the slot generation;
3. the slot state is `OPEN`.

Only after those checks does the runtime touch the OS descriptor. If a private
token escapes through a generic or closure edge case, later use fails because
the generation no longer matches or the slot state is no longer `OPEN`. The
runtime must not call `read`, `write`, or `close` on an integer fd that the OS
may already have reused.

If a future interop API transfers a `File` into an async or raw runtime owner,
it marks the slot `TRANSFERRED` before handing ownership to the new runtime
owner. Later blocking operations on the old scoped token return an error. The
private token shape and `R: Message` result bound reduce the ways a token can
escape; the generation-checked slot table is the safety backstop while Ciel has
no noescape closure parameter.

This design deliberately uses value handles rather than `*const File` wrapper
pointers. A pointer wrapper would need either stack storage, which becomes
unsound if the token escapes, or one runtime allocation per scoped open, which
would turn simple blocking I/O helpers into a GC-heavy path. A small value
token plus a reusable slot table keeps stale-handle detection and avoids those
costs.

The scoped API is intended for short-lived synchronous file operations:
read-all, write-file, append, copy, configuration loading, and formatting. It
is not the right abstraction for actor state, long-lived sockets, servers,
or async operations. Those use explicit runtime handles described below.

A low-level interop module may still expose raw descriptor values:

```rust
// /std/os/fd or another interop-only module
export struct RawFd {
    c::c_int raw;
}
```

`RawFd` is for host interop and platform glue, not the safe `/std/io` facade.
Conversions from `RawFd` into safe or async handles are policy boundaries and
must document descriptor ownership.

## Async File Descriptor Model

The async I/O surface is operation-token based. Starting an operation does not
suspend the current handler stack. The handler records the returned operation
token in actor state, registers a completion message, and returns. When
libdispatch finishes the operation, the runtime sends that preboxed completion
message to the actor. The next actor job calls `finish_read` or `finish_write`
to take the operation result.

This is actor-native async I/O: the suspended state is ordinary actor state,
not a hidden continuation.

`/std/io` owns scoped blocking I/O. A separate module owns long-lived async
handles:

```rust
// /std/async_io
import /std/actor;
import /std/io;
import /std/message;
import /std/result;

export struct Bytes {
    *void handle;
}

export struct AsyncFd {
    *void handle;
}

export struct AsyncRead {
    *void handle;
}

export struct AsyncWrite {
    *void handle;
}

export Result<Bytes, Error> bytes_copy([]const u8 data);
export usize bytes_len(Bytes bytes);
export Result<usize, Error> bytes_copy_to(Bytes bytes, []u8 out);

export Result<AsyncFd, Error> open_async([]const char path, io::OpenMode mode);
export Result<AsyncFd, Error> open_async_read([]const char path);
export Result<AsyncFd, Error> create_async([]const char path);
export Result<AsyncFd, Error> append_async([]const char path);

export Result<void, Error> close_async(AsyncFd fd);

export Result<AsyncRead, Error> read_bytes(AsyncFd fd, usize max_len);
export Result<AsyncWrite, Error> write_bytes(AsyncFd fd, Bytes data);

export Result<void, Error> notify_read_done<M: Message>(
    *const AsyncRead op,
    *const Actor<M> actor,
    M message
);

export Result<void, Error> notify_write_done<M: Message>(
    *const AsyncWrite op,
    *const Actor<M> actor,
    M message
);

export Result<Bytes, Error> finish_read(AsyncRead op);
export Result<usize, Error> finish_write(AsyncWrite op);

export Result<void, Error> cancel_read(AsyncRead op);
export Result<void, Error> cancel_write(AsyncWrite op);
```

The preferred constructors are `open_async`, `open_async_read`, `create_async`,
and `append_async`. They create the OS descriptor and the dispatch I/O channel
as one operation, so no scoped blocking `File` token or raw descriptor is
exposed.

An interop module may provide a raw-descriptor adoption hook for platform glue:

```rust
export unsafe Result<AsyncFd, Error> async_from_raw_fd(os::RawFd fd);
```

That hook is not part of the safe `/std/io` facade. It transfers control of the
descriptor to the async runtime by policy. Because Ciel does not have move-only
handles, the type system cannot invalidate the source raw value. Calling it
requires `unsafe {}` through the unsafe proposal. After successful adoption,
code must not use the old raw descriptor. APIs that can create an async
descriptor directly should prefer the direct async constructors.

For stream descriptors, dispatch I/O takes control of descriptor flags while
operations are pending. Code must not call ordinary blocking read or write on a
descriptor while it is controlled by `AsyncFd`.

`Bytes` is an owned immutable byte buffer. It may be implemented as a
runtime-owned copy or as a retained immutable dispatch data object wrapped in a
Ciel handle. `Bytes` implements `Message` by sharing immutable storage or by
copying. It never exposes a mutable interior pointer. `bytes_copy_to` copies
from a `Bytes` value into actor-local caller storage.

`AsyncRead` and `AsyncWrite` are synchronized operation handles. They implement
`Message` by copying the handle. The operation result remains in the runtime
operation object until exactly one successful `finish_read` or `finish_write`
call consumes it. Repeated finish attempts return an error.

`notify_read_done` and `notify_write_done` are one-shot. They clone and box the
provided message immediately. If the operation is already complete, the runtime
sends the boxed message immediately. Otherwise it stores the boxed message in
the operation and sends it when the dispatch I/O handler reaches its final
callback. This avoids a generic runtime clone callback.

The completion message usually carries the operation token:

```rust
enum ClientMsg {
    StartRead,
    ReadDone(AsyncRead),
    Shutdown,
}
```

This keeps the runtime independent from user message construction. The runtime
only stores and sends a boxed `M` value that was already produced through
ordinary `Message` conversion.

Example:

```rust
Result<State, Error> handle(State state, ClientMsg msg) {
    switch (msg) {
        case StartRead:
            AsyncRead op = read_bytes(state.fd, 4096)?;
            notify_read_done(&op, &state.self, ReadDone(op))?;
            state.pending_read = op;
            return Ok(state);

        case ReadDone(op):
            Bytes data = finish_read(op)?;
            if (bytes_len(data) == 0) {
                return Ok(close_client(state));
            }
            state = consume_bytes(state, data)?;
            AsyncRead next = read_bytes(state.fd, 4096)?;
            notify_read_done(&next, &state.self, ReadDone(next))?;
            state.pending_read = next;
            return Ok(state);

        case Shutdown:
            cancel_read(state.pending_read)?;
            close_async(state.fd)?;
            return Ok(state);
    }
}
```

The actor never blocks while the read is pending. Its handler returns after
registering the completion message, so the dispatch worker can run other actor
jobs. Completion resumes the actor by enqueueing another mailbox message, not
by restoring a suspended stack frame.

The actor owns the descriptor and consumes immutable `Bytes` values. No
borrowed pointer or slice crosses actors or survives in a pending runtime
operation.

The async I/O surface is pull-based. The runtime starts only the operations that
the actor explicitly requests. It does not install a hidden read loop on
`AsyncFd`, so an actor can apply backpressure by waiting to call `read_bytes`
again until it has consumed the previous `Bytes`. This proposal does not solve
general actor mailbox backpressure; that remains the existing standard-library
mailbox policy problem. The important I/O guarantee is narrower: async I/O will
not generate more completion messages than the actor has outstanding operation
tokens.

## Dispatch I/O Backend

The async I/O runtime uses public dispatch I/O channel APIs:

1. `dispatch_io_create` or `dispatch_io_create_with_path` creates the channel;
2. `dispatch_io_read` starts an asynchronous read into dispatch-managed data;
3. `dispatch_io_write` starts an asynchronous write from a `dispatch_data_t`;
4. `dispatch_io_close` closes the channel and optionally stops pending work.

The runtime shim owns the Blocks required by these public APIs. Each Block
captures only a runtime operation pointer. The operation pointer roots or owns
all Ciel-visible handles it needs.

For reads:

1. enters the counted runtime callback scope;
2. accumulates each delivered `dispatch_data_t` chunk in the operation object;
3. records the final error or EOF state when `done` is true;
4. sends the preboxed completion message to the actor, if one was registered;
5. leaves the callback scope.

`read_bytes(fd, max_len)` schedules one read for up to `max_len` bytes. The
runtime may set water marks to reduce callback frequency, but correctness must
not depend on receiving a single callback. `finish_read` has simple
message-oriented semantics:

1. if the final error is `0`, return `Ok(Bytes)` containing the bytes
   accumulated for that operation;
2. EOF is `Ok(Bytes)` with length `0`;
3. if the final error is nonzero, return `Err(code_error(error))` and discard
   any partial bytes;
4. repeated `finish_read` calls return an error after the first successful
   finish consumes the result.

The conversion must retain immutable backing storage or copy into runtime-owned
immutable storage before returning.

For writes, `write_bytes` converts `Bytes` into a retained `dispatch_data_t` or
copies it into dispatch-managed data before calling `dispatch_io_write`.
`finish_write` returns `Ok(bytes_written)` only when the final error is `0`;
otherwise it returns `Err(code_error(error))`.

`cancel_read` and `cancel_write` cancel the Ciel operation token, not the whole
dispatch channel. If cancellation wins before final completion, the runtime
releases the registered completion message, suppresses actor notification, and
makes later `finish_*` return `ECANCELED`. The underlying dispatch I/O work may
still run to its final handler; that handler must observe the canceled operation
state, release its runtime roots, and avoid sending a completion message.

`close_async` closes the channel. It uses `dispatch_io_close` with the stop flag
when the caller requests descriptor shutdown, so outstanding dispatch operations
may finish with `ECANCELED` and partial data. The runtime treats channel close
as stronger than operation cancellation: after close, new reads and writes
return an error, pending user notifications are suppressed, and operation roots
are released when their final callbacks arrive.

Each operation object is a small state machine. Completion, cancellation,
finish, actor-close notification failure, and channel close all race through the
same state. Exactly one path may consume the result or notification box; all
other paths become no-ops after releasing any roots they own.

## Platform Policy

The runtime is dispatch-only for this design slice:

1. Darwin uses the system libdispatch.
2. Linux uses swift-corelibs-libdispatch with BlocksRuntime where required by
   the installed libdispatch build.
3. Other targets are unsupported until they have an explicitly validated
   dispatch runtime and test coverage.

There is no `pthread` runtime backend and no source-level backend selection in
the first implementation. If `<dispatch/dispatch.h>`, libdispatch, or the
required BlocksRuntime pieces are missing on a supported target, compilation
fails with a toolchain error.

The Ciel source API must not expose the selected platform details.

## Build Requirements

Dispatch backend builds need:

1. `<dispatch/dispatch.h>`;
2. a C compiler that supports Blocks for the runtime shim using public
   `dispatch_io` APIs;
3. BlocksRuntime on non-Darwin platforms when libdispatch requires it;
4. link flags for libdispatch and BlocksRuntime;
5. the existing BDWGC/libgc flags.

Actor dispatch jobs can use function-pointer APIs and do not need Blocks. The
Blocks requirement is for the runtime shim that calls the public `dispatch_io`
channel APIs. Ciel-generated translation units should not contain Blocks; the
runtime prelude or runtime support object owns that code.

## Implementation Plan

1. Add build checks for dispatch headers, libdispatch, and Linux
   BlocksRuntime requirements.
2. Add `ciel_runtime_enter_callback` and `ciel_runtime_leave_callback`.
3. Replace the actor runtime implementation behind the existing
   `ciel_actor_*` ABI with a dispatch-only backend.
4. Implement serial actor queues, group-based join, self-join detection, and
   GC-visible job storage.
5. Add stress tests for actor FIFO order, many actors, concurrent sends, stop,
   join drain, handler failure, self-join, and GC pressure.
6. Reshape safe `/std/io` around scoped blocking helpers with a private `File`
   value handle backed by a generation-checked slot table, leaving raw
   descriptors to a low-level interop module.
7. Add `/std/async_io` and runtime hooks for `Bytes`, `AsyncFd`,
   `AsyncRead`, `AsyncWrite`, completion notification, finish, and
   cancellation.
8. Add pipe/socket/file tests where many actors wait on async operations while
   the dispatch worker pool stays smaller than the actor count.
9. Add Linux CI with scripted swift-corelibs-libdispatch and BlocksRuntime
   installation before claiming Linux support complete.

## Tests

Actor runtime tests:

1. one actor receives messages in send order;
2. many actors process messages without creating one OS thread per actor;
3. `stop` rejects later sends and drains accepted messages;
4. `join` rejects later sends and waits for accepted messages;
5. handler `Err` makes `join` return an error and rejects later sends;
6. `join` from inside the same actor returns `EDEADLK` instead of deadlocking;
7. repeated GC collections during queued actor work do not lose state, handler,
   or message boxes.

Async I/O tests:

1. async pipe read completion sends one actor message carrying the operation
   token;
2. `finish_read` returns immutable `Bytes` and consumes the result once;
3. async write completion sends one actor message and `finish_write` returns
   the completed byte count;
4. cancel before completion does not send the completion message;
5. actor closed before completion drops the notification without leaking roots;
6. many pending async operations do not require one OS thread per descriptor;
7. `Bytes` implements `Message` without exposing a mutable interior pointer.
8. direct async open constructors do not expose an intermediate `Fd`;
9. raw descriptor adoption is confined to an interop module, requires
   `unsafe {}`, and documents descriptor transfer;
10. read completion handles multiple dispatch chunks, EOF as empty `Bytes`, and
    error completion as `Err` without exposing partial mutable buffers.

Scoped blocking I/O tests:

1. `with_open_read` closes the descriptor after callback success;
2. `with_open_read` closes the descriptor after callback error or `?` return;
3. nested scoped file callbacks can copy data between two files;
4. importers cannot name the private `File` type from `/std/io`;
5. safe `/std/io` does not expose raw descriptor constructors or accessors;
6. stale or escaped `File` tokens fail through slot generation or slot state
   checks and never touch a possibly reused OS descriptor.

Generated C tests:

1. generated C includes dispatch headers on supported targets;
2. generated C links BDWGC, libdispatch, and BlocksRuntime as required by
   the target;
3. a missing dispatch toolchain reports a clear configuration error;
4. no Ciel-generated code includes `private/io_private.h`.

## Acceptance Criteria

The proposal is implemented when:

1. existing actor tests pass with the dispatch runtime on Darwin;
2. the same actor tests pass with swift-corelibs-libdispatch on Linux;
3. Linux dispatch toolchain setup is covered by CI or an equivalent scripted
   validation path;
4. `cargo test -q --test ciel_cases` still passes on supported targets;
5. async I/O tests prove high actor counts do not imply high thread
   counts;
6. GC stress tests prove dispatch callbacks keep actor state, handlers,
   messages, and notification boxes reachable.
