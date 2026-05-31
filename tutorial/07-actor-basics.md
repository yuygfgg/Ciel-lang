# 7. Actor Basics

Think of an actor as one worker with one desk.

The worker owns private state on the desk. Other code cannot reach in and change
that state directly. Other code can only put messages in the worker's mailbox.
The worker takes one message at a time, updates the private state, then moves to
the next message.

In Ciel, `spawn_actor_cloned<S, M>(state, handler)` starts an actor with state type `S`
and message type `M`. The handler receives the current state and one message,
then returns the next state.

The handler is also a value crossing into the actor runtime. Its real parameter
type is a retained closure shape:

```ciel
Result<S, Error> |(S, M): Message|
```

The `: Message` part matters for the same reason it mattered in the closure
chapter: once a function or closure is stored behind an erased callable shape,
the runtime still needs proof that the handler value can be cloned and kept by
the actor safely. A top-level function like `handle` can be converted to that
retained handler type automatically.

For a counter worker, the state is the current total and each message is an
amount to add:

```ciel
Result<i64, Error> handle(i64 total, i64 amount) {
    i64 next = total + amount;
    return Ok(next);
}
```

The handle type only mentions the message type: `Actor<i64>`. The private state
type is known to the runtime, but callers only know what messages the actor
accepts.

```ciel
import /std/lib;

Result<i64, Error> handle(i64 total, i64 amount) {
    // The actor owns `total`. Each message carries one `amount`.
    i64 next = total + amount;

    // Printing here lets the example show the actor processing messages.
    print("{} ", [next])?;

    // Returning `next` replaces the actor's private state.
    return Ok(next);
}

i32 main() {
    // This actor's state is i64 and its message type is also i64.
    Actor<i64> worker = must(spawn_actor_cloned<i64, i64>(0, handle));

    // `send` puts messages into the actor's mailbox.
    must(send(&worker, 2));
    must(send(&worker, 5));

    // `join` waits until queued messages are processed.
    must(join(&worker));
    return 0;
}
```

`send` queues work and returns after the message is accepted. `join` waits for
already queued work to finish and closes the actor to later sends.
