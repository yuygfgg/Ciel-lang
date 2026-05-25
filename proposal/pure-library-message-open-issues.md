# Pure Library Message Open Issues

This note records the follow-up design issues after landing the strict
`pure-library-message` direction.

The important correction is that strict pure-library structural `Message` does
not require const generics or callable-kind impls. The strict model is:

```rust
Event: Message               // no automatic structural derivation
meta::Repr<Event>: Message   // ordinary SOP library policy
```

User structs and enums can be structurally messageable through
`meta::Repr<T>`. The original nominal type implements `Message` only when an
explicit `clone_message(*T)` policy exists.

## Settled Baseline

1. `/std/meta` owns structural representation.
2. `/std/message` owns structural `Message` policy for owned SOP nodes:
   `HNil`, `HCons`, `Field`, `CoNil`, `Coproduct`, `Variant`, and `Payload`.
3. Actor and channel APIs continue to require `T: Message`.
4. Structural cross-actor APIs use representation boundary types such as
   `meta::Repr<Event>`.
5. Generic convenience wrappers such as `send_structural<T>` still need
   computed type constraints like `meta::Repr<T>: Message`; the strict model
   does not require those helpers.

## Open Issue 1: Concrete Closure Capability Proof

Capability-erased closure types solve the witness-retention problem after
erasure:

```rust
type Handler = i64 |(i64): Message|;
```

They do not by themselves define where the initial `Message` proof for a
concrete capturing closure comes from. A conversion like this still needs a
source proof:

```rust
Handler handler = |i64 value| value + base;
```

Possible routes:

1. Keep a narrow compiler rule for concrete closure capture environments. The
   compiler checks that every captured value implements `Message`, but it does
   not structurally derive `Message` for user structs or enums.
2. Define retained-closure conversion in terms of `meta::Repr<ConcreteClosure>`:
   the conversion succeeds only when the closure's capture representation
   implements `Message`, and the generated erased value stores the resulting
   clone witness.
3. Avoid capturing handlers at actor boundaries and require named functions
   plus explicit state/message values. This is the purest model, but it rejects
   the intended `spawn_actor(..., |state, msg| ...)` ergonomics.

Recommended next step: implement the `capability-erased-closures` first slice
for `Message`, using route 1 or route 2 as the proof source. Route 2 is cleaner
once `/std/meta` closure representation is stable, but route 1 is a pragmatic
bridge and matches how Rust and Swift handle anonymous closure environments.

## Open Issue 2: Fixed-Size Arrays

Current Ciel has type generics but no const generics, so the standard library
cannot write one blanket impl for all fixed-size arrays:

```rust
impl<const N: usize, T: Message> clone_message(*[N]T value) {
    ...
}
```

This is the main ordinary value-type gap left by the strict model.

Possible routes:

1. Add const generics and implement `[N]T: Message` in `/std/message`.
2. Extend `/std/meta` so `meta::Repr<[N]T>` expands to an SOP product of array
   elements. This keeps structural representation messageable without needing
   direct `[N]T: Message`.
3. Provide limited concrete impls for selected lengths, such as `[4]T` or
   `[16]T`. This is only a stopgap.

Recommended next step: prefer route 2 before full const generics. It keeps
`meta::Repr<StructWithArray>` useful and stays within the representation-first
model. Const generics can later recover direct `[N]T: Message` ergonomics.

## Open Issue 3: Ciel ABI Function Pointers

Ciel ABI `fn` values have no capture environment. Cloning them is a plain
function-pointer copy, so they are closer to primitive approved leaves than to
structural user data.

The language still cannot express a blanket impl over all function signatures:

```rust
impl<R, Args...> clone_message(*(R fn(Args...)) value) {
    return Ok(*value);
}
```

Possible routes:

1. Keep Ciel ABI `fn` as a compiler-known primitive-like `Message` leaf.
2. Require explicit impls for concrete signatures that cross actor boundaries.
3. Add callable-kind/type-pack generics and move the blanket impl into
   `/std/message`.

Recommended next step: keep route 1 unless strict "zero compiler leaves" becomes
a priority. It is not a structural safety hole, because Ciel ABI `fn` values do
not carry captured mutable state.

## Open Issue 4: Diagnostics For Representation Paths

The strict model reports failures through normalized SOP types. Diagnostics
should keep improving from mechanical paths like:

```text
meta head -> meta field value (`*i64`)
```

toward source paths when available:

```text
field `ptr` (`*i64`)
```

This is not a semantic blocker. It is a usability issue for larger nested
representations, especially arrays and closure captures.

## Recommended Work Order

1. Remove any remaining primitive `Message` compiler fallback now that
   `/std/message` has primitive impls.
2. Extend `/std/meta` representation for fixed-size arrays, then add
   `meta::Repr<[N]T>` tests inside structs and enums.
3. Implement `capability-erased-closures` first slice for `Message`.
4. Decide whether concrete closure proof should remain a narrow compiler rule
   or be phrased as `meta::Repr<ConcreteClosure>: Message` during retained
   closure conversion.
5. Leave callable-kind/type-pack generics as a later ergonomics feature unless
   direct blanket `fn` policies become important.

