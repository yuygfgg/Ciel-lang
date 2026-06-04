# SOP Structural Representation Proposal

This proposal adds structural metaprogramming through compiler-normalized
sum-of-products representations. The public surface is ordinary Ciel syntax:
generic data types, interfaces, impls, `switch`, and function calls.

The core mechanism is:

1. `/std/meta` defines ordinary product and sum data types.
2. The compiler recognizes canonical `/std/meta` representation markers.
3. A source type's visible shape normalizes to ordinary SOP types.
4. Compiler-lowered projection functions build borrowed or owned SOP values.
5. Library code processes those SOP values through normal generic recursion.

This proposal owns the core representation and lowering machinery. Concrete
policies such as pure-library `Message`, encoding, decoding, hashing, and error
formatting are consumers of this mechanism.

## Proposal Order

```text
local-type-holes <= metaprogramming
metaprogramming < pure-library-message

metaprogramming :> capability-erased-closures[structural representation]
pure-library-message :> capability-erased-closures[message witness source]

metaprogramming :> error-box[structural representation]
pure-library-message || error-box[structural formatting policy]
```

Local type holes are an ergonomic companion for examples that bind verbose
normalized representation types.

Capability-erased closures consume the concrete closure capture representation
defined here. The later policy that decides whether those captures imply
`Message` belongs to the message proposal, not to this one.

Error boxing may use the representation defined here to implement structural
formatting policies, but those policies are standard-library work.

## Problem

Ciel already has interface algebra:

```ciel
T: Message + printable + !ThreadLocal
```

That algebra checks capabilities on a receiver type. Structural libraries need
a separate operation: expose the visible shape of a type so ordinary generic
code can walk fields, enum variants, and concrete closure captures.

The design provides:

1. a small `/std/meta` structural vocabulary
2. compiler normalization from source types to SOP types
3. projection functions that build borrowed or owned SOP values
4. reconstruction from owned SOP values
5. structural diagnostics tied back to source positions where possible

## SOP Model

The representation follows the usual sum-of-products view:

- a struct is a product of fields
- an enum is a sum of variants
- a variant payload is a product of positional payload values
- a concrete closure environment is a product of captures
- an empty product is `HNil`
- product extension is `HCons`
- an empty sum is `CoNil`
- sum extension is `Coproduct`

In `/std/meta`:

```ciel
export struct HNil {}

export struct HCons<H, T> {
    H head;
    T tail;
}

export struct FieldRef<T> {
    []char name;
    *T value;
}

export struct Field<T> {
    []char name;
    T value;
}

export struct PayloadRef<T> {
    usize index;
    *T value;
}

export struct Payload<T> {
    usize index;
    T value;
}

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
string slices supplied by the compiler. They are value-level metadata. Type
identity and reconstruction use SOP position.

## Type Forms

`/std/meta` exposes compiler-recognized type forms:

```ciel
export struct RefRepr<T> {}
export struct Repr<T> {}
```

When semantic analysis resolves the canonical `/std/meta` definitions, it
normalizes `RefRepr<T>` and `Repr<T>` to ordinary nested generic types.

For a visible struct:

```ciel
struct Packet {
    i64 id;
    bool ok;
}

meta::RefRepr<Packet>
// meta::HCons<
//     meta::FieldRef<i64>,
//     meta::HCons<meta::FieldRef<bool>, meta::HNil>
// >

meta::Repr<Packet>
// meta::HCons<
//     meta::Field<i64>,
//     meta::HCons<meta::Field<bool>, meta::HNil>
// >
```

For a visible enum:

```ciel
enum Token {
    Number(i64),
    End,
}

meta::RefRepr<Token>
// meta::Coproduct<
//     meta::VariantRef<meta::HCons<meta::PayloadRef<i64>, meta::HNil>>,
//     meta::Coproduct<meta::VariantRef<meta::HNil>, meta::CoNil>
// >
```

The owned enum representation uses `Variant` and `Payload` instead of
`VariantRef` and `PayloadRef`.

For a concrete closure instance, representation exposes captures in capture
order:

```ciel
i64 base = 10;
_ f = |i64 x| x + base;

_ refs = meta::as_ref_repr(&f);
// refs has type meta::HCons<meta::FieldRef<i64>, meta::HNil>
```

Source spelling for closure instance types remains intentionally limited.
Local holes can bind values whose exact concrete closure type is verbose or
compiler-created.

Erased closure signature types such as `i64 |(i64)|` do not expose captures.

## Projection Functions

`/std/meta` exposes compiler-recognized functions:

```ciel
export RefRepr<T> as_ref_repr<T>(*T value);
export Repr<T> into_repr<T>(T value);
export T from_repr<T>(Repr<T> value);
```

`as_ref_repr` builds a borrowed SOP value:

- for a struct, it creates one `FieldRef` per visible field
- for an enum, it switches on the active variant and returns the matching
  `Coproduct` branch with `PayloadRef` values
- for a concrete closure, it creates one `FieldRef` per captured value

`into_repr` copies a Ciel value into an owned SOP value:

- struct fields become `Field<T>`
- enum payloads become `Payload<T>` under the active `Variant`
- closure captures become `Field<T>` entries named `capture#0`,
  `capture#1`, and so on

`from_repr` reconstructs a value from an owned representation. For supported
types:

```text
from_repr<T>(into_repr<T>(value)) == value
```

at the Ciel value-semantics level.

## Safety And Visibility

`RefRepr<T>` is a borrowed view. It contains ordinary Ciel pointers into the
original value. Therefore:

1. `RefRepr<T>` stays actor-local because it contains borrowed pointers.
2. `as_ref_repr<T>` has a lifetime bounded by the storage referenced by its
   field, payload, or capture pointers.
3. Generated addresses follow the same rules as hand-written address-taking.
4. A module can inspect structure only for types visible to that module.

`Repr<T>` is an owned value. Capability-sensitive uses go through ordinary
checks on the representation's leaves.

Opaque C handles, dynamic interface values, erased closure signatures, raw
pointers, and slices can appear as leaves when the containing type is visible.
Generic libraries decide whether such leaves satisfy a policy through ordinary
interface constraints.

## Policy Example

Policies are library code. A type opts into a policy by projecting itself and
delegating to generic impls over the representation.

```ciel
import /std/meta as meta;

interface<T> u64 hash(*T value, u64 seed);
interface hashable = hash;

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

struct Packet {
    i64 id;
    bool ok;
}

impl hash(*Packet value, u64 seed) {
    meta::RefRepr<Packet> repr = meta::as_ref_repr(value);
    return hash(&repr, seed);
}
```

This proposal defines the representation and projection needed by such code. It
does not require standard-library hash, encode, decode, `Message`, or
formatting policies to exist before the core mechanism is considered complete.

## Phase Ordering

The compiler handles SOP metaprogramming during normal semantic analysis:

1. resolve imports and identify the canonical `/std/meta` declarations
2. normalize `meta::RefRepr<T>` and `meta::Repr<T>`
3. type-check generic constraints and impl calls against normalized types
4. lower `as_ref_repr`, `into_repr`, and `from_repr`
5. run ordinary monomorphization, escape analysis, and C code generation

## Completed Compiler Work

1. `/std/meta` product and sum representation types
2. canonical recognition for `RefRepr`, `Repr`, `as_ref_repr`, `into_repr`, and
   `from_repr`
3. normalization for visible structs, visible enums, and concrete closure
   capture environments
4. lowering of `as_ref_repr<T>(*T)` for structs, enums, and concrete closures
5. lowering of `into_repr<T>(T)` for structs, enums, and concrete closures
6. lowering of `from_repr<T>(Repr<T>)` for structs, enums, and concrete closures
7. escape-analysis treatment for borrowed representations
8. structural diagnostics that name field, variant payload, or capture paths
   where the compiler has source shape information

## Completion Criteria

The core proposal is complete when these are implemented and covered by tests:

1. `/std/meta` definitions for `HNil`, `HCons`, `FieldRef`, `Field`,
   `CoNil`, `Coproduct`, `VariantRef`, `Variant`, `PayloadRef`, and `Payload`
2. `RefRepr<T>` and `Repr<T>` normalization for visible structs, visible enums,
   and concrete closure capture environments
3. `as_ref_repr<T>(*T)` for visible structs, visible enums, and concrete
   closure capture environments
4. `into_repr<T>(T)` for owned projection
5. `from_repr<T>(Repr<T>)` for reconstruction from owned representation
6. generic recursion tests over `HNil`, `HCons`, `CoNil`, and `Coproduct`
7. diagnostics that name source field, variant payload, or capture paths

Standard-library policies built on top of the representation are tracked by
their own proposals.
