# 8. Messages and Actor-Owned State

Sending an integer is easy. Sending a struct is the moment where Ciel asks:
"what exactly is safe to copy into another actor?"

Actor messages must satisfy `Message`, which means "there is a known safe way to
construct the receiver-owned copy." Primitive values and synchronized handles
already have that policy. Plain user structs and enums do not automatically get
it, because a field might later contain a pointer, slice, dynamic interface, or
other actor-local value.

For ordinary user data, the recommended path is to put the data in a safe
envelope:

`meta::Repr<Event>`

Read that as "an owned, safe-to-clone representation of `Event`". You create the
envelope with `meta::into_repr(&event)` before sending. The actor opens it with
`meta::from_repr<Event>(envelope)`.

This is deliberately a little explicit. It turns the boundary into a visible
line in the program:

- outside the actor, you have local application data
- at the boundary, you seal it into `meta::Repr<Event>`
- inside the actor, you open the envelope and continue with normal `Event`
  field access

The sending side is just:

```ciel
must(send(&worker, meta::into_repr(&event)));
```

The receiving side opens the envelope:

```ciel
Event event = meta::from_repr<Event>(envelope);
```

That is the user-level habit. The mailbox type mentions the envelope because the
mailbox stores the safe copied form.

```ciel
import /std/lib;
import /std/meta as meta;

// This is ordinary application data.
struct Event {
    i64 amount;
    bool important;
}

Result<i64, Error> handle(i64 total, meta::Repr<Event> envelope) {
    // The actor opens the safe envelope back into an Event.
    Event event = meta::from_repr<Event>(envelope);

    // From here on, the handler uses normal field access.
    i64 next = total + event.amount;
    if (event.important) {
        print("{}", [next])?;
    }
    return Ok(next);
}

i32 main() {
    // The mailbox stores safe envelopes, not borrowed local Event values.
    Actor<meta::Repr<Event>> worker = must(spawn_actor_cloned<i64, meta::Repr<Event>>(0, handle));

    Event first = { amount: 4, important: false };
    Event second = { amount: 6, important: true };

    // `into_repr` seals an Event into its safe envelope before sending.
    must(send(&worker, meta::into_repr(&first)));
    must(send(&worker, meta::into_repr(&second)));
    must(join(&worker));
    return 0;
}
```

The channel or actor type mentions `meta::Repr<Event>` because the mailbox does
not store your local `Event` object. It stores the envelope. That envelope has
owned contents and uses the standard `Message` policy, so the runtime can clone
it safely when crossing the actor boundary.

You do not need to know how the envelope is built to use it. The last chapter
opens the envelope and explains the SOP machinery inside.

The envelope is still checked. `meta::Repr<T>` implements `Message` only when
the owned structural representation is messageable all the way down and does not
carry a forbidden capability such as `ThreadLocal`. If a field contains a raw
pointer, actor-local slice, dynamic interface without an explicit policy, or
opaque C handle, the normal `Message` constraint fails. SOP is a safe
representation path, not a permission bypass.

## Actor-Owned State

Chapter 7 used `spawn_actor_cloned`: the state type `S` must implement
`Message`, and the handler returns the next state value. That is a good fit for
small state such as an integer counter.

Some actor state should stay actor-local. A stream table, a queue, or a
`HashMap` may be perfectly safe for one actor to own, but it should not become a
message that can be copied into another actor. For that shape, use
`spawn_actor_state`:

```ciel
Result<Actor<M>, Error> spawn_actor_state<S, M: Message>(
    Result<S, Error> |(): Message| init,
    Result<void, Error> |(*S, Actor<M>, M): Message| handler
);
```

Read the signature in three parts:

- `S` is the private actor state. It does not need to implement `Message`.
- `init` constructs `S` before the actor accepts messages. The initializer
  itself is `: Message`, so it can capture safe seed values such as channels or
  actor handles, but not a non-message local `HashMap`.
- `handler` receives a writable pointer to actor-owned state, the actor's own
  handle, and one message. It mutates state in place and returns `Ok`.

The complete example is in `tutorial/examples/08_actor_owned_state.ciel`. Its
state owns a `HashMap`, while its mailbox still receives safe envelopes:

```ciel
struct Counts {
    map::HashMap<u32, i64> table;
    Channel<i64> out;
}

Result<Counts, Error> init_counts(Channel<i64> out) {
    return Ok({
        table: map::hash_map_new<u32, i64>()?,
        out: out
    });
}

Result<void, Error> handle(
    *Counts state,
    Actor<meta::Repr<CountMsg>> self,
    meta::Repr<CountMsg> envelope
) {
    CountMsg msg = meta::from_repr<CountMsg>(envelope);
    // Handle Add and Flush messages here.
    return Ok;
}
```

The actor is spawned by giving the runtime an initializer, not by passing an
already-built `Counts` value from the caller:

```ciel
Actor<meta::Repr<CountMsg>> worker =
    must(spawn_actor_state<Counts, meta::Repr<CountMsg>>(
        || init_counts(out),
        handle
    ));
```

This is the important safety line: the `HashMap` is created inside
`init_counts`, then lives behind the actor boundary. Messages still cross the
mailbox as `meta::Repr<CountMsg>`, so message copying remains explicit and
checked.
