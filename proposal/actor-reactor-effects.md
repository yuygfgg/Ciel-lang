# Actor Reactor Effects Proposal

This proposal adds a reactor-style concurrency layer for Ciel actors. It is a
replacement candidate for using `/std/async` flow chains as the main async
programming model.

The core idea is simple: actor code handles explicit events and schedules
typed effects. There is no suspended Ciel stack, no `async` function color, and
no hidden borrow that lives across an asynchronous boundary.

## Proposal Order

```text
dispatch-actor-io-runtime <= actor-reactor-effects[async operation backend]
actor-owned-state <= actor-reactor-effects[safe actor-owned state]
pure-library-message <= actor-reactor-effects[event payload policy]
monomorphized-c-callbacks || actor-reactor-effects[runtime callback ABI]
```

`dispatch-actor-io-runtime` provides the current actor and async operation
backend. `actor-owned-state` is the preferred safe state model for reactors.
Without it, a reactor can still reduce async I/O boilerplate, but examples with
non-`Message` state still need the same application-level unsafe actor-state
wrapper they need today. `pure-library-message` owns event payload cloning:
reactor events are ordinary messages, while actor state remains actor-local.

## Problem

The current high-level async API, `/std/async`, models a one-shot flow:

```rust
AsyncTask<S, Out>
then(task, continuation)
start(runner, task, finish)
```

That is useful for straight-line workflows such as:

1. read a file;
2. wait for a timer;
3. connect to a socket;
4. write the bytes.

It is a poor fit for long-lived concurrent services. The intranet tunnel demo
needs:

1. repeated accepts on two listeners;
2. one control connection and many logical streams;
3. many reads and writes pending at the same time;
4. per-stream cancellation and stale completion handling;
5. backpressure and write queues;
6. heartbeats, reconnect timers, and shutdown policy.

Those are not linear flows. They are an event loop with typed state transitions.
Trying to express them through nested task chains recreates callback-heavy code
with worse generic inference and weaker cancellation structure.

Adding source-level `async` / `await` is also not the right first answer for
Ciel. Without borrow checking, suspending a stack frame across I/O either
forces excessive cloning, permits hidden shared mutable state, or requires a
large stackful coroutine runtime. Ciel actors already have the right outer
shape: serialized event handling. The missing piece is a better event/effect
surface.

## Goals

1. Make long-lived actor-based async services easier to write.
2. Support multiple concurrent pending operations per actor.
3. Keep asynchronous boundaries explicit as events.
4. Avoid `async` function coloring.
5. Avoid suspended Ciel stack frames.
6. Support cancellation, grouping, timeout, and stale completion handling.
7. Make `/std/async` flow chains optional sugar instead of the primary
   concurrency abstraction.

## Non-Goals

1. Adding `async` / `await` syntax.
2. Adding a borrow checker or full lifetime system.
3. Making actor-local state `Message`.
4. Hiding all state machines. Reactor code is still event-driven; it should
   make the state machine smaller and regular, not pretend it does not exist.
5. Replacing actors, channels, `send`, `stop`, or `join`.

## Proposed Model

Add a standard-library module, tentatively `/std/reactor`.

A reactor owns actor state `S` and handles events `E`. `E` must be `Message`
because events are sent through the actor mailbox. `S` is actor-owned state and
should not need `Message` once `actor-owned-state` lands.

The user writes one update function:

```rust
Result<void, Error> |(*S, *reactor::Context<E>, E): Message|
```

The update function:

1. inspects one event;
2. mutates actor-owned state;
3. schedules zero or more effects through `Context<E>`;
4. returns.

An effect is an asynchronous operation whose completion becomes a future event
for the same actor.

## API Sketch

The exact names are open. The important shape is event construction at the
operation boundary:

```rust
export struct Reactor<E> {
    Actor<meta::Repr<E>> actor_handle;
}

export struct Context<E> {
    Actor<meta::Repr<E>> actor_handle;
}

export struct OpKey {
    u64 id;
}

export Result<Reactor<E>, Error> spawn_reactor<S, E: Message>(
    Result<S, Error> |(Reactor<E>): Message| init,
    Result<void, Error> |(*S, *Context<E>, E): Message| update
);

export Result<void, Error> send<E: Message>(*const Reactor<E> reactor, E event);
export Result<void, Error> stop<E: Message>(*const Reactor<E> reactor);
export Result<void, Error> join<E: Message>(*const Reactor<E> reactor);
```

Async effects attach a result-to-event function:

```rust
export Result<OpKey, Error> accept<E: Message>(
    *Context<E> cx,
    async_net::AsyncTcpListener listener,
    Result<E, Error> |(Result<async_net::AsyncTcpStream, Error>): Message| event
);

export Result<OpKey, Error> connect<E: Message>(
    *Context<E> cx,
    net::SocketAddr addr,
    Result<E, Error> |(Result<async_net::AsyncTcpStream, Error>): Message| event
);

export Result<OpKey, Error> read<E: Message>(
    *Context<E> cx,
    async_net::AsyncTcpStream stream,
    usize max_len,
    Result<E, Error> |(Result<async_net::Bytes, Error>): Message| event
);

export Result<OpKey, Error> write<E: Message>(
    *Context<E> cx,
    async_net::AsyncTcpStream stream,
    async_net::Bytes bytes,
    Result<E, Error> |(Result<usize, Error>): Message| event
);

export Result<OpKey, Error> sleep_ms<E: Message>(
    *Context<E> cx,
    u64 ms,
    Result<E, Error> |(Result<void, Error>): Message| event
);

export Result<void, Error> cancel<E: Message>(*Context<E> cx, OpKey key);
export Result<void, Error> cancel_group<E: Message>(*Context<E> cx, u64 group);
```

The `event` closure captures only the small messageable data needed to route
the completion, such as a stream id. It does not capture actor state. Actor
state is available only when the resulting event is handled.

## Example Shape

The tunnel server can become an event reducer instead of a hand-rolled async
notification state machine:

```rust
enum ServerEvent {
    Boot,
    ControlAccepted(Result<anet::AsyncTcpStream, Error>),
    PublicAccepted(Result<anet::AsyncTcpStream, Error>),
    ControlRead(Result<anet::Bytes, Error>),
    ControlWritten(Result<usize, Error>),
    ClientRead(u32, Result<anet::Bytes, Error>),
    ClientWritten(u32, Result<usize, Error>),
    Timer(u64),
}

Result<void, Error> update(
    *ServerCore state,
    *reactor::Context<ServerEvent> cx,
    ServerEvent event
) {
    switch (event) {
        case Boot:
            cx.accept(state->control_listener, |result| ControlAccepted(result))?;
            cx.accept(state->public_listener, |result| PublicAccepted(result))?;
            return Ok;

        case PublicAccepted(Ok(client)):
            u32 id = add_client_stream(state, client)?;
            cx.read(client, 16384, |result| ClientRead(id, result))?;
            cx.accept(state->public_listener, |result| PublicAccepted(result))?;
            return Ok;

        case ClientRead(id, Ok(bytes)):
            enqueue_data_frame(state, id, bytes)?;
            flush_control(state, cx)?;
            schedule_next_client_read(state, cx, id)?;
            return Ok;
    }
}
```

This is still an explicit state machine, but the async operation lifecycle is
regular and centralized. Completion routing is visible in the event type, not
buried in nested closures.

## Operation Keys And Groups

Every scheduled effect returns an `OpKey`. Reactor state may store keys to
cancel stale work:

1. cancel a pending read when a stream closes;
2. cancel a reconnect timer after a successful connection;
3. cancel all per-stream operations through a group id;
4. ignore stale completions that arrive after cancellation if the backend could
   not stop the operation in time.

The context should also support optional group assignment:

```rust
OpKey key = cx.in_group(stream_id).read(stream, 16384, |r| ClientRead(stream_id, r))?;
```

The exact fluent API is not important. The contract is that grouping is
standard, because long-lived services need bulk cancellation.

## Error Handling

Effect completion should deliver `Result<Out, Error>` to the event constructor.
The update function decides whether an error is fatal, per-stream, retryable,
or ignorable as stale.

Errors produced while scheduling an effect return immediately from the update
function. If an update returns `Err`, the reactor follows actor failure policy:
mark the actor failed and close it to new work.

## Relationship To `/std/async`

`/std/async` flow chains should become a convenience layer for one-shot
workflows. They can be implemented on top of reactor effects:

1. create a private event type for each flow runner;
2. schedule each task as an effect;
3. feed completion events into the next task;
4. call the final continuation.

The public tutorial path for general concurrency should move to reactors. Flow
chains remain useful for examples that are truly linear.

## Implementation Plan

1. Define `Context<E>`, `OpKey`, and effect scheduling wrappers for
   `/std/async_net` and `/std/async_time`.
2. Add `spawn_reactor` on top of `actor-owned-state` once actor-owned state is
   available.
3. Before actor-owned state lands, allow examples to use the same necessary
   application-level unsafe state wrapper they use with raw actors; do not hide
   that wrapper in reactor as a fake safe abstraction.
4. Migrate the intranet tunnel demo to reactor events.
5. Re-express the current `/std/async` flow API as a thin reactor helper or
   leave it as a small compatibility surface.
6. Update the tutorial so reactors are the primary long-lived async service
   model, and flows are introduced later as linear convenience.

## Safety Invariants

1. Reactor events are `Message`.
2. Reactor state is actor-local and is not sent as an event.
3. Event constructor closures must be `Message` and should capture only routing
   data, not actor state.
4. Async operation handles remain owned by the standard-library async modules.
5. Cancellation may be best-effort; stale completion handling is part of the
   reactor contract.

## Test Plan

1. A reactor fixture accepts one TCP connection and reads one payload.
2. A reactor fixture schedules two reads and verifies completions route by id.
3. A reactor fixture cancels an operation and ignores or rejects the stale
   completion according to documented policy.
4. A timer fixture verifies `sleep_ms` produces an event without blocking the
   actor.
5. A negative fixture rejects non-`Message` reactor events.
6. The intranet tunnel integration tests pass after migrating to reactor
   events.

## Open Questions

1. Whether `Context<E>` should expose operation groups through fluent methods,
   explicit parameters, or separate helper functions.
2. Whether stale completions should always be delivered as events or filtered
   inside the reactor runtime when an `OpKey` is known cancelled.
3. Whether reactor shutdown should cancel all pending operations automatically
   or let update code perform explicit shutdown effects first.
4. How much of `/std/async` should remain public after reactor becomes the main
   service model.
