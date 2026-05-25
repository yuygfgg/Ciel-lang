# Local Type Holes Proposal

This proposal adds explicit type holes for initialized local declarations.

The goal is to make local code ergonomic when the exact type is verbose,
compiler-created, or unnameable, while keeping public APIs and assignment
semantics explicit.

## Proposal Order

```text
local-type-holes <= metaprogramming
```

Local type holes are a soft baseline for later metaprogramming work. They give
local code a compact way to name values whose exact concrete type is verbose or
compiler-created.

Future metaprogramming proposals may assume this proposal when examples need a
local binding for generated or compiler-created types. Reflection, type-shape
inspection, and declaration generation belong to the metaprogramming proposal.

## Problem

Local variables currently require a complete written type:

```rust
Actor<Command<i64>> actor = must(spawn_actor<State<i64>, Command<i64>>(
    initial,
    |State<i64> state, Command<i64> command| {
        return handle(state, command);
    },
));
```

This is manageable for simple actor handles, but it gets noisy when message
types are nested. It also does not work well for temporary concrete closure
values, because every closure literal has a unique compiler-created type:

A temporary handler has the desired shape, but there is currently no source
spelling for the type before `handler`.

Erasing the closure to a signature type is not always equivalent. Actor handlers
and other `Message`-checked APIs often need the concrete closure type so the
compiler can inspect captured values.

## Proposed Syntax

Use `_` as a type hole inside initialized local declarations:

```rust
_ handler = |State<i64> state, Command<i64> command| handle(state, command);

_ actor = must(spawn_actor<State<i64>, Command<i64>>(initial, handler));
```

The same hole may appear inside a partial local type annotation:

```rust
Actor<_> actor = must(spawn_actor<State<i64>, Command<i64>>(initial, handler));
Result<Actor<_>, Error> pending =
    spawn_actor<State<i64>, Command<i64>>(initial, handler);
[]_ values = [1, 2, 3];
[3]_ fixed = [1, 2, 3];
```

Grammar sketch:

```ebnf
Type            ::= [ AbiSpec ] PrefixType { CallableSuffix }
PrefixType      ::= { TypePrefix } PrimaryType
PrimaryType     ::= TypeHole
                 | NamedType
                 | "never"
                 | "void"
                 | ArrayType
                 | SliceType
                 | "(" Type ")"
TypeHole        ::= "_"

VarDeclStmt     ::= Type Identifier [ "=" Expr ] ";"
ForInit         ::= Type Identifier [ "=" Expr ]
                 | LValue "=" Expr
                 | Expr
```

Semantic restrictions keep holes local:

1. A type containing `_` is valid only in an initialized local declaration or
   initialized `for` declaration.
2. The initializer is required when the declared type contains a hole.
3. All holes must be solved while checking that initializer.
4. Holes are rejected in function signatures, struct fields, enum payloads,
   interface declarations, impl signatures, type aliases, extern declarations,
   casts, and explicit generic call arguments.

`_` remains a pattern wildcard in pattern grammar. In type grammar it is a type
hole, not a type name.

## Semantics

Checking an initialized local declaration with holes creates fresh inference
variables for those holes, then checks the initializer against the partial
expected type.

Examples:

```rust
_ count = 0;          // i64, by the existing integer literal default
_ scale = 1.0;        // f64, by the existing float literal default
[]_ values = [1, 2];  // []i64
Actor<_> actor = must(spawn_actor<State<i64>, Command<i64>>(initial, handler));
```

After the initializer is checked, the compiler substitutes the solved type into
the local binding. Later assignments must use that concrete type:

```rust
_ value = 1;
value = 2;   // ok
value = 2.0; // error: expected i64
```

Type holes do not infer from later uses:

```rust
_ value = null; // error: `null` needs an expected nullable pointer type

?*_ ptr = null; // error: pointer element type is not known
?*i64 ok = null;
```

Expressions that already require an expected type still require one:

```rust
_ point = { x: 1, y: 2 }; // error: struct literal needs an expected struct type
_ empty = [];             // error: empty array literal has no element type
```

Partial annotations can provide the missing context:

```rust
Point point = { x: 1, y: 2 };
[]i64 empty = [];
```

Closure literals follow the existing closure rules. A fully typed closure can
produce its concrete closure type:

```rust
_ inc = |i64 value| value + 1;
```

An untyped closure still needs an expected callable type:

```rust
_ inc = |value| value + 1; // error: closure parameter type is not known

i64 |(i64)| erased = |value| value + 1;
```

Block-bodied closures keep the existing return-type rule. They need a position
that already supplies an expected callable type; `_` alone does not infer block
return types.

## Assignment Is Still Assignment

This proposal does not make assignment declare variables.

```rust
actor = make_actor(); // assignment to an existing binding
actro = make_actor(); // error: unknown name `actro`
```

New locals still use declaration syntax:

```rust
_ actor = make_actor();
Actor<_> other = make_actor();
```

This preserves spelling-error diagnostics and keeps shadowing explicit.

## Diagnostics

Diagnostics should point at the unsolved hole or the expression that failed to
provide enough context:

```text
cannot infer type hole in local `value`: initializer `null` needs an expected nullable pointer type
cannot infer element type for `[]_`: array literal is empty
type hole is only allowed in initialized local declarations
```

For nested holes, include the surrounding type:

```text
cannot infer `_` in `Actor<_>` from initializer
```

## Compiler Work

1. Parse `_` as a type hole in type grammar while keeping pattern `_` unchanged.
2. Represent type holes in the AST/HIR type nodes.
3. Reject holes outside initialized local and `for` declarations.
4. During local initializer checking, convert holes to fresh inference
   variables.
5. Check the initializer with the partial expected type, using the existing
   unification rules for literals, generics, closures, and aggregate literals.
6. Reject the declaration if any hole remains unresolved.
7. Store the solved concrete type on the local binding before THIR/codegen.
8. Add diagnostics for missing initializer, illegal hole context, and unresolved
   holes.

No runtime or standard-library change is required.

## First Slice

The first implementation should support:

```rust
_ x = expr;
Actor<_> actor = expr;
Result<Actor<_>, Error> pending = expr;
[]_ values = [1, 2, 3];
for (_ i = 0; i < n; i = i + 1) {
    use(i);
}
```

Required behavior:

1. Fully inferred locals can hold concrete closure values.
2. Partial local annotations solve holes from the initializer.
3. Struct, array, nullable pointer, and closure diagnostics remain explicit when
   the initializer lacks enough type information.
4. Assignment to an unknown name remains an unknown-name error.
5. The solved local type is concrete before monomorphization and codegen.
