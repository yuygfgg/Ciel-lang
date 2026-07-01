# Alpha Limitations

This document records the intentional public limitations of the first Ciel
alpha. It is a usage boundary, not a post-alpha wish list.

## Standard Library Surface

The alpha-supported standard-library and bundled-library import paths are the
ones listed in `alpha-todo.md`. Public APIs in those modules should use concrete
module error enums. `/std/error.Error` remains the application-boundary error
type for task bodies, top-level drivers, compatibility facades, and places that
explicitly erase errors for user-facing reporting.

Concrete errors implement `format_error`, and `?` is the normal path for
propagating a concrete module error into `/std/error.Error` at an application
boundary.

## Bytes And Buffers

`/std/bytes.Bytes` is immutable owned byte storage. It does not expose a raw
runtime handle. Use `bytes_copy`, `bytes_from_text`, `bytes_concat`,
`Bytes.prepend`, `Bytes.append`, `Bytes.slice`, `Bytes.copy_to`, and
`Bytes.to_slice` for construction and copying.

Use `/std/buf.ByteBuf` for reusable mutable byte buffers. Async reads that need
capacity reuse use `ByteBuf`; immutable `Bytes` is not a mutable read target.

## Collections And Iteration

Use `/std/vec.Vec<T>` for growable sequences and `/std/iter.collect` or
`.collect()` for iterator collection. Arrays and slices are still useful for
fixed-size data and borrowed views, but examples should not teach hand-rolled
growable collection patterns.

Alpha iterator entrypoints are:

- `[]const T.iter()`
- `Vec<T>.iter()`
- `Bytes.iter()`
- `ByteBuf.iter()`
- `Text.chars()`

`Text.chars()` yields stored UTF-8 bytes as `char` code units. It does not
perform Unicode scalar decoding.

`HashMap<K, V>` and `SharedMap<K, V>` do not expose borrowed `.iter()` in the
alpha surface. Borrowed map iteration needs lifetime enforcement to prevent
mutation invalidating outstanding entries, and snapshot iteration needs a
separate fallible cloning design. `SharedMap` keeps destructive `pop_any` as
its public iteration-like operation for alpha.

## Async And Resource Boundaries

Task bodies and actor handlers are application/runtime boundaries and continue
to use `/std/error.Error`. Reusable std modules, package libraries, and protocol
helpers should use concrete error enums and erase only when crossing into those
boundaries.

Resource and async handle structs remain opaque. Safe code should use the
published operations and scoped helpers, not inspect or manufacture raw handle
state.
