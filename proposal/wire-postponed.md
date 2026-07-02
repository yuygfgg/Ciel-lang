# Postponed Wire Policy Work

The implemented wire and JSON surface is now specified in `design.md`. This
proposal tracks the remaining work that is intentionally postponed because it
needs new language/compiler support, would require awkward user-written policy
objects, or is not required for the first usable JSON slice.

## Arbitrary Fixed Arrays

Current JSON policy implementations cover `[0]T` through `[16]T`. This is
enough for small structural tuples and common packet fields, but it is not a
real solution for values such as `[32]u8` hashes, `[64]u8` signatures, or fixed
protocol buffers.

The clean solution is array-length generics:

```ciel
impl<T: wire::encode_value<json::Writer>, const N: usize>
    wire::encode_value(*const [N]T value, *json::Writer writer);

impl<T: wire::decode_value<json::Reader>, const N: usize>
    wire::decode_value(meta::Type<[N]T> target, *json::Reader reader);
```

Encode can use array-to-slice conversion once `N` is known to the monomorphized
body. Decode also needs a safe way to construct `[N]T` from fallible element
initialization, for example a compiler-recognized
`array_try_from_fn<T, E, const N: usize>` helper.

Generating impls beyond 16 is an acceptable temporary unblocker, but it should
not become the language model.

## Derived Thin Wrappers

Visible structs and enums can already opt into JSON by writing ordinary
`wire::encode_value` and `wire::decode_value` wrappers that call
`meta::schema<T>()`, `meta::as_ref_repr`, and the JSON structural helpers.

The postponed part is declaration-level convenience. A future derive-like
feature should emit those wrappers from explicit wire metadata while preserving
the current opt-in model. There should still be no blanket impl for all visible
structs and enums.

## Field And Variant Metadata

The following policies are possible to model with explicit policy objects, but
the current language has no good declaration syntax for them:

1. field and variant rename;
2. `rename_all`;
3. field aliases;
4. field defaults;
5. skip encode/decode;
6. flatten;
7. enum representation selection.

These should wait for a concrete metadata design, likely attributes or an
equivalent declaration-level mechanism visible to ordinary Ciel libraries.

## Missing Optional Fields

`Option<T>` has a JSON null policy. Missing-field handling is separate. The
ergonomic rule should probably be that a missing `Option<T>` field decodes as
`None`, while a missing non-optional field remains `MissingField` unless a
field default exists.

This is postponed because implementing it cleanly wants field-level policy
metadata or specialization-like behavior for missing slots. A hand-written
struct wrapper can implement this policy today, but the generic structural
helper should not grow an overlapping blanket/specialized impl set.

## Additional Enum Shapes

The implemented enum policy is externally tagged:

```json
{"Variant": null}
{"Variant": [payload0, payload1]}
```

Internally tagged, adjacently tagged, untagged, and unit-as-string policies are
postponed until field and variant metadata exists. They can be implemented in
library code over `json::Value`, but exposing them without metadata would force
too much boilerplate onto users.

## Borrowed Decoding

The first JSON decoder returns owned values. Borrowed decoding into slices that
reference the input buffer needs a way to express that the decoded value cannot
outlive the reader/input. Without lifetime-style type information this would be
unsafe or overly restrictive, so it is postponed.

## Native JSON Backends

The public JSON API should remain Ciel-owned. A native C JSON backend may be
useful later for performance, but it must be hidden behind the same
`/std/json` reader, writer, and value policy surface. It is not needed for
correctness or first-use ergonomics.
