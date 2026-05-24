# Concurrency Safety Proposal

This proposal defines an actor-first concurrency model for Ciel.

Ciel should make ordinary mutable objects local to one actor and use explicit
message conversion for communication between actors. Interface algebra remains
the way to express cross-actor capabilities. The runtime provides actor
mailboxes and synchronized handles; the compiler checks that safe code crosses
actor boundaries only through those capabilities.

## Model

The model has four parts:

1. An actor is an isolated execution domain with private mutable state.
2. Actor code processes one message at a time.
3. Sending a message constructs an independent receiver value through a
   `Message` capability.
4. Shared identity is represented by explicit synchronized handles such as
   actor handles, channels, atomics, and selected standard-library services.

Ordinary pointers and slices are actor-local. They may be used freely inside the
actor that owns the pointed-to data. Cross-actor APIs accept message values or
synchronized handles.

## Actors

An actor handle is a shareable reference to a mailbox:

```rust
struct Actor<M> {
    *void handle;
}
```

Actor state remains encapsulated by the actor runtime. It is initialized when
the actor starts and is updated by the actor's handler.

One possible handler capability:

```rust
interface<H, S, M> Result<S, Error> handle(*H handler, S state, M message);
```

One possible spawn API:

```rust
Result<Actor<M>, Error> spawn_actor<S: Message, M: Message, H: Message + Handler<S, M>>(
    S initial_state,
    H handler
);
```

The exact spelling can change. The required property is that actor state is
handled inside the actor loop and is never exposed as a cross-actor `*S`.
Closure handlers use the closure literal's concrete, unnameable type; an erased
closure signature alone is not enough to prove `Message`.

## Messages

`Message` is an explicit conversion capability:

```rust
interface<T> Result<T, Error> clone_message(*T value);
```

`clone_message` constructs the value that will be owned by the receiver. It may
copy fields, allocate fresh backing storage, serialize and decode, duplicate a
resource handle, intern immutable data, or report an error.

Sending is ordinary interface-constrained code:

```rust
Result<void, Error> send<T: Message>(*Actor<T> actor, *T value);
```

Conceptually:

```rust
Result<void, Error> send<T: Message>(*Actor<T> actor, *T value) {
    T copy = clone_message(value)?;
    enqueue(actor, copy);
    return Ok({});
}
```

The sender keeps its original value. The receiver receives the converted value
with independent mutable identity.

Example:

```rust
Buffer buf = make_buffer();
*Buffer p = &buf;
send(actor, &buf);       // enqueues clone_message(&buf)
append(p, "local only"); // mutates only the sender's buffer
```

## Message Implementations

`Message` is implemented per concrete type. Each implementation is ordinary
Ciel or generated monomorphized code for that type.

Compiler-derived `Message` should be limited to simple value trees:

- integers, floats, `bool`, and `char`;
- fixed-size arrays whose elements are `Message`;
- structs whose fields are all `Message`;
- enums whose payloads are all `Message`.

The standard library should provide hand-written implementations for common
owned containers:

- strings;
- byte buffers;
- vectors or growable arrays;
- `Result<T, E>`;
- standard error values;
- actor and channel handles where handle sharing is intended.

Resource wrappers define their own policy. A file wrapper might duplicate the
file descriptor, reopen by path, or remain actor-local. That policy belongs in
the wrapper's `Message` implementation.

## Shared Handles

Shared mutable identity is represented through synchronized handle types:

```rust
struct Channel<T> { *void handle; }
struct AtomicI64 { *void handle; }
struct Actor<M> { *void handle; }
```

Their safe APIs expose operations:

```rust
Result<void, Error> channel_send<T: Message>(*Channel<T> ch, *T value);
Result<T, Error> channel_recv<T: Message>(*Channel<T> ch);

i64 atomic_load(*AtomicI64 value);
void atomic_store(*AtomicI64 value, i64 next);
```

Handles can implement `Message` when copying the handle is safe and intentional.
The implementation is responsible for synchronization and lifetime rooting.

## Mutexes

Mutexes are a low-level library feature. The first safe API should use value
replacement.

Preferred shape:

```rust
struct Update<T, R> {
    T value;
    R result;
}

interface<F, T, R> Result<Update<T, R>, Error> update_value(*F f, T value);

Result<R, Error> mutex_update<T, F, R>(*Mutex<T> mutex, *F f);
```

`mutex_update` takes the current value, calls `update_value`, stores the
replacement value, unlocks, and returns the result. Implementations may optimize
the storage path internally, but the safe API exposes value replacement rather
than a borrowed interior pointer.

## Interface Algebra

The actor model uses interfaces for capability classification:

```rust
interface<T> Result<T, Error> clone_message(*T value);
interface<T> bool share_handle_marker(*T value);
interface<T> bool thread_local_marker(*T value);
```

Useful aliases:

```rust
interface Message = clone_message;
interface ShareHandle = share_handle_marker;
interface ThreadLocal = thread_local_marker;
```

Examples:

```rust
Result<void, Error> send<T: Message>(*Actor<T> actor, *T value);
Result<void, Error> accept_handle<T: ShareHandle>(T handle);
void local_resource<T: ThreadLocal>(*T value);
```

Negative constraints remain useful for APIs that require a type to stay
actor-local:

```rust
void bind_local<T: !Message>(*T value);
```

## C Interop

C interop is a trusted boundary. C wrappers decide which C-backed values are
messageable, shareable handles, or actor-local resources.

Default wrapper policy:

- C opaque handles start as `ThreadLocal`;
- wrappers implement `Message` by explicitly duplicating, reconnecting, or
  otherwise constructing an independent receiver value;
- wrappers implement `ShareHandle` only when operations are internally
  synchronized or immutable.

## Compiler Work

The compiler needs these additions:

1. Built-in recognition for `Message` and `clone_message` through
   `/std/message`. `ShareHandle` and `ThreadLocal` remain marker declarations
   until their runtime policies are implemented.
2. Optional structural derivation for `Message` on pointer-free value trees.
3. Diagnostics for failed `Message` derivation on raw pointers, slices missing
   an owning container implementation, dynamic interface values missing a
   concrete message path, and C opaque handles missing an explicit wrapper impl.
4. Type checking for actor APIs so `send` and channel send operations require
   `T: Message`.
5. Monomorphized calls to `clone_message` for every concrete message type.
6. Codegen support for actor runtime calls: actor creation, enqueue, dequeue,
   dispatch, shutdown, and pthread worker GC attachment.
7. Escape-analysis integration for diagnostics and storage placement. Actor
   isolation and `Message` capability checks are the safety proof.
8. Coherence checks that prevent conflicting `Message` implementations in the
   closed whole program.

Closure capture is by value for actor handlers. Each closure literal has a
concrete, unnameable type. That concrete closure type implements `Message` only
when each captured field has an explicit message conversion; erased closure
signature types do not implement `Message` by default.

## Standard Library And Runtime Work

The standard library and runtime need these additions:

1. `/std/actor` with `Actor<M>`, `spawn_actor`, `send`, actor lifecycle helpers,
   and error types for closed mailboxes or backpressure. The current runtime
   reports pthread/mailbox failures through `Error::Code`.
2. Runtime mailbox support: allocation, enqueue, dequeue, wakeup, shutdown, and
   GC thread attachment for worker threads. The first implementation uses
   pthreads.
3. `/std/message` with the `Message` interface. Primitive, fixed-array,
   pointer-free struct/enum, actor-handle, Ciel ABI `fn`, and concrete-closure
   support is compiler-derived; strings, byte buffers, results with non-derived
   errors, and standard errors still need library policies.
4. `/std/channel` built on the same message conversion rules.
5. `/std/atomic` for primitive atomics that expose value operations.
6. A revised `/std/sync` where mutexes expose value-update APIs first.
7. Clear wrapper policies for `/std/io` handles: actor-local by default, with
   explicit duplicate/share implementations where valid.
8. Tests covering diagnostics for pointers, slices, and C opaque handles that
   lack an explicit message implementation.

## Soundness Sketch

For safe Ciel code:

1. Actor-local mutable data is reachable only from its actor.
2. Cross-actor communication calls `clone_message`.
3. `clone_message` returns a receiver-owned value or fails.
4. Shared handles expose synchronized operations instead of interior mutable
   pointers.
5. Therefore safe actor APIs preserve the invariant that each ordinary mutable
   object belongs to one actor.

This guarantee depends on correct compiler checks, correct standard-library
implementations, and trusted C wrappers honoring their declared policies.

## Recommended First Slice

1. Add `Message` and hand-written primitive/container implementations.
2. Add derived `Message` for simple pointer-free value trees.
3. Add `Actor<M>`, `spawn_actor`, and `send<T: Message>`.
4. Add the minimal runtime mailbox scheduler.
5. Make pointers, slices, mutexes, and opaque C handles actor-local unless a
   wrapper provides an explicit message conversion.
6. Add channel and atomic handles after actor send is working.
