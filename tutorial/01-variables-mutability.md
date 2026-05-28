# 1. Variables and Mutability

This chapter starts with ordinary values: naming them, computing with them, and
printing a result.

The first Ciel rule to notice is assignment permission. `x` is initialized once.
`@x` is mutable and can be assigned again. The marker is part of the binding
name, so `i64 @score` means "a mutable binding named score whose type is i64".

Arithmetic and comparison are explicit. `score + 5` produces a new integer.
`score != start` produces a `bool`. A `bool` is not an integer and an integer is
not a condition.

The central difference is small:

```ciel
i64 start = 10;
i64 @score = start;
score = score + 5;
```

`start` is stable. `score` is deliberately a changing local.

```ciel
import /std/lib;

i32 main() {
    // `start` is initialized once. It cannot be assigned again.
    i64 start = 10;

    // `@score` is mutable, so later assignments are allowed.
    i64 @score = start;

    // Arithmetic expressions produce new values. Assignment stores the new value.
    score = score + 5;

    // Comparisons produce bool values. There is no bool-to-integer shortcut.
    bool changed = score != start;

    if (changed) {
        // `print` returns Result<void, Error>; `must` unwraps it in examples.
        must(print("{}", [score]));
        return 0;
    }

    return 1;
}
```

Use immutable bindings by default. Add `@` when the code is really replacing
the local value.
