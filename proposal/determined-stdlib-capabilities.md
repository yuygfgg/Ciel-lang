# Determined Standard-Library Capabilities Proposal

This proposal follows the implemented Inferred Capability Types (ICT) feature.
It uses determined capability parameters to make selected standard-library
interfaces more expressive, and it reduces compiler special cases that now can
be routed through normal capability solving.

The first two targets are:

1. make async awaitability expose its output through `Awaitable<Out = _>`;
2. add `/std/iter` with an `Iterator<Item = _>` capability family.

The cleanup target is to keep only intrinsic compiler boundaries hardcoded.
High-level stdlib policy and API composition should move back to stdlib code.

## Proposal Order

```text
inferred-capability-types < determined-stdlib-capabilities[determined params]
async-await <= determined-stdlib-capabilities[awaitable output]
local-type-holes <= determined-stdlib-capabilities[hidden binding spelling]
```

ICT is a hard prerequisite because this proposal relies on determined interface
parameters, hidden constraint bindings, and opaque constrained returns.

`async-await` is a soft baseline: this proposal does not redesign async
lowering, but it changes the public capability shape used by `await`,
`block_on`, `spawn`, and async combinators.

## Problem

Ciel now has the type-system machinery needed to express associated-type-like
relationships without adding associated items. Some standard-library surfaces
still predate that machinery:

- `Awaitable<Out>` carries the output as an ordinary explicit parameter even
  though the awaitable receiver determines it.
- Iterator examples in the ICT design are not yet a standard-library module.
- Type checking, mono, and codegen still contain repeated stdlib-name checks
  for high-level concepts that should be capability queries.

This leaves two problems. First, generic APIs must name types that are already
determined by another type. Second, the compiler grows more coupled to the exact
spelling of stdlib APIs than necessary.

## Goals

1. Make awaitable output inference an ordinary ICT capability query.
2. Add a static iterator abstraction whose item type is determined by the
   iterator receiver.
3. Use opaque constrained returns for iterator adapters instead of exposing
   nested concrete adapter types.
4. Consolidate canonical stdlib identity checks behind small helpers.
5. Remove high-level hardcoded stdlib API checks when a capability query can
   express the same fact.
6. Preserve existing async runtime lowering, resource safety, and static
   dispatch semantics.

## Non-Goals

1. Moving async frame construction or runtime future ABI into stdlib.
2. Moving `meta::Repr` / `meta::RefRepr` normalization into stdlib.
3. Adding dynamic iterator objects in the first iterator implementation.
4. Adding generic associated types, projection syntax, or type equality
   constraints.
5. Changing C output for its own sake; generated C only needs semantic
   equivalence and test coverage.

## Determined Awaitable Output

Keep `awaitable_future` as the unsafe runtime boundary, but make `Out`
determined by the awaitable receiver:

```ciel
export unsafe interface<A -> Out> *void awaitable_future(*const A awaitable);
export interface Awaitable<Out> = awaitable_future<Out>;
```

Existing concrete impls keep the same source-level arity for non-receiver
arguments:

```ciel
unsafe impl<T> awaitable_future<T>(*const Future<T> future) {
    return unsafe { ciel_future_from_handle(future->handle) as *void };
}

unsafe impl<T> awaitable_future<Result<T, Error>>(*const Task<T> task) {
    return unsafe { ciel_task_future_from_handle(task->handle) as *void };
}
```

Callers can still write the explicit output:

```ciel
A: Awaitable<Result<T, Error>>
```

But generic APIs that need to name the output without exposing it as an
explicit source parameter should use a named ICT binding:

```ciel
A: Awaitable<Out = _>
```

Recommended stdlib surface:

```ciel
export Out block_on<A: Awaitable<Out = _> + Abortable>(A future);

export Result<Task<T>, Error> spawn<T, A: Awaitable<Result<T, Error>> + Abortable>(
    A body
);

export interface SelectableFuture<Out> =
    Awaitable<Out> + CancelSafe + Abortable;
```

`await expr` should ask the capability layer for the determined `Out` of
`expr`'s type. It should not infer the output by separately recognizing every
standard future wrapper.

Candidate async APIs enabled by this shape:

- `async::map`
- `async::then`
- `async::timeout`
- `async::race`
- `async::join2`
- `async::select2`
- `async::detach`

Acceptance criteria:

- `await`, `block_on`, and generic async helpers derive awaitable output
  through the ICT solver.
- `spawn` validates its `Awaitable<Result<T, Error>>` body constraint through
  the same capability path instead of a separate future-name path.
- `SelectableFuture<Out = _>` works for generic losing arms in select-like
  APIs.
- Ambiguous awaitable output diagnostics name the `Awaitable::Out` binding
  rather than printing two indistinguishable expanded types.
- Runtime lowering still owns future handle extraction and async frame details.

## `/std/iter`

Add a standard iterator package whose item type is determined by the iterator
receiver.

Core surface:

```ciel
export enum Next<Item> {
    Item(Item),
    Done,
}

export interface<I -> Item> Next<Item> next(*I iter);
export interface Iterator<Item> = next<Item>;
```

The item type of a concrete iterator is unique:

```ciel
impl next<i64>(*Range iter) { ... }
impl next<u8>(*Range iter) { ... } // error
```

Initial iterator types:

- `Range`
- `Once<T>`
- `Empty<T>`
- `SliceIter<T>`
- fixed-array iterators if the array API is ready

Initial adapters:

- `map`
- `filter`
- `take`
- `enumerate`
- `zip`
- `chain`
- `flatten` once nested hidden bindings remain readable

Initial consumers:

- `count`
- `fold`
- `find`
- `any`
- `all`
- `collect` only for collection types that already have clean construction APIs

Adapter state structs can hide derived item types with ICT:

```ciel
interface<F, In -> Out> Out map_call(*F f, In value);
interface Mapper<In, Out> = map_call<In, Out>;

struct MapIter<I: Iterator<In = _>, F: Mapper<In, Out = _>> {
    I iter;
    F f;
}
```

Public adapter constructors should prefer opaque constrained returns:

```ciel
export _: Iterator<Out> map<I: Iterator<In = _>, F: Mapper<In, Out = _>>(
    I iter,
    F f
) {
    return { iter: iter, f: f };
}
```

Acceptance criteria:

- The item type of an iterator is determined by the iterator receiver.
- Duplicate or overlapping `next` impls with different item types are rejected
  by determined-parameter coherence.
- Opaque iterator adapter returns preserve distinct identity per defining
  function and type arguments.
- Static dispatch through generic `Iterator<Item = _>` constraints works in
  typeck, mono, and codegen.
- Diagnostics name the iterator family and `Item` binding where possible.

## Hardcoding Cleanup

The compiler should still know about canonical stdlib identities when the
feature is intrinsic, but scattered high-level stdlib-name checks should be
reduced.

Keep compiler-owned:

- async frame construction, `await` lowering, and runtime future handle ABI
- resource affine checks, escape analysis, and lifecycle validation
- `meta::Repr` / `meta::RefRepr` normalization and lowering
- dynamic interface object layout and vtable ABI
- ICT hidden-parameter solving, determined coherence, and opaque return
  identity
- C ABI and extern safety checks

Move toward capability queries:

- awaitable output extraction
- abortable and cancel-safe checks for select-like APIs
- selectable future constraints
- iterator item extraction
- stdlib aliases whose expansion is already available through normal interface
  alias resolution

The target shape is a small set of capability helpers used by typeck, mono, and
codegen:

```text
awaitable_output(ty) -> Option<Ty>
is_abortable(ty) -> bool
is_cancel_safe(ty) -> bool
selectable_future_output(ty) -> Option<Ty>
iterator_item(ty) -> Option<Ty>
```

These helpers should live next to capability resolution and use resolved
canonical interface definitions, not local source spelling. Code outside that
layer should not repeat raw checks for `awaitable_future`,
`cancel_safe_marker`, `abort_future`, or `next` unless it is establishing the
canonical identity itself.

Acceptance criteria:

- The compiler has one narrow canonical-identity path for stdlib capability
  families.
- Typeck, mono, and codegen ask capability helpers for determined outputs
  instead of reconstructing them from type names where practical.
- Tests cover renamed imports and aliases to prove behavior depends on resolved
  capability identity rather than source spelling.
- Any hardcoded stdlib check left behind is documented as an intrinsic boundary.
