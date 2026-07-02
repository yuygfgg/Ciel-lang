# Serde Capability Proposal

This proposal defines the language and standard-library surface needed for
usable automatic serialization and deserialization, with JSON as the first
target format.

Schema reflection is the structural substrate. It can describe a visible type
and let library code build `meta::Repr<T>` before a value exists, but it is not
itself a complete serialization system. A production serde surface also needs
format readers and writers, container policies, optional/null handling, field
customization, and actionable error paths.

## Proposal Order

```text
metaprogramming < schema-reflection < serde

generic-growable-storage <= serde[Vec-backed sequence decode]
stdlib-baseline-utilities <= serde[Text, Bytes, parse, formatting]
pure-library-message || serde[structural policy boundaries]
```

`schema-reflection` owns the raw structural model: `Schema<T>`, `Repr<T>`,
field names, variant names, payload indices, and source/repr type witnesses.
This proposal owns format policies, container policies, customization metadata,
and user-facing encode/decode APIs.

## Problem

With schema reflection, a library can walk this type:

```ciel
struct Packet {
    i64 id;
    bool ok;
}
```

and construct `meta::Repr<Packet>` from parsed input. That is enough for a
minimal structural decoder. It is not enough for real JSON:

1. JSON has `null`, variable-length arrays, strings, and object maps.
2. Ciel has nominal library containers such as `Text`, `Bytes`, `Vec<T>`, and
   `HashMap<K, V>` whose internal layout must not become the wire format.
3. Fields often need rename, default, skip, optional, flatten, or unknown-field
   policy.
4. Enum wire shapes differ across APIs.
5. Deserialization errors must report useful source and logical paths.
6. Automatic serde must be explicit enough not to accidentally expose private or
   unstable nominal layouts.

## Goals

1. Provide `encode_json<T>` and `decode_json<T>` for ordinary opted-in types.
2. Let visible structs and enums use schema-guided structural serde without
   hand-written field walking.
3. Treat standard containers and special nominal types through explicit leaf or
   container policies rather than structural layout.
4. Support `Option<T>`/nullable and missing-field semantics.
5. Support variable-length sequences and owned strings/bytes.
6. Provide field and variant customization needed by practical JSON APIs.
7. Return rich errors with source location when available and logical paths such
   as `.items[3].name`.
8. Keep the core schema reflection surface format-neutral.

## Non-Goals

1. Making every visible struct or enum serializable by default.
2. Exposing private fields, private variants, or private container layouts.
3. Borrowed deserialization into slices that reference the input buffer in the
   first implementation.
4. Supporting arbitrary cyclic pointer graphs.
5. Replacing `meta::Repr<T>` or changing reconstruction semantics.
6. Defining binary formats in the first implementation.

## Public Surface

The standard library should provide a format-neutral `/std/serde` layer and a
JSON-specific `/std/json` layer.

Illustrative serde interfaces:

```ciel
import /std/meta as meta;
import /std/result;

export struct SerdePath {}

export enum SerdeErrorKind {
    Eof,
    Syntax,
    TypeMismatch,
    MissingField,
    DuplicateField,
    UnknownField,
    UnknownVariant,
    InvalidValue,
    UnsupportedType,
}

export struct SerdeError {
    SerdeErrorKind kind;
    SerdePath path;
    []const char message;
    usize byte_offset;
}

export interface<T, W> Result<void, SerdeError> serialize(
    *const T value,
    *W writer
);

export interface<T, R> Result<T, SerdeError> deserialize(
    meta::Type<T> target,
    *R reader
);
```

JSON convenience functions:

```ciel
export Result<Text, SerdeError> encode_json<T: serialize<JsonWriter>>(T value);
export Result<T, SerdeError> decode_json<T: deserialize<JsonReader>>(
    meta::Type<T> target,
    []const char input
);
```

The concrete generic syntax may need to follow the repository's determined
interface conventions. The semantic requirement is that the format backend type
is part of the interface, so the same nominal type can support multiple formats.

## JSON Format Backend

`/std/json` should expose streaming reader and writer operations rather than
requiring every decode to allocate a full `JsonValue` tree.

The writer protocol needs operations for:

1. `null`, booleans, signed and unsigned integers, floating-point numbers, and
   strings;
2. beginning and ending arrays and objects;
3. writing object field names;
4. emitting separators and escaping strings correctly.

The reader protocol needs operations for:

1. peeking the next token;
2. reading primitive values;
3. entering and leaving arrays and objects;
4. reading object field names;
5. skipping values for ignored unknown fields;
6. tracking byte offsets and logical path segments.

A tree API can be layered on top, but automatic serde should not require it.

## Structural Serialization

For a visible struct or enum that opts into structural serde, serialization uses
`meta::Schema<T>` for names and `meta::as_ref_repr<T>` for values:

```ciel
impl serialize(*const Packet value, *JsonWriter writer) {
    meta::Schema<Packet> schema = meta::schema<Packet>();
    meta::RefRepr<Packet> repr = meta::as_ref_repr(value);
    return serde::serialize_structural(&schema, &repr, writer);
}
```

The structural helper walks schema and repr together. `FieldSchema<T, R>` gives
the declared field type and representation slot type; `FieldRef<T>` provides the
borrowed value. Enum serialization matches the active `Coproduct` branch against
the full variant schema list to obtain the variant name and payload schema.

Fixed-size arrays are handled by standard serde impls over the `/std/meta`
array tree. Application code must not need to implement `ArrayChunk1` through
`ArrayChunk16` manually for JSON.

## Structural Deserialization

Deserialization starts from the target type and schema:

```ciel
impl deserialize(meta::Type<Packet> target, *JsonReader reader) {
    meta::Schema<Packet> schema = meta::schema<Packet>();
    meta::Repr<Packet> repr = serde::deserialize_structural(
        &schema,
        meta::type_tag<meta::Repr<Packet>>(),
        reader
    )?;
    return Ok(meta::from_repr<Packet>(repr));
}
```

For structs, the decoder reads a JSON object, matches input field names against
schema field names after applying policy, decodes each representation slot, and
constructs the `HCons<Field<R>, Tail>` product. Missing fields are accepted only
when policy supplies a default or when the field type policy treats missing as a
valid value.

For enums, the decoder maps the input representation to one variant schema and
constructs the corresponding `Coproduct<Variant<P>, Tail>` branch. The default
JSON shape should be externally tagged:

```json
{"VariantName": payload}
```

Unit variants may also encode as a string when explicitly configured.

## Standard Type Policies

Primitive JSON policies:

1. `bool` maps to JSON booleans.
2. Signed and unsigned integers map to JSON numbers with range checks.
3. Floating-point numbers map to JSON numbers; non-finite values require an
   explicit policy and are rejected by default.
4. `char` maps to a one-byte string or an integer only with explicit policy; the
   default should be rejected until text semantics are settled.

Nominal standard-library policies:

1. `Text` maps to a JSON string.
2. `Bytes` maps to a configured representation, defaulting to base64 text once a
   base64 utility exists.
3. `Vec<T>` maps to a JSON array when `T` is serializable/deserializable.
4. Fixed-size `[N]T` maps to a JSON array of exactly `N` elements.
5. `HashMap<Text, V>` maps to a JSON object when `V` is serializable.
6. Other `HashMap<K, V>` forms require an explicit key policy.
7. `Result<T, E>` has no default JSON policy; APIs should use explicit enums or
   domain types.
8. Resource handles, raw pointers, dynamic interfaces, erased closures, and
   thread-local handles are rejected unless an explicit nominal policy exists.

## Option And Nullability

Serde needs a standard optional type:

```ciel
export enum Option<T> {
    None,
    Some(T),
}
```

Default JSON policy:

1. `None` serializes as `null`.
2. `Some(value)` serializes as `value`.
3. A missing object field of type `Option<T>` decodes as `None` unless field
   policy says missing is an error.
4. `null` for a non-optional field is a type mismatch unless a custom policy
   accepts it.

This should be a nominal policy for `Option<T>`, not a generic enum-tagging
policy, because JSON null semantics are format-specific.

## Customization Metadata

Usable serde needs per-type, per-field, and per-variant configuration. The
default policy uses source names exactly as `meta::Schema<T>` reports them, but
the serde layer should define an additional policy metadata view aligned with
schema traversal.

Required customization:

1. rename a field or variant;
2. apply `rename_all` to fields or variants;
3. skip serializing a field;
4. skip deserializing a field;
5. provide a default for a missing field;
6. deny or ignore unknown fields;
7. flatten a nested object field;
8. choose enum representation: externally tagged, internally tagged, adjacently
   tagged, untagged, or unit-as-string;
9. deserialize aliases for fields and variants.

The syntax for attaching this metadata can be a later concrete design. Plausible
forms include attributes on declarations and fields, or explicit policy impls.
The important requirement is that metadata is available to generic serde code
without requiring text macros or hand-written field walkers.

## Opt-In Model

Serde is explicit. A nominal type becomes serializable or deserializable by one
of these mechanisms:

1. a hand-written `serialize` or `deserialize` impl;
2. an impl that delegates to schema-guided structural helpers;
3. a future declaration convenience that emits those impls from serde metadata.

There should be no unconditional blanket impl for all visible structs and enums.
Opt-in preserves control over wire compatibility and avoids accidentally making
private nominal layout part of a public protocol.

## Error Paths

Errors carry both a machine-readable kind and a path. The reader maintains path
segments as structural decoding enters fields, variants, payloads, and array
indices:

```text
$
$.items[3].name
$.message.Store.payload[1].quality
```

The JSON reader also records byte offsets when they are known. Structural
helpers must wrap leaf errors with the current path segment instead of losing
context.

## Visibility And Policy Boundaries

The serde layer follows schema reflection visibility. It can structurally walk a
type only when its fields, variants, or closure captures are visible. Imported
private nominal types remain leaves and require public serde impls from their
own module.

Standard containers are nominal leaves even when their unsafe layout is visible
inside the standard library. Their wire shape is defined by serde policies, not
by `meta::Repr<T>` over private storage fields.

## Compiler Work

1. No compiler changes are required for basic structural helpers beyond
   completed schema reflection.
2. Add a standard `Option<T>` type if the language does not already have one.
3. Add `/std/serde` and `/std/json` modules with reader, writer, error, and
   policy interfaces.
4. Provide primitive, `Text`, `Bytes`, `Vec<T>`, fixed-array, and selected map
   policies.
5. Provide structural helper impls over `/std/meta` products, sums, schema
   nodes, and array trees.
6. Add a concrete customization metadata mechanism.
7. Optionally add a derive-like convenience that emits explicit serde impls.

## Test Plan

1. Encode and decode primitive values with range and type mismatch errors.
2. Round-trip visible structs using schema-guided structural serde.
3. Round-trip nested structs, enums, fixed arrays, `Vec<T>`, `Text`, and
   `Option<T>`.
4. Decode missing optional fields, missing required fields, duplicate fields,
   and unknown fields under both ignore and deny policies.
5. Round-trip externally tagged and internally tagged enums.
6. Verify field rename, variant rename, default, skip, alias, and flatten
   behavior.
7. Verify error paths for nested arrays and objects.
8. Verify private imported types require explicit public serde impls.
9. Verify unsupported handles, raw pointers, dynamic interfaces, and resources
   produce clear diagnostics or runtime errors at the policy boundary.

## Completion Criteria

The proposal is complete when an ordinary library user can write or derive
explicit serde impls for a nested API data type containing structs, enums,
fixed arrays, `Vec<T>`, `Text`, `Option<T>`, and maps with string keys, then
successfully round-trip it through JSON with useful errors and field-level
customization.
