# SOP Structural Representation Proposal

This proposal adds type-level metaprogramming through compiler-normalized
structural representations. The surface is ordinary Ciel syntax: generic data
types, interfaces, impls, `switch`, and normal function calls.

The core idea is:

1. `/std/meta` defines ordinary generic data types for products and sums.
2. The compiler recognizes a small set of `/std/meta` type forms and functions.
3. A source type's structure is projected into a normal sum-of-products value.
4. Library code processes that value through ordinary interfaces, impls,
   generic constraints, `switch`, and recursion.

This keeps metaprogramming inside Ciel's existing type checker as one semantic
pipeline.

## Proposal Order

```text
local-type-holes <= metaprogramming

metaprogramming :> capability-erased-closures[structural capability proofs]
capability-erased-closures || metaprogramming[retained closure witness storage]

metaprogramming :> error-box[derived format_error]
error-box || metaprogramming[owned error erasure and ? propagation]
```

Local type holes are a soft ergonomic companion for examples that bind verbose
normalized representation types.

Capability-erased closures keep owning erased closure witness storage.
Metaprogramming owns the structural proof path that can show a concrete closure
type implements a capability from its captures.

Error boxing keeps owning erased error storage and `?` propagation.
Metaprogramming owns any automatic generation or generic derivation of
`format_error`.

## Problem

Ciel already has interface algebra:

```rust
T: Message + printable + !ThreadLocal
```

That algebra is linear. It checks capabilities on one receiver type.

Structural derivation needs a second operation: expose the shape of a type so
ordinary generic code can walk its fields, enum variants, and closure captures.
Today `Message` has compiler-specific structural rules. A scalable design
centralizes structural access and lets future capabilities reuse it.

The design provides:

1. a small `/std/meta` structural vocabulary
2. compiler normalization from source types to SOP types
3. projection functions that build borrowed or owned SOP values
4. library-defined policies expressed as ordinary interfaces and impls
5. privacy, lifetime, and actor-boundary checks in the existing type system

## Design Scope

The metaprogramming surface consists of:

1. ordinary `/std/meta` product and sum types
2. compiler-normalized `RefRepr<T>` and `Repr<T>` type forms
3. compiler-lowered projection and reconstruction functions
4. ordinary generic recursion over SOP values
5. explicit policy wrappers that opt concrete types into generic structural code

## SOP Model

The representation follows the usual sum-of-products view of algebraic data
types:

- a struct is a product of fields
- an enum is a sum of variants
- a variant payload is a product of positional payload values
- an empty product is `HNil`
- product extension is `HCons`
- an empty sum is `CoNil`
- sum extension is `Coproduct`

In `/std/meta`:

```rust
// Product lists.
export struct HNil {}

export struct HCons<H, T> {
    H head;
    T tail;
}

// Named struct fields.
export struct FieldRef<T> {
    []char name;
    *T value;
}

export struct Field<T> {
    []char name;
    T value;
}

// Positional enum payloads.
export struct PayloadRef<T> {
    usize index;
    *T value;
}

export struct Payload<T> {
    usize index;
    T value;
}

// Sum lists.
export enum CoNil {}

export enum Coproduct<H, T> {
    This(H),
    Next(T),
}

export struct VariantRef<P> {
    []char name;
    P payload;
}

export struct Variant<P> {
    []char name;
    P payload;
}
```

The names stored in `FieldRef`, `Field`, `VariantRef`, and `Variant` are static
string slices supplied by the compiler. They are value-level metadata. The
type-level structure is carried by the nested `HCons` and `Coproduct` types.

This means two fields with the same Ciel type still have the same SOP type
component. Their labels are available to policies such as serialization and
diagnostics. Type equality and reconstruction use the SOP position that came
from `Repr<T>`.

## Compiler-Normalized Type Forms

`/std/meta` also exposes compiler-recognized type forms:

```rust
export type RefRepr<T>; // compiler-normalized borrowed SOP view
export type Repr<T>;    // compiler-normalized owned SOP value
```

When semantic analysis resolves these fully qualified `/std/meta` names, the
compiler normalizes them to ordinary generic Ciel types.

For:

```rust
struct Packet {
    i32 id;
    f64 score;
}
```

the compiler treats:

```rust
meta::RefRepr<Packet>
```

as:

```rust
meta::HCons<
    meta::FieldRef<i32>,
    meta::HCons<meta::FieldRef<f64>, meta::HNil>
>
```

and:

```rust
meta::Repr<Packet>
```

as:

```rust
meta::HCons<
    meta::Field<i32>,
    meta::HCons<meta::Field<f64>, meta::HNil>
>
```

The field order is declaration order.

For:

```rust
enum Expr {
    Lit(i32),
    Add(i32, i32),
}
```

the borrowed representation is:

```rust
meta::Coproduct<
    meta::VariantRef<
        meta::HCons<meta::PayloadRef<i32>, meta::HNil>
    >,
    meta::Coproduct<
        meta::VariantRef<
            meta::HCons<
                meta::PayloadRef<i32>,
                meta::HCons<meta::PayloadRef<i32>, meta::HNil>
            >
        >,
        meta::CoNil
    >
>
```

The owned representation is the same shape with `Variant` and `Payload`
replacing `VariantRef` and `PayloadRef`.

Enum payloads are positional because Ciel enum variants currently have unnamed
payload lists. The payload index is stored as a value for diagnostics and
serialization policies.

## Projection Functions

`/std/meta` exposes compiler-recognized functions:

```rust
export RefRepr<T> as_ref_repr<T>(*T value);
export Repr<T> into_repr<T>(T value);
export T from_repr<T>(Repr<T> value);
```

`as_ref_repr` builds a borrowed SOP value. For a struct, it creates one
`FieldRef` per field and points each `value` at the original field:

```rust
meta::RefRepr<Packet> repr = meta::as_ref_repr(value);
```

conceptually becomes:

```rust
meta::HCons<meta::FieldRef<i32>, meta::HCons<meta::FieldRef<f64>, meta::HNil>> repr = {
    head: { name: "id", value: &value->id },
    tail: {
        head: { name: "score", value: &value->score },
        tail: {},
    },
};
```

For an enum, `as_ref_repr` switches on the active variant and returns the
corresponding `Coproduct` branch with `PayloadRef` values pointing at the active
payloads.

`into_repr` consumes or copies a Ciel value into its owned SOP representation.
For a struct it stores one `Field<T>` per field. For an enum it stores the active
variant as a `Coproduct` branch with owned `Payload<T>` values.

`from_repr` reconstructs a Ciel value from an owned representation. It is the
inverse of `into_repr` for supported shapes:

```text
from_repr<T>(into_repr<T>(value)) == value
```

at the Ciel value-semantics level.

## Safety And Visibility

`RefRepr<T>` is a borrowed view. It contains ordinary Ciel pointers into the
original value. Therefore:

1. `RefRepr<T>` stays actor-local because it contains borrowed pointers.
2. `as_ref_repr<T>` has a lifetime bounded by the storage that its field
   pointers reference.
3. The generated addresses follow the same safety rules as hand-written
   `&value->field`.
4. A module can inspect private fields only for types whose structure is visible
   in that module.

`Repr<T>` is an owned value. Actor transfer and other capability-sensitive uses
go through the ordinary capability checks for its fields and payloads.

For opaque C handles, dynamic interface values, erased closure signatures, raw
pointers, and slices, representation is still possible when the containing type
is visible, but generic libraries must decide whether the leaf type implements
the requested capability. Leaves participate in serialization, hashing, message
cloning, and similar policies through ordinary interface constraints.

## Encoding Example

Serialization can be expressed as ordinary interface recursion over SOP values.
The receiver type is the first generic parameter, matching Ciel's capability
model:

```rust
import /std/meta as meta;

export interface<T> Result<void, Error> encode(*T value, *Encoder out);
export interface encodable = encode;
```

The product base case encodes nothing:

```rust
impl encode(*meta::HNil value, *Encoder out) {
    return Ok({});
}
```

A named field encodes its name and then the referenced field value:

```rust
impl<V: encodable, Tail: encodable> encode(
    *meta::HCons<meta::FieldRef<V>, Tail> list,
    *Encoder out,
) {
    encode_string(out, list->head.name)?;
    encode(list->head.value, out)?;
    encode(&list->tail, out)?;
    return Ok({});
}
```

Enum variants use ordinary `switch` over `Coproduct`:

```rust
impl encode(*meta::CoNil value, *Encoder out) {
    switch (*value) {
    }
}

impl<P: encodable> encode(*meta::VariantRef<P> variant, *Encoder out) {
    encode_string(out, variant->name)?;
    encode(&variant->payload, out)?;
    return Ok({});
}

impl<H: encodable, T: encodable> encode(*meta::Coproduct<H, T> value, *Encoder out) {
    switch (*value) {
        case This(head):
            H local = head;
            return encode(&local, out);
        case Next(tail):
            T local = tail;
            return encode(&local, out);
    }
}
```

`CoNil` has no values. The base impl exists only to close recursive constraints
on `Coproduct` tails.

A user opts a concrete type into this encoding policy with a small ordinary impl:

```rust
struct Packet {
    i32 id;
    f64 score;
}

impl encode(*Packet value, *Encoder out) {
    meta::RefRepr<Packet> repr = meta::as_ref_repr(value);
    return encode(&repr, out);
}
```

The compiler normalizes `RefRepr<Packet>` and lowers `as_ref_repr(value)` into
safe field-address construction.

## Decoding Example

Decoding needs owned representation, because it constructs a new value rather
than borrowing fields from an existing one.

```rust
export interface<T> Result<T, Error> decode(*Decoder in);
export interface decodable = decode;
```

Library code can implement `decode` for `HNil`, `HCons<Field<V>, Tail>`,
`Variant<P>`, and `Coproduct<H, T>` using ordinary generic impls. Conceptually,
the struct case builds an owned representation and converts it back:

```rust
impl decode(*Decoder in) {
    meta::Repr<Packet> repr = decode(in)?;
    return Ok(meta::from_repr<Packet>(repr));
}
```

The important property is that `from_repr` reconstructs `Packet` from the SOP
positions. The generic decoder produces ordinary nested `HCons` values.

The exact source syntax for implementing return-only interfaces such as
`decode<T>` follows the same rules as existing return-only capabilities such as
`make<T, U>`.

## Hashing Example

Hashing can fold over the borrowed product representation:

```rust
export interface<T> u64 hash(*T value, u64 seed);
export interface<T> bool eq(*T left, T right);
export interface hashable = hash + eq;

impl hash(*meta::HNil value, u64 seed) {
    return seed;
}

impl<V: hashable, Tail: hashable> hash(
    *meta::HCons<meta::FieldRef<V>, Tail> list,
    u64 seed,
) {
    u64 next = hash(list->head.value, seed);
    return hash(&list->tail, next);
}

impl hash(*Packet value, u64 seed) {
    meta::RefRepr<Packet> repr = meta::as_ref_repr(value);
    return hash(&repr, seed);
}
```

The field traversal is ordinary generic recursion over `HCons`.

## Message Example

`Message` can be expressed through owned representation:

```rust
export interface<T> Result<T, Error> clone_message(*T value);
export interface Message = clone_message;
```

A library helper can clone a borrowed product into an owned product. The input
receiver and output type are intentionally different: `RefRepr<T>` is borrowed,
while `Repr<T>` is owned.

```rust
export interface<Ref, Owned> Result<Owned, Error> clone_to_owned(*Ref value);

impl clone_to_owned<meta::HNil>(*meta::HNil value) {
    return Ok({});
}

impl<
    V: Message,
    TailOwned,
    TailRef: clone_to_owned<TailOwned>,
> clone_to_owned<meta::HCons<meta::Field<V>, TailOwned>>(
    *meta::HCons<meta::FieldRef<V>, TailRef> value,
) {
    V head_value = clone_message(value->head.value)?;
    TailOwned tail_value = clone_to_owned<TailOwned>(&value->tail)?;
    return Ok({
        head: { name: value->head.name, value: head_value },
        tail: tail_value,
    });
}
```

A concrete type opts in by projecting to `RefRepr<T>`, cloning that structure to
`Repr<T>`, then reconstructing `T`:

```rust
impl clone_message(*Packet value) {
    meta::RefRepr<Packet> refs = meta::as_ref_repr(value);
    meta::Repr<Packet> owned = clone_to_owned<meta::Repr<Packet>>(&refs)?;
    return Ok(meta::from_repr<Packet>(owned));
}
```

This keeps the safety decision in ordinary capability constraints:

```text
V: Message
TailRef: clone_to_owned<TailOwned>
```

The type argument written on `clone_to_owned<TailOwned>` is the existing Ciel
non-receiver interface argument. The receiver type remains the first function
argument.

If a field is a raw pointer, a dynamic interface whose message path is
unavailable, or an actor-local resource, the generic recursion reports the
required ordinary constraint at that structural position.

## Wrapper Policy

The core model uses explicit wrappers. A type author opts into a generic
structural policy by projecting the value and calling the policy over its SOP
representation:

```rust
impl encode(*Packet value, *Encoder out) {
    meta::RefRepr<Packet> repr = meta::as_ref_repr(value);
    return encode(&repr, out);
}
```

A later declaration-level convenience can emit the same wrapper. The essential
metaprogramming mechanism is the SOP representation plus ordinary policy code.

## Phase Ordering

The compiler handles SOP metaprogramming during normal semantic analysis:

1. resolve imports and identify the canonical `/std/meta` declarations
2. when a type mentions `meta::RefRepr<T>` or `meta::Repr<T>`, normalize it to
   the corresponding ordinary nested generic type
3. type-check generic constraints and impl calls against the normalized type
4. lower `as_ref_repr`, `into_repr`, and `from_repr` using visible type layout
5. run ordinary impl coherence, monomorphization, and codegen

All work happens in the normal semantic-analysis pipeline.

## Compiler Work

1. Add the `/std/meta` product and sum representation types.
2. Recognize canonical `/std/meta::RefRepr`, `/std/meta::Repr`,
   `/std/meta::as_ref_repr`, `/std/meta::into_repr`, and
   `/std/meta::from_repr`.
3. Normalize `RefRepr<T>` and `Repr<T>` for visible structs and enums.
4. Lower `as_ref_repr<T>(*T)` to borrowed product or coproduct construction.
5. Lower `into_repr<T>(T)` to owned product or coproduct construction.
6. Lower `from_repr<T>(Repr<T>)` to struct literals or enum variant
   construction.
7. Enforce privacy and lifetime restrictions for borrowed representations.
8. Add diagnostics that report the original type path and structural position
   for unsatisfied generic recursion constraints.

## Required Capability Set

1. `/std/meta` definitions for `HNil`, `HCons`, `FieldRef`, `Field`,
   `CoNil`, `Coproduct`, `VariantRef`, `Variant`, `PayloadRef`, and `Payload`
2. `RefRepr<T>` and `Repr<T>` normalization for visible structs, enums, and
   concrete closure capture environments
3. `as_ref_repr<T>(*T)` for visible structs, enums, and concrete closure capture
   environments
4. `into_repr<T>(T)` for owned projection
5. `from_repr<T>(Repr<T>)` for reconstruction from owned representation
6. generic recursion over `HNil`, `HCons`, `CoNil`, and `Coproduct`
7. structural diagnostics that name the original type and SOP position
8. examples and standard-library policies for encode, decode, hash, `Message`,
   and derived error formatting
