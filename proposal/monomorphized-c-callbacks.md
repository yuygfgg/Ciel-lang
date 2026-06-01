# Monomorphized C Callbacks Proposal

This proposal lets Ciel code pass a concrete instance of a generic C ABI
function template as a C callback value. It is a narrow FFI feature. It does not
own actor lowering, actor state safety, async/await lowering, or conversion from
closures to C callbacks.

## Proposal Order

```text
binding-mutability < monomorphized-c-callbacks
unsafe <= monomorphized-c-callbacks[C callback declarations]
```

`binding-mutability` supplies the final local-binding rules used by generic
function bodies. `unsafe` owns imported C declarations, raw pointer casts, and
calls through foreign function pointers. This proposal only adds a way to name
and emit a monomorphized C ABI function item.

## Problem

C APIs often accept a function pointer plus an untyped context pointer:

```c
typedef int (*CompareFn)(const void *left, const void *right, void *ctx);
int c_sort(void *items, size_t len, size_t stride, CompareFn compare, void *ctx);
```

Ciel can model the imported function pointer type, but a reusable standard
library wrapper sometimes needs a typed helper function whose C-visible
signature is concrete while its body is generic. Today a generic function can be
called from Ciel after monomorphization, but there is no source-level expression
for "the concrete C ABI function pointer for this generic instance", and mono
collection does not know that a generic function instance is needed when it is
only passed to C.

Without this feature, every typed wrapper must be hand-specialized or moved into
compiler code. That is unnecessary for ordinary FFI callback adapters.

## Goals

1. Allow non-exported generic functions with C ABI bodies.
2. Allow Ciel code to refer to a concrete monomorphized function item as a value.
3. Ensure the concrete instance is emitted even when it is only passed to C.
4. Keep exported C symbols non-generic and stable.
5. Keep closure-to-C-callback conversion out of scope.
6. Keep actor and async/await lowering out of scope.

## Non-Goals

1. Removing actor compiler builtins.
2. Implementing async/await dispatch.
3. Passing Ciel closures directly as C callbacks.
4. Making callbacks from arbitrary C threads safe.
5. Supporting generic exported C symbols.
6. Supporting type parameters in C-visible parameter or return types in the
   first implementation.

## Syntax

Allow non-exported `extern "C"` function definitions:

```rust
extern "C" c::c_int compare_items<T>(
    *const void left_raw,
    *const void right_raw,
    *void ctx_raw,
) {
    ...
}
```

This defines a C ABI function template. It is not an imported declaration and
not an exported symbol. Each used type argument list produces one internal C ABI
function.

Add explicit type application for function-item expressions:

```rust
compare_items::<Item>
```

Generic calls may keep the existing `f<T>(args)` syntax. The `::<...>` form is
used when the result is a function value rather than a call.

Example:

```rust
type CompareFn = extern "C" c::c_int fn(*const void, *const void, *void);

CompareFn compare = compare_items::<Item>;
```

## Semantics

A generic C ABI function body is a template. A type-applied function item such
as `compare_items::<Item>`:

1. resolves `compare_items` to a generic function item;
2. checks generic arity;
3. checks generic constraints;
4. substitutes the type arguments into the function body and function type;
5. has the resulting C function-pointer type;
6. records that the concrete instance must be monomorphized and emitted;
7. lowers to the generated internal C symbol for that instance.

`export extern "C"` remains the spelling for user-visible C symbols. Exported C
ABI functions cannot be generic because C needs one stable symbol name and one
concrete signature. Imported declarations in `extern "C" { ... }` remain
non-generic declarations of external C symbols.

## C ABI Restrictions

The first implementation is intentionally conservative:

1. A generic C ABI function body may be generic.
2. The C-visible parameter and return types must be valid C ABI types under the
   existing C ABI rules.
3. `void` by value remains invalid in C ABI parameters.
4. Closure types remain invalid in C ABI parameters and return types.
5. Exported generic C ABI functions are rejected.
6. Imported generic C declarations are rejected.
7. Type parameters may appear in the body.
8. Type parameters may not appear in the C-visible callback signature in the
   first implementation.

This keeps the ABI surface stable: generic typing is a Ciel-side implementation
detail, while C sees a concrete function pointer.

## Safety Model

This proposal does not make foreign callbacks safe by itself.

1. Calling an imported C function remains an unsafe operation according to the
   `unsafe` proposal.
2. Raw context pointers passed to C remain owned by the wrapper that constructs
   them.
3. A callback body that casts `*void` back to a Ciel type must do so inside an
   unsafe block.
4. If a C library may invoke the callback after the wrapper returns, the wrapper
   must keep the context alive for that duration.
5. If a C library may invoke the callback from a non-Ciel thread, that thread
   must be attached to the Ciel runtime before executing Ciel code.
6. A C callback must not capture Ciel stack references. Generic C ABI function
   items are named functions and have no closure environment.

The key safety boundary is explicit: this proposal gives the programmer a
function pointer value, but it does not prove that the C library's callback
lifetime, threading, or context-pointer contract is correct.

## Type Checking

Type checking adds a function-item type application expression:

```text
Expr ::= FunctionItem "::" TypeArgList
```

The expression is valid only when the receiver is a function item or a qualified
function item. It is rejected for variables, closures, dynamic interface calls,
method values, and arbitrary expressions.

For `f::<A, B>`:

1. resolve `f` to a generic function template;
2. check the type argument count;
3. substitute generic parameters in the function signature;
4. check generic constraints;
5. produce a `Ty::Function { abi, ret, params }`;
6. produce a typed function item that records the template `DefId` and concrete
   type arguments.

If an expected function-pointer type is present, ordinary assignability checks
ensure that the ABI and signature match.

## Monomorphization And Codegen

Monomorphization must treat type-applied function items as uses of the concrete
generic instance. This differs from ordinary generic calls because the instance
may be needed even when no Ciel call expression invokes it.

Required lowering:

1. Store type-applied function items in THIR with `def_id`, concrete type args,
   and the substituted function type.
2. During mono collection, instantiate the generic function body and mark the
   instance as emitted.
3. During codegen, lower the expression to the generated instance symbol.
4. For non-exported `extern "C"` generic instances, emit an internal C ABI
   function with deterministic mangling based on the original name and type
   arguments.
5. Keep `export extern "C"` non-generic so exported C names remain stable and do
   not depend on Ciel type mangling.

The existing generic-function instance naming machinery can be reused. The main
change is that function values must trigger instantiation just like calls.

## Diagnostics

Examples:

```text
generic function item `compare_items` needs explicit type arguments when used
as a value
```

```text
`extern "C"` generic function `compare_items` cannot be exported; remove
`export` or provide a concrete non-generic wrapper
```

```text
type-applied expression must be a generic function item
```

```text
cannot pass Ciel ABI function `f::<T>` where `extern "C" ... fn(...)` is
expected
```

```text
generic C ABI callback parameter type cannot mention type parameter `T`
```

## Implementation Plan

1. Extend the parser with postfix `::<TypeArgs>` on expression function items.
2. Add an AST/HIR/THIR node for type-applied function items, or reuse the
   generic-function item representation with a flag that distinguishes value use
   from call use.
3. Relax C ABI validation to allow non-exported `extern "C"` function bodies and
   generic non-exported `extern "C"` templates.
4. Keep imported and exported C ABI declarations non-generic.
5. Type-check `f::<Args>` by reusing generic function substitution and
   constraint checking.
6. Reject type parameters in C-visible callback signatures for the first
   implementation.
7. Teach mono collection that type-applied function values instantiate and emit
   their generic bodies.
8. Teach codegen to emit generic C ABI instances and lower function-item values
   to their generated symbols.

## Tests

1. A generic internal C ABI callback can be assigned to an `extern "C" ... fn`
   value and passed to an imported C helper.
2. A type-applied generic callback is emitted even when it is only passed as a C
   function pointer.
3. Exported generic C ABI functions are rejected.
4. Imported generic C ABI declarations are rejected.
5. Type application on non-functions is rejected.
6. ABI mismatch between Ciel ABI and `extern "C"` function pointers is rejected.
7. Type parameters in C-visible callback signatures are rejected.
8. A callback from an unsafe wrapper can recover typed values from `*void` only
   inside an unsafe block.
