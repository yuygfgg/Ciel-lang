# Monomorphized C Callbacks Proposal

This proposal lets Ciel standard-library code pass a monomorphized generic C
ABI function as a C callback value. The goal is to remove compiler-known
lowering for APIs such as `/std/actor.spawn_actor` by making the missing
callback glue an ordinary language and FFI feature.

## Proposal Order

```text
capability-erased-closures < monomorphized-c-callbacks
pure-library-message <= monomorphized-c-callbacks
unsafe <= monomorphized-c-callbacks[C callback declarations]

monomorphized-c-callbacks :> actor-stdlib-lowering[dispatch callback]
actor-owned-state < actor-stdlib-lowering[spawn_actor semantics]
```

The callback feature does not own actor safety. It only supplies the missing
generic C ABI callback mechanism. Actor state ownership is owned by
`actor-owned-state`; actor message cloning is owned by `pure-library-message`.
Retained closure handler types are provided by `capability-erased-closures`.
When the unsafe proposal is active, imported C runtime hooks used by this
proposal are declared in `unsafe extern "C"` blocks and called inside safe
standard-library wrappers.

The `actor-stdlib-lowering` step should not freeze the old clone-state
`spawn_actor<S: Message, M: Message>` semantics. If `actor-owned-state` is
accepted, `/std/actor` lowering must use the owned-state dispatch shape
described below.

## Problem

The actor runtime is already shaped like a typed Ciel layer over an untyped C
runtime:

```c
int32_t ciel_actor_spawn(
    CielActor **out,
    void *state,
    void *handler,
    void (*dispatch)(void *state, void *handler, void *message, int32_t *failed)
);
```

The missing piece is the typed dispatch callback. For each actor instantiation
the callback needs to cast raw pointers back to `S`, `M`, and the retained
handler type, call the handler, and store the next state:

```c
static void dispatch(void *state_raw, void *handler_raw, void *message_raw,
                     int32_t *failed) {
    S *state = state_raw;
    Handler *handler = handler_raw;
    M *message = message_raw;

    Result<S, Error> result = (*handler)(*state, *message);
    if (result is Err) {
        *failed = 1;
        return;
    }
    *state = result.Ok;
}
```

Today the compiler recognizes `spawn_actor` and emits this dispatch thunk as an
actor-specific special case. The same operation should be expressible as a
generic C ABI function in `/std/actor`.

## Goals

1. Let a generic Ciel function body use the C ABI for its callable signature.
2. Let Ciel code refer to a concrete monomorphized instance as a function value.
3. Ensure monomorphized callback instances are emitted even when they are only
   passed to C, not called from Ciel.
4. Keep exported C symbols non-generic and stable.
5. Keep closure-to-C-callback conversion out of scope; callbacks are named
   function items.

## Syntax

Allow non-exported `extern "C"` function definitions:

```rust
extern "C" void actor_dispatch<S: Message, M: Message>(
    *void state_raw,
    *void handler_raw,
    *void message_raw,
    *c::c_int failed,
) {
    ...
}
```

This defines a C ABI function template, not an imported external symbol and not
an exported C symbol. Each used type argument list produces one internal C
function.

Add explicit type application for function-item expressions:

```rust
actor_dispatch::<S, M>
```

`::<...>` is chosen because plain `f<T>` is ambiguous with relational
expressions. Generic calls may keep the existing `f<T>(args)` syntax; type
application is needed when the result is a function value rather than a call.

Example with an alias:

```rust
type ActorDispatch = extern "C" void fn(*void, *void, *void, *c::c_int);

ActorDispatch dispatch = actor_dispatch::<S, M>;
```

## Semantics

A generic C ABI function with a body is a template. It has no standalone C
symbol. A type-applied function item such as `actor_dispatch::<State, Msg>`:

1. checks generic arity and constraints;
2. substitutes the type arguments into the function body and signature;
3. has the resulting function-pointer type;
4. records that the concrete instance must be monomorphized and emitted;
5. lowers to the generated C symbol for that instance.

`export extern "C"` remains the spelling for user-visible C symbols. Exported C
ABI functions cannot be generic, because C needs one stable symbol name and one
concrete signature.

Imported declarations in `extern "C" { ... }` remain non-generic declarations of
external C symbols.

## C ABI Signature Restrictions

The first implementation should be conservative:

1. A generic C ABI callback body may be generic, but its C-visible parameter and
   return types must be valid C ABI types under the existing C ABI rules.
2. `void` by value remains invalid in C ABI parameters.
3. Closure types remain invalid in C ABI parameters and return types.
4. Exported generic C ABI functions remain invalid.
5. Type parameters may appear in the body freely. The first slice should reject
   type parameters in the C-visible callback signature itself.

This keeps the feature sufficient for actor dispatch, where the C ABI signature
is `*void`-based and the typed values are recovered inside the body.

## Actor Lowering Without Compiler Builtins

With this proposal, `/std/actor` can be written as ordinary Ciel code over C
runtime hooks:

```rust
import /std/c as c;
import /std/meta;

unsafe extern "C" {
    opaque struct CielActor;

    c::c_int ciel_actor_spawn(
        *?*CielActor out,
        *void state,
        *void handler,
        extern "C" void fn(*void, *void, *void, *c::c_int) dispatch,
    );
    c::c_int ciel_actor_send(*CielActor actor, *void message);
    c::c_int ciel_actor_stop(*CielActor actor);
    c::c_int ciel_actor_join(*CielActor actor);

    ?*void ciel_box_copy(usize size, usize align, *const void source);
}
```

The standard library owns typed cloning and boxing:

```rust
type ActorDispatch = extern "C" void fn(*void, *void, *void, *c::c_int);

extern "C" void actor_dispatch<S: Message, M: Message>(
    *void state_raw,
    *void handler_raw,
    *void message_raw,
    *c::c_int failed,
) {
    *S state = state_raw as *S;
    *(Result<S, Error> |(S, M): Message|) handler =
        handler_raw as *(Result<S, Error> |(S, M): Message|);
    *M message = message_raw as *M;

    Result<S, Error> result = (*handler)(*state, *message);
    switch (result) {
        case Ok(next):
            *state = next;
        case Err(_):
            *failed = 1;
    }
}

export Result<Actor<M>, Error> spawn_actor<S: Message, M: Message>(
    S initial_state,
    Result<S, Error> |(S, M): Message| handler,
) {
    S state = clone_message(&initial_state)?;
    Result<S, Error> |(S, M): Message| handler_copy = clone_message(&handler)?;

    ?*void state_box = ciel_box_copy(type_size<S>(), type_align<S>(), &state as *const void);
    if (state_box == null) {
        return Err(Code(5));
    }
    ?*void handler_box = ciel_box_copy(
        type_size<Result<S, Error> |(S, M): Message|>(),
        type_align<Result<S, Error> |(S, M): Message|>(),
        &handler_copy as *const void,
    );
    if (handler_box == null) {
        return Err(Code(5));
    }

    ?*CielActor raw = null;
    ActorDispatch dispatch = actor_dispatch::<S, M>;
    c::c_int rc = ciel_actor_spawn(&raw, state_box, handler_box, dispatch);
    if ((rc as i64) != 0) {
        return Err(Code(rc as i64));
    }
    return Ok({ handle: raw as *void });
}
```

`send<M: Message>` can follow the same pattern as `/std/channel`: clone the
payload, box it with `ciel_box_copy`, and pass the raw pointer to the C runtime.
`stop` and `join` are direct C calls.

After this migration, the compiler no longer needs to recognize the name
`spawn_actor`. It only needs to implement generic C callback function values.

## Interaction With Actor-Owned State

The callback mechanism remains valid if actor state moves from clone-state to
owned-state semantics. Only the typed dispatch body changes.

An owned-state dispatch uses the same C-visible ABI:

```c
void (*dispatch)(void *state, void *handler, void *message, int32_t *failed)
```

but recovers a mutable state pointer and calls an in-place handler:

```rust
extern "C" void actor_dispatch_owned<S, M: Message>(
    *void state_raw,
    *void handler_raw,
    *void message_raw,
    *c::c_int failed,
) {
    *S state = state_raw as *S;
    *(Result<void, Error> |(*S, M): Message|) handler =
        handler_raw as *(Result<void, Error> |(*S, M): Message|);
    *M message = message_raw as *M;

    Result<void, Error> result = (*handler)(state, *message);
    switch (result) {
        case Ok(_):
            return;
        case Err(_):
            *failed = 1;
    }
}
```

In that model, `spawn_actor` does not call `clone_message` for `S`. It boxes
the actor-owned state according to the rules in `actor-owned-state`. `send`
still clones `M`, and the retained handler value still needs the callback-safe
capability required by the actor API.

This means `monomorphized-c-callbacks` can land before actor-owned state as a
general FFI feature, but the actor-specific stdlib lowering should wait until
the actor state semantics are settled. Otherwise the stdlib migration would
encode the obsolete `S: Message` clone-state API and need another rewrite.

## Thread Attachment

This proposal does not make arbitrary C libraries safe to call Ciel callbacks
from unknown threads. A C thread that calls a Ciel callback must already be
attached to the Ciel runtime.

The actor runtime satisfies this by calling `ciel_thread_attach` in the worker
thread before invoking the dispatch callback and `ciel_thread_detach` before the
thread exits. Other runtime facilities must provide the same guarantee or expose
explicit attach/detach wrappers.

## Type Checking

Type checking should add a function-item type application expression:

```text
Expr ::= FunctionItem "::" TypeArgList
```

The expression is valid only when the receiver is a function item or a qualified
function item. It is rejected for variables, closures, dynamic interface calls,
and arbitrary expressions.

For `f::<A, B>`:

1. resolve `f` to a generic function template;
2. check the type argument count;
3. substitute generic parameters in the function signature;
4. check generic constraints;
5. produce a `Ty::Function { abi, ret, params }`;
6. produce a typed function item that records the template `DefId` and concrete
   type arguments.

If an expected function-pointer type is present, ordinary assignability checks
ensure the ABI and signature match.

## Monomorphization And Codegen

Monomorphization must treat type-applied function items as uses of the concrete
generic instance. This is the important difference from today's generic calls:
the instance may be needed even when no Ciel call expression invokes it.

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
generic function item `actor_dispatch` needs explicit type arguments when used
as a value
```

```text
`extern "C"` generic function `actor_dispatch` cannot be exported; remove
`export` or provide a concrete non-generic wrapper
```

```text
type-applied expression must be a generic function item
```

```text
cannot pass Ciel ABI function `f::<T>` where `extern "C" ... fn(...)` is
expected
```

## Implementation Plan

1. Extend the parser with postfix `::<TypeArgs>` on expression function items.
2. Add an AST/HIR/THIR node for type-applied function items, or reuse
   `GenericFunction` with a flag that distinguishes value use from call use.
3. Relax C ABI validation to allow non-exported `extern "C"` function bodies and
   generic non-exported `extern "C"` templates.
4. Keep imported and exported C ABI declarations non-generic.
5. Type-check `f::<Args>` by reusing generic function substitution and
   constraint checking.
6. Teach mono collection that type-applied function values instantiate and emit
   their generic bodies.
7. Teach codegen to emit generic C ABI instances and lower function-item values
   to their generated symbols.
8. Add `/std/runtime` or `/std/c` boxing hook such as `ciel_box_copy`, then move
   `/std/actor` from compiler builtins to ordinary Ciel code after the
   `actor-owned-state` direction has settled.
9. Delete actor-specific `TExprKind::ActorSpawn`, `ActorSend`, `ActorStop`, and
   `ActorJoin` after the stdlib actor fixtures pass through the ordinary call
   path.

## Tests

1. A generic internal C ABI callback can be assigned to an `extern "C" ... fn`
   value and passed to an imported C helper.
2. A type-applied generic callback is emitted even when it is only passed as a C
   function pointer.
3. Exported generic C ABI functions are rejected.
4. Type application on non-functions is rejected.
5. ABI mismatch between Ciel ABI and `extern "C"` function pointers is rejected.
6. `/std/actor` can spawn, send, stop, and join without any compiler-recognized
   actor call path.
