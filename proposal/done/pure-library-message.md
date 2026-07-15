# Pure Library Structural Message Proposal

## Historical Status

The structural `Message` model in this document remains part of the language.
`error-downcast` supersedes its policy for erased errors: `Error` is local-only
and no longer implements `Message`, while `Report` is the transferable erased
diagnostic type. `clone_message` still returns `Result<T, Error>` because clone
failure is observed in the source owner. Any statement below that lists an
explicit `clone_message` implementation for `Error` describes the earlier
policy. A successful clone is freely discardable; Message has no user-defined
drop operation, and deterministic cleanup belongs to resource-affine values.
`design.md` is normative.

This proposal removes compiler-derived `Message` as a special case. Structural
message derivation becomes an ordinary standard-library policy over `/std/meta`
owned representations.

The main API change is intentional:

```ciel
Event: Message                 // no automatic structural derivation
meta::Repr<Event>: Message     // structural message wrapper
```

Programs that want pure structural message behavior send, store, and cross actor
boundaries with the owned representation type. The original value type remains
ordinary user data.

## Proposal Order

```text
metaprogramming < pure-library-message

pure-library-message :> capability-erased-closures[message witness source]
pure-library-message || error-box[structural formatting policy]
```

This proposal consumes `/std/meta` structural representation. It owns the
library-level `Message` policy for structural values.

## Core Decision

Do not implement a hidden blanket rule that turns every structural `T` into
`T: Message`.

Instead, the standard library implements `Message` for the structural wrapper
itself:

```ciel
meta::Repr<T>
```

For a concrete type, `meta::Repr<T>` normalizes to ordinary SOP data:

```ciel
struct Packet {
    i64 id;
    bool ok;
}

meta::Repr<Packet>
// meta::HCons<
//     meta::Field<i64>,
//     meta::HCons<meta::Field<bool>, meta::HNil>
// >
```

The normalized representation can implement `Message` through normal generic
impls over `HNil`, `HCons`, `Field`, `CoNil`, `Coproduct`, `Variant`, and
`Payload`.

## Why Not `Derived<T>: Message`

A nominal wrapper looks attractive:

```ciel
struct Derived<T> {
    T value;
}
```

but the pure-library impl needs this condition:

```ciel
impl<T> clone_message(*Derived<T> value)
where meta::Repr<T>: Message
```

Ciel does not currently have constraints over computed type expressions such as
`meta::Repr<T>`. Without that condition, `impl<T> clone_message(*Derived<T>)`
would either accept every `T`, or fail to type-check as a generic impl.

Using `meta::Repr<T>` directly avoids that problem. For concrete `T`, the type
has already normalized to ordinary SOP constructors, so existing generic impl
selection can prove or reject `Message` without a Message-specific compiler
rule.

## Standard Library Surface

`/std/message` keeps the existing interface:

```ciel
export interface<T> Result<T, Error> clone_message(*T value);
export interface Message = clone_message;
```

The standard library supplies explicit impls for primitive values and approved
shared handles:

```ciel
impl clone_message(*i64 value) {
    return Ok(*value);
}

impl<T> clone_message(*Actor<T> value) {
    return Ok(*value);
}
```

It also supplies structural impls for owned SOP nodes:

```ciel
impl clone_message(*meta::HNil value) {
    return Ok({});
}

impl<T: Message> clone_message(*meta::Field<T> value) {
    T copied = clone_message(&value->value)?;
    return Ok({ name: value->name, value: copied });
}

impl<Head: Message, Tail: Message> clone_message(*meta::HCons<Head, Tail> value) {
    Head head = clone_message(&value->head)?;
    Tail tail = clone_message(&value->tail)?;
    return Ok({ head: head, tail: tail });
}

impl<T: Message> clone_message(*meta::Payload<T> value) {
    T copied = clone_message(&value->value)?;
    return Ok({ index: value->index, value: copied });
}

impl<P: Message> clone_message(*meta::Variant<P> value) {
    P payload = clone_message(&value->payload)?;
    return Ok({ name: value->name, payload: payload });
}

impl clone_message(*meta::CoNil value) {
    switch (*value) {
    }
}

impl<Head: Message, Tail: Message> clone_message(*meta::Coproduct<Head, Tail> value) {
    switch (*value) {
        case meta::This(head):
            Head copied = clone_message(&head)?;
            return Ok(meta::This(copied));
        case meta::Next(tail):
            Tail copied = clone_message(&tail)?;
            return Ok(meta::Next(copied));
    }
}
```

If any field, payload, or capture has no `Message` impl, the normal generic
constraint fails at the SOP position.

## Basic Use

User code explicitly crosses the actor/channel boundary with an owned
representation:

```ciel
import /std/channel;
import /std/meta as meta;

struct Event {
    i64 value;
    bool ok;
}

type EventMessage = meta::Repr<Event>;

Result<void, Error> send_event(*Channel<EventMessage> channel, Event event) {
    EventMessage message = meta::into_repr(event);
    channel_send(channel, message)?;
    return Ok;
}

Result<Event, Error> recv_event(*Channel<EventMessage> channel) {
    EventMessage message = channel_recv(channel)?;
    return Ok(meta::from_repr<Event>(message));
}
```

`Event` itself does not implement `Message`. `EventMessage` does.

## Actor Use

Actors can use representation types as their state and message types:

```ciel
type StateMessage = meta::Repr<State>;
type CommandMessage = meta::Repr<Command>;

Result<StateMessage, Error> run(StateMessage state_message, CommandMessage command_message) {
    State state = meta::from_repr<State>(state_message);
    Command command = meta::from_repr<Command>(command_message);

    State next = handle(state, command)?;
    return Ok(meta::into_repr(next));
}

Actor<CommandMessage> actor = spawn_actor_cloned<StateMessage, CommandMessage>(
    meta::into_repr(initial_state),
    run
)?;
```

The runtime remains typed by `Message`; the chosen message types are structural
owned representations.

## Convenience Helpers

Strict pure-library code can provide concrete adapters:

```ciel
Result<void, Error> send_packet(*Channel<meta::Repr<Packet>> channel, Packet packet) {
    return channel_send(channel, meta::into_repr(packet));
}
```

Generic convenience helpers need a general type-system feature:

```ciel
Result<void, Error> send_structural<T>(
    *Channel<meta::Repr<T>> channel,
    T value,
)
where meta::Repr<T>: Message
```

That feature is not a `Message` compiler hole. It is ordinary support for
constraints over computed type expressions. The strict version of this proposal
does not require it; users can write concrete adapters or use `meta::Repr<T>`
directly.

## Closure Captures

Concrete closure values can be projected:

```ciel
_ handler = |i64 value| value + base;
_ message = meta::into_repr(handler);
```

This works only while the concrete closure type is still visible. Once a value
is erased to a signature type such as `i64 |(i64)|`, its capture structure is no
longer available. Capability-erased closure witnesses remain a separate
proposal.

## Failure Model

Failure is compile-time capability failure, not a runtime `Err`.

```ciel
struct Bad {
    *i64 ptr;
}

type BadMessage = meta::Repr<Bad>;
Channel<BadMessage> channel = make_channel<BadMessage>(); // error
```

`meta::Repr<Bad>` normalizes to a product containing `meta::Field<*i64>`.
`meta::Field<*i64>: Message` requires `*i64: Message`, which is absent.

The diagnostic should mention the normalized SOP path and, when available, the
original structural path:

```text
Message derivation blocked at field `ptr` (`*i64`): raw pointer.
```

## Migration

1. Keep explicit `clone_message` impls for primitives, `Report`, function
   pointers, actors, channels, mutexes, and other approved handles. The later
   `error-downcast` design removes this implementation from `Error`.
2. Add standard-library `Message` impls for owned SOP nodes.
3. Move channel, mutex, and actor examples to `meta::Repr<T>` message types when
   they want structural behavior.
4. Remove compiler-derived `T: Message` for pointer-free structs, enums, and
   concrete closures.
5. Keep explicit user-defined `impl clone_message(*T)` as the way to make the
   original `T` itself a message type.

## Consequences

This is a real API change:

```ciel
send(&actor, event);                       // old structural auto-derive style
send(&actor, meta::into_repr(event));       // pure library style
```

The benefit is a clean separation:

- `T` is the application type.
- `meta::Repr<T>` is the structural message type.
- `T: Message` means the type author or standard library explicitly chose that
  policy.
- no compiler path special-cases `Message`.

The cost is ergonomics. Users must either expose representation types at
cross-actor boundaries or write small concrete adapters.

## Acceptance Criteria

1. `meta::Repr<Struct>` and `meta::Repr<Enum>` implement `Message` when all
   owned leaves implement `Message`.
2. A raw pointer, actor-local slice, dynamic interface without an explicit
   policy, or opaque C handle inside a representation rejects at compile time.
3. Channels and actors work with `meta::Repr<T>` message types.
4. The compiler no longer treats `clone_message` as a structural derivation
   hook for the original `T`.
5. Existing explicit `impl clone_message(*T)` code continues to work.
