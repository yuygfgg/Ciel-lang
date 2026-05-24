# Void As Unit Proposal

This proposal changes Ciel's `void` from a special "no value" marker into a
proper zero-size, single-value type.

The goal is to remove API splits such as `must` versus `must_void`,
`expect` versus `expect_void`, and later `error_context` versus
`error_context_void`, while preserving C ABI `void` lowering.

## Problem

`void` is currently valid as a function return type and as a type argument, such
as `Result<void, E>`, but plain locals, fields, and parameters of type `void` are
invalid.

That means generic functions cannot naturally handle `T = void`:

```rust
export T expect<T, E>(Result<T, E> value, []char message) {
    switch (value) {
        case Ok(result):
            return result;
        case Err(error):
            panic(message);
    }
}
```

For `T = void`, the `Ok` value has no runtime payload, `result` has no storage,
and `return result` should lower to `return;`. Today this shape requires a
separate helper:

```rust
export void expect_void<E>(Result<void, E> value, []char message);
```

Every generic helper around `Result<T, E>` repeats the same split.

## Proposal

`void` is a normal inhabited type with exactly one value.

```rust
void done = {};
```

The value has no runtime storage. It may appear in locals, parameters, fields,
enum payloads, generic substitutions, and return positions.

The C backend erases `void` values:

```rust
void f() {
    return {};
}
```

lowers like:

```c
void f(void) {
    return;
}
```

`void` remains the spelling for a C ABI `void` return. The language-level change
is that `void` is also a valid zero-size value type inside Ciel.

## Unit Literal

The existing empty struct literal spelling is the unit value:

```rust
void value = {};
```

`return;` remains valid in a `void` function and is equivalent to:

```rust
return {};
```

`return Ok;` remains valid for `Result<void, E>` when `Ok` is the `Result` success
variant instantiated with a `void` payload.

## Result Helpers

With `void` as a real value type, `/std/result` can use one set of helpers:

```rust
export T expect<T, E>(Result<T, E> value, []char message) {
    switch (value) {
        case Ok(result):
            return result;
        case Err(error):
            panic(message);
    }
}

export T must<T, E>(Result<T, E> value) {
    return expect(value, "must failed");
}
```

Both of these work:

```rust
i64 value = must(read_number());
must(close(fd));
```

where `close(fd)` returns `Result<void, Error>`.

The specialized `must_void` and `expect_void` helpers become unnecessary
compatibility wrappers during migration:

```rust
export void must_void<E>(Result<void, E> value) {
    must(value);
}
```

They can be removed after the standard library and tests are updated.

## Parameters, Locals, and Fields

`void` values are allowed in ordinary value positions:

```rust
void ignore(void value) {
    return value;
}

struct Marker {
    void tag;
}

Marker marker = { tag: {} };
```

These values have no runtime storage. They exist so generic code can remain
uniform.

A function parameter list containing only zero-size parameters still lowers to a
valid C function signature. The backend may omit erased parameters from internal
monomorphized functions. For `extern "C"` declarations, `void` parameters are
invalid except for the existing C spelling of an empty parameter list, because C
does not have a by-value `void` parameter.

## Enums and Patterns

Enum variants may contain `void` payloads at the type level:

```rust
enum Result<T, E> {
    Ok(T),
    Err(E),
}
```

For `Result<void, Error>`, `Ok` carries the single `void` value and has no
runtime payload.

Both concrete and generic code may bind a `void` payload:

```rust
void expect_done<E>(Result<void, E> result) {
    switch (result) {
        case Ok(value):
            return value;
        case Err(error):
            panic("expected success");
    }
}
```

The binding has type `void` and no storage. A concrete `Result<void, E>` switch
may also use the payloadless spelling:

```rust
void expect_done<E>(Result<void, E> result) {
    switch (result) {
        case Ok:
            return {};
        case Err(error):
            panic("expected success");
    }
}
```

The payload-binding form is useful for generic code; the payloadless form is
more readable in concrete `void` code.

## Assignment And Calls

Assignments and argument passing involving `void` are type checked normally:

```rust
void a = {};
void b = a;
take_void(b);
```

The generated code performs no data movement. Evaluation order is still
preserved for expressions that produce `void`.

```rust
void value = side_effecting_void_call();
next();
```

The call to `side_effecting_void_call` must still execute before `next`.

## Struct Layout

`void` fields have no runtime size and no addressable storage:

```rust
struct Pair<T> {
    T left;
    void marker;
    T right;
}
```

The `marker` field type checks and can be selected:

```rust
void marker = pair.marker;
```

but taking its address is invalid:

```rust
*void ptr = &pair.marker; // error
```

This avoids exposing fake addresses for erased storage. If a program needs an
addressable sentinel, it should use a normal zero-field struct instead.

## Arrays And Slices

Arrays of `void` are allowed at the type level:

```rust
[4]void markers = [{};];
```

They have length but no element storage. Indexing returns the single `void`
value after performing the normal bounds check.

Slices of `void` are allowed only if the runtime slice representation can carry a
length with a null or sentinel data pointer. If this complicates the first
implementation, `[]void` can remain temporarily unsupported as a staged
restriction. The long-term model should allow it for generic consistency.

## Pointers

`*void` keeps its existing meaning as an opaque pointer target for C interop.
This proposal does not make `void` values addressable.

Rules:

```rust
*void opaque;     // opaque pointer
void value = {};  // unit value
&value;           // error: void values have no addressable storage
```

This preserves the current role of `*void` without turning unit values into
objects with identity.

## Type Checking

The type checker should stop rejecting `void` in plain value positions.

It should still reject:

1. address-of a `void` lvalue;
2. `extern "C"` by-value `void` parameters;
3. operations that require representation, such as direct equality on structs
   containing only erased fields if the current equality rules already reject
   direct aggregate comparison.

`void` unifies like any other concrete type in generics.

```rust
T id<T>(T value) {
    return value;
}

void done = id({});
```

Expected-type inference can infer `T = void` when the expected type is known.

## Lowering

The backend should erase `void` storage while preserving control flow and side
effects.

Examples:

```rust
void x = call();
return x;
```

lowers like:

```c
call();
return;
```

Struct fields of type `void` are omitted from the C struct. Enum payload fields
of type `void` are omitted from the C variant payload. Function parameters of
type `void` are omitted for internal generated functions.

When all fields of a generated C struct would be erased, the backend should emit
a legal placeholder representation for C, but the Ciel type still has no
observable storage.

## Interaction With `?`

The `?` operator already treats a successful `Result<void, E>` as an expression
of type `void`.

With this proposal, that expression can flow through generic helpers:

```rust
Result<void, Error> cleanup() {
    close(fd)?;
    return Ok;
}

void done = must(cleanup());
```

Most call sites should write the statement form:

```rust
must(cleanup());
```

The assignment form is legal for generic uniformity.

## Migration Plan

1. Change the semantic rule so `void` is permitted in locals, parameters, fields,
   and pattern bindings.
2. Teach assignment, return, call, and pattern checking to treat `void` as a
   normal concrete type.
3. Update C lowering to erase `void` locals, parameters, fields, and payloads
   while preserving side effects.
4. Rewrite `/std/result` to keep only generic `must` and `expect`.
5. Keep `must_void` and `expect_void` as wrappers for one transition period.
6. Add regression fixtures for:
   - `must(Result<void, E>)`;
   - `expect(Result<void, E>, message)`;
   - generic identity with `T = void`;
   - enum payload binding where payload type is `void`;
   - struct fields containing `void`;
   - rejection of address-of `void`;
   - rejection of `extern "C"` by-value `void` parameters.

## Open Questions

1. Should concrete `Result<void, E>` pattern matching prefer `case Ok:` in
   diagnostics even though `case Ok(value):` is valid?
2. Should `[]void` be implemented immediately or staged after arrays and fields?
3. Should the language eventually add an explicit `unit` alias for readability,
   or keep `void` as the only spelling?
4. Should taking the address of a zero-field struct remain valid while taking the
   address of `void` remains invalid? This proposal says yes.
5. Should exported non-`extern` Ciel functions with erased `void` parameters keep
   ABI-stable placeholder parameters, or may the compiler omit them because their
   ABI is Ciel-internal?
