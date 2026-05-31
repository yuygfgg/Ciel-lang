# 8. Sending Complex Data Across Actors

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
