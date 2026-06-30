# Alpha TODO

This checklist defines the minimum work required before publishing the first
public alpha. The goal is not to finish every proposal, but to avoid exposing
standard-library shapes that are known to be temporary or structurally wrong.

## Release Rules

- Do not publish an alpha with a public API that is already scheduled for
  removal or semantic replacement.
- Do not partially migrate a standard-library module and leave mixed public
  conventions behind.
- Every exported fallible standard-library API and bundled library API must
  follow the error policy in this document before alpha.
- Do not use an "experimental" label as a substitute for finishing an API
  migration. Public alpha APIs must be internally consistent.
- Every alpha-blocking item below needs tests and examples updated in the same
  change set.

## Alpha Blockers

### 1. Freeze The Published Alpha Surface

Decide exactly which modules and bundled libraries ship in the public alpha.
Everything that ships as a public import path must meet this document's alpha
policy.

Standard-library migration scope:

- `/std/result`
- `/std/error`
- `/std/vec`
- `/std/bytes`
- `/std/text`
- `/std/buf`
- `/std/iter`
- `/std/io`
- `/std/net`
- `/std/async`
- `/std/async_io`
- `/std/async_net`
- `/std/resource`
- `/std/message`
- `/std/map`
- `/std/codec`
- `/std/channel`
- `/std/sync`
- `/std/atomic`
- `/std/shared_map`
- `/std/time`
- `/std/async_time`
- `/std/env`
- `/std/crypto`

Bundled library migration scope:

- `/sqlite`

Done criteria:

- Every exported public import path in the alpha release is listed in this
  section.
- No listed public import path exposes an API that violates the alpha policy.
- The public facade `/std/lib` only re-exports modules that meet the alpha
  policy.
- Bundled library manifests do not expose legacy APIs outside the alpha policy.

### 2. Implement Generic Growable Storage

Implement the minimum storage primitive needed by growable containers.

Required shape:

- Add an unsafe `/std/storage` or equivalent internal standard-library module.
- Provide generic zeroed allocation for `T` at runtime capacity.
- Provide generic reallocation preserving initialized elements.
- Preserve GC safety for pointer-containing element types.
- Remove new container dependence on one-off runtime helpers such as
  `ciel_runtime_u8_alloc_slice` and `ciel_map_alloc_buckets`.

Done criteria:

- `ByteBuf`, `Vec<T>`, and future map bucket storage can use the same primitive.
- Tests cover pointer-containing storage, byte storage, growth, zero capacity,
  overflow rejection, and preservation during growth.
- One-off allocation helpers are either removed or confined to compatibility
  code that is not part of the public API.

### 3. Add `Vec<T>`

Add the public growable sequence type before alpha.

Minimum API:

- `vec_new<T>(capacity)`
- `vec_len`
- `vec_capacity`
- `vec_reserve`
- `vec_push`
- `vec_clear`
- `vec_slice`
- `vec_mut_slice`
- `vec_from_slice`

Expected receiver selectors:

- `.len`
- `.capacity`
- `.reserve`
- `.push`
- `.clear`
- `.slice`
- `.mut_slice`

Done criteria:

- `Vec<T>` is implemented on generic growable storage.
- `Vec<T>` is usable with primitive, struct, enum, and pointer-containing item
  types.
- `Vec<T>` implements the required message/clone behavior or explicitly rejects
  unsupported cases with tests.
- `/std/lib` re-exports `Vec<T>` only after the API and error type are final for
  alpha.

### 4. Clean Up Public Error Types

The standard erased `Error` type is an application-boundary error type, not the
default error type for ordinary libraries.

Policy:

- Low-level and reusable library APIs return concrete error enums.
- Application-facing convenience helpers may return `/std/error.Error`.
- `?` remains the ergonomic conversion path from concrete errors into
  `/std/error.Error`.
- Assigning, returning, or passing a concrete `format_error` error where
  `/std/error.Error` is expected is a compiler-inserted conversion, not a
  source-level `error_box()` call.
- A module must not expose a mixed public API where some functions return a new
  concrete module error and comparable functions still return erased `Error`.

Alpha-required concrete errors:

- `VecError`
- `BufError`
- `BytesError`
- `TextError` or a shared bytes/text error type
- `CodecError`
- `MapError`
- `ResourceError`
- `IoError`
- `NetError`
- `AsyncError`
- `AsyncIoError`
- `AsyncNetError`
- `TimeError`
- `AsyncTimeError`
- `ChannelError`
- `SyncError`
- `AtomicError`
- `SharedMapError`
- `EnvError`
- `CryptoError`
- `SqliteError`

Modules and bundled libraries that must be fully migrated:

- `/std/buf`
- `/std/bytes`
- `/std/text`
- `/std/codec`
- `/std/map`
- `/std/resource`
- `/std/io`
- `/std/net`
- `/std/async`
- `/std/async_io`
- `/std/async_net`
- `/std/time`
- `/std/async_time`
- `/std/channel`
- `/std/sync`
- `/std/atomic`
- `/std/shared_map`
- `/std/crypto`
- `/std/env`
- `/sqlite`

Done criteria:

- No alpha-supported module exposes `Result<_, Error>` unless it is explicitly
  an application-boundary helper or an error-erasing convenience facade.
- Every concrete module error implements `format_error`.
- Tests cover `?` conversion from every concrete alpha error type into
  `/std/error.Error`.
- Tests cover direct expected-type conversion from concrete errors into
  `/std/error.Error` for return, argument, nested `Result`, and local
  initialization contexts.
- Tests cover direct matching on each concrete error enum where the error is
  recoverable.
- Existing `text_error` and `code_error` uses inside reusable modules are
  replaced by concrete variants.
- If an operation can fail due to cleanup after a successful user result, its
  return type models both domains explicitly instead of collapsing everything
  into erased `Error`.

Implementation status:

- Completed for low-level, reusable, scoped, and callback APIs in the
  alpha-supported modules and `/sqlite`: public fallible APIs now return
  concrete module or combined error enums and those enums implement
  `format_error`.
- Remaining public `Result<_, Error>` surfaces are intentional boundaries:
  `/std/lib` compatibility facades, `/std/error` helpers, task body results,
  message cloning and internal async operation adapters, internal
  `/std/storage`, and `/std/actor` outside the alpha scope.
- Tests cover direct matching, `?` conversion, and direct expected-type
  conversion into `/std/error.Error` for the alpha std concrete errors and
  callback combined errors, and `/sqlite` tests cover `SqliteError` plus
  scoped/transaction combined errors through application-facing drivers.

### 5. Fix Scoped And Callback Error Shapes

Higher-order helpers currently force callback bodies to return `Result<R,
Error>` in several places. That makes erased errors contagious.

Required work:

- Audit scoped resource helpers, actor/task helpers, map `with`, mutex `with`,
  and I/O `with_*` helpers.
- Let callback bodies return concrete error types when possible.
- Introduce combined concrete error enums when helpers can fail both in the body
  and during cleanup.

Done criteria:

- Public higher-order helpers do not force erased `Error` unless the helper is
  explicitly an application-boundary convenience API.
- Cleanup failure is not silently erased into an unrelated body error type.
- Tests cover body failure, setup failure, cleanup failure, and successful
  cleanup after body failure.

Implementation status:

- Completed for `/std/resource`, `/std/io`, `/std/net`, `/std/map`,
  `/std/sync`, `/std/shared_map`, `/std/async::with_task_group`, and `/sqlite`
  scoped/transaction helpers.
- Public helper body errors are generic where they can be generic. Helpers that
  combine setup/body/cleanup domains expose concrete combined enums such as
  `ScopedError<E>`, `IoWithError<E>`, `NetWithError<E>`,
  `TaskGroupError<E>`, `MapWithError<E>`, `SyncWithError<E>`,
  `SqliteWithError<E>`, and `SqliteTransactionError<E>`.
- Task body and actor handler protocols still use `/std/error.Error` as
  explicit runtime/application boundaries and remain outside this callback
  cleanup migration.

### 6. Replace The `Bytes` Runtime Handle Public Shape

`Bytes` must not expose a runtime handle as its durable public representation.

Required work:

- Decide whether public `Bytes` is immutable `Vec<u8>`-backed storage,
  `ByteBuf`-backed storage, or a small public wrapper over a private runtime
  representation.
- Keep async runtime internals free to use native `CielBytes`, but do not expose
  that shape as the public model.
- Make `async_io` and `async_net` return the final alpha `Bytes` shape.

Done criteria:

- Public `Bytes` does not expose `*void handle`.
- Bytes construction, slicing, append, copy-out, async read, async write, and
  message cloning all use the final alpha representation or a private adapter.
- Compatibility facades do not leak legacy names as preferred public APIs.
- Tests cover bytes conversion across `/std/bytes`, `/std/async_io`, and
  `/std/async_net`.

### 7. Add Generic Iterator Collection

Iteration needs collection as a real abstraction.

Minimum API:

- A generic `collect` interface with a target collection capability.
- A `Vec<T>` implementation of that target collection capability.
- Receiver selector `.collect` for the generic form.

Done criteria:

- `range`, `slice_iter`, `map`, `filter`, `take`, `chain`, `zip`, `enumerate`,
  and `flatten` can be collected through the generic collection interface.
- `Vec<T>` collection is implemented through the same interface used by generic
  `collect`.
- Allocation failure or capacity overflow returns a concrete error.
- Tests cover successful collection, empty collection, and overflow/error
  propagation.

### 8. Add Container Iterator Entrypoints

Containers need a standard way to produce iterators before generic collection is
useful in real programs.

Required API shape:

- Container iterator functions use ordinary exported functions with receiver
  selector `.iter`.
- The public function name may be container-specific, such as `vec_iter`, but
  the selector spelling is unified as `.iter()`.
- Do not use `to_iter` for borrowed iteration; the operation should not imply an
  ownership conversion.

Required coverage:

- `Vec<T>.iter()`
- `[]const T.iter()` through `slice_iter` or the final equivalent
- `Bytes.iter()` if public `Bytes` is a byte sequence
- `Text.chars()` or `Text.iter()`, with the byte-vs-character contract stated
  explicitly
- `HashMap<K, V>.iter()` if `/std/map` remains in the public alpha surface

Done criteria:

- Every alpha container has an iterator entrypoint or an explicit reason why it
  is not iterable.
- Iterator entrypoints compose with `.map`, `.filter`, generic `.collect`, and
  ordinary function-call equivalents.
- Tests cover iteration over empty, single-item, and multi-item containers.
- Tests cover that iterator item types are inferred through the `Iterator`
  determined type.

### 9. Complete Receiver Selectors For Core Std APIs

Receiver selectors are already implemented, but core library ergonomics are
incomplete.

Required work:

- Add selectors for iterator adapters and consumers.
- Add selectors for container iterator entrypoints.
- Add selectors for `Vec<T>`.
- Add selectors for the final `Bytes` API.
- Verify existing selectors after error type changes.

Done criteria:

- Common examples can use idiomatic chained calls from container `.iter()` to
  adapter calls and generic `.collect()`.
- Selector calls and ordinary function calls are both tested for the same core
  APIs.
- No selector exposes a different semantic contract from the ordinary call.

### 10. Update Tests, Examples, And Docs

The alpha surface must be documented through examples that use the final public
API.

Required work:

- Update examples to use `Vec<T>`, final `Bytes`, concrete module errors, and
  receiver selectors.
- Update tests that currently rely on erased `Error` in reusable modules.
- Add a short alpha limitations document.

Done criteria:

- The regression suite passes.
- Examples compile with the final alpha facade.
- Docs do not teach legacy `Bytes` handles, erased std errors, or pre-`Vec`
  collection patterns as normal usage.

## Post-Alpha Work

These proposals and cleanups should not block the first alpha unless they become
necessary for one of the blockers above.

- Instance-free schema reflection.
- Monomorphized C callbacks.
- Full compiler file/module refactoring.
- Rewriting map bucket storage on generic storage if the public API is already
  clean.
- Downcasting or richer inspection for erased `Error`.
- Whole-program devirtualization of dynamic `format_error` calls.

## Final Alpha Gate

Before tagging alpha:

- Search the alpha-supported std surface for `Result<_, Error>`.
- Search reusable modules for `text_error` and `code_error`.
- Search public structs for raw runtime handles.
- Search examples for legacy function names that have receiver selectors.
- Run the full regression suite.
- Build every documented example.

The alpha is not ready if any remaining result requires the reader to remember
that a public API is already known to be temporary.
