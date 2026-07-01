# Instance-Free Schema Reflection Proposal

This proposal extends `/std/meta` with schema reflection for generic
deserialization and other policies that need a type's visible field and variant
metadata before a value exists.

The existing structural metaprogramming proposal exposes value projection:

1. `as_ref_repr<T>(&value)` borrows an existing value.
2. `into_repr<T>(&value)` copies an existing value into an owned representation.
3. `from_repr<T>(repr)` rebuilds a value from an owned representation.

That is sufficient for encoding and structural message policies. It is not
sufficient for generic decoding, because decoding starts with only a target type
and input bytes. A decoder must know expected field names and variant names
before it can build `meta::Repr<T>`.

## Proposal Order

```text
metaprogramming < schema-reflection

schema-reflection :> serialization[instance-free structural decode schema]
schema-reflection || pure-library-message[leaf policy boundaries]
```

`metaprogramming` owns the existing SOP representation, projection, and
reconstruction machinery. This proposal adds a schema view over the same visible
source shapes. Serialization and decoding libraries consume this schema, but the
format-specific parsers and policies are not part of this proposal.

## Problem

For a visible struct:

```ciel
struct Packet {
    i64 id;
    bool ok;
}
```

`meta::as_ref_repr(&packet)` can produce field metadata:

```ciel
meta::HCons<
    meta::FieldRef<i64>,
    meta::HCons<meta::FieldRef<bool>, meta::HNil>
>
```

The `FieldRef` values contain `"id"` and `"ok"`, but those names are produced by
projecting an existing `Packet`. A decoder for `decode<Packet>(json)` does not
have a `Packet` yet.

The decoder can parse the JSON object, but it cannot generically ask:

1. which fields `Packet` expects;
2. what names those fields have;
3. what value type each field should decode into;
4. which enum variant names are valid;
5. how a matched variant name maps to a `Coproduct` branch.

Writing one concrete `decode(Packet)` implementation remains possible, but a
library-level `decode<T>(json) -> T` cannot be implemented with the existing
metadata surface.

## Goals

1. Let ordinary library code inspect a visible type's schema without an existing
   value.
2. Keep the existing `RefRepr<T>` and `Repr<T>` value representation unchanged.
3. Make schema values ordinary Ciel values that generic interfaces can recurse
   over.
4. Provide field names, variant names, payload indices, and type witnesses.
5. Preserve current module visibility and leaf-policy boundaries.
6. Support generic construction of `meta::Repr<T>` from parsed data, followed by
   `meta::from_repr<T>`.
7. Keep format-specific parsing, JSON policy choices, and serde-style
   customization as library or later-proposal work.

## Non-Goals

1. Text macros, token pasting, or declaration-level source generation.
2. Automatic `serialize` or `deserialize` impl generation for nominal types.
3. Rename, skip, default, flatten, tagging-style, or unknown-field attributes.
4. A JSON parser or standard serialization library in the first slice.
5. Type-level strings or const generics.
6. Changing how `meta::Repr<T>` reconstructs values. Reconstruction remains
   positional.
7. Exposing private fields or private enum variants across module boundaries.

## Public Surface

Add these canonical `/std/meta` declarations:

```ciel
export struct Schema<T> {}

export struct FieldSchema<T> {
    []const char name;
    Type<T> ty;
}

export struct PayloadSchema<T> {
    usize index;
    Type<T> ty;
}

export struct VariantSchema<P> {
    []const char name;
    P payload;
}

export Schema<T> schema<T>();
```

`Schema<T>` is a compiler-recognized marker, like `Repr<T>` and `RefRepr<T>`.
Type checking normalizes it to ordinary schema node types.

`schema<T>()` is a compiler-lowered function. It returns a schema value for
`T` without requiring a `T` value.

The names stored in `FieldSchema` and `VariantSchema` are static string slices
with program lifetime. They use the same source names that `Field`,
`FieldRef`, `Variant`, and `VariantRef` use today.

## Schema Model

A struct schema is a product list:

```ciel
struct Packet {
    i64 id;
    bool ok;
}

meta::Schema<Packet>
// meta::HCons<
//     meta::FieldSchema<i64>,
//     meta::HCons<meta::FieldSchema<bool>, meta::HNil>
// >
```

The runtime value from `meta::schema<Packet>()` carries the field names:

```ciel
meta::HCons<
    meta::FieldSchema<i64>,
    meta::HCons<meta::FieldSchema<bool>, meta::HNil>
> packet_schema = meta::schema<Packet>();

// packet_schema.head.name == "id"
// packet_schema.tail.head.name == "ok"
```

An enum schema is a list of variant schemas, not a `Coproduct` value:

```ciel
enum Token {
    Number(i64),
    End,
}

meta::Schema<Token>
// meta::HCons<
//     meta::VariantSchema<
//         meta::HCons<meta::PayloadSchema<i64>, meta::HNil>
//     >,
//     meta::HCons<
//         meta::VariantSchema<meta::HNil>,
//         meta::HNil
//     >
// >
```

This is intentionally different from `meta::Repr<Token>`, which is a
`Coproduct`. A schema value must describe every valid variant at once, while a
representation value stores only the active variant.

Payload fields remain positional. `PayloadSchema<T>.index` is the zero-based
payload position inside the variant.

Concrete closure schemas expose capture entries in capture order, using
`FieldSchema<T>` nodes whose names are `capture#0`, `capture#1`, and so on. This
matches the existing value representation. Erased closure signature types do not
expose captures.

Fixed-size array schemas normalize to the same bounded `ArrayNil`,
`ArrayChunk1` through `ArrayChunk16`, and `ArrayCat<L, R>` tree shape used by
owned array representation, but with `Type<T>` schema leaves:

```ciel
meta::Schema<[3]u8>
// meta::ArrayChunk3<meta::Type<u8>>
```

Array schema expansion uses the same budget as `meta::Repr<[N]T>`.

## Normalization

When semantic analysis resolves the canonical `/std/meta` `Schema` declaration,
`meta::Schema<T>` normalizes according to the visible shape of `T`:

1. visible struct: `HCons<FieldSchema<FieldType>, Tail>`
2. visible enum: `HCons<VariantSchema<PayloadSchemaProduct>, Tail>`
3. concrete closure instance: `HCons<FieldSchema<CaptureType>, Tail>`
4. fixed-size array: bounded array schema tree
5. unsupported root type: diagnostic
6. generic or hole-containing source type: keep a private schema marker until
   substitution supplies a concrete type

The compiler must normalize schema markers both at initial lookup and after
generic substitution, matching the existing `Repr<T>` and `RefRepr<T>` rules.

Opaque C handles, dynamic interface values, erased closure signatures, raw
pointers, and slices may appear as schema leaves when the containing type is
visible. Libraries decide whether those leaves are decodable through ordinary
interface constraints.

## Lowering `schema<T>()`

`schema<T>()` is valid with exactly one type argument and no value arguments:

```ciel
meta::Schema<Packet> schema = meta::schema<Packet>();
```

The lowered expression constructs ordinary Ciel values:

1. struct fields become `FieldSchema<T>` values with static names and `Type<T>`
   tags;
2. enum variants become `VariantSchema<P>` entries in declaration order;
3. payload entries become `PayloadSchema<T>` with zero-based indices;
4. concrete closure captures become `FieldSchema<T>` entries in capture order;
5. arrays become bounded schema trees.

The generated schema value contains no pointers into a program value and no
field values of type `T`. Schema policy code should treat `Type<T>` as metadata,
not as ownership of a `T`.

## Decode Shape

This proposal does not add a decoder, but it must make this library pattern
possible:

```ciel
import /std/meta as meta;

interface<S, R> Result<R, Error> decode_schema(
    *const S schema,
    meta::Type<R> out,
    JsonValue json
);

export Result<T, Error> decode<T>(JsonValue json) {
    meta::Schema<T> schema = meta::schema<T>();
    meta::Repr<T> repr = decode_schema(
        &schema,
        meta::type_tag<meta::Repr<T>>(),
        json
    )?;
    return Ok(meta::from_repr<T>(repr));
}
```

For products, the decoder matches schema and output representation together:

```ciel
impl decode_schema(
    *const meta::HNil schema,
    meta::Type<meta::HNil> out,
    JsonValue json
) {
    return Ok({});
}

impl<V, STail, RTail> decode_schema(
    *const meta::HCons<meta::FieldSchema<V>, STail> schema,
    meta::Type<meta::HCons<meta::Field<V>, RTail>> out,
    JsonValue json
) {
    V value = decode_field<V>(schema->head.name, json)?;
    RTail tail = decode_schema(
        &schema->tail,
        meta::type_tag<RTail>(),
        json
    )?;
    meta::Field<V> field = {
        name: schema->head.name,
        value: value,
    };
    return Ok({ head: field, tail: tail });
}
```

For enums, schema traversal compares the parsed variant name against
`VariantSchema.name`. When the head matches, the decoder builds the active
`Coproduct::This(Variant(payload))`. When it does not match, it recurses over the
remaining variant schema list and wraps the result in `Coproduct::Next`.

The exact JSON names and helper interfaces are illustrative. The required
property is that schema values provide enough metadata for ordinary library code
to construct a `meta::Repr<T>` without a preexisting `T`.

## Why Not Field Name Builtins Only?

A function such as `field_name<T, I>()` is not enough for generic decode over the
current representation. After normalization, the product node for `Packet` is:

```ciel
meta::HCons<
    meta::Field<i64>,
    meta::HCons<meta::Field<bool>, meta::HNil>
>
```

A generic impl matching `meta::Field<i64>` does not know that this field came
from `Packet.id`, nor that it is field index `0`. Ciel also does not have
type-level integer parameters that would let the recursive impl track the field
index at the type level.

Schema reflection solves this by carrying the field name as a value alongside
the recursive traversal. It is additive: existing `Field<T>`, `FieldRef<T>`,
`Variant<P>`, and `VariantRef<P>` types keep their current shape.

An alternative design would change representation nodes to include a compiler
generated field identity type, such as `Field<FieldId, T>`. That would also make
generic decode possible, but it would be more invasive and would disturb
existing SOP policy code.

## Visibility And Privacy

Schema reflection follows the same visibility rule as structural
representation: a module can inspect only types whose shape is visible to that
module.

For an imported private type, `meta::Schema<T>` is rejected or preserved as an
opaque leaf according to the same rules used by `meta::Repr<T>`. The compiler
must not reveal private field names, private variant names, or private payload
shapes through schema values.

## Safety

`schema<T>()` does not borrow from a source value. It produces static metadata
and type witnesses. It does not introduce new pointer lifetimes.

Decoding policies remain responsible for leaf safety:

1. raw pointers require explicit decode policies or are rejected;
2. slices require a borrowed-output or owned-copy policy;
3. dynamic interfaces require an explicit decoding representation;
4. handles and thread-local resources require nominal policies;
5. recursive pointer graphs do not become decodable by default.

`from_repr<T>` remains the boundary that reconstructs a nominal value from a
fully decoded owned representation.

## Compiler Work

1. Add `/std/meta` declarations for `Schema<T>`, `FieldSchema<T>`,
   `PayloadSchema<T>`, `VariantSchema<P>`, and `schema<T>()`.
2. Recognize canonical `Schema` and `schema` declarations by `DefId`.
3. Normalize `Schema<T>` for visible structs, enums, concrete closure instances,
   and fixed-size arrays.
4. Preserve private internal schema markers for generic or hole-containing
   source types.
5. Normalize schema markers after substitution in type checking and
   monomorphization.
6. Lower `schema<T>()` to ordinary schema values with static string slices.
7. Add codegen for schema literals, including array schema trees.
8. Add diagnostics for unsupported root schema types and array schema budget
   overflow.

## Test Plan

1. `schema<Packet>()` exposes field names and type shape for a visible struct.
2. `schema<Token>()` exposes all variant names and payload schema products for a
   visible enum.
3. Generic recursion over `FieldSchema` builds a `meta::Repr<Packet>` and
   `from_repr<Packet>` reconstructs the value.
4. Generic recursion over `VariantSchema` selects a branch by variant name and
   reconstructs an enum.
5. Generic `Schema<T>` markers normalize correctly after substitution.
6. Private imported field names are not exposed.
7. Unsupported root types produce actionable diagnostics.
8. Large fixed-array schemas respect the existing expansion budget.

## Completion Criteria

The proposal is complete when:

1. `Schema<T>` and `schema<T>()` are implemented in `/std/meta`;
2. schema normalization works for visible structs, visible enums, concrete
   closure instances, and fixed-size arrays;
3. schema values carry field names, variant names, payload indices, and type
   witnesses;
4. generic code can construct `meta::Repr<T>` from schema-guided parsed data and
   call `meta::from_repr<T>`;
5. visibility, generic substitution, and array-budget tests are covered.

## Open Questions

1. Should closure schemas be part of the first implementation, or should the
   first slice cover only structs, enums, and arrays?
2. Should `schema<T>()` be allowed for primitive root types as a leaf schema, or
   should primitive decoding continue to rely only on `Type<T>`?
3. Should schema values implement `Message` by default through ordinary
   `/std/message` impls, or should they remain local metadata unless a policy
   needs to send them?
4. Should later customization metadata live in separate wrapper policies, or
   should this proposal reserve extension fields for rename/default/skip
   attributes?
