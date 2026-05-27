# Binding Mutability And Read-Only Views Proposal

This proposal separates two concepts that Ciel currently mixes through a
C/C++-style `const` type prefix:

1. whether a source binding may be assigned again;
2. whether a pointer-like view may write through the storage it references.

Bindings become immutable by default and use `@` for mutable storage. Pointer
and slice views carry their own write permission. Standalone `const T` is
removed from the source language.

The intended result is that Ciel never constructs meaningless types such as
`const i64` or `const bool`. Reading through a read-only view produces ordinary
values. Read-only-ness is a property of an access path, not a value qualifier.

## Proposal Order

```text
binding-mutability <= error-box
binding-mutability < monomorphized-c-callbacks
binding-mutability || metaprogramming[borrowed representation pointers]
binding-mutability || pure-library-message[read-only clone source]
```

This proposal owns source-level binding mutability, pointer and slice view
mutability, and the removal of standalone `const T`.

`error-box` can be specified without this proposal, but source examples should
use the final binding rules once both are active. `monomorphized-c-callbacks`
should follow this proposal so `/std/actor` and callback helper code are written
once with the final local, parameter, and pointer surface.

`metaprogramming` and `pure-library-message` remain independent for their core
policies. They consume this proposal's read-only pointer forms for borrowed
representation and clone-source APIs.

## Surface Syntax

`@` belongs to a binding name:

```ebnf
BindingName  ::= [ "@" ] Identifier

LocalDecl    ::= Type BindingName [ "=" Expr ] ";"
Param        ::= Type BindingName
ClosureParam ::= BindingName | Type BindingName
PatternBind  ::= BindingName
```

Examples:

```rust
i64 value = 1;      // immutable binding
i64 @count = 0;     // mutable binding

void step(i64 input, i64 @state) {
    state = state + input;
}

_ inferred = make_value();
_ @cursor = make_cursor();

switch (event) {
    case Click(pos):
        pos.x = 1; // error: pos is an immutable binding
    case Drag(@pos):
        pos.x = 1; // ok
}
```

`const` is not a type prefix. It appears only inside the spelling of read-only
view constructors. The four pointer forms are sibling constructors, not a
derivation chain where `const` modifies `*`:

```ebnf
Type            ::= [ AbiSpec ] PrefixType { CallableSuffix }
PrefixType      ::= { PointerConstructor } PrimaryType
PointerConstructor ::= "*" | "*const" | "?*" | "?*const"

PrimaryType     ::= NamedType
                 | TypeHole
                 | "never"
                 | "void"
                 | ArrayType
                 | SliceType
                 | "(" Type ")"

SliceType       ::= "[]" Type | "[]const" Type
ArrayType       ::= "[" IntegerLiteral "]" Type
```

The analogous slice forms are also sibling constructors:

```rust
[]i64        // writable slice view over i64 elements
[]const i64  // read-only slice view over i64 elements
```

Read-only view types:

```rust
*const i64       // non-null read-only pointer to i64
?*const i64      // nullable read-only pointer to i64
[]const u8       // read-only slice of u8 elements
*const []u8      // read-only pointer to a mutable-slice value
```

Invalid standalone const forms:

```rust
const i64 value = 1;       // error
const bool flag = true;    // error
const Point p = make();    // error
[4]const i64 values;       // error
Result<const i64, Error> r; // error
```

The source language has no `const T` type constructor. Implementations should
not represent this proposal with `Ty::Const(Box<Ty>)`; pointer and slice types
carry their view kind directly. `*const T` and `*T` may share implementation
helpers, but semantically they are distinct view constructors with explicit
conversion rules.

## Binding Semantics

A binding without `@` is immutable after it is initialized. A binding with `@`
may be assigned repeatedly.

```rust
i64 x = 1;
x = 2; // error

i64 @y = 1;
y = 2; // ok
```

Owned aggregate mutation follows the binding:

```rust
Point p = { x: 1, y: 2 };
p.x = 3; // error

Point @q = { x: 1, y: 2 };
q.x = 3; // ok

[4]i64 values = [1, 2, 3, 4];
values[0] = 9; // error

[4]i64 @scratch = [0, 0, 0, 0];
scratch[0] = 9; // ok
```

Function parameters, closure parameters, `for` initializer bindings, and
pattern bindings use the same binding rule. A parameter behaves like an
already-initialized local declared with the same `BindingName`:

```rust
void f(i64 value) {
    value = 1; // error
}

void g(i64 @value) {
    value = 1; // ok
}

void |(i64, i64)| add = |i64 @state, i64 delta| {
    state = state + delta; // ok
};
```

Parameters have no mutability privilege. `T value` and `T local = arg` differ
only in how the initial value is supplied.

`@` does not mean "non-const". It only controls the source binding. A mutable
binding may hold a read-only pointer or slice:

```rust
*const i64 @cursor = start;
cursor = next; // ok: the pointer binding is mutable
*cursor = 1;   // error: the view is read-only
```

## Delayed Initialization

Immutable locals may be declared before their initializer, but they may be
initialized only once on every control-flow path. The assignment must target
the whole binding.

```rust
i64 x;
if (cond) {
    x = 1; // ok: initializes x on this path
} else {
    x = 2; // ok: initializes x on this path
}
return x;
```

Repeating initialization is an error:

```rust
i64 x;
x = 1;
x = 2; // error: x is already initialized
```

Partial writes cannot initialize an immutable aggregate:

```rust
Point p;
p.x = 1; // error: immutable delayed initialization must assign the whole value
```

The definite-assignment analysis needs three states for immutable locals:

- `unassigned`: a whole-binding initialization is allowed;
- `assigned`: reads are allowed and another assignment is rejected;
- `maybe-assigned`: both reads and initialization are rejected until control
  flow proves a single state again.

Branch merge rules:

```text
assigned + assigned       => assigned
unassigned + unassigned   => unassigned
assigned + unassigned     => maybe-assigned
maybe-assigned + anything => maybe-assigned
```

Loop bodies are conservative. Assigning an immutable local declared outside a
`while` or `for` body is rejected unless a later analysis can prove the body
executes at most once. Mutable locals keep the existing definite-assignment
behavior.

Type holes still require initializers:

```rust
_ value = make_value();  // ok
_ value;                 // error
_ @value;                // error
```

## View Mutability

Pointer and slice views are either writable or read-only.

```rust
*T         // writable non-null pointer to T
*const T   // read-only non-null pointer to T
?*T        // writable nullable pointer to T
?*const T  // read-only nullable pointer to T
[]T        // writable slice view over T elements
[]const T  // read-only slice view over T elements
```

The write permission belongs to the view edge. It is not deep immutability.

```rust
*const *i64 p = source;
*p = other; // error: cannot overwrite the pointer value stored at source
**p = 1;    // ok if the loaded *i64 points to writable storage

*const *const i64 q = source;
**q = 1; // error: the loaded pointer is also read-only
```

Reading through a read-only view produces an ordinary value type:

```rust
*const i64 p = get_value();
i64 value = *p; // ok: *p reads an i64

[]const bool flags = get_flags();
if (flags[0]) { // ok: flags[0] reads a bool
    work();
}
```

There is no `const i64` result from `*p`, no `const bool` condition, and no
implicit conversion from `const bool` to `bool` because such a source type does
not exist.

Assignments through views require a writable view:

```rust
*i64 p = get_mut_ptr();
*p = 1; // ok

*const i64 ro = get_ro_ptr();
*ro = 1; // error

[]u8 bytes = get_mut_bytes();
bytes[0] = 1; // ok

[]const u8 text = "hello";
text[0] = 1; // error
```

Assigning to a slice descriptor still follows binding mutability:

```rust
[]u8 s = get_mut_bytes();
s[0] = 1;        // ok: element write through a writable view
s = other_bytes; // error: s is an immutable binding

[]u8 @t = get_mut_bytes();
t = other_bytes; // ok
```

## Lvalues And Access Modes

The type checker should track lvalue access separately from expression type.
Each checked expression is one of:

- not an lvalue;
- read-only lvalue of type `T`;
- writable lvalue of type `T`.

This is the central implementation rule that prevents standalone const types.

Examples:

```rust
i64 x = 1;
x          // read-only lvalue of type i64

i64 @y = 2;
y          // writable lvalue of type i64

*i64 p = &y;
*p         // writable lvalue of type i64

*const i64 q = &y;
*q         // read-only lvalue of type i64
```

Projection uses the edge that actually reaches the storage being written:

- struct fields and fixed-array elements are owned subobjects, so they follow
  the base lvalue's access mode;
- pointer dereference follows the pointer view's mutability;
- slice element and subview access follows the slice view's mutability;
- assigning a new pointer or slice descriptor still follows the descriptor
  binding or containing-field access mode.

```rust
Point p = make_point();
p.x = 1; // error: field of an immutable owned binding

Point @m = make_point();
m.x = 1; // ok

*Point mp = &m;
mp->x = 1; // ok

*const Point rp = &m;
rp->x = 1; // error

[]Point points = get_mut_points();
points[0].x = 1; // ok

[]const Point view = points;
view[0].x = 1; // error
```

Read-only lvalues are not const-qualified rvalues. Reading a field, pointer, or
slice descriptor from a read-only aggregate produces the ordinary stored value,
including whatever view mutability that stored value carries:

```rust
struct Holder {
    *i64 ptr;
}

*const Holder h = get_holder();
h->ptr = other; // error: cannot overwrite the field
*(h->ptr) = 1;  // ok: the stored pointer value is *i64

struct ViewHolder {
    []u8 bytes;
}

*const ViewHolder vh = get_view_holder();
vh->bytes = other; // error: cannot overwrite the slice descriptor
vh->bytes[0] = 1;  // ok: the stored slice value is []u8
```

## Address-Of

`&expr` requires an lvalue and produces a non-null pointer. The pointer
mutability follows the lvalue access mode:

```rust
i64 x = 1;
i64 @y = 2;

*const i64 px = &x; // ok
*i64 py = &y;       // ok
```

Taking a mutable pointer from a read-only lvalue is rejected:

```rust
i64 x = 1;
*i64 p = &x; // error
```

Taking a read-only pointer from a writable lvalue is allowed by view weakening:

```rust
i64 @x = 1;
*const i64 p = &x; // ok
```

Parameters follow the same address-of rule as initialized locals. `T value`
is a read-only lvalue and `T @value` is a writable lvalue:

```rust
Result<T, Error> clone<T: Message>(T value) {
    return clone_message(&value); // &value has type *const T
}

void update(Point @p) {
    mutate(&p); // &p has type *Point
}
```

Captured outer bindings are not addressable through the closure snapshot. Code
that needs an address inside a closure should capture an explicit pointer or
handle before constructing the closure.

## Assignability And Casts

The pointer and slice view constructors have only the following implicit
conversions:

```rust
*T         -> *const T
*T         -> ?*T
*T         -> ?*const T
*const T   -> ?*const T
?*T        -> ?*const T
[]T        -> []const T
```

This list is the whole relationship between the constructors. In particular,
`*const T` is not "a const-qualified `*T`"; it is a separate read-only pointer
view that a writable pointer may weaken into.

Array-to-slice conversion uses the source access path. A writable array lvalue
can become `[]T`; a read-only array lvalue can become only `[]const T`.

```rust
[4]i64 values = [1, 2, 3, 4];
[]const i64 view = values; // ok
[]i64 mut_view = values;  // error

[4]i64 @scratch = [0, 0, 0, 0];
[]i64 scratch_view = scratch; // ok
```

Array literals and repeat literals may still create fresh backing storage in an
expected slice context:

```rust
[]i64 writable = [1, 2, 3];
[]const i64 readonly = [1, 2, 3];
```

The reverse direction is rejected, including under `as`:

```rust
*const T ro = get_ro();
*T rw = ro;        // error
*T rw2 = ro as *T; // error

[]const T view = get_ro_slice();
[]T mut_view = view; // error
```

Pointer casts involving `void` preserve nullability and never remove read-only
view mutability:

```rust
*T p;
*void raw = p as *void;             // ok
*const void ro_raw = p as *const void; // ok

*const T ro;
*const void ro_any = ro as *const void; // ok
*void mut_any = ro as *void;            // error

*const void erased;
*const U u = erased as *const U; // ok
*U bad = erased as *U;           // error
```

No cast can manufacture write access to a view that was type-checked as
read-only. If C interop needs a trusted escape hatch later, it should be a
small unsafe standard-library function or compiler-owned runtime shim, not the
ordinary `as` operator.

## C Interop

The source language does not model C's general `const` qualifier. Ciel source
models only caller-visible pointer and slice view mutability.

```rust
extern "C" {
    i32 puts(*const char s);
    c::c_ssize_t write(c::c_int fd, *const void buf, c::c_size_t count);
    void free(?*void ptr);
}
```

Generated C spelling:

```text
*T         => T *
*const T   => const T *
?*T        => T *
?*const T  => const T *
```

C top-level const on a value parameter or pointer parameter is not part of the
caller-visible Ciel type. It is ignored by the Ciel type system and by any
future header importer:

```c
void f(const int value);      // Ciel: void f(i32 value)
void g(char * const buffer);  // Ciel: void g(*char buffer)
void h(const char * const s); // Ciel: void h(*const char s)
```

The generated declaration for `void h(*const char s)` may spell the parameter
as `const char *s`. That remains compatible with an included C header spelling
`const char * const s`: the second `const` qualifies the by-value parameter
variable, not the caller-visible function type.

Only pointee const is preserved because it changes what the callee may write
through the argument:

```c
void takes_mut(char *buffer);        // Ciel: void takes_mut(*char buffer)
void takes_ro(const char *buffer);   // Ciel: void takes_ro(*const char buffer)
```

Ciel does not use a compiler flag to erase this distinction. A global
`-Wno-discarded-qualifiers`-style policy would hide real source-level
mistakes: passing `*const T` to a C parameter declared as `*T` would let C write
through a read-only Ciel view.

Calls obey the Ciel declaration exactly:

```rust
extern "C" {
    void read_only(*const char s);
    void may_write(*char s);
}

[]const char text = "hello";
read_only(text.ptr); // ok
may_write(text.ptr); // error
```

If a legacy C API accepts `void *` for data it only reads, the binding should
use a C shim or a corrected declaration that exposes `*const void` to Ciel. The
ordinary Ciel call path must not insert a `*const T` to `*T` cast.

For rare C declarations where exact spelling matters but no Ciel semantics are
needed, users should keep using C spelling aliases:

```rust
extern "C" type CHandle = "const struct CHandle";
```

Compiler-inserted casts are allowed only in generated C at ABI boundaries or
runtime shims after Ciel type checking has already enforced read-only access.
These casts may normalize C top-level const or generated helper spelling, but
they must not let a read-only Ciel view satisfy a writable source-level C
parameter. They also must not create a source-level conversion from `*const T`
to `*T` or from `[]const T` to `[]T`.

For exported Ciel functions, generated prototypes preserve pointee const:

```rust
export extern "C" void inspect(*const Packet packet) { ... }
export extern "C" void mutate(*Packet packet) { ... }
```

```c
void inspect(const Packet *packet);
void mutate(Packet *packet);
```

Top-level const on C parameters may appear in a user-written C header, but Ciel
does not need to reproduce it in generated definitions. Pointee const must
match; otherwise the C and Ciel declarations describe different write
permissions.

## Message And Actors

Actor safety remains based on `Message`, not on read-only views. A read-only
pointer or slice is still a borrowed view and does not prove that the referenced
storage can cross an actor boundary.

```rust
*const Buffer ptr; // not Message by default
[]const u8 view;   // not Message by default
```

Cross-actor APIs still require `T: Message` and still construct a receiver-owned
value through `clone_message`.

The `clone_message` source parameter should be read-only:

```rust
interface<T> Result<T, Error> clone_message(*const T value);
interface Message = clone_message;
```

This matches the operation: cloning reads the source and constructs an
independent destination. It also lets immutable parameters and locals be cloned
without first making their binding mutable:

```rust
Result<void, Error> send<M: Message>(*Actor<M> actor, M value) {
    M copy = clone_message(&value)?;
    enqueue(actor, copy);
    return Ok;
}
```

If a policy wants to send data behind a pointer or slice, it must copy that data
into an owned message type or provide an explicit `clone_message`
implementation. Read-only view mutability alone is not a transfer policy.

## SOP And Reflection

Borrowed structural representation should use read-only source pointers:

```rust
meta::RefRepr<T> as_ref_repr<T>(*const T value);
meta::Repr<T> into_repr<T>(*const T value);
T from_repr<T>(meta::Repr<T> value);
```

Field and payload references use read-only pointers:

```rust
struct FieldRef<T> {
    []const char name;
    *const T value;
}

struct PayloadRef<T> {
    usize index;
    *const T value;
}
```

`RefRepr<T>` remains a borrowed actor-local view. It does not become `Message`
because its fields are read-only. `Repr<T>` remains the owned copy boundary.

There is no `Repr<const T>` normalization rule because `const T` no longer
exists. Nested read-only view leaves remain part of the represented type:

```rust
meta::Repr<*const i64>  // raw read-only pointer leaf
meta::Repr<[]const u8>  // raw read-only slice leaf
```

## Closures And Captures

Closure parameters use `BindingName`. Captured bindings keep the existing
snapshot rule: the closure body cannot reassign a captured outer binding or
mutate owned fields or indices rooted in that captured binding.

```rust
i64 @total = 0;
void |(i64)| add = |i64 delta| {
    total = total + delta; // error: captured binding snapshot
};
```

Shared mutation is expressed by capturing an explicit writable view or a
synchronized handle:

```rust
i64 @total = 0;
*i64 total_ptr = &total;

void |(i64)| add = |i64 delta| {
    *total_ptr = *total_ptr + delta; // ok
};
```

Capturing a read-only view preserves read-only access:

```rust
*const i64 total_view = &total;
void |()| bad = || {
    *total_view = 1; // error
};
```

## Diagnostics

Diagnostics should name the violated concept directly:

```text
cannot assign to immutable binding `x`
cannot initialize immutable binding `x` more than once
cannot partially initialize immutable binding `p`
cannot mutate field `x` through immutable binding `p`
cannot write through read-only pointer `ptr`
cannot write through read-only slice `bytes`
cannot convert `*const T` to `*T`
standalone `const T` is not a Ciel type; use `*const T` or `[]const T`
```

## Implementation Plan

1. Add `@` as a token and parse `BindingName` for locals, parameters, closure
   parameters, `for` initializers, and pattern bindings.
2. Replace the current prefix-type `const` grammar with pointer and slice view
   mutability grammar.
3. Remove the general `Ty::Const` representation. Add view mutability to
   pointer and slice types instead.
4. Add lvalue access mode to checked expressions or to the assignment/address
   checking path: not-lvalue, read-only lvalue, writable lvalue.
5. Implement binding mutability and delayed single initialization using the
   three-state definite-assignment model for immutable locals.
6. Type `&expr` as `*T` for writable lvalues and `*const T` for read-only
   lvalues.
7. Make field, index, dereference, pointer-field, and slice-subview projection
   preserve access mode without constructing const-qualified value types.
8. Implement implicit view weakening and nullable widening.
9. Reject all source-level conversions that remove read-only view mutability,
   including `as` casts.
10. Update C emission for `*const T`, `?*const T`, and read-only runtime shims.
11. Update `/std/message` so `clone_message` takes `*const T`.
12. Update `/std/meta` so borrowed representation and owned projection read
    from `*const T`, and use `[]const char` for read-only names/text.
13. Update string literals to produce `[]const char` unless a deliberate
    mutable copy API is used.
14. Add fixtures for binding assignment, delayed initialization, parameters,
    closure parameters, pattern bindings, address-of, pointer/slice weakening,
    rejected const removal, `as` soundness, actor `Message`, and SOP helpers.
15. Update `design.md` only after this proposal is accepted.

## Non-Goals

This proposal does not add ownership, borrowing, or alias exclusivity. `*const T`
and `[]const T` are read-only access paths, not proofs that the underlying
object will never change through another mutable path.

This proposal does not add field-level immutability to struct definitions.
Field writes are controlled by the lvalue path used to reach the field.

This proposal does not make read-only pointers or slices actor-safe. Actor
transfer remains an owned-copy policy expressed through `Message`.

This proposal does not expose an unsafe `const_cast`. If such an operation is
ever needed, it should be designed separately and kept out of ordinary `as`.
