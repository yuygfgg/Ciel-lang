# Self-Referential Types Proposal

This proposal defines how Ciel checks recursive type layout. The goal is to
allow ordinary indirection-based recursive data structures while rejecting
infinite by-value layouts.

## Proposal Order

```text
self-referential-types || metaprogramming[layout expansion boundaries]
self-referential-types || pure-library-message[layout-valid does not imply Message]
```

This proposal is about layout only. It does not decide `Message`, ownership, or
deep-clone policy for recursive graphs.

## Rule

Layout expansion follows storage edges:

1. A struct field or enum payload stored by value continues layout expansion.
2. A pointer edge (`*T`, `*const T`, `?*T`, `?*const T`) stops layout
   expansion at that edge.
3. Generic aggregate layout depends only on substituted field and payload
   storage types. An unused type parameter does not by itself force expansion.

If layout reaches the same concrete struct or enum instance again through only
by-value storage edges, the type is rejected.

Examples:

```rust
struct Node {
    i64 data;
    ?*Node next;
}
```

This is valid because the cycle is cut by a pointer edge.

```rust
struct Node {
    i64 data;
    Node next;
}
```

This is invalid because the cycle is by value.

```rust
enum List {
    Cons(i64, List),
    Nil,
}
```

This is invalid for the same reason.

## Generic Aggregates

A generic aggregate may be recursive only through the storage types that remain
after substitution.

```rust
struct Wrapper<T> {
    i64 tag;
}

struct Outer {
    Wrapper<Outer> inner;
}
```

This is valid because `Wrapper<T>` stores only `i64`; the type parameter does
not participate in layout.

```rust
struct Wrapper<T> {
    T value;
}

struct Outer {
    Wrapper<Outer> inner;
}
```

This is invalid because substitution produces a by-value cycle.

## Diagnostics

The compiler must reject unsupported recursive layouts with a normal semantic
diagnostic. Recursive layout must never cause unbounded recursion or a compiler
stack overflow.

The diagnostic should identify the concrete type and the reason, for example:

```text
recursive by-value type is not supported: `List`
```

or:

```text
recursive by-value type is not supported: `Wrapper<Outer>`
```

## Interaction With Message

A layout-valid recursive type does not automatically implement `Message`.

```rust
struct Node {
    i64 data;
    ?*Node next;
}
```

This layout is valid, but `Node` should still fail ordinary `Message`
derivation unless user code writes an explicit policy or uses a different owned
representation. A raw pointer graph is not a safe default cross-actor clone
format.

The same separation applies to other policy surfaces such as structural
metaprogramming. Layout validity only means the type has a finite in-memory
shape.
