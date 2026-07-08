# Async Frame Safety Soundness Gaps

This document records two related async-frame-safety bugs found while comparing
`design.md` with the current implementation. It is an issue analysis document,
not an implemented language change. The goal is to keep the root causes and
repair constraints explicit before changing the checker.

## Summary

The current async-frame-safety analysis can accept values that should not be
stored in a suspended async frame:

1. resource-affine values such as `io::File` can cross `await` because the
   frame-safety walker recursively inspects their scalar fields and loses the
   nominal resource fact;
2. preserved `/std/meta` representation markers can cross `await` because the
   frame-safety walker treats them as ordinary named types when their marker is
   not normalized to the concrete SOP representation.

These have different direct causes. The resource bug is not caused by meta
markers. The meta bug is caused by marker preservation combined with a checker
that does not know the safety meaning of preserved markers.

## Normative Rules Involved

`design.md` specifies that values live across `await` must be safe to store in
the private async frame. Safe code rejects raw pointers, non-static borrowed
slices, thread-local handles, closures capturing forbidden locals, and compound
values that transitively contain those rejected views or handles.

The same section states that standard-library resource handles are not
whitelisted by name for async-frame storage. They must be compiler-generated
futures or operation keys, prove `ShareHandle`, or carry an explicit unsafe
`async_frame_opt_in_marker` derive or impl.

For structural metaprogramming, `meta::RefRepr<T>` is a borrowed structural
view: `as_ref_repr` creates read-only pointers to visible fields, enum payloads,
or closure captures. `meta::Repr<T>` is an owned structural value and is
available only for non-resource-affine source types.

## Finding 1: Resource-Affine Values Cross Await

The following program currently compiles and runs, but the local `file` is a
standard-library resource handle live across an `await`:

```ciel
import /std/async as async;
import /std/async_time as async_time;
import /std/env as env;
import /std/io as io;
import /std/result;

async Result<usize, Error> broken() {
    []const char path = env::arg(1)?;
    io::File file = io::open_read(path)?;
    await async_time::sleep_ms(0)?;
    [8]u8 @buffer = [0;];
    usize n = io::read(&file, buffer[..])?;
    io::close(file)?;
    return Ok(n);
}

i64 main() {
    usize n = must(async::block_on(broken()));
    if (n != 8) {
        return 1;
    }
    return 0;
}
```

### Root Cause

`type_is_affine` correctly recognizes resource handles and resource structs as
affine. In particular, `resource::Handle` is treated as an affine leaf, and
`io::File` is a `resource unsafe struct` containing a `resource::Handle`.

The async-frame-safety walker does not consult that nominal affine fact before
recursing into named aggregates. It reaches `io::File`, expands its field,
reaches `resource::Handle`, expands its `u64` fields, and concludes that the
value is frame-safe because all physical fields are scalars.

This is a semantic loss: resource ownership is a nominal fact, not merely a
property of the physical fields.

### Fix Direction

`async_frame_safety_violation` should treat affine values as rejected unless a
more specific allowed case has already matched.

The ordering matters:

1. compiler-generated futures and opaque future state must keep their existing
   hidden-state checks;
2. values with explicit `async_frame_opt_in_marker` must remain allowed;
3. all remaining `type_is_affine(ty)` values should be rejected as
   non-frame-safe resource-affine state.

This keeps explicitly opted-in async operation handles working while preventing
ordinary resource wrappers from being accepted merely because their
representation contains scalars.

## Finding 2: Preserved Meta Markers Can Cross Await

The direct reproducer uses a structural reference view whose source type is
legally imported through an alias:

```ciel
// repr_inner.ciel
import /std/meta as meta;

export struct Packet {
    i64 id;
}

export i64 read_id(meta::RefRepr<Packet> repr) {
    return *repr.head.value;
}
```

```ciel
// main.ciel
import /std/async as async;
import /std/async_time as async_time;
import /std/meta as meta;
import /std/result;
import ./repr_inner as inner;

async Result<i64, Error> broken() {
    inner::Packet packet = { id: 1234 };
    meta::RefRepr<inner::Packet> repr = meta::as_ref_repr(&packet);
    await async_time::sleep_ms(0)?;
    return Ok(inner::read_id(repr));
}

i64 main() {
    return must(async::block_on(broken())) - 1234;
}
```

This currently compiles. The generated code promotes the source object because
`MetaAsRefRepr` is treated as an escape source, so this example does not
necessarily crash. The checker still accepted a borrowed structural view across
`await`, which is inconsistent with the language rule that reference views must
not be stored in suspended frames unless proven frame-safe.

### Root Cause

The immediate problem is marker preservation. The checker can preserve
`meta::RefRepr<T>`, `meta::Repr<T>`, and `meta::Schema<T>` marker types instead
of normalizing them to SOP types. Preserved markers are useful for generics,
type holes, recursive expansion, and other delayed-normalization cases.

However, the current visibility predicate is too narrow for Ciel's whole-program
model. `meta_repr_source_visible_from_current_module` checks whether a nominal
type is visible by bare lookup in the current module. An alias-qualified import
such as `inner::Packet` is a legal source type, but it may not be visible as a
bare name. The predicate therefore preserves the marker even though the source
program can legally name the type and the whole-program compiler knows its
layout.

After preservation, async-frame-safety analysis does not interpret the marker.
The preserved marker is a `Ty::Named` value. If normal struct or enum instance
lookup does not reveal fields for that marker, the walker returns success
instead of conservatively checking the marker's semantic payload.

There are therefore two layers:

1. ordinary concrete types may be preserved unnecessarily because
   qualified-visible types are treated as not visible;
2. any marker that is legitimately preserved later has no marker-aware
   frame-safety rule.

### RefRepr Semantics

`meta::RefRepr<T>` should not cross `await` merely because it is represented as
a preserved marker. If it is normalized, the SOP form contains `FieldRef<T>` or
`PayloadRef<T>` values with raw pointers, and the existing checker will reject
those pointers. Preserved `RefRepr` should be at least as strict as the
normalized form.

The current escape behavior for `MetaAsRefRepr` does not make this a sound
language rule. Escape promotion extends the storage lifetime of the projected
source object, but it does not by itself prove that transitive borrowed fields,
thread-local handles, or other view-like fields are valid to store across
`await`. It also couples a type-system safety property to a backend storage
placement decision.

### Repr Semantics

`meta::Repr<T>` is different from `RefRepr<T>`. It is owned and should not be
rejected wholesale. A preserved owned representation should be checked through
the owned representation slots that would appear after normalization.

For example, `meta::Repr<Packet>` where `Packet` contains only `i64` and `bool`
fields should be frame-safe. But if a field slot is `[]const u8`, `*const T`, a
thread-local policy leaf, or another non-frame-safe value, that fact must remain
visible to the async-frame-safety analysis.

The checker must not blindly recurse through the physical `/std/meta` structs
as ordinary user structs. Metadata fields such as `Field.name`,
`Variant.name`, `Payload.index`, and `meta::Type<T>` witnesses are compiler
metadata and should not be treated like user payload fields. The safety walk
should inspect only the representation payload slots.

### Schema Semantics

`meta::Schema<T>` is instance-free metadata. It contains static field names,
variant names, payload indices, and type witnesses, but no source `T` value and
no borrowed field pointer. A schema value should generally be frame-safe once
schema construction itself is legal.

The schema rule should still be explicit so that preserved `meta::Schema<T>` is
not accidentally handled as an arbitrary opaque named type.

## Repair Plan

The preferred repair has two stages.

### Stage 1: Fix Unnecessary Meta Marker Preservation

The visibility predicate for meta normalization should match source-level type
nameability, not bare-name lookup. In a whole-program compiler, a resolved,
concrete nominal type that the source program can legally name through an
import alias should be eligible for normalization.

The existing bare lookup check should not be the sole test for visibility. The
implementation should either:

1. remove the bare-name restriction for already-resolved concrete nominal
   source types; or
2. replace it with a predicate that recognizes both bare and
   alias-qualified visibility.

The first option is simpler and better aligned with whole-program compilation:
once semantic analysis has resolved the source type to a `DefId`, the compiler
can normalize its structure for internal analysis. Source privacy is already
enforced by resolution; safety checks should not deliberately become blind to
resolved layout facts.

This change will make ordinary `meta::RefRepr<inner::Packet>` and
`meta::Repr<inner::Packet>` normalize to their SOP forms instead of remaining
opaque markers. Then the existing raw-pointer and slice checks will catch many
cases naturally.

### Stage 2: Add Marker-Aware Frame-Safety Fallback

Marker-aware fallback is still needed for generic, type-hole, recursive, or
otherwise delayed markers.

`async_frame_safety_violation` should recognize canonical `/std/meta` marker
types before the ordinary named-type recursion:

1. preserved `meta::RefRepr<T>` should be rejected as a borrowed structural
   view, unless a future design adds a precise proof that the represented view
   is frame-safe;
2. preserved `meta::Repr<T>` should run a semantic owned-representation walker
   over the value slots of the owned representation;
3. preserved `meta::Schema<T>` should be accepted as instance-free static
   metadata, assuming schema construction was already accepted.

The owned-representation walker should follow representation semantics rather
than physical `/std/meta` struct fields:

1. struct fields map to `Field<Slot>` and only `Slot` is checked;
2. enum payloads map to `Payload<Slot>` and only `Slot` is checked;
3. variants, products, sums, fixed-array chunks, and array cats recursively
   check their payload slots;
4. metadata fields such as names, indices, and type witnesses are ignored;
5. nominal policy leaves remain nominal and are checked by the normal
   async-frame rule for that leaf type.

This fallback should reject generic `meta::Repr<T>` unless the compiler has a
specific proof that `T`'s representation slots are frame-safe. The existing
generic rejection in async-frame safety may already provide this behavior; the
marker fallback should preserve it rather than silently accepting unknown
generic payloads.

## Regression Tests

The fix should add focused fixtures before broadening coverage:

1. `async_await/resource_file_across_await_rejected`: a blocking `io::File`
   local remains live after `await` and is rejected.
2. `async_await/foreign_ref_repr_across_await_rejected`: an alias-qualified
   exported `Packet` is projected with `meta::as_ref_repr`, kept live across
   `await`, and rejected.
3. `async_await/foreign_owned_repr_scalar_across_await_allowed`: an
   alias-qualified exported `Packet` with only scalar fields is projected with
   `meta::into_repr`, kept live across `await`, and accepted.
4. `async_await/foreign_owned_repr_slice_across_await_rejected`: an
   alias-qualified exported `Packet` with a borrowed slice field is projected
   with `meta::into_repr`, kept live across `await`, and rejected.
5. `async_await/schema_across_await_allowed`: a schema value is kept live
   across `await` and accepted.
6. `metaprogramming/qualified_visible_repr_normalizes`: a type reachable only
   through an import alias still normalizes for `meta::RefRepr`,
   `meta::Repr`, and `meta::Schema`.

The tests should assert diagnostics on the source local that crosses `await`,
not on incidental codegen details.

## Non-Goals

This document does not propose changing Ciel's source module system, removing
`export`, or making private top-level declarations nameable from other modules.
It also does not propose disabling marker preservation entirely. Preservation
is still useful for delayed normalization; the bug is accepting preserved
markers without an explicit safety interpretation.

This document also does not require changing the runtime or async lowering.
The primary fixes belong in type checking and meta representation
normalization.
