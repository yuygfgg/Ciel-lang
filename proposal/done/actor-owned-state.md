# Actor-Owned State Proposal

This proposal changes actor state from a `Message`-cloned value into an
actor-owned value. The immediate motivation is the intranet tunnel demo:
server and agent state naturally contain actor-local resources such as
`HashMap<u32, StreamState>`, async socket handles, frame readers, and queues.
Those resources must not become `Message`, but the clone-state actor API
requires `S: Message` for the actor state type.

The previous workaround was a shallow pointer state that implemented
`Message` manually. That is an unsafe escape hatch, not the intended model.
The actor runtime should own state directly.

## Proposal Order

```text
dispatch-actor-io-runtime <= actor-owned-state[actor runtime state storage]
pure-library-message || actor-owned-state[message payload policy]
monomorphized-c-callbacks || actor-owned-state[runtime callback ABI]
```

This proposal deliberately avoids a consumed-local or affine type system.
Non-`Message` actor state is constructed inside a state initializer and is
never accepted as an already-existing caller local. `pure-library-message`
still owns message payload cloning; this proposal does not make actor-local
resources messageable. `monomorphized-c-callbacks` may later move actor
lowering out of compiler builtins, so the runtime ABI changes here must remain
compatible with that direction.

## Problem

The clone-state API treats actor state and actor messages as if they had the
same transfer requirements:

```rust
export Result<Actor<M>, Error> spawn_actor_cloned<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler
);
```

That conflates two different concepts:

1. `M` crosses into the actor mailbox for every send. It must be cloned through
   `Message`.
2. `S` is private actor state. It should be owned by the actor after spawn and
   should not need to be cloned or sent.

Removing the `S: Message` bound from this shape would be unsound without
ownership tracking: the caller could keep using the same non-`Message`
resource after the actor starts. Instead, the safe owned-state API avoids
transferring an existing `S` from the caller at all.

## Goals

1. Allow actor state to contain non-`Message` actor-local resources.
2. Keep actor messages `M: Message`.
3. Remove the need for application code to wrap actor state in raw pointers.
4. Avoid a full language-wide move-semantics rollout.
5. Give handlers access to the actor's own handle without storing fake
   placeholders in state.
6. Preserve serial actor processing: one accepted message mutates the actor
   state at a time.

## Non-Goals

1. Making `HashMap`, async socket handles, crypto contexts, or raw pointer
   shells generally `Message`.
2. Adding deterministic destruction or ownership-based `Drop`.
3. Supporting arbitrary field moves or a complete affine type system.
4. Allowing safe code to share actor-local mutable state between actors.
5. Changing mailbox ordering, `send`, `stop`, or `join` semantics.
6. Providing a safe API that adopts an already-existing non-`Message` local as
   actor state.

## Proposed Model

Actor state is owned by the actor runtime. After a successful spawn, safe code
outside the actor cannot access that state directly. The state is constructed
by an initializer closure, and the handler receives a mutable pointer to the
actor-owned state plus the actor's own handle:

```rust
export Result<Actor<M>, Error> spawn_actor_state<S, M: Message>(
    Result<S, Error> |(): Message| init,
    Result<void, Error> |(*S, Actor<M>, M): Message| handler
);
```

The initializer must be `Message`, so it can capture only messageable seed
values such as actor handles, channels, socket addresses, and other safe
shareable handles. Non-`Message` resources such as `HashMap`, async streams,
frame readers, and queues are constructed inside the initializer and returned
as `S`. The caller never holds an `S` local, so no consumed-local tracking is
needed.

The handler mutates `*S` in place and returns `Result<void, Error>`. The
`Actor<M>` parameter is the actor's self handle for the current message. Code
that needs to schedule async completions passes this handle to the async
notification helpers instead of storing a fake self handle in state.

The existing value-oriented API remains available under an explicit name:

```rust
export Result<Actor<M>, Error> spawn_actor_cloned<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler
);
```

`spawn_actor_cloned` is useful for small messageable state and for existing
tests. `spawn_actor_state` is the preferred API for actor-local resources.

## Lowering

`spawn_actor_state` lowering differs from `spawn_actor_cloned` in three ways:

1. It calls the initializer synchronously and boxes the returned `S` directly
   into actor-owned storage.
2. It does not call `clone_message` for `S`.
3. The generated dispatch function receives the raw actor pointer, rebuilds
   `Actor<M>` as the handler's self handle, and passes `S *` to the handler.

The generated dispatch is conceptually:

```c
static void dispatch(CielActor *actor_raw, void *state_raw, void *handler_raw,
                     void *message_raw, int32_t *failed) {
    S *state = (S *)state_raw;
    Handler *handler = (Handler *)handler_raw;
    M *message = (M *)message_raw;
    Actor<M> self = { .handle = (void *)actor_raw };
    Result<void, Error> result = handler(state, self, *message);
    if (result is Err) {
        *failed = 1;
    }
}
```

`send` still clones `M` before enqueueing. The handler closure still needs a
callback-safe capability, currently expressed through `Message` on the closure
value.

## Safety Invariants

The safe API guarantees:

1. Safe caller code never transfers an existing non-`Message` `S` local into an
   actor.
2. Only the actor runtime owns the state after the initializer succeeds.
3. Only one handler invocation mutates the state at a time.
4. Messages are still cloned through `Message`.
5. State does not become messageable merely because it is actor-owned.
6. Raw pointer escape hatches remain unsafe and do not establish general actor
   state transfer rules.

Unsafe standard-library helpers may allocate actor-owned boxes or internal
slots, but application code should not need to implement `Message` for a raw
state pointer.

## Migration Plan

1. Rename the old value-state API to `spawn_actor_cloned`.
2. Add `spawn_actor_state` with initializer-built state and in-place handlers.
3. Migrate examples that need non-`Message` state, starting with the intranet
   tunnel server and agent.
4. Add negative fixtures proving `HashMap` is still not `Message` and cannot
   be captured by the initializer.
5. Add positive fixtures proving actor state can contain `HashMap`.
6. Keep `spawn_actor_cloned` for existing messageable-state examples and tests.

## Tunnel Demo Cleanup

After this proposal lands, the intranet tunnel demo should:

1. Store `ServerCore` and `AgentCore` directly as actor state.
2. Remove `ServerState` and `AgentState` shallow pointer wrappers.
3. Remove their unsafe `clone_message` impls.
4. Pass the handler's self handle to async notification helpers instead of
   storing fake self placeholders in state.
5. Keep stream tables as `/std/map::HashMap<u32, ServerStream>` and
   `/std/map::HashMap<u32, AgentStream>`.

## Test Plan

1. A fixture where actor state contains `HashMap<u32, i64>` compiles and runs.
2. A fixture that sends `HashMap<u32, i64>` as a message still fails.
3. A fixture where the initializer captures an external `HashMap` fails because
   the initializer closure must be `Message`.
4. A fixture verifies handler state mutation persists across messages.
5. The intranet tunnel loopback integration tests pass without state-pointer
   `Message` impls.

## Open Questions

1. Whether a future unsafe or affine API should adopt an already-existing
   non-`Message` local as actor state.
2. Whether `spawn_actor_cloned` should remain permanently or move to a
   compatibility module after owned-state actors are the normal teaching path.
