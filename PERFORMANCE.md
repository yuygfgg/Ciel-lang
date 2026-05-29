# C Codegen Performance Notes

This file records performance issues observed while compiling tutorial examples
through the current C backend. The examples are intentionally small, so the
findings below focus on generated C, LLVM IR, symbol tables, and binary size
rather than process-level wall time.

## Landed Mitigations

- Release builds pass platform dead-code stripping flags. macOS uses
  `-Wl,-dead_strip`; Linux uses `-ffunction-sections`, `-fdata-sections`, and
  `-Wl,--gc-sections`.
- The runtime prelude now hides its runtime symbols with a visibility pragma.
  This keeps unused runtime helpers out of the exported symbol table and makes
  linker stripping more effective.
- Generated Ciel helper functions, closure thunks, retained-closure witnesses,
  and dynamic interface shims now use internal C linkage unless they are
  `export extern "C"` bodies. This gives Clang better local visibility facts and
  keeps release executables/shared libraries from exporting internal `_ciel_*`
  symbols.
- The runtime prelude annotates cold panic paths and allocator-like helpers so
  Clang has better noreturn, cold-code, allocation-size, and aliasing facts.

## Remaining Problems

### Whole Prelude Emission

Every generated C file currently embeds the full runtime prelude. Even tiny
programs that only use formatting start with networking, crypto, actor, channel,
atomic, file, and async helpers in the translation unit.

Dead stripping reduces binary size, but it does not reduce C parse time or LLVM
optimization work. A longer-term fix is to split the runtime into feature
groups or link a runtime library whose objects are already separated by subsystem.

### Heap Allocation For Non-Escaping Values

The backend often lowers short-lived values through `ciel_alloc` or
`ciel_alloc_array` even when the value is only consumed locally. Observed cases
include slice literals, dynamic `printable` boxes, local dynamic interface views,
closure environments, and retained closure wrapper environments.

Clang cannot generally remove these allocations because `GC_malloc` and
`GC_MALLOC_UNCOLLECTABLE` are side-effecting calls. The compiler needs Ciel-level
escape analysis and allocation placement that can choose stack storage or scalar
replacement before C is emitted.

### Dynamic Boxing And Interface Calls

Some local dynamic interface calls are devirtualized by Clang when the concrete
vtable is visible in the same function. That helps narrow tutorial examples, but
it is fragile and disappears across generic, retained-closure, actor, or async
boundaries.

The backend should specialize known dynamic views earlier. For formatting, it
should also avoid building heap-allocated arrays of boxed `printable` values
when the argument list is statically known and used immediately.

### Closure And Retained Closure ABI

Plain closure calls can optimize well when the closure is local. Retained closure
adaptation remains expensive: it creates wrapper records, copies closure records,
and performs indirect calls through witness fields.

Useful follow-up work includes specializing retained closure conversions when the
source closure is known, stack-allocating non-escaping closure environments, and
lowering retained witness forwarding without heap wrappers in local cases.

### Actor And Async Boundaries

Actor and async operations intentionally cross runtime boundaries. Message boxes,
actor state boxes, dispatch queue jobs, async operation records, and notification
messages usually escape by construction.

Escape analysis can still remove some setup allocations around immediate error
paths or known-safe cloning, but the main improvement here is ABI design:
batching small messages, avoiding redundant clone boxes, and generating typed
dispatch entry points with fewer generic wrappers.

### Checked Arithmetic And Panic Paths

Release builds still retain overflow checks in places where the source program
has small constant loops or obvious ranges. Clang eliminates some of this after
inlining, but not all of it.

Ciel-side range facts would let the backend skip checks for proven-safe loop
counters, slice offsets, and simple constant-bounded arithmetic while keeping the
existing checked semantics elsewhere.

### Runtime Initialization Checks

Each runtime allocation helper calls `ciel_runtime_init`, and generated functions
with several allocations can therefore contain repeated initialization guards.
Clang can simplify some repeated checks but not all of them across helper calls.

A future lowering pass could emit one dominating runtime init check for functions
that use the runtime, or split allocator helpers into a checked public entry and
an internal fast path once initialization is known.

## Suggested Order

1. Keep release dead stripping and internal linkage enabled, and add regression
   coverage for stripped release builds on macOS and Linux.
2. Add Ciel-level allocation placement for local slice literals and immediate
   formatting boxes.
3. Stack-allocate non-escaping closure environments and dynamic interface boxes.
4. Specialize retained closure forwarding and known dynamic interface calls.
5. Split the runtime prelude or move it to separately linkable runtime objects.
6. Add range analysis for arithmetic and bounds checks.
