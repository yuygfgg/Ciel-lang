# Compiler Refactor Plan

This document is the implementation contract for a large compiler rewrite.
The current compiler can pass many tests, but high-level language concepts
survive too far into C code generation. That phase ordering is the root
architectural bug. The rewrite must make the compiler pipeline explicit,
design-grounded, and hostile to late backend semantic special cases.

## Scope

`design.md` is the semantic contract for this refactor. Existing tests are a
regression net, not a promise to preserve every accidental behavior. When the
current implementation disagrees with `design.md`, fix the implementation and
add design-aligned regression coverage.

The current implementation should be treated as bug-prone. Passing the current
test suite does not mean the current behavior is correct, complete, or worth
preserving. The goal is not a behavior-preserving rewrite. The goal is a
cleaner compiler that implements `design.md` more reliably across feature
combinations.

`proposal/` is not an implementation contract for this refactor. Proposal files
describe future development pressure. Read them only to avoid building an
architecture that blocks future work. Do not implement proposal-only semantics
as part of this rewrite unless the same semantics already exist in `design.md`.

Tests may only move in one direction: more coverage. Do not delete tests, do
not weaken expectations, do not edit fixture inputs to make the new compiler
pass, and do not special-case the harness. New tests are allowed and encouraged
when they cover behavior required by `design.md` or lock down a bug found
during the rewrite.

Everything else may be rewritten aggressively: compiler modules, IRs, standard
library internals, runtime helpers, and generated C structure.

## Bug Fixing Is In Scope

Fixing bugs discovered during the rewrite is a required part of the work. Do
not copy an old behavior into the new pipeline merely because the old compiler
already had that behavior.

Treat these findings as bugs unless `design.md` explicitly says otherwise:

1. a feature combination compiles only because C emission recognizes a
   source-level special case;
2. a construct works in isolation but fails when combined with generics,
   closures, interfaces, `void`, representation operations, actors, slices, or
   ADTs;
3. a semantic decision depends on traversal order, C declaration order, or
   helper discovery order;
4. a generic instance is checked or emitted with stale representation,
   capability, closure, or aggregate information;
5. a runtime value can escape without explicit storage ownership;
6. diagnostics hide a real type, capability, or lowering error behind an
   internal backend failure;
7. generated C is only correct for the tested spelling of a construct rather
   than for the lowered semantics.

For each bug fixed during the rewrite:

1. identify the violated `design.md` rule or compiler invariant;
2. fix the earliest responsible phase, not the C emitter;
3. add or extend a design-aligned regression test;
4. keep the backend invariant that C emission prints already-lowered Core.

Bug fixes must not be deferred just to keep the rewrite smaller. If a bug fix
would require changing an existing test expectation, do not silently edit the
test. Document the conflict and stop for review, because either the test is
encoding an old bug or `design.md` needs clarification.

## Current Failure Mode

The compiler currently has these structural problems:

1. `typeck` owns semantic collection, type lowering, generic inference, impl
   selection, closure capture analysis, stdlib recognition, partial lowering,
   flow checks, and diagnostics.
2. `THIR` is not a stable typed IR. It still carries source-level and runtime
   concepts such as closures, retained closures, dynamic interface
   construction and dispatch, meta projection calls, actor builtins, builtin
   message cloning, and unmonomorphized generic function references.
3. `mono` is not only monomorphization. It calls back into type checking,
   reconstructs aggregate instances, finds retained witness types, and
   normalizes representation markers again.
4. `codegen` is not a printer. It discovers closure layouts, dynamic vtables,
   retained wrappers, witnesses, actor dispatches, slice types, source
   locations, and string literals. It also performs statement lowering for
   expressions that should have been normalized earlier.
5. The backend contains name-based stdlib special cases. A semantic decision
   made in C emission is almost always a phase-ordering bug.

The result is fragile feature composition. New combinations fail because they
depend on C emission traversal order instead of a lowered IR contract.

## Target Pipeline

The final compiler pipeline must be:

```text
source files
  -> tokens / AST
  -> resolved HIR
  -> SemanticProgram
  -> ElaboratedProgram
  -> MonoProgram
  -> CoreProgram
  -> LayoutPlan
  -> CModule
```

Each stage has a narrow contract.

`SemanticProgram` is allowed to contain source-level language concepts. It is
the last stage where unresolved language semantics are allowed.

`ElaboratedProgram` resolves capability evidence, canonical stdlib identity,
normalizable representation requests, inserted control-flow desugarings, and
other obligations that still depend on semantic lookup. It is allowed to
contain generics. It must not contain backend layout decisions.

`MonoProgram` contains only concrete executable function bodies and concrete
aggregate instances. It is allowed to contain typed high-level operations that
are still waiting for Core lowering. It must not contain unresolved generics,
generic type holes, or generic function templates in executable bodies.

`CoreProgram` is the primary backend IR. It is concrete, C-shaped, and free of
high-level Ciel semantics. Everything after `CoreProgram` must be mechanical.

`LayoutPlan` assigns C names, aggregate layouts, enum layouts, closure layouts,
dynamic interface layouts, witness records, helper symbols, and emission order.
It does not inspect source-level semantics.

`CModule` prints C from `CoreProgram` and `LayoutPlan`. It must not select
impls, normalize representation types, discover closure metadata, generate
actor dispatch from stdlib names, or invent witnesses.

The final `src/driver.rs` must run these stages directly. A legacy THIR-to-C
fallback is not an acceptable final state. Transitional adapters are allowed
only inside the migration sequence when they preserve the invariants at the
Core boundary and are deleted before the rewrite is accepted.

## Non-Negotiable Invariants

The compiler must enforce these invariants with assertions and tests:

1. No `Ty::Generic`, `Ty::Hole`, or unresolved `Ty::Unknown` reaches
   `CoreProgram`.
2. No generic function template reaches `CoreProgram` as an executable value.
3. No closure literal reaches C emission. Closure literals must be lowered into
   explicit environment allocation, closure values, and thunk functions before
   `LayoutPlan`.
4. No semantic closure-instance type reaches C emission. If a concrete closure
   identity is still needed for names or diagnostics, it must be represented by
   an explicit lowered closure definition id.
5. No dynamic interface construction semantic node reaches C emission. Dynamic
   boxing must be represented by explicit Core storage and a known vtable id.
6. No dynamic interface call semantic node reaches C emission. Dynamic calls
   must be explicit indirect calls through known vtable slots.
7. No retained closure conversion or retained closure interface call reaches C
   emission. Retained closure values and witness calls must be explicit Core
   data and calls.
8. No meta projection or reconstruction semantic node reaches C emission.
   Representation operations must lower to Core field, payload, switch, copy,
   and constructor operations.
9. No actor semantic node reaches C emission. Current actor behavior, if kept
   as compiler-recognized semantics, must lower before Core exits.
10. No builtin message-clone semantic node reaches C emission. Any selected
    clone behavior must already be an explicit call, witness call, or Core
    operation before layout.
11. No expression lowering may fail in C emission with messages like
    "needs statement lowering". Core lowering must introduce temporaries,
    blocks, switches, and evaluation-order statements before the C printer.
12. The C emitter must not search HIR or original source modules to decide what
    helpers, layouts, or shims exist. It reads only `CoreProgram` and
    `LayoutPlan`.

## Required Modules

Implement these ownership boundaries as separate Rust modules. The module names
below are required unless the rewrite updates this document in the same commit
with an equivalent, explicit module map.

### `semantic`

Responsibilities:

1. Build symbol tables for functions, generic functions, aggregates, aliases,
   interfaces, interface aliases, impls, extern declarations, and canonical
   stdlib definitions.
2. Type-check bodies against `design.md` source semantics.
3. Solve local type holes before the body leaves semantic checking.
4. Preserve enough source spans and semantic paths for diagnostics.
5. Produce a `SemanticProgram` with language-level operations in a controlled
   enum.

It must not:

1. Emit backend helper functions.
2. Decide C layout order.
3. Generate actor dispatch thunks.
4. Encode backend-specific behavior into source-level type checking.

### `evidence`

Responsibilities:

1. Resolve interface aliases and interface views.
2. Select concrete impls for static capability calls.
3. Instantiate generic impls when concrete arguments are known.
4. Represent selected evidence explicitly, including retained closure witness
   requirements when the current language requires them.
5. Report nested diagnostics for failed capability requirements.

It must not:

1. Synthesize fallback impls to rescue backend lowering.
2. Hide failed capability selection behind hard-coded clone or conversion
   paths.
3. Depend on C emission order.

### `repr`

Responsibilities:

1. Recognize canonical representation-related stdlib definitions by definition
   identity, not by user-visible spelling alone.
2. Normalize representation types after substitution points where generic
   types become concrete.
3. Generate representation types for currently specified language constructs.
4. Maintain an expansion budget and emit clear diagnostics when normalized
   structural types would become unreasonable.
5. Lower representation projection and reconstruction requests into Core
   operations.

It must not:

1. Treat user-defined lookalike names as compiler markers.
2. Require C emission to inspect source-level representation requests.

### `mono`

Responsibilities:

1. Build a monomorphization use graph.
2. Instantiate generic functions called from Ciel.
3. Instantiate generic functions referenced as concrete values when that is
   part of the current language.
4. Instantiate generic impl functions selected by `evidence`.
5. Produce concrete aggregate instances reachable from concrete functions,
   signatures, witnesses, dynamic vtables, and representation types.
6. Detect infinite generic growth with a useful instantiation chain.

It must not call the full semantic checker as a black box for normal instance
processing. If generic instantiation needs body checking, extract a reusable
body checker that operates under an explicit substitution environment.

### `core`

Responsibilities:

1. Represent normalized statements, expressions, temporaries, blocks, switches,
   calls, indirect calls, aggregate construction, pointer operations, casts,
   bounds checks, panic calls, and runtime calls.
2. Preserve source spans for runtime checks and diagnostics.
3. Have explicit function ids, local ids, type ids, layout ids, vtable ids,
   closure ids, and witness ids.
4. Carry no high-level Ciel operation that requires semantic lookup.

Required Core categories:

```rust
CoreProgram {
    types,
    functions,
    globals,
    closures,
    vtables,
    witnesses,
}

CoreFunction {
    id,
    abi,
    params,
    ret,
    body,
}

CoreStmt {
    Let,
    Assign,
    Store,
    If,
    Switch,
    Loop,
    Break,
    Continue,
    Return,
    Expr,
}

CoreExpr {
    Local,
    Function,
    Literal,
    AddressOf,
    Load,
    Field,
    Index,
    Call,
    IndirectCall,
    StructValue,
    EnumValue,
    Cast,
    RuntimeCall,
    SizeOf,
    AlignOf,
}
```

The exact Rust enum and struct names may vary only when the same categories are
present and documented in `src/core.rs`. The required property is that Core is
concrete and backend-ready.

### `lower_core`

Responsibilities:

1. Preserve Ciel evaluation order by introducing explicit temporaries.
2. Lower closure literals to:
   - a closure environment type;
   - environment allocation and initialization;
   - a closure value containing environment pointer, call pointer, and witness
     fields when needed;
   - one thunk Core function per concrete closure body.
3. Lower function-to-closure conversion to an explicit empty-environment or
   wrapper value.
4. Lower retained closure conversion to explicit wrapper values and witness
   records when retained closures are part of the current language.
5. Lower static interface calls to direct calls selected by `evidence`.
6. Lower dynamic interface boxing to explicit storage ownership plus a known
   vtable.
7. Lower dynamic interface calls to explicit indirect calls through known
   slots.
8. Lower `?` into explicit switch/return code according to current
   `design.md` semantics.
9. Lower `void` values by preserving evaluation order and removing storage.
10. Lower slice literals, slice subviews, array repeats, and array-to-slice
    conversions into explicit temporaries and checks.
11. Lower representation projection and reconstruction through `repr`.
12. Lower current actor operations into explicit Core calls/data before Core
    exits.

It must not:

1. Implement proposal-only language rules.
2. Leave a backend TODO for the C emitter to finish.

### `layout`

Responsibilities:

1. Assign deterministic C names.
2. Assign aggregate layouts.
3. Assign closure value and environment layouts.
4. Assign dynamic interface value and vtable layouts.
5. Assign witness function and field layouts.
6. Assign slice layouts.
7. Sort declarations and definitions topologically.
8. Report layout cycles before C emission.

Layout must be a planned output, not a side effect of recursive C string
generation.

### `c_emit`

Responsibilities:

1. Print runtime prelude, includes, declarations, layouts, prototypes, helper
   definitions, and function bodies from `CoreProgram` and `LayoutPlan`.
2. Preserve source-location hooks for runtime checks.
3. Emit deterministic, warning-clean C.

It must not:

1. Traverse HIR modules except for already-lowered include data passed in the
   plan.
2. Discover closures, vtables, witnesses, actor dispatches, or representation
   helper code.
3. Run impl lookup, generic instantiation, or representation normalization.
4. Mutate semantic state.

## Feature Requirements

These requirements are about currently specified language behavior. If a
feature is only present in a proposal file and not in `design.md`, it is not a
required deliverable for this refactor.

### Closures

Every closure literal starts as a concrete closure type in semantic checking.
That concrete identity must survive long enough for generic inference,
capability checks, and representation normalization required by current
semantics.

Before Core exits `lower_core`, closure literals must be lowered to explicit
data and functions. The C backend must never inspect a closure body.

Concrete closure captures are by-value snapshots unless `design.md` says
otherwise. Captured bindings are read-only in the closure body. Capturing a
pointer copies the pointer. Capturing a slice must preserve backing storage
when the closure escapes.

### Interfaces And Capabilities

Static capability calls must lower to direct calls before Core.

Dynamic interface values must have an explicit lowered representation with a
data component and dispatch component. Dynamic interface boxing must define
whether the receiver value is addressed in existing storage or copied into
owned storage. Non-pointer values stored in long-lived dynamic interface values
must be rooted or owned. This ownership decision belongs to lowering and escape
analysis, not C emission.

Interface aliases must be expanded by identity and must preserve non-receiver
type arguments through dynamic dispatch.

### Representation Operations

Representation normalization must run at all points where current semantics can
turn a generic type into a concrete representation request:

1. initial type lowering;
2. generic function instantiation;
3. generic impl instantiation;
4. aggregate reconstruction;
5. retained witness signature generation when applicable;
6. Core lowering of representation functions.

User-defined names that look like representation markers must not trigger
compiler behavior. Recognition must use canonical definition identity or
reserved internal marker names created from canonical definitions.

### Actors

Actor behavior currently specified by `design.md` may remain compiler-recognized
before Core. It must not remain compiler-recognized in C emission. Actor
operations must lower to explicit Core data, calls, and runtime hooks before
layout.

If future work moves actor APIs further into the standard library, this
refactor must not block that change. That future move is not part of this
refactor's acceptance criteria.

### Builtin Clone Or Conversion Operations

If the current language requires compiler-selected clone or conversion
operations, selection must finish before Core exits. The backend may print
already-selected calls or witness calls, but it must not choose behavior by
inspecting stdlib names.

### Void

`void` is a real unit value in source semantics and an erased storage type in
Core and C layout. Lowering must preserve evaluation order while removing
storage. No C emitter branch is allowed to discover whether a value was a
source-level implicit void.

### Escape Analysis

Escape analysis must operate on Core or a near-Core form where storage
operations are explicit.

It must understand:

1. address-taking;
2. array-to-slice and slice subviews;
3. dynamic interface boxing;
4. closure environment allocation and captured values;
5. C calls and `noescape`;
6. runtime thread entry storage.

Escape analysis decides storage placement. It does not decide semantic safety.
Semantic safety must already be represented by explicit types, conversions,
evidence, or diagnostics.

## Future-Proofing Boundary

The refactor must make future language proposals easier to implement, but it
must not implement those proposals as hidden side effects.

The architecture passes this requirement only if future features can be added
by changing semantic analysis, evidence selection, monomorphization, Core
lowering, standard library code, or runtime helpers without teaching the C
emitter new source-level concepts.

If a future feature would require `c_emit` to inspect HIR, stdlib names, generic
templates, or semantic expression variants, the refactor has failed.

## Migration Strategy

Large rewrites are allowed. Delete obsolete compatibility layers once the
replacement path passes the existing tests plus any new design-aligned tests.

Required migration order:

1. Add the new IR module skeletons and wire a no-op pass pipeline beside the
   current pipeline. The no-op path exists only to make reviewable commits; it
   must not become a permanent compatibility layer.
2. Define `CoreProgram` and `LayoutPlan` with invariant checks before moving
   features.
3. Move statement normalization first: `void`, `?`, slice subviews, array
   repeats, array-to-slice, and temporary introduction.
4. Move closure lowering out of C emission. Generate Core thunk functions and
   environment layouts before the backend.
5. Move dynamic interface boxing and calls out of C emission. Generate explicit
   vtable records and indirect calls before layout.
6. Move retained closure witnesses out of C emission when retained closures are
   present in current semantics. Generate witness records and wrapper functions
   before layout.
7. Move representation lowering out of C emission. Use `repr` for all current
   projection and reconstruction paths.
8. Move actor lowering out of C emission. Keep pre-Core actor recognition only
   if it reflects current `design.md` semantics.
9. Move builtin clone or conversion lowering out of C emission. C emission
   prints already-selected operations only.
10. Simplify C emission until it has no semantic lookup.
11. Delete old THIR/codegen helper paths that violate the invariants.

Do not postpone deletion of old paths once their replacement passes tests.

## Verification Requirements

Existing tests must remain valid. Do not remove coverage, weaken expectations,
edit fixtures to fit the new implementation, or special-case the harness.

New tests are required when the rewrite fixes a bug, clarifies an uncovered
`design.md` rule, or introduces new internal invariants. New tests must use the
existing discovered fixture system under `tests/cases/**` unless the current
test infrastructure already has a more specific location for compiler-internal
assertions.

New tests must be consistent with `design.md`. Do not add tests for
proposal-only semantics unless those semantics have already been accepted into
`design.md`.

At minimum, the final implementation must pass:

```sh
cargo test
```

Required acceptance checks:

1. Existing closure, generic, ADT, dynamic interface, `void`, representation,
   actor, and capability composition cases keep passing unless a case is
   proven to contradict `design.md`. If such a contradiction exists, stop and
   document it instead of silently editing the test.
2. Core invariant assertions reject any high-level node after Core lowering.
3. C emission has no "needs statement lowering" diagnostics.
4. C emission does not discover closures, vtables, witnesses, actor helpers, or
   representation helpers by walking semantic IR.
5. Monomorphization does not call the full type checker as a black box for
   normal instance processing.
6. Bug fixes discovered during the rewrite have added regression coverage.

## Review Checklist

A rewrite is incomplete if any of these are true:

1. C codegen still pattern-matches semantic nodes such as closures, actors,
   representation calls, retained closure calls, builtin clone operations, or
   dynamic interface construction.
2. Codegen still traverses HIR modules to discover required runtime structures.
3. Monomorphization still calls the full type checker as a black box for normal
   instance processing.
4. A new or existing feature requires changing C declaration emission order
   instead of adding a lowered dependency to `LayoutPlan`.
5. Core contains unresolved generics, type holes, source-only semantic nodes,
   or backend TODO expressions.
6. The backend relies on stdlib names to choose semantic behavior.
7. Tests were removed, weakened, skipped, rewritten, or special-cased to make
   the rewrite pass.
8. Proposal-only behavior was implemented as part of the refactor without a
   matching `design.md` contract.

## Expected End State

The backend prints concrete types and concrete functions from a plan. Language
features are implemented in semantic analysis, evidence selection,
monomorphization, representation normalization, and Core lowering.

The successful rewrite does not freeze current half-implemented shortcuts. It
creates a compiler where:

1. current `design.md` semantics are easier to test in composition;
2. discovered design mismatches can be fixed with targeted regression coverage;
3. future proposals can add semantic rules without turning C emission back into
   a semantic phase;
4. generated C order, helper definitions, closure layouts, dynamic dispatch,
   and runtime hooks are products of `CoreProgram` plus `LayoutPlan`;
5. backend complexity decreases as features are lowered earlier.
