# Pure Library Message Follow-up Decisions

This note records the adopted follow-up design after landing the strict
`pure-library-message` direction. Rejected route lists have been removed so the
document describes the implementation target.

The important correction is that strict pure-library structural `Message` does
not require const generics or callable-kind impls. The strict model is:

```ciel
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
6. The compiler may identify canonical `/std/message` and `/std/meta` names,
   normalize structural representation types, and provide narrow marker facts
   for unnameable value categories. It must not make a type `Message` as a
   policy shortcut.

## Decision 1: Fixed-Size Arrays Use Chunked Representation

Current Ciel has type generics but no const generics, and const generics are
not part of the planned language surface. The standard library therefore cannot
and should not write a blanket direct array impl for every possible length.

Direct `[N]T: Message` is not the target. Fixed-size arrays become structural
only through `/std/meta` representation. `meta::Repr<[N]T>` normalizes to a
fixed-arity, chunked representation, not to `[N]T` itself.

The exact node family can evolve, but it should have this shape:

```ciel
meta::ArrayNil
meta::ArrayChunk1<T>
...
meta::ArrayChunk16<T>
meta::ArrayCat<L, R>
```

For small arrays, the representation can be a single fixed-arity chunk. For
larger arrays, the compiler builds a balanced tree of chunks. A `[1000]T`
representation should be dozens of chunks plus a shallow `ArrayCat` tree, not a
1000-deep `HCons` chain. This keeps type recursion depth near `O(log N)` while
the number of representation leaves remains proportional to the array length.

`/std/message` owns the policy for these nodes:

```ciel
impl<T: Message> clone_message(*meta::ArrayChunk16<T> value) {
    ...
}

impl<L: Message, R: Message> clone_message(*meta::ArrayCat<L, R> value) {
    ...
}
```

The compiler's role is structural normalization and `into_repr` / `from_repr`
code generation. It does not prove `[N]T: Message`.

Array representation expansion must be budgeted. Very large static arrays are
bulk storage, not record-like structural data. If a representation would exceed
the configured structural expansion budget, compilation should reject it and
ask for an explicit wrapper policy or a standard-library owned buffer type:

```text
meta::Repr<[1048576]u8> expands too many structural array nodes;
use an explicit Message wrapper or an owned buffer type.
```

## Decision 2: Ciel ABI Function Pointers Use A Meta Marker

Ciel ABI `fn` values have no capture environment. Cloning them is a plain
function-pointer copy, so their `Message` policy can be ordinary library code.
They do not need callable-kind or type-pack generics.

`/std/meta` should expose a compiler-recognized marker:

```ciel
export interface<T> bool ciel_fn_value_marker(*T value);
export interface CielFnValue = ciel_fn_value_marker;
```

The compiler provides only this fact:

```text
Ciel ABI function pointer: meta::CielFnValue
```

Then `/std/message` owns the policy:

```ciel
impl<F: meta::CielFnValue> clone_message(*F value) {
    return Ok(*value);
}
```

Extern C function pointers do not receive this marker. C interop remains a
trusted boundary, and C-backed function values need explicit wrapper policy when
they cross an actor or channel boundary.

## Decision 3: Concrete Closure Message Policy Uses Representation

Concrete closure instances have real compiler-created types, but those types are
anonymous. They should not be direct compiler-known `Message` leaves.

`/std/meta` should expose a compiler-recognized closure-kind marker:

```ciel
export interface<T> bool closure_value_marker(*T value);
export interface ClosureValue = closure_value_marker;
```

The compiler provides only this fact:

```text
concrete closure instance: meta::ClosureValue
```

It does not make plain erased closure signatures such as `i64 |(i64)|`
`ClosureValue`, because erased signatures no longer expose captures.

Then `/std/message` can own the policy:

```ciel
impl<C: meta::ClosureValue> clone_message(*C value) {
    meta::Repr<C> repr = meta::into_repr(*value);
    meta::Repr<C> copied = clone_message(&repr)?;
    return Ok(meta::from_repr<C>(copied));
}
```

The body is checked after generic instantiation. For a concrete closure type,
`meta::Repr<C>` normalizes to the existing owned SOP representation of its
captures. If every capture is messageable, the ordinary `/std/message` SOP impls
clone the environment. If a capture contains a raw pointer, slice, dynamic
interface value, plain erased closure signature, or another non-message leaf,
the generic call fails through the normal `Message` constraint path.

This keeps the safety policy in library code while leaving layout reflection to
the compiler. The compiler still needs to normalize `meta::Repr<C>` for concrete
closure types and generate `from_repr<C>` reconstruction, but it no longer needs
a hard-coded rule that directly classifies closure instances as `Message`.

Retained closure signatures such as `R |(A): Message|` are separate from this
marker. They carry whichever capability witness was proven at conversion time.
Plain erased signatures such as `R |(A)|` carry no `Message` proof.

## Compiler Boundary

The final model has no `Message` policy holes in the compiler.

The compiler may:

1. recognize canonical `/std/message::clone_message` and related marker
   interfaces for constraint checking, diagnostics, retained closure witnesses,
   and coherence
2. normalize `/std/meta` representation markers, including structs, enums,
   concrete closures, and chunked fixed-size arrays
3. provide narrow `/std/meta` marker facts for Ciel ABI function pointers and
   concrete closure instances
4. emit code for `as_ref_repr`, `into_repr`, `from_repr`, and retained closure
   witness calls

The compiler must not:

1. synthesize `clone_message` implementations for user structs or enums
2. auto-prove direct `[N]T: Message`
3. auto-prove Ciel ABI `fn` values as `Message` without the `/std/message`
   marker impl
4. auto-prove concrete closure instances as `Message` without the
   `/std/message` marker impl
5. emit a `Message`-specific clone fallback when no ordinary impl was selected

## Recommended Work Order

1. Add `meta::CielFnValue` and the `/std/message` marker impl, then remove the
   direct Ciel ABI `fn` `Message` leaf.
2. Add `meta::ClosureValue` and the `/std/message` representation-based impl,
   then remove the direct concrete closure `Message` leaf.
3. Add chunked fixed-size array representation nodes, `/std/message` impls for
   those nodes, and an expansion budget; then remove the direct `[N]T`
   `Message` leaf.
4. Keep callable-kind and type-pack generics out of this work. They are future
   ergonomics features only if unrelated APIs need them.
