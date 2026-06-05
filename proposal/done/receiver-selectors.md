# Receiver Selectors Proposal

This proposal lets callable declarations provide a receiver-call spelling such
as `map.insert(key, value)` while keeping the semantic model as ordinary calls.
A receiver selector is syntax sugar for rewriting one call expression into
another call expression. It is not an object system, not a capability by
itself, not a method value, and not a general overload mechanism.

## Proposal Order

```text
binding-mutability <= receiver-selectors[receiver addressability]
receiver-selectors <= interfaces[interface calls keep their existing semantics]
```

`binding-mutability` supplies the rules for whether a receiver expression may
be addressed and whether a writable pointer receiver may be formed. Interfaces
continue to own capability queries, impl matching, and dynamic dispatch.
Receiver selectors only add an alternate call spelling for existing callable
declarations.

## Problem

Ciel's standard library is mostly ordinary functions. That keeps the language
small, but it makes receiver-heavy APIs harder to read and remember:

```ciel
hash_map_insert(&map, key, value)?;
byte_buf_push_slice(&buf, data)?;
```

The receiver value already gives useful context, but the function name must
still carry a type prefix and the programmer must remember whether to pass the
receiver by value or by pointer.

Using `interface` as the method system is also the wrong model. Interfaces are
stable capability contracts. They can answer questions such as "does this type
implement `insert_into<K, V>`?" only because they have one declared signature.
Receiver selectors should not infer or create those contracts.

Receiver selector lookup is ad-hoc polymorphism over a declared receiver
position: it chooses one visible desugaring target for this call spelling.
Interface constraints are parametric polymorphism: generic code asks for a
named capability with a fixed signature.

The desired feature is smaller: allow any existing callable declaration that
has an ordinary call form to also expose a receiver-call form. The receiver-call
form desugars back to the ordinary call before normal call semantics are
checked.

## Goals

1. Let functions and interfaces declare one receiver-call spelling.
2. Keep receiver selectors as expression syntax sugar.
3. Keep ordinary function names non-overloaded.
4. Keep interface capability queries tied to interface declarations, not
   selector names.
5. Make selector resolution depend on exactly one declared receiver parameter.
6. Support any parameter position as the receiver, with the first parameter as
   the default.
7. Derive copy, read-only pointer, and writable pointer behavior from the
   declared receiver parameter type.
8. Preserve existing callable-field calls.
9. Keep diagnostics grounded in the equivalent ordinary call.

## Non-Goals

1. Inherent `impl Type { ... }` blocks.
2. `impl Trait for Type` or any new trait system.
3. General function overloading by arity, argument type, or return type.
4. Inferring an `interface` from a selector name such as `.insert`.
5. Method values, bound methods, or selector lookup without a call.
6. Automatic borrowing for ordinary non-receiver function arguments.
7. Treating the receiver parameter name as ordinary name lookup. It is resolved
   only against the callable signature's parameter list.

## Syntax

Add an optional receiver selector after a function signature or interface
signature:

```ebnf
InterfaceDecl    ::= [ "unsafe" ] "interface" GenericParamList
                     InterfaceSignature [ ReceiverSelector ] ";"

FunctionDecl     ::= [ "unsafe" ] [ AbiSpec ] [ "async" ] FunctionSignature
                     [ ReceiverSelector ]
                     ( Block | ";" )

ReceiverSelector ::= "=" ( "." Identifier | Identifier "." Identifier )
```

`= .name` uses the first parameter as the receiver. `= parameter.name` uses
the parameter named `parameter` as the receiver.

The call-site selector may be unqualified or qualified through an import alias:

```ebnf
ReceiverCallExpr     ::= PostfixExpr "." ReceiverSelectorName
                         [ TypeArgList ] "(" [ ArgList ] ")"
ReceiverSelectorName ::= Identifier | QualifiedName
```

The declaration still stores only the final selector identifier. Qualification
at the call site controls which imported namespace contributes selector
declarations.

The receiver parameter name in a declaration is not ordinary name lookup. It is
resolved only against the current callable signature's parameter list. A
parameter written with binding mutability such as `@map` is named `map` for
selector purposes. Imported namespaces, local variables, types, and module
aliases do not participate in this declaration-side lookup.

Function example:

```ciel
export Result<void, Error> byte_buf_push_slice(
    *ByteBuf buf,
    []const u8 data
) = .push_slice {
    ...
}
```

Interface example:

```ciel
export interface<C, K, V> Result<void, Error> insert_into(
    *C target,
    K key,
    V value
) = .insert;
```

Non-first receiver parameters are named explicitly:

```ciel
export Result<bool, Error> hash_map_contains_entry<K: map_key, V>(
    K key,
    *HashMap<K, V> map
) = map.contains {
    ...
}
```

`impl` declarations do not declare selectors. An `impl` only implements the
interface signature it names. If an interface exposes a selector, every impl of
that interface inherits the same receiver-call spelling through the interface
call.

## Desugaring

For a callable declaration:

```text
R f(P0 p0, P1 p1, ..., PN pN) = pI.selector
```

a receiver call:

```ciel
receiver.selector(a0, a1, ...)
```

desugars to an ordinary call to `f`. The receiver expression fills parameter
`pI`. The explicit receiver-call arguments fill every other parameter slot in
declaration order. `= .selector` is equivalent to `= p0.selector`.

For a first-parameter receiver:

```ciel
map.insert(key, value)
```

desugars to:

```ciel
hash_map_insert(&map, key, value)
```

For a named non-first receiver:

```ciel
map.contains(key)
```

desugars to:

```ciel
hash_map_contains_entry(key, &map)
```

The desugared ordinary call is the semantic form. Evaluation order, unsafe
checking, async-call behavior, generic function inference, interface dispatch,
dynamic-interface dispatch, and error propagation all follow that ordinary
call. For the default first-parameter receiver, the visual receiver order and
the ordinary call order coincide. For non-first receiver parameters, the
desugared call's parameter order is the source of truth.

Receiver selectors apply to declarations that already have ordinary call
syntax:

1. A selector on a function desugars to a function call.
2. A selector on an interface desugars to an interface call.

The selector itself does not decide whether the call is static, generic,
dynamic, unsafe, or async. The desugared target call decides that through the
existing language rules.

## Receiver Adaptation

Desugaring may adapt only the receiver expression. Other arguments are checked
exactly as ordinary function-call or interface-call arguments.

For receiver parameter `PI`:

1. If the receiver expression is assignable to `PI` as written, it is passed as
   written.
2. Otherwise, if `PI` is a pointer view of `T` and the receiver expression is an
   addressable `T`, the compiler may insert `&receiver`.
3. For writable pointer receivers such as `*T` or `?*T`, the receiver
   expression must be writable according to normal lvalue and binding-mutability
   rules.
4. For read-only pointer receivers such as `*const T` or `?*const T`, the
   receiver expression only needs to be addressable.
5. Nullable pointer widening and read-only view widening follow ordinary
   assignability rules after the optional receiver address-take.

No nullable pointer is implicitly unwrapped. If the receiver expression is a
`?*T`, it is passed as a nullable pointer only when that is assignable to the
declared receiver parameter.

Receiver adaptation is not an overload-ranking rule. If two selector
declarations differ only in whether the receiver parameter is `T`, `*T`,
`*const T`, `?*T`, or `?*const T` for the same receiver root, the declarations
overlap whenever a source receiver expression could match both. The compiler
must not prefer an exact receiver match over an address-taken receiver match.
Copying, read-only borrowing, and writable borrowing are semantically different
operations, so selector declarations cannot use pointer-ness as an overload
dimension.

## Declaration Rules

A callable declaration with a receiver selector must satisfy these rules:

1. An explicit receiver parameter name must name an existing parameter.
2. Function selectors and interface selectors use the same `= parameter.name`
   syntax.
3. `impl` declarations cannot attach selectors.
4. Each callable declaration may expose at most one receiver selector. Multiple
   public receiver spellings require separate wrapper functions.
5. Imported C declarations cannot attach selectors directly. A safe or unsafe
   Ciel wrapper can attach a selector and preserve the C boundary rules.

The selector name does not enter the ordinary function namespace. It is not a
bare callable name:

```ciel
map.insert(key, value); // receiver-call sugar
insert(map, key, value); // ordinary lookup only
```

## Selector Resolution

Selector resolution exists only to choose a desugaring target. It is not a
capability query and it is not general overload resolution.

Resolution proceeds as follows:

1. Resolve and type-check the receiver expression enough to know its static
   type.
2. Find visible callable declarations with the requested selector name.
3. Filter candidates using only the declared receiver parameter and receiver
   expression type.
4. If there is exactly one candidate, build the desugared ordinary call.
5. Type-check the desugared call through the existing function-call or
   interface-call path.
6. If there is no candidate, report that the receiver type has no such selector.
7. If more than one candidate matches the receiver, report an ambiguity.

Non-receiver arguments never participate in selector choice. This is the rule
that prevents receiver-call sugar from becoming general overload resolution.

A generic interface selector may be the unique desugaring target for many
receiver types. That does not mean those receiver types implement the
interface. It only means the receiver-call spelling can be rewritten to the
ordinary interface call; the existing interface-call rules still decide whether
an impl, generic constraint, or dynamic interface value satisfies that call.

Declarations conflict when they expose the same selector name for overlapping
receiver type patterns, even if their non-receiver arguments differ:

```ciel
export Result<InsertResult<V>, Error> hash_map_insert<K: map_key, V>(
    *HashMap<K, V> map,
    K key,
    V value
) = .insert;

export Result<void, Error> hash_map_insert_default<K: map_key, V>(
    *HashMap<K, V> map,
    K key
) = .insert; // error: overlapping `HashMap<K, V>.insert`
```

Different receiver roots may use the same selector:

```ciel
export Result<InsertResult<V>, Error> hash_map_insert<K: map_key, V>(
    *HashMap<K, V> map,
    K key,
    V value
) = .insert;

export Result<void, Error> byte_buf_insert(
    *ByteBuf buf,
    usize index,
    u8 value
) = .insert; // ok: different receiver root
```

The implementation must be conservative when checking generic overlap. If two
receiver patterns unify for the same selector, reject the second declaration
rather than relying on non-receiver constraints, interface constraints, or
argument counts to disambiguate calls. Constraints are checked after selector
desugaring, not during selector choice.

Pointer view differences do not make receiver patterns disjoint:

```ciel
export usize packet_len(Packet packet) = .len;

export usize packet_len_ref(*const Packet packet) = .len;
// error: `Packet.len` and `*const Packet.len` overlap because `p.len()`
// could be rewritten either as `packet_len(p)` or as `packet_len_ref(&p)`
```

This means a broad interface selector such as `insert_into<C, K, V>(*C, K, V)
= .insert` overlaps a concrete function selector such as
`hash_map_insert<K, V>(*HashMap<K, V>, K, V) = .insert`. Public APIs should
choose one selector owner. If the operation is meant to be queried as a
capability, put the selector on the interface and keep concrete functions as
ordinary helper calls or impl bodies.

## Interface Boundaries

Selectors do not make a type implement an interface. Only `impl` declarations
do that.

Ciel does not have a Type Class system with associated types in this proposal.
If a capability needs names such as `Key` or `Value`, those relationships must
be spelled as ordinary interface type parameters or left for a separate
associated-types proposal.

This function selector:

```ciel
export Result<InsertResult<V>, Error> hash_map_insert<K: map_key, V>(
    *HashMap<K, V> map,
    K key,
    V value
) = .insert;
```

allows:

```ciel
map.insert(key, value)?;
```

It does not imply any `insertable` capability.

If a capability is needed, it must be declared as an interface:

```ciel
export interface<C, K, V> Result<InsertResult<V>, Error> insert_into(
    *C target,
    K key,
    V value
) = .insert;
```

Generic code queries the interface, not the selector:

```ciel
Result<void, Error> put<C: insert_into<K, V>, K, V>(
    *C target,
    K key,
    V value
) {
    target.insert(key, value)?;
    return Ok({});
}
```

The call `target.insert(key, value)` desugars to
`insert_into(target, key, value)` because the visible `insert_into` interface
declares `.insert`. The ordinary interface-call rules then check the
constraint, find the impl, or dispatch through a dynamic interface value.

An implementation remains ordinary:

```ciel
impl insert_into<HashMap<K, V>, K, V>(
    *HashMap<K, V> map,
    K key,
    V value
) {
    return hash_map_insert(map, key, value);
}
```

The impl does not choose a selector. The selector belongs to the interface
callable signature.

## Visibility

A receiver selector has the same visibility as its callable declaration. A
selector declared on a non-exported function or interface is visible only where
that callable is visible. An exported callable's selector is exported with the
callable. Re-exporting a module re-exports the selectors attached to the
module's exported callables.

Selector declarations do not affect interface impl collection. They are
ordinary module-visible call spellings, not whole-program capability proofs.

Import aliasing follows ordinary Ciel namespace rules. An unaliased import can
make exported selectors available to unqualified receiver calls:

```ciel
import /std/map;

table.insert(key, value)?;
```

An aliased import does not add its exported selectors to unqualified receiver
lookup. Use a qualified selector through the alias:

```ciel
import /std/map as map;

table.map::insert(key, value)?;
```

The qualified receiver call above desugars through the selector declarations
exported by the `map` namespace, just as an ordinary call such as
`map::hash_map_insert(&table, key, value)` resolves through that alias. This
keeps aliases as explicit namespace boundaries instead of leaking selectors
into the bare selector set.

## Field Calls

Existing callable-field syntax remains valid:

```ciel
box.value(1)
```

If `obj.name(args)` can be type-checked as an ordinary call through a callable
field named `name`, that interpretation wins. If the field call does not apply,
the compiler may try receiver selector desugaring.

Qualified receiver calls such as `obj.map::insert(args)` are selector calls
only. They do not conflict with ordinary field calls because `obj.map::insert`
is not a field-access expression.

`obj.name` without a following call remains field access only. Receiver
selectors do not create first-class method values.

## Generic Code

Receiver selector calls can appear in generic code when selector resolution can
choose a visible callable declaration from the receiver's known type or from
interface constraints in scope.

Concrete nominal receiver:

```ciel
Result<void, Error> put<K: map_key, V>(*HashMap<K, V> map, K key, V value) {
    map.insert(key, value)?;
    return Ok({});
}
```

Interface-constrained receiver:

```ciel
Result<void, Error> put_any<C: insert_into<K, V>, K, V>(
    *C target,
    K key,
    V value
) {
    target.insert(key, value)?;
    return Ok({});
}
```

A naked generic receiver with no selector-providing constraint has no selector
lookup:

```ciel
void call_insert<T>(T value) {
    value.insert(); // error: no visible `.insert` selector for unconstrained `T`
}
```

## Diagnostics

Diagnostics should show the desugared ordinary call and the selector
declaration that produced it.

For an argument error:

```ciel
map.insert("bad", value)
```

the compiler can report:

```text
error: expected `K`, got `[]const char`
note: selector call desugared to `hash_map_insert(&map, "bad", value)`
note: `.insert` is declared on `hash_map_insert` with receiver parameter `map`
```

For an interface selector:

```text
error: generic constraint not satisfied: `C` does not implement `insert_into<K, V>`
note: selector call desugared to `insert_into(target, key, value)`
note: `.insert` is declared on interface `insert_into` with receiver parameter `target`
```

For missing selectors:

```text
error: no selector `.insert` for receiver type `Packet`
note: selector lookup uses only the declared receiver parameter, not the remaining arguments
```

For overlap:

```text
error: conflicting selector `.insert` for receiver `HashMap<K, V>`
note: selector declarations are not overloaded by non-receiver arguments
```

## Implementation Notes

Parsing can keep the existing postfix shape and recognize selector calls during
type checking. In current syntax, `receiver.selector(args)` already parses as a
call whose callee is a field expression. The type checker can:

1. preserve the existing callable-field path;
2. when that path does not apply, try selector desugaring;
3. construct the desugared ordinary call expression;
4. type-check the ordinary call through the existing function-call or
   interface-call path.

The canonical form used for diagnostics should include any inserted receiver
address-take, for example `hash_map_insert(&map, key, value)`.

Because the desugared call uses existing call checking, receiver selectors do
not need separate code generation. Codegen sees the ordinary function call,
interface call, dynamic-interface call, async call, or unsafe call that would
have been written without the receiver selector.
