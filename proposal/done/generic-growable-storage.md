# Generic Growable Storage Proposal

This proposal adds a reusable storage primitive for standard-library
containers that need dynamic capacity. The immediate motivation is `/std/buf`
and `/std/map`: both are ordinary library data structures, but both currently
need bespoke C runtime-prelude helpers because unsafe Ciel cannot allocate or
resize typed owned storage for a runtime length.

## Proposal Order

```text
unsafe <= generic-growable-storage[trusted raw storage construction]
metaprogramming <= generic-growable-storage[type size and alignment]
```

The feature is an unsafe standard-library/runtime boundary. Unsafe Ciel marks
the trusted wrapper sites, and `/std/meta` supplies concrete type size and
alignment information. This proposal does not change `Message`, actor safety,
or structural policy derivation.

## Problem

Ciel can describe dynamic slice views, but it cannot currently create an owned
GC-backed slice for an arbitrary element type and runtime capacity.

Safe source can write fixed arrays and array literals when the element count is
known from the type or from a compile-time literal. It cannot express:

```ciel
[]?*void buckets = zeroed_slice<?*void>(capacity);
[]u8 next = realloc_slice<u8>(old, next_capacity);
```

`unsafe` does not add an allocator. It only permits trusted operations such as C
calls, raw pointer casts, and unsafe wrapper construction while preserving
ordinary type checking. As a result, each growable container has to import a new
C helper for its storage shape:

- `/std/buf` uses `ciel_runtime_u8_alloc_slice` and
  `ciel_runtime_u8_realloc_slice`;
- `/std/map` uses `ciel_map_alloc_buckets`, `ciel_map_bucket_get`, and
  `ciel_map_bucket_set` for pointer-slot bucket storage.

This does not scale. The missing primitive is not a map-specific feature; it is
generic owned storage with dynamic length and capacity.

## Goals

1. Let standard-library code allocate GC-backed storage for any concrete `T`
   using a runtime element count.
2. Let standard-library code grow that storage while preserving initialized
   elements.
3. Let pointer-containing storage be zero-initialized so the GC never scans
   uninitialized pointer slots.
4. Keep public application code on safe container APIs such as `ByteBuf` and
   `HashMap<K, V>`.
5. Remove the need for one-off runtime-prelude allocation helpers per
   container.

## Non-Goals

1. Manual `free`, custom allocators, arenas, or deterministic destruction.
2. Pointer arithmetic as a source-level language feature.
3. Exposing uninitialized values to safe Ciel.
4. Making slices own their storage. Slices remain views; ownership belongs to a
   standard-library storage wrapper.
5. A full `Vec<T>` API in the first slice.

## Proposed Shape

Add a small unsafe standard-library module, tentatively `/std/storage`, backed
by compiler/runtime primitives:

```ciel
export unsafe struct RawStorage<T> {
    []T storage;
}

export unsafe Result<RawStorage<T>, Error> raw_zeroed<T>(usize capacity);
export unsafe Result<RawStorage<T>, Error> raw_realloc_zeroed<T>(
    RawStorage<T> old,
    usize initialized,
    usize next_capacity
);
export []T raw_slice<T>(*RawStorage<T> storage);
```

The exact names are open. The important contract is that `RawStorage<T>` owns a
GC-backed allocation whose descriptor can be stored inside a higher-level
container. Safe code cannot construct or resize it directly.

`raw_zeroed<T>` returns a storage block with `capacity` elements. All bytes are
zeroed. This is required for pointer-containing elements because BDWGC scans
the block conservatively.

`raw_realloc_zeroed<T>` preserves the first `initialized` elements from `old`
and returns storage with `next_capacity` elements. Newly added slots are zeroed.
The function rejects capacity overflow. Shrinking below `initialized` is an
error.

`raw_slice<T>` exposes the full capacity as a mutable slice to the standard
library wrapper. Safe containers still separately track their initialized
length and expose only valid initialized prefixes.

## Container Rewrites

`ByteBuf` can store `RawStorage<u8>` instead of calling `u8`-specific runtime
helpers:

```ciel
export unsafe struct ByteBuf {
    storage::RawStorage<u8> storage;
    usize len;
}
```

`HashMap<K, V>` can store a real Ciel slice of nullable bucket pointers:

```ciel
export unsafe struct HashMap<K, V> {
    storage::RawStorage<?*void> buckets;
    usize len;
    u64 seed;
}
```

The map implementation can then use ordinary indexing on `[]?*void` instead of
importing bucket get/set C helpers.

## Safety Contract

The unsafe wrapper must maintain these invariants:

1. `RawStorage<T>` descriptors always point to GC-backed storage or to a valid
   non-null empty allocation.
2. The allocation size is `capacity * size_of(T)` with overflow checked.
3. The allocation has alignment suitable for `T`.
4. Pointer-containing storage is zero-initialized before it is visible to the
   GC.
5. Safe APIs expose only initialized elements, even if the raw capacity is
   larger.
6. Reallocation does not invalidate long-lived safe references because safe
   containers do not expose interior pointers across operations that may grow.

## Implementation Notes

The first implementation may still use a small C runtime primitive internally,
but that primitive should be type-agnostic:

```c
void *ciel_raw_alloc_zeroed(size_t elem_size, size_t align, size_t capacity);
void *ciel_raw_realloc_zeroed(
    void *old,
    size_t elem_size,
    size_t align,
    size_t initialized,
    size_t next_capacity
);
```

The Ciel wrapper supplies `meta::type_size<T>()` and `meta::type_align<T>()`.
After that, new containers should depend on `/std/storage`, not add their own
runtime-prelude helpers.

## Open Questions

1. Should the first API expose only zeroed allocation, or also an unsafe
   uninitialized variant for POD-like types?
2. Should `RawStorage<T>` store capacity explicitly, or should capacity stay in
   each higher-level container?
3. Should `raw_realloc_zeroed` accept and return `[]T` directly, or should the
   owner wrapper be mandatory to keep ownership explicit?
4. How should the compiler reject `T` values whose C representation cannot be
   stored in a homogeneous raw array?
