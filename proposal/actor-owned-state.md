# Actor-Owned State Proposal

This proposal changes actor state from a `Message`-cloned value into an
actor-owned value. The immediate motivation is the intranet tunnel demo:
server and agent state naturally contain actor-local resources such as
`HashMap<u32, StreamState>`, async socket handles, frame readers, and queues.
Those resources must not become `Message`, but today `spawn_actor` requires
`S: Message` for the actor state type.

The current workaround is a shallow pointer state that implements `Message`
manually. That is an unsafe escape hatch, not the intended model. The actor
runtime should own state directly.

## Proposal Order

```text
binding-mutability <= actor-owned-state[consumed state locals]
dispatch-actor-io-runtime <= actor-owned-state[actor runtime state storage]
pure-library-message || actor-owned-state[message payload policy]
monomorphized-c-callbacks || actor-owned-state[runtime callback ABI]
```

`binding-mutability` gives the compiler a place to track consumed local
bindings. `dispatch-actor-io-runtime` is the current runtime baseline.
`pure-library-message` still owns message payload cloning; this proposal does
not make actor-local resources messageable. `monomorphized-c-callbacks` may
later move the actor lowering out of compiler builtins, so the runtime ABI
changes here must remain compatible with that direction.

## Problem

The source API currently treats actor state and actor messages as if they had
the same transfer requirements:

```rust
export Result<Actor<M>, Error> spawn_actor<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler
);
```

That conflates two different concepts:

1. `M` crosses into the actor mailbox for every send. It must be cloned through
   `Message`.
2. `S` is private actor state. It should be owned by the actor after spawn and
   should not need to be cloned or sent.

Requiring `S: Message` blocks useful actor-local state. Removing the bound as a
signature-only change would be unsound: the current lowering clones state into
the runtime, and without ownership tracking the caller could keep using the
same non-`Message` resource after the actor starts.

## Goals

1. Allow actor state to contain non-`Message` actor-local resources.
2. Keep actor messages `M: Message`.
3. Remove the need for application code to wrap actor state in raw pointers.
4. Avoid a full language-wide move-semantics rollout as the first step.
5. Support state initialization patterns that need the actor's own handle.
6. Preserve serial actor processing: one accepted message mutates the actor
   state at a time.

## Non-Goals

1. Making `HashMap`, async socket handles, crypto contexts, or raw pointer
   shells generally `Message`.
2. Adding deterministic destruction or ownership-based `Drop`.
3. Supporting arbitrary field moves or a complete affine type system in the
   first implementation.
4. Allowing safe code to share actor-local mutable state between actors.
5. Changing mailbox ordering, `send`, `stop`, or `join` semantics.

## Proposed Model

Actor state is owned by the actor runtime. After a successful spawn, safe code
outside the actor cannot access that state directly. The handler receives a
mutable pointer to the actor-owned state and mutates it in place:

```rust
export Result<Actor<M>, Error> spawn_actor_owned<S, M: Message>(
    S initial_state,
    Result<void, Error> |(*S, M): Message| handler
);
```

This API is intentionally named `spawn_actor_owned` while the old
clone-state `spawn_actor` still exists. Once the migration is complete, the
owned-state semantics should become the default `spawn_actor` semantics, and
the clone-state form should either be removed or renamed to an explicit helper
such as `spawn_actor_cloned`.

For state that needs the actor handle during construction, add an initializer
form:

```rust
export Result<Actor<M>, Error> spawn_actor_init<S, M: Message>(
    Result<S, Error> |(Actor<M>): Message| init,
    Result<void, Error> |(*S, M): Message| handler
);
```

The runtime creates the actor handle, runs `init` before accepting messages,
stores the returned state in actor-owned storage, and then makes the actor
available to `send`. This removes the need for fake `Actor<M>` placeholders in
state structs.

The first implementation may expose only `spawn_actor_init`. That avoids
moving arbitrary non-`Message` values from caller locals: non-`Message`
resources are constructed inside the initializer and immediately become actor
owned. A later ergonomic phase can add `spawn_actor_owned(initial_state, ...)`
with narrow consumed-local checking.

## Narrow Consumed-State Rule

If `spawn_actor_owned(initial_state, handler)` is implemented, the compiler
must treat `initial_state` as consumed:

```rust
HashMap<u32, Stream> table = hash_map_new<u32, Stream>()?;
Actor<Msg> actor = spawn_actor_owned(table, handler)?;
hash_map_len(&table); // compile error: table was consumed
```

This is not a complete move system. The first version only needs to track local
bindings consumed by a small set of compiler-recognized operations. It should:

1. Mark a consumed local as unavailable for later reads, writes, borrows, or
   sends.
2. Reject consuming a value through a shared reference or raw pointer.
3. Allow consuming temporary expressions and aggregate literals.
4. Avoid partial field moves in the first version.
5. Keep ordinary copyable `Message` values usable through explicit
   `clone_message` at the call site when the caller wants a second copy.

This narrow rule is enough to make actor state transfer safe without committing
to a full language-wide ownership model immediately.

## Lowering

The current lowering must change in two important ways:

1. It must not call `clone_message` for the actor state.
2. It should pass a pointer to actor-owned state into the generated dispatch
   function.

The generated dispatch should become conceptually:

```c
static void dispatch(void *state_raw, void *handler_raw, void *message_raw,
                     int32_t *failed) {
    S *state = (S *)state_raw;
    Handler *handler = (Handler *)handler_raw;
    M *message = (M *)message_raw;
    Result<void, Error> result = handler(state, *message);
    if (result is Err) {
        *failed = 1;
    }
}
```

The runtime can continue to store `void *state`; the main changes are in
type checking and code generation. `send` still clones `M` before enqueueing.
The handler closure still needs a callback-safe capability, currently expressed
through `Message` on the closure value.

## Safety Invariants

The safe API must guarantee:

1. Only the actor runtime owns the state after spawn.
2. Only one handler invocation mutates the state at a time.
3. Messages are still cloned through `Message`.
4. State does not become messageable merely because it is actor-owned.
5. Raw pointer escape hatches remain unsafe and do not establish general actor
   state transfer rules.

Unsafe standard-library helpers may allocate actor-owned boxes or internal
slots, but application code should not need to implement `Message` for a raw
state pointer.

## Migration Plan

1. Add `spawn_actor_init` with in-place state handlers.
2. Migrate examples that need non-`Message` state, starting with the intranet
   tunnel server and agent.
3. Add negative fixtures proving `HashMap` is still not `Message`.
4. Add positive fixtures proving actor state can contain `HashMap`.
5. Add `spawn_actor_owned` only after narrow consumed-local checking exists.
6. Deprecate or rename the old clone-state `spawn_actor`.
7. Make owned-state semantics the default `spawn_actor` behavior.

## Tunnel Demo Cleanup

After this proposal lands, the intranet tunnel demo should:

1. Store `ServerCore` and `AgentCore` directly as actor state.
2. Remove `ServerState` and `AgentState` shallow pointer wrappers.
3. Remove their unsafe `clone_message` impls.
4. Initialize self actor handles through `spawn_actor_init` instead of fake
   placeholders.
5. Keep stream tables as `/std/map::HashMap<u32, ServerStream>` and
   `/std/map::HashMap<u32, AgentStream>`.

## Test Plan

1. A fixture where actor state contains `HashMap<u32, i64>` compiles and runs.
2. A fixture that sends `HashMap<u32, i64>` as a message still fails.
3. A fixture that uses a consumed state local after `spawn_actor_owned` fails.
4. A fixture that explicitly clones a `Message` state before spawn can keep the
   original value.
5. A fixture verifies handler state mutation persists across messages.
6. The intranet tunnel loopback integration tests pass without state-pointer
   `Message` impls.

## Open Questions

1. Whether `spawn_actor_init` should replace `spawn_actor_owned` entirely for
   the first implementation.
2. Whether the old clone-state API should remain as `spawn_actor_cloned` or be
   removed after migration.
3. Whether in-place state handlers should be the only owned-state handler
   shape, or whether `Result<S, Error> |(S, M)|` should remain available for
   purely value-oriented state.
4. Whether actor initialization errors should leave behind a stopped actor
   handle internally or fail before exposing the handle to user code.
