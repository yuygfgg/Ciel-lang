# 10. Errors, Defer, and Panic

Most recoverable failures use `Result<T, Error>`. The `?` operator unwraps `Ok`
or returns the `Err` from the current function.

`must(result)` is convenient in examples: it unwraps success and panics on
failure. Production code usually keeps returning `Result` until the caller can
handle the error.

The common shape is:

```ciel
i64 first = half(20)?;
```

If `half(20)` returns `Ok(value)`, `first` receives `value`. If it returns
`Err(error)`, the current function returns that error immediately.

When a function owns cleanup work, register it with `defer` before the code that
might return early:

```ciel
defer mark(1);
i64 first = half(20)?;
```

`defer` registers one direct function call. Its arguments are evaluated when the
`defer` statement runs. The call itself runs when the current block exits through
normal control flow: reaching the end of the block, `return`, `?`, `break`, or
`continue`.

Deferred calls run in LIFO order. If an inner block has its own defer, the inner
cleanup runs before the outer cleanup.

```ciel
import /std/lib;

void mark(i64 value) {
    // This makes the defer order visible when the example runs.
    must(print("{}", [value]));
}

Result<i64, Error> half(i64 value) {
    // Recoverable failure returns Err.
    if ((value % 2) != 0) {
        return Err(text_error("odd value"));
    }

    return Ok(value / 2);
}

Result<i64, Error> pipeline() {
    // This runs when `pipeline` exits through return or `?`.
    defer mark(1);

    // `?` unwraps Ok or returns Err from `pipeline`.
    i64 first = half(20)?;

    {
        // This defer belongs to the inner block, so it runs before mark(1).
        defer mark(2);

        // If this line returned Err, both active defers would still run.
        i64 second = half(first)?;
        return Ok(second);
    }
}

i32 main() {
    // Examples use `must` when failure should abort the sample.
    i64 value = must(pipeline());
    must(print("{}", [value]));
    return 0;
}
```

This program prints `215`: the inner defer prints `2`, the outer defer prints
`1`, then `main` prints the returned value `5`.

The return value of a deferred call is ignored. A `defer` statement also cannot
use `?`; write cleanup functions so they do not depend on caller-side error
handling.

Panic is for unrecoverable failure. It does not unwind through user cleanup
logic, so it does not run `defer` handlers. Do not use panic as ordinary control
flow.
