# Inferred Capability Types Proposal

This proposal adds a small associated-type equivalent without adding trait
members, type projections, or a separate associated item namespace.

The design has three parts:

1. interface parameters may declare that later parameters are determined by
   earlier parameters;
2. static capability constraints may bind inferred type arguments with
   `Name = _`;
3. ordinary functions may return an opaque static type with `_: Constraint`.

The first two parts are the core feature. Opaque returns are specified here
because they are the main way to keep iterator and state-machine adapter APIs
from exposing large concrete return types, but they can be implemented later.

## Proposal Order

```text
local-type-holes <= inferred-capability-types[constraint binding spelling]
capability-erased-closures || inferred-capability-types[retained witnesses]
inferred-capability-types || monomorphized-c-callbacks[generic mono collection]
```

Local type holes reserve `_` as the spelling for compiler-solved types. This
proposal reuses that spelling in static capability constraints. Capability
erased closures are independent; they erase closure values while retaining
witnesses, while this proposal keeps values statically typed. Monomorphized C
callbacks and opaque returns both require mono collection to keep track of
generic instances used as values, but neither feature depends on the other.

## Problem

Ciel interfaces are function capabilities. This keeps the language smaller than
a trait system with member types, but it makes a few generic APIs hard to spell.

Without an associated type equivalent, an iterator capability must expose the
item type as an ordinary type parameter:

```ciel
enum Next<Item> {
    Item(Item),
    Done,
}

interface<I, Item> Next<Item> next(*I iter) = .next;
interface Iterator<Item> = next<Item>;
```

Generic constraints can mention `Item`, but composite adapter types then need
to carry every derived type argument in their public type name:

```ciel
Peekable<MapIter<BaseIter, i64, f64>, f64> iter;
```

This is not only verbose. It leaks implementation details and gets worse as
adapters are nested. `Flatten` shows the missing abstraction:

```ciel
struct Flatten<I: Iterator<Inner>, Inner: Iterator<Item>, Item> {
    I inner;
    Option<Inner> current;
}
```

The desired source shape is closer to:

```ciel
struct Flatten<I: Iterator<Inner = _: Iterator<Item = _>>> {
    I inner;
    Option<Inner> current;
}
```

`Inner` and `Item` are real static types. They affect layout, signatures, and
monomorphization. They should not have to appear in the source-level arity of
`Flatten`.

Functions that build adapters have a second problem. Even if `Flatten<I>` hides
its derived parameters, a combinator returning a nested adapter still exposes a
large concrete return type unless the language has an opaque static return:

```ciel
_: Iterator<Item> flatten<I: Iterator<Inner = _: Iterator<Item = _>>>(I iter);
```

That return is not a dynamic interface value. It is one concrete type chosen by
the function body and hidden from callers.

## Goals

1. Express common associated-type patterns using Ciel's existing capability
   model.
2. Keep interfaces as function capabilities, not member containers.
3. Avoid `Self::Item`, `<T as Trait>::Item`, projection normalization, and
   general type equality constraints.
4. Let a receiver type determine one or more capability type arguments.
5. Let declarations bind those inferred type arguments and use them in fields,
   signatures, and additional constraints.
6. Keep hidden derived parameters out of source-level type arity.
7. Add an opaque return spelling for static values whose concrete type should
   stay private.
8. Preserve whole-program monomorphization and static dispatch.

## Non-Goals

1. Generic associated types.
2. Higher-ranked lifetime or borrowing abstractions.
3. Existential dynamic interface values with inferred type arguments.
4. Binding inferred types from negative constraints.
5. General return type inference with a bare `_` return type.
6. Opaque return types for C ABI functions.
7. Opaque return types in interface declarations in the first implementation.
8. A way to write the hidden concrete type of an opaque return from outside its
   defining function.

## Determined Interface Parameters

An interface generic parameter list may contain one `->`. Parameters before the
arrow determine parameters after the arrow:

```ciel
interface<I -> Item> Next<Item> next(*I iter) = .next;
interface Iterator<Item> = next<Item>;
```

For every concrete `I`, there may be at most one concrete `Item` such that
`I` implements `Next<Item>`.

The determinant side may contain more than the receiver:

```ciel
interface<F, In -> Out> Out map_call(*const F f, In value);
interface Mapper<In, Out> = map_call<In, Out>;
```

For every concrete pair `(F, In)`, there may be at most one concrete `Out`.

Grammar sketch:

```ebnf
InterfaceGenericParamList ::=
    "<" InterfaceGenericParam { "," InterfaceGenericParam }
    [ "->" InterfaceGenericParam { "," InterfaceGenericParam } ]
    [ "," ] ">"

InterfaceGenericParam ::= [ "resource" ] Identifier [ ":" ConstraintExpr ]
```

Only interface declarations use this arrow. Structs, enums, type aliases,
functions, and impl generic lists keep the existing generic parameter syntax.

The arrow does not change which parameter is the receiver. The first interface
generic parameter remains the receiver type. Written type arguments on impls,
constraints, aliases, and dynamic interface types continue to bind only the
non-receiver parameters.

Example:

```ciel
impl next<i64>(*Range iter) {
    ...
}
```

The written `i64` binds `Item`; `Range` is still supplied by the first
parameter of `next`.

### Coherence

Determined parameters add a whole-program uniqueness rule.

For an interface declared as:

```ciel
interface<A, B -> C, D> R cap(*A value, B input);
```

the complete impl table must not contain two applicable impls where the same
concrete `(A, B)` can produce different concrete `(C, D)`.

These impls conflict:

```ciel
impl next<i64>(*Range iter) { ... }
impl next<u8>(*Range iter) { ... } // error
```

Generic impls are checked conservatively. If two generic impls may overlap on
the determinant side and may produce different determined types, the program is
rejected unless the existing coherence machinery can prove the determinant
sets are disjoint.

Example:

```ciel
impl<T> next<T>(*VecIter<T> iter) { ... }
impl next<u8>(*VecIter<i64> iter) { ... } // error
```

The second impl overlaps the first at `T = i64` and would give
`VecIter<i64>` two item types.

Duplicate impls that produce the same determined types are still rejected by
the ordinary duplicate-impl rule. Determination is not an overload mechanism.

## Named Constraint Bindings

A static constraint may bind a type argument with `Name = _`:

```ciel
I: Iterator<Item = _>
```

The binding creates a hidden generic type parameter named `Item`. The hidden
parameter is available everywhere an explicit generic parameter of the same
declaration would be available: fields, parameter types, return types, impl
target type arguments, and function bodies.

```ciel
struct Peekable<I: Iterator<Item = _>> {
    I inner;
    Option<Item> cached;
}
```

The source-level arity of `Peekable` is one:

```ciel
Peekable<Range> p;
```

Internally the compiler may represent the concrete instance as:

```text
Peekable<Range; i64>
```

The `; i64` part is not source syntax. It is the solved hidden parameter used
for layout, monomorphization, and diagnostics.

### Grammar

Constraint type arguments gain a binding form:

```ebnf
ConstraintTerm      ::= [ "!" ] Identifier [ ConstraintArgList ]
ConstraintArgList   ::= "<" ConstraintArg { "," ConstraintArg } [ "," ] ">"
ConstraintArg       ::= Type | ConstraintBinding
ConstraintBinding   ::= Identifier "=" "_" [ ":" ConstraintExpr ]
```

`Name = _` is not a named type argument. It is a binding form valid only as a
type argument inside a positive static constraint term. The identifier names a
hidden type parameter, and `_` is the compiler-solved type for that parameter.
Argument matching remains positional; the identifier before `=` does not select
an interface parameter by name.

`Name = _: Constraint` binds `Name` and adds an additional constraint on that
same inferred type:

```ciel
I: Iterator<Inner = _: Iterator<Item = _>>
```

This is equivalent to the constraint set:

```text
I: Iterator<Inner>
Inner: Iterator<Item>
```

with `Inner` and `Item` hidden from source-level type arity.

### Binding Scope

All named constraint bindings in a declaration are collected before resolving
the declaration's types. A hidden parameter may therefore be used in the return
type even though Ciel writes function generic parameters after the function
name:

```ciel
Vec<Item> collect<I: Iterator<Item = _>>(I iter);
```

A hidden parameter name must be unique in its declaration. It must not duplicate
an explicit generic parameter name or another named constraint binding:

```ciel
struct Bad<I: Iterator<Item = _>, J: Iterator<Item = _>> {} // error
struct Bad<Item, I: Iterator<Item = _>> {}                  // error
```

After a hidden parameter is bound, later references use the ordinary name:

```ciel
struct MapIter<I: Iterator<In = _>, F: Mapper<In, Out = _>> {
    I inner;
    F f;
}
```

Repeating `In = _` is an error. Use `In` to refer to the existing hidden
parameter.

Hidden parameters follow the same namespace and shadowing rules as explicit
generic parameters.

### Legal Contexts

Named constraint bindings are valid in static constraints that introduce or
check a generic environment:

1. generic parameter bounds on structs, enums, aliases, functions, and impls;
2. constraints attached to hidden parameters with `Name = _: Constraint`.

Opaque return constraints may mention already-bound hidden names, but they do
not introduce new named constraint bindings in the first implementation.

Named constraint bindings are rejected in:

1. dynamic interface value types;
2. interface alias declarations;
3. impl target type argument lists;
4. explicit generic call or type arguments;
5. closure retained-capability types;
6. casts;
7. negative constraint terms and `-` masked terms.

Examples:

```ciel
struct Good<I: Iterator<Item = _>> {
    Option<Item> cached;
}

Iterator<Item = _> value; // error: dynamic interface type cannot bind `Item`

impl next<Item = _>(*Range iter) { ... } // error: impl target args are not binding sites

void bad<I: !Iterator<Item = _>>() {} // error: negative constraints cannot bind
```

Dynamic interface values still need all non-receiver type arguments supplied:

```ciel
Iterator<i64> iter; // dynamic interface value
```

### Solving Hidden Parameters

Named constraint bindings are source-hidden only when they are functionally
determined by explicit parameters through positive capability constraints.

The compiler builds a dependency graph from determined interface declarations.
For:

```ciel
interface<I -> Item> Next<Item> next(*I iter);
interface<F, In -> Out> Out map_call(*const F f, In value);
```

the graph has these dependencies:

```text
I -> Item
F, In -> Out
```

A hidden parameter is valid only if it can be derived from explicit parameters
and already-derived hidden parameters by repeatedly applying such dependencies.

Valid:

```ciel
struct Peekable<I: Iterator<Item = _>> {
    Option<Item> cached;
}
```

`Item` is determined by explicit `I`.

Valid:

```ciel
struct Flatten<I: Iterator<Inner = _: Iterator<Item = _>>> {
    I inner;
    Option<Inner> current;
}
```

`Inner` is determined by `I`; `Item` is determined by `Inner`.

Valid:

```ciel
struct MapIter<I: Iterator<In = _>, F: Mapper<In, Out = _>> {
    I inner;
    F f;
}
```

`In` is determined by `I`; `Out` is determined by `(F, In)`.

Invalid:

```ciel
interface<T, U> bool related(*const T value, U other);

struct Bad<T: related<U = _>> {
    U value;
}
```

`related` does not declare `T -> U`, so `U` is not derivable from explicit
parameters. Making `U` an explicit generic parameter is required:

```ciel
struct Good<T: related<U>, U> {
    U value;
}
```

Cycles with no explicit source are rejected:

```ciel
struct Bad<A: Uses<B = _>, B: Uses<A = _>> {} // error unless one side is explicit
```

Cycles that are already grounded by explicit parameters are allowed when the
solver can prove a unique solution.

### Instantiation

When a type with hidden parameters is instantiated, callers supply only the
explicit parameters:

```ciel
Peekable<Range> p;
```

The compiler solves hidden parameters from the declaration constraints, the
current generic constraint environment, and the complete impl table. If any
hidden parameter remains unsolved or has more than one solution, instantiation
is rejected.

Diagnostics should show the public type and the failed hidden parameter:

```text
cannot instantiate `Peekable<Range>`: could not infer hidden parameter `Item`
from constraint `Range: Iterator<Item>`
```

When useful, diagnostics may include the internal solved form:

```text
note: `Peekable<Range>` inferred `Item = i64`
```

Explicit type argument lists cannot supply hidden parameters:

```ciel
Peekable<Range, i64> p; // error: `Peekable` has 1 explicit type parameter
```

### Impl Example

An adapter impl uses the hidden type in the impl's own generic list and then
passes it as the ordinary non-receiver type argument of the interface:

```ciel
impl<I: Iterator<Item = _>> next<Item>(*Peekable<I> iter) {
    if (iter->cached.is_some()) {
        return Next::Item(iter->cached.take());
    }

    return next(&iter->inner);
}
```

`Flatten` can express the nested relationship directly:

```ciel
impl<I: Iterator<Inner = _: Iterator<Item = _>>> next<Item>(*Flatten<I> iter) {
    while (true) {
        switch (iter->current) {
        case Option::Some(current):
            switch (next(&current)) {
            case Next::Item(value):
                return Next::Item(value);
            case Next::Done:
                iter->current = Option::None;
            }
        case Option::None:
            switch (next(&iter->inner)) {
            case Next::Item(inner):
                iter->current = Option::Some(inner);
            case Next::Done:
                return Next::Done;
            }
        }
    }
}
```

The example is schematic. The exact `Option` API is library-defined.

## Opaque Constrained Returns

An ordinary Ciel function may use `_: ConstraintExpr` as its return type:

```ciel
_: Iterator<i64> range(i64 start, i64 end) {
    return Range{ start: start, end: end };
}
```

The function returns a concrete type selected by the body. Callers cannot name
that concrete type, but the value statically satisfies the written constraint.

Opaque return syntax is intentionally not `some`; it reuses `_` as the marker
for a compiler-solved type.

Grammar sketch:

```ebnf
FunctionReturnType ::= Type | OpaqueReturnType
OpaqueReturnType   ::= "_" ":" ConstraintExpr

FunctionSignature  ::= FunctionReturnType Identifier [ GenericParamList ]
                       "(" [ ParamList ] ")"
```

The constraint may refer to explicit and hidden generic parameters of the
function:

```ciel
_: Iterator<Item> flatten<I: Iterator<Inner = _: Iterator<Item = _>>>(I iter) {
    return Flatten<I>{ inner: iter, current: Option::None };
}
```

The opaque return constraint does not introduce new named constraint bindings
in the first implementation. Bind derived names in the function generic list
instead.

### Opaque Return Semantics

Each function with an opaque return defines a fresh opaque type family. The
family is keyed by the function identity and its concrete explicit and hidden
generic arguments.

Two calls to the same opaque-returning function with the same concrete generic
arguments have the same source-level opaque type:

```ciel
_ a = range(0, 10);
_ b = range(10, 20);
a = b; // ok
```

Two different functions produce different opaque types even if their bodies
return the same concrete type:

```ciel
_: Iterator<i64> odds();
_: Iterator<i64> evens();

_ a = odds();
a = evens(); // error: distinct opaque return types
```

The value can satisfy generic constraints proven by the opaque return
constraint:

```ciel
usize n = count(range(0, 10));
```

If a dynamic interface value is expected, ordinary dynamic interface coercion
may erase the opaque value:

```ciel
Iterator<i64> dyn = range(0, 10);
```

That coercion is explicit in the expected type. `_: Iterator<i64>` itself is
not a dynamic interface type and does not require dynamic dispatch.

### Return Checking

For each concrete generic instance, every normal return path must return the
same concrete type.

Valid:

```ciel
_: Iterator<Item> peekable<I: Iterator<Item = _>>(I iter) {
    return Peekable<I>{ inner: iter, cached: Option::None };
}
```

Invalid:

```ciel
_: Iterator<i64> choose(bool flag) {
    if (flag) {
        return Range{ start: 0, end: 10 };
    }

    return EmptyRange{};
}
```

The two branches return different concrete types. Use an enum wrapper, a named
adapter, or a dynamic interface value when runtime choice of representation is
required.

An opaque-returning function may return the opaque value from another function.
In that case the returned opaque type identity becomes the concrete return type
for this function, subject to the same single-type rule.

The selected concrete type must satisfy the written constraint expression after
generic substitution. Positive capabilities must be implemented. Forbidden
capabilities must be absent under the existing negative-constraint rules.

### Restrictions

Opaque return types are valid only on ordinary Ciel functions in the first
implementation.

They are rejected on:

1. `extern "C"` declarations and definitions;
2. exported C ABI functions;
3. interface declarations;
4. impl declarations for interface functions;
5. type aliases;
6. struct fields and enum payloads;
7. local declarations.

Returning closures should keep using ordinary erased closure signatures such as
`i64 |(i64)|` or retained closure signatures such as `i64 |(i64): Message|`
unless a later proposal adds callable capability constraints. This proposal's
opaque return constraint describes capabilities, not direct callable syntax.

No bare return inference is added:

```ciel
_ make_iter() { ... } // still invalid
```

Opaque static return requires a written constraint:

```ciel
_: Iterator<i64> make_iter() { ... }
```

## Constraint Solving Order

Semantic analysis uses the existing whole-program shape, with extra steps for
determined parameters and hidden bindings:

1. Resolve imports and collect interface declarations.
2. Record each interface functional dependency from its `->` parameter list.
3. Collect impl declarations.
4. Check ordinary impl signature compatibility.
5. Check determined-parameter coherence across the complete impl table.
6. Lower generic declarations and collect named constraint bindings.
7. Convert each named constraint binding into a hidden generic parameter.
8. Verify that every hidden parameter is derivable from explicit parameters by
   positive determined constraints.
9. During instantiation, solve hidden parameters from concrete explicit
   arguments, the current generic constraint environment, and the complete impl
   table.
10. Type-check bodies with explicit and hidden parameters in scope.
11. For opaque returns, infer the single concrete return type per generic
    instance and check it against the written constraint.
12. Monomorphize using the full canonical type arguments, including hidden
    parameters and opaque return concrete types.

No runtime representation is required for named constraint bindings. They are
compile-time generic parameters.

Opaque returns also have no required runtime representation. They lower to the
selected concrete type. The opacity is a source typing rule.

## Interactions

### Interface Aliases

Interface aliases remain capability views. A named constraint binding used
through an alias is checked after alias expansion.

```ciel
interface Iterator<Item> = next<Item>;

struct Peekable<I: Iterator<Item = _>> {
    Option<Item> cached;
}
```

After expansion, the binding is in the determined `Item` position of `next`.

If alias expansion maps one binding to multiple determined positions, those
positions must solve to the same type. If any mapped position is not derivable,
the binding is rejected.

### Negative Constraints

Negative constraints can mention already-bound names:

```ciel
void check<I: Iterator<Item = _> + !ThreadLocal>() {}
```

They cannot introduce new names:

```ciel
void bad<I: !Iterator<Item = _>>() {} // error
```

The absence of an impl is not a source of type information.

### Dynamic Interface Values

Dynamic interfaces require fixed method signatures. Named constraint bindings do
not appear in dynamic interface types:

```ciel
Iterator<Item = _> value; // error
Iterator<i64> value;   // ok
```

Opaque return values may be coerced to dynamic interface values only by the
ordinary expected-type coercion path.

### Type Holes

`Name = _` is separate from local type holes.

```ciel
_ local = make_value();       // local type hole
I: Iterator<Item = _>         // named constraint binding
_: Iterator<Item> make_iter() // opaque return type
```

The three uses share the same visual marker because each asks the compiler to
solve a type. They are legal in different grammar positions and have different
scopes.

### ABI And Code Generation

Hidden parameters participate in the canonical mono key:

```text
Peekable<Range; i64>
```

Generated C names should include hidden parameters to avoid collisions, but
source diagnostics should print the public type unless the hidden value is
needed to explain an error.

Opaque return functions use the selected concrete return type for internal Ciel
ABI lowering. They cannot be C ABI functions because C needs a concrete
source-visible signature.

## Diagnostics

Examples:

```text
interface `next` determines `Item` from `I`; duplicate impl gives `Range`
both `i64` and `u8`
```

```text
hidden parameter `U` in `Bad<T>` is not determined by explicit parameters
```

```text
named constraint binding `Item = _` is not allowed in dynamic interface type
```

```text
`Peekable` expects 1 explicit type argument, but 2 were supplied
note: hidden parameter `Item` is inferred from `I: Iterator<Item>`
```

```text
opaque return function `choose` returns both `Range` and `EmptyRange`
```

```text
opaque return type `_: Iterator<i64>` cannot be used on an `extern "C"`
function
```

## Implementation Plan

1. Parse `->` inside interface generic parameter lists and store determinant
   and determined parameter ranges.
2. Extend resolved interface declarations with functional dependency metadata.
3. Extend constraint argument parsing with `Identifier = _` and optional
   `: ConstraintExpr`.
4. Reject named constraint bindings outside positive static constraints.
5. Add hidden generic parameters during HIR lowering for declarations that bind
   named constraint bindings.
6. Track source arity and canonical arity separately for generic nominal types.
7. Add a derivability check that every hidden parameter is functionally
   determined by explicit parameters.
8. Extend impl coherence to reject overlapping determined-parameter conflicts.
9. Teach generic instantiation to solve hidden parameters from concrete
   explicit arguments, the current generic constraint environment, and the
   complete impl table.
10. Add `FunctionReturnType::OpaqueConstraint` for `_: ConstraintExpr`.
11. Type-check opaque-returning bodies by collecting the concrete type of each
    return expression and enforcing the single-type rule.
12. Let opaque return values satisfy constraints through the written opaque
    constraint while codegen lowers them as their selected concrete type.
13. Include hidden parameters and opaque concrete return identities in mono
    collection and deterministic name mangling.

## First Slice

The first implementation should include:

1. `interface<I -> Item>` with one arrow per interface declaration;
2. named constraint bindings in generic parameter bounds;
3. hidden generic parameters on structs, enums, type aliases, functions, and
   impls;
4. source arity checks that hide derived parameters;
5. determined-parameter coherence for concrete impls and conservative rejection
   of overlapping generic impls;
6. diagnostics for unsolved, ambiguous, duplicate, or illegal named bindings.

Opaque constrained returns can follow as a second slice:

1. parse `_: ConstraintExpr` in ordinary function return position;
2. reject it in C ABI, interface, impl, field, payload, alias, and local
   contexts;
3. enforce the single concrete return type rule;
4. expose only the written static constraints to callers.

This split lets Ciel gain the associated-type equivalent before taking on the
larger API and diagnostic surface of opaque result types.
