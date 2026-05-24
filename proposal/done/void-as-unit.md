# Void As Unit Proposal

This proposal changes Ciel's `void` from a special "no value" marker into a
proper zero-size, single-value type.

The goal is to let one generic `must` and `expect` implementation handle
`Result<void, E>` while preserving C ABI `void` lowering.

## Problem

Before this change, `void` was valid as a function return type and as a type
argument, such as `Result<void, E>`, but plain locals, fields, and parameters of
type `void` were invalid.

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
and `return result` should lower to `return;`. Previously this shape required
separate non-generic helper APIs.

## Proposal

`void` is a normal inhabited type with exactly one value.

```rust
void done;
```

The value has no runtime storage and has no literal spelling. It may appear in
locals, parameters, fields, enum payloads, generic substitutions, and return
positions.

The C backend erases `void` values:

```rust
void f() {
    return;
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

## Implicit Void Value

`void` has no literal. The empty struct literal remains a struct literal and is
not reused as a unit literal.

Concrete `void` locals are implicitly initialized:

```rust
void value;
```

Concrete code does not explicitly initialize or assign `void` values:

```rust
void value = make_done(); // error
value = make_done();      // error
```

Use the expression statement form when evaluation is needed:

```rust
make_done();
```

Generic code may still contain value flow that instantiates to `void`; the
backend erases that flow after preserving side effects.

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

## Parameters, Locals, and Fields

`void` values are allowed in ordinary value positions:

```rust
void ignore(void value) {
    return value;
}

struct Marker {
    void tag;
    i64 value;
}

Marker marker = { value: 7 };
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
            return;
        case Err(error):
            panic("expected success");
    }
}
```

The payload-binding form is useful for generic code; the payloadless form is
more readable in concrete `void` code.

## Initialization, Assignment, And Calls

Concrete `void` locals and fields are implicit. They cannot be explicitly
initialized or assigned:

```rust
void a;
void b = a; // error
a = b;      // error
```

Argument passing and returns can still use existing `void` expressions:

```rust
void a;
take_void(a);
```

The generated code performs no data movement. Evaluation order is preserved for
generic expressions that instantiate to `void`.

```rust
side_effecting_void_call();
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
pair.marker;
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
[4]void markers;
```

They have length but no element storage. Indexing returns the single `void`
value after performing the normal bounds check.

Slices of `void` carry their length with a null data pointer.

## Pointers

`*void` keeps its existing meaning as an opaque pointer target for C interop.
This proposal does not make `void` values addressable.

Rules:

```rust
*void opaque;     // opaque pointer
void value;       // implicit void value
&value;           // error: void values have no addressable storage
```

## Type Checking

The type checker should stop rejecting `void` in plain value positions.

It should still reject:

1. explicit initialization or assignment of concrete `void` values;
2. address-of a `void` lvalue;
3. `extern "C"` by-value `void` parameters;
4. operations that require representation, such as direct equality on structs
   containing only erased fields if the current equality rules already reject
   direct aggregate comparison.

`void` unifies like any other concrete type in generics.

```rust
T id<T>(T value) {
    return value;
}

void done;
id(done);
```

Expected-type inference can infer `T = void` when the expected type is known.

## Lowering

The backend should erase `void` storage while preserving control flow and side
effects.

Examples:

```rust
T forward<T>(T value) {
    return value;
}
```

For `T = void`, this lowers like:

```c
void forward(void) {
    return;
}
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

must(cleanup());
```

Call sites write the statement form:

```rust
must(cleanup());
```

## Migration Plan

1. Change the semantic rule so `void` is permitted in locals, parameters, fields,
   and pattern bindings.
2. Reject explicit concrete initialization and assignment of `void` values.
3. Teach return, call, and pattern checking to treat `void` as a single implicit
   value.
4. Update C lowering to erase `void` locals, parameters, fields, and payloads
   while preserving side effects.
5. Rewrite `/std/result` to keep only generic `must` and `expect`.
6. Add regression fixtures for:
   - `must(Result<void, E>)`;
   - `expect(Result<void, E>, message)`;
   - generic identity with `T = void`;
   - enum payload binding where payload type is `void`;
   - struct fields containing `void`;
   - rejection of address-of `void`;
   - rejection of `extern "C"` by-value `void` parameters.

## Resolved Decisions

1. Concrete `Result<void, E>` examples and diagnostics prefer `case Ok:`, while
   `case Ok(value):` remains valid for generic uniformity.
2. No `unit` alias is added. `void` remains the only spelling.
3. Taking the address of a zero-field struct remains valid. Taking the address of
   `void` remains invalid.
4. Exported non-`extern` Ciel functions may omit erased `void` parameters because
   their ABI is Ciel-internal. Stable C-facing ABI remains explicit
   `extern "C"`.
