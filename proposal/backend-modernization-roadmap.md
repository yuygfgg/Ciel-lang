# Backend Modernization Roadmap

This proposal records the long-term backend plan for the post-alpha compiler.
It is a roadmap rather than a source-language feature. Its purpose is to make
the next backend work explicit before large implementation changes begin.

The current compiler lowers the monomorphized program directly to one generated
C translation unit and relies on BDWGC/libgc. That path has been valuable for
getting the language to alpha, and the next stage should preserve its strongest
property: C ABI interop remains handled by the target C compiler. The long-term
backend direction in this roadmap is therefore not a near-term LLVM rewrite.
Instead, the C backend should become a serious multi-translation-unit backend
with a stable Ciel ABI, precise per-frame GC roots, sharded generated headers,
and a build driver that can compile generated C units in parallel.

## Proposal Order

```text
package-runtime-compiler-organization < backend-modernization-roadmap
async-await <= backend-modernization-roadmap[future layout and async lowering]
resource-management <= backend-modernization-roadmap[cleanup lowering]
monomorphized-c-callbacks <= backend-modernization-roadmap[C ABI wrappers]
```

`package-runtime-compiler-organization` supplies the current package and native
build model that the new driver must extend. `async-await` and
`resource-management` own the source semantics for futures and affine cleanup;
this roadmap only owns their physical lowering. `monomorphized-c-callbacks`
owns the source-level callback feature; this roadmap owns the backend ABI path
used to emit such functions.

## Problem

The current backend has four structural limits:

1. It emits one generated C translation unit. This is simple, but compile time
   and memory use will scale poorly as programs grow.
2. A naive split into many C files plus one giant generated header would only
   move the bottleneck from the C source file to the header parser.
3. BDWGC/libgc provides a simple working memory model, but conservative stack
   and heap scanning can retain false references and does not provide the
   intended long-term precise GC model.
4. The current C emission path is not organized around generated artifacts,
   root maps, object descriptors, or build units, so these backend products are
   hard to test and compile independently.

The compiler needs a better C backend before it needs a different backend. The
new backend shape should keep C interop easy while removing the single-TU and
conservative-GC limits.

## Goals

1. Define a stable Ciel internal ABI and Ciel-owned data layout.
2. Add precise GC roots through per-frame shadow root tables, not per-root
   linked-list updates.
3. Use MMTk as the precise-GC heap manager and collector, with Ciel-generated
   shadow frames as the root source.
4. Add exact heap tracing through generated type descriptors or trace
   functions.
5. Split generated C into many translation units that can compile in parallel.
6. Avoid one large generated header by sharding type, unit, and GC metadata
   declarations by dependency.
7. Extend the build plan to handle generated header shards, generated C source
   units, generated GC metadata, runtime libraries, native package targets, and
   parallel compilation.
8. Preserve the existing user-facing FFI model by continuing to rely on the
   target C compiler for `extern "C"` ABI details.
9. Keep frontend work limited to bug fixes and small metadata plumbing needed
   by precise GC and multi-TU lowering.
10. Postpone any stable backend IR until the C backend proves that local
   planning structures are insufficient.

Everything in this stage should justify itself by serving one of two
deliverables: precise GC or parallel compilation of generated C. ABI notes,
codegen-local plans, descriptor metadata, and artifact graphs are supporting
work, not independent architecture goals.

## Non-Goals

1. Replacing the C backend with LLVM in this stage.
2. Implementing a target-specific C ABI classifier in the Ciel compiler.
3. Parsing arbitrary C record layout as part of this roadmap.
4. Generating one umbrella header included by every generated C file.
5. Maintaining GC roots by linking or unlinking one node per GC variable on
   every assignment.
6. Writing a custom collector from scratch before evaluating MMTk.
7. Implementing a moving or generational GC in the first precise-GC milestone.
8. Rewriting source syntax, the type checker, resolver, or monomorphization
   model.
9. Removing BDWGC before the precise-GC path has stress-test coverage.
10. Introducing a new mandatory backend IR as a prerequisite for precise GC or
    parallel C compilation.
11. Removing the possibility of a future LLVM backend. This roadmap does not
    block LLVM; it simply does not make LLVM the next required step.

## Ciel ABI And Data Layout

The first deliverable is a written Ciel ABI for generated C units. The ABI must
be C-expressible and deterministic so that separately generated translation
units can agree on every Ciel-owned value.

The ABI should specify:

1. the physical representation of scalars, pointers, slices, fixed arrays,
   structs, enums, closures, dynamic interfaces, generated futures, and erased
   `void` values;
2. alignment and padding rules for Ciel-owned aggregates;
3. internal function symbols, linkage, visibility, and deterministic mangling;
4. return lowering, including out-pointers for arrays and large aggregates;
5. argument lowering, including by-pointer passing for large immutable values;
6. how `never` and `void` are represented at call boundaries;
7. closure call and environment conventions;
8. async future creation, run, cleanup, and result storage conventions;
9. GC descriptor attachment for heap objects;
10. shadow-frame and safepoint conventions.

This ABI is an internal implementation ABI, not the C ABI. Any declaration
marked `extern "C"` or `export extern "C"` remains governed by the target C ABI
and should continue to be emitted as C declarations that the C compiler lowers.

## Codegen-Local Plans, Not A New IR

This roadmap does not require a new stable MIR or backend IR. The existing
checked, THIR-like, and monomorphized structures should remain the input to the
C backend. The next stage should add small, task-specific planning structures
inside or immediately around C codegen instead of introducing a general-purpose
lowered IR.

The useful planning products are:

1. a Ciel ABI layout plan for Ciel-owned types and call boundaries;
2. a function GC plan that lists shadow-frame root slots, generated C
   temporaries that can hold GC pointers, frame maps, and safepoint sites;
3. a type tracing plan that emits object descriptors or trace functions;
4. a generated artifact plan that owns C source units, sharded headers, GC
   metadata sources, and their include dependencies;
5. a build artifact plan that tells the driver which generated C files can be
   compiled in parallel.

These plans are metadata and ownership records, not a second semantic
representation of the program. They do not need expression-level operations,
basic blocks, or a new optimizer. In particular, the GC root plan should be
computed after or during C lowering because the C backend may introduce
temporary variables that do not exist in THIR but still need to be rooted.

Escape analysis, affine legality checks, and user-facing diagnostics should
stay in their existing semantic phases for this stage. The first precise-GC
implementation may use conservative function-level root lifetimes and can defer
last-use or storage-placement optimization. If later work needs serious
destination-passing optimization, stack allocation, or a second backend, that
should be proposed as a separate backend IR project.

## Multi-Translation-Unit C Backend

The generated program should become a directory of generated artifacts rather
than one C string. This is driven by the generated artifact plan, not by a new
program IR. A conceptual output layout is:

```text
build/ciel/
    abi.h
    types/
        T_<type-key>.h
    units/
        <unit-key>.h
        <unit-key>.c
    gc/
        G_<type-key>.h
        G_<type-key>.c
        F_<unit-key>.c
    shims/
        <shim-key>.h
        <shim-key>.c
```

The exact names are not normative, but the generated outputs should be
deterministic and cache-friendly.

### Codegen Units

The first implementation may use one generated C source per Ciel source module.
Longer term, the compiler should treat this as a codegen-unit policy rather
than a language rule.

Ownership rules should be deterministic:

1. ordinary non-generic functions belong to their defining module's unit;
2. generic instances belong to the template owner or to a deterministic
   instance owner chosen by the compiler;
3. closure thunks belong to the owning function's unit;
4. async run and cleanup functions belong to the async function or async
   closure owner;
5. dynamic interface shims and retained closure wrappers belong to the unit
   that owns the generated wrapper key;
6. resource cleanup helpers belong to the owning type or a generated support
   unit;
7. exported C ABI definitions remain externally visible, while internal helpers
   stay `static` whenever possible.

The backend should be free to merge tiny units or split very large units later.
This is an implementation policy, not a source-level semantic promise.

### Header Sharding

The backend must not generate one large header that every C source includes.
Instead, generated C should include only the declarations it actually needs.

`abi.h` should stay small. It should contain only common ABI definitions,
runtime declarations, primitive helper macros, and shared Ciel runtime types
that are needed broadly.

Type headers should be generated per type or per small type pack. A type header
contains the forward declarations, full layout, and required dependent includes
for one Ciel-owned type instance.

Unit headers should contain only cross-unit callable prototypes owned by that
unit. Functions used only inside one unit should remain `static` in the source
file and should not appear in a unit header.

GC metadata headers and sources should be sharded like types. The compiler
should avoid one giant GC metadata table source unless a later linker or runtime
registration design proves that it is not a bottleneck.

### Type Dependencies

Type header dependencies follow storage edges:

1. a by-value aggregate field needs the full definition of the field type;
2. a fixed array needs the full element type definition;
3. a pointer, nullable pointer, function pointer, or opaque handle needs only a
   forward declaration for the pointed-to nominal type;
4. a slice needs the slice descriptor definition and only the element
   declaration required by pointer use;
5. recursive value layout is already rejected by the language; recursive
   pointer graphs use forward declarations.

The compiler should build a type dependency graph and emit minimal includes
instead of relying on source declaration order.

## MMTk-Backed Precise Shadow-Frame GC

The precise-GC path should use MMTk for heap management and collection, while
generated C supplies exact roots through one shadow frame per function that has
GC roots live across safepoints. This is different from a per-root linked list.
The frame is pushed once on function entry and popped once on function exit.

A representative shape is:

```c
typedef struct CielGcFrame {
    struct CielGcFrame *prev;
    const CielFrameMap *map;
    void **slots;
    uint32_t safepoint_id;
} CielGcFrame;
```

Generated functions with roots allocate a fixed root slot array:

```c
void *ciel_roots[N] = {0};
CielGcFrame ciel_frame = {
    .prev = ciel_tls_gc_frame,
    .map = &ciel_frame_map_this_function,
    .slots = ciel_roots,
    .safepoint_id = 0,
};
ciel_tls_gc_frame = &ciel_frame;
```

Function exit restores the previous frame:

```c
ciel_tls_gc_frame = ciel_frame.prev;
```

Assignments to rooted values are ordinary stores into known slots. There is no
per-assignment linked-list update.

```c
ciel_roots[0] = value;
```

The first implementation may use function-level root lifetimes. That is precise
with respect to root type and root location, but conservative with respect to
last use inside a function. Later optimizations can clear slots at scope end,
after moves, after last use, or use safepoint live bitmaps.

### Safepoints

Safepoints include:

1. GC allocation calls;
2. runtime calls that may allocate or trigger collection;
3. calls to Ciel functions that may allocate;
4. async suspension points;
5. explicit GC polls on long-running loops when needed;
6. callbacks from foreign code into Ciel after the thread has attached to the
   runtime.

Before a safepoint, generated C must ensure root slots contain the current
values for live GC references. After a safepoint, generated C should reload
values from root slots when a collector may have updated them in a later moving
configuration. The first collector is non-moving, but this reload discipline
keeps the lowering model future-compatible.

Functions with no GC roots live across safepoints do not need shadow frames.
Leaf functions that cannot allocate and cannot call allocating code do not need
shadow frames.

### Heap Tracing

Precise roots are not enough by themselves. Heap objects must also be traced
exactly.

In precise mode, Ciel heap objects should be allocated through an MMTk-backed
runtime API. Objects must carry an object header or external metadata that
identifies a Ciel type descriptor. Type descriptors should provide:

1. object size and alignment;
2. pointer field locations for fixed-layout types;
3. element descriptors for arrays and raw storage;
4. trace functions for enums, closures, futures, dynamic interfaces, and other
   layout-dependent values;
5. whether the object is pointer-free and can use no-scan allocation.

Runtime APIs that currently accept only `size` and `align` for copied Ciel
values must become descriptor-aware before precise GC can be correct. This
includes future results, actor messages, async channel queues, task groups,
mutex and atomic storage, boxed errors, and similar runtime containers.

The first precise collector should be non-moving. Moving and generational
collectors require write barriers, relocation rules, and careful handling of
interior pointers and slices; those are intentionally postponed.

### MMTk Binding

MMTk is a Rust library while the current Ciel runtime is C. The precise-GC
implementation must define a stable boundary between generated C, the C runtime,
and the Rust MMTk binding.

Generated C should call C ABI runtime entry points such as:

```c
void *ciel_gc_alloc(const CielTypeDesc *desc, size_t size, size_t align);
void ciel_gc_safepoint(void);
int32_t ciel_gc_thread_attach(void);
void ciel_gc_thread_detach(void);
```

Exact names are not normative. Internally, these entry points are implemented
by a Rust static library or equivalent runtime component that embeds MMTk.

The binding must provide MMTk with:

1. allocation entry points for generated C;
2. a Ciel object model and object header interpretation;
3. root enumeration by walking each thread's `CielGcFrame` chain;
4. global and runtime root enumeration;
5. object tracing through generated Ciel type descriptors and trace functions;
6. mutator thread attachment and safepoint coordination;
7. a first non-moving collector plan, such as MarkSweep or another suitable
   non-moving MMTk configuration;
8. stress modes that force collection at frequent or every allocation points.

The shadow-frame design is the root source. MMTk is the heap manager,
collector, and tracing work scheduler. LLVM stack maps are not required for
this roadmap.

## Runtime And BDWGC Transition

BDWGC should remain available while the precise-GC path is developed. The
compiler should support at least two runtime modes during the transition:

```text
--gc=bdwgc
--gc=precise
```

Exact flag names are not normative.

The BDWGC mode continues to use the existing allocation path. The precise mode
uses generated shadow frames, type descriptors, exact heap tracing, and the
MMTk-backed runtime binding.

The runtime migration should avoid changing source-language semantics. If a
runtime container stores Ciel values, it must eventually store or receive enough
descriptor information for exact tracing.

## C Interop

This roadmap deliberately preserves the current C interop advantage. Ciel FFI
declarations continue to lower through generated C declarations and calls, so
the target C compiler remains responsible for the platform C ABI.

The compiler should not implement a general C ABI classifier. It also should
not require a C layout oracle for the first multi-TU and precise-GC milestones.
C spelling types continue to be emitted as C spelling in generated C. If future
work needs C layout information outside ordinary generated C compilation, that
work should be proposed separately.

Generated C ABI functions and imported C calls must still participate in the GC
root discipline. If a call may allocate or re-enter Ciel, it is a safepoint. If
the call is trusted not to retain pointers, existing `noescape` annotations can
continue to inform escape analysis, but they do not remove the need to keep live
Ciel references rooted across the call.

## Build Plan And Driver

`BuildPlan` must stop treating generated C as the only compiler output. It
should become a multi-artifact plan that can contain:

1. generated ABI headers;
2. generated type headers;
3. generated unit headers;
4. generated C source units;
5. generated GC metadata headers and sources;
6. runtime libraries;
7. native package CMake targets;
8. package and source inputs;
9. debug, profile, target, and feature configuration.

For executable and shared-library output, the driver should compile generated
C sources in parallel through the build system. For object output, the driver
must define whether it emits one combined object, an archive, or a directory of
objects.

The user-facing `--emit-c` behavior should be revised carefully. A multi-TU
backend naturally emits a directory, not a single file. The compiler may offer a
debug-only single-file concatenation mode, but the primary generated-C output
should preserve the real multi-file shape.

Candidate debugging flags include:

```text
--save-generated-dir
--emit-c-dir
--single-tu
--gc=bdwgc
--gc=precise
```

Exact flag names are not part of this roadmap, but the driver must make backend
and GC selection visible enough for fixture tests and debugging.

## Phasing

The work should stay split into two independently flaggable tracks. They share
the Ciel ABI and generated metadata model, but neither track should require a
new backend IR.

### Precise GC Track

1. Specify the Ciel-owned data layout, object header model, descriptor model,
   shadow-frame convention, and safepoint convention.
2. Add descriptor-aware type metadata generation and runtime allocation API
   stubs while BDWGC remains the default collector.
3. Add codegen-local function GC plans, frame-map metadata, and safepoint
   classification in the current single-TU C backend.
4. Emit per-frame shadow roots in generated C, initially with function-level
   root lifetimes and a non-moving collector assumption.
5. Add the MMTk runtime binding with a first non-moving collector plan.
6. Add exact heap tracing and migrate runtime copied-value containers to carry
   type descriptors.
7. Add GC stress tests and keep precise GC behind an explicit flag until the
   runtime and standard library are covered.

### Parallel C Compilation Track

1. Specify the Ciel function ABI, symbol mangling, linkage, visibility, and
   cross-unit declaration rules.
2. Introduce `GeneratedProgram` or an equivalent multi-artifact output model.
3. Split generated type declarations, unit declarations, and GC metadata into
   sharded headers and sources.
4. Split generated function bodies into multiple C translation units.
5. Extend `BuildPlan` and the driver to compile generated C sources in
   parallel.
6. Add differential tests while single-TU and multi-TU modes both exist.

### Promotion

Precise GC and multi-TU emission should be promoted separately. The compiler
can make one path the default before the other if its tests and runtime support
are ready first.

## Testing Strategy

The test strategy should grow with the migration:

1. Existing language fixtures must keep passing through the current C backend
   while precise GC and multi-TU modes are developed behind explicit flags.
2. Precise GC tests should support collection at every allocation through the
   MMTk-backed runtime mode.
3. GC tests should cover closures, slices, arrays, async frames, futures,
   actor/channel queues, dynamic interfaces, boxed errors, raw storage,
   standard containers, and runtime-owned copied values.
4. Generated C tests should check that rooted locals, codegen temporaries,
   frame maps, safepoints, and descriptor-aware allocations are emitted where
   required.
5. Multi-TU fixture tests should validate cross-unit calls, generic instances,
   closure thunks, async helpers, dynamic interface shims, resource cleanup
   helpers, and exported C ABI functions.
6. Generated header tests should check that no generated C file includes a
   global umbrella header and that type headers include only required
   dependencies.
7. Differential tests should compare single-TU and multi-TU output for selected
   programs while both modes exist.
8. FFI tests should continue to exercise scalar values, C spelling aliases,
   small structs, large structs, by-value arguments, struct returns, function
   pointers, callbacks, and exported C ABI functions.
9. Build tests should verify parallel source compilation, generated-directory
   preservation, object/shared/executable outputs, and native package targets.

## Risks And Mitigations

### ABI Drift

Generated units may disagree on Ciel-owned aggregate layout or call lowering.
Mitigation: specify Ciel data layout first, generate ABI conformance tests, and
keep Ciel ABI definitions in small shared headers with deterministic names.

### Header Bottlenecks

Splitting one C file into many C files can fail if every file includes one large
generated header. Mitigation: shard type, unit, and GC metadata declarations;
forbid a global generated umbrella header in tests.

### Excessive Header Count

One header per type may produce too many tiny files for some programs.
Mitigation: start with deterministic per-type headers for simplicity, then add
small type packs or module-local type packs as a build-time optimization.

### GC Root Bugs

Missing roots can cause rare memory corruption. Mitigation: use non-moving GC
first, collect at every allocation in stress tests, clear root slots
conservatively, keep BDWGC available for comparison, and add targeted tests for
each runtime container that stores Ciel values.

### Over-Retention

Function-level root lifetimes can keep objects alive longer than necessary.
Mitigation: accept this in the first precise-GC milestone, then add scope-end,
move, last-use, or safepoint-bitmap root clearing once correctness is stable.

### Runtime API Drift

Runtime containers that copy Ciel values may keep only size and alignment.
Mitigation: migrate these APIs to accept type descriptors before enabling
precise GC for programs that use them.

### Frontend Churn

Backend migration can accidentally trigger source-language redesign. Mitigation:
only make frontend changes needed to preserve existing semantics in precise GC
and multi-TU lowering; track source-language changes in separate proposals.

### Accidental IR Project

GC and multi-TU planning can grow into a hidden general-purpose IR. Mitigation:
keep planning structures narrowly scoped to ABI layout, root maps, descriptors,
generated artifacts, and build dependencies. Do not add expression semantics,
optimization passes, or a second lowering pipeline without a separate proposal.

## Open Questions

1. Which MMTk non-moving plan should be the first precise-GC target?
2. What exact size threshold should make aggregate returns and arguments use
   out-pointers or by-pointer passing in the Ciel ABI?
3. Should initial codegen units be source modules, package modules, SCCs, or
   compiler-chosen packs?
4. Should type headers be strictly per type at first, or should the first
   implementation already group small dependent types?
5. How should generic instances be assigned to units when they are used from
   multiple modules?
6. Should GC metadata be emitted in type companion C files, unit companion C
   files, runtime registration calls, or linker-visible tables?
7. How precise should first-generation root liveness be: function-level,
   lexical-scope-level, or safepoint-bitmap-level?
8. Which runtime containers need descriptor-aware API changes before precise GC
   can be enabled for the standard library?
9. How should the C runtime and Rust MMTk binding be linked on supported
   targets?
10. How should `--emit-c` behave in a multi-TU world?
11. What small codegen-local planning structures are enough before a real
    backend IR becomes justified?
12. When should `design.md` stop describing the implementation as one generated
    C translation unit using BDWGC?

## Acceptance Criteria

This roadmap is complete when:

1. Ciel ABI and data layout are documented.
2. The precise-GC mode can emit shadow-frame roots, frame maps, type
   descriptors, safepoints, and descriptor-aware allocations.
3. The MMTk-backed precise-GC mode can enumerate shadow-frame roots and trace
   heap objects without BDWGC.
4. GC stress tests pass for representative closures, async programs, standard
   containers, runtime copied values, and FFI-heavy programs.
5. The compiler can emit a multi-file generated C program without a giant
   umbrella header.
6. The build driver can compile generated C units in parallel and link them
   with runtime and native package targets.
7. Existing FFI source experience remains unchanged.
8. `design.md` is updated to describe the accepted backend and runtime model
   once the implementation no longer matches the current single-C-TU BDWGC
   description.
