# 10. Errors and Panic

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

```ciel
import /std/lib;

Result<i64, Error> half(i64 value) {
    // Recoverable failure returns Err.
    if ((value % 2) != 0) {
        return Err(text_error("odd value"));
    }

    return Ok(value / 2);
}

Result<i64, Error> pipeline() {
    // `?` unwraps Ok or returns Err from `pipeline`.
    i64 first = half(20)?;
    i64 second = half(first)?;
    return Ok(second);
}

i32 main() {
    // Examples use `must` when failure should abort the sample.
    i64 value = must(pipeline());
    must(print("{}", [value]));
    return 0;
}
```

Panic is for unrecoverable failure. It does not unwind through user cleanup
logic, so do not use it as ordinary control flow.
