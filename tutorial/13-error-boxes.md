# 13. Error Boxes

Precise error enums are good for libraries. A parser can return `ParseError`, a
database layer can return `DbError`, and callers that know those domains can
recover from specific variants.

Application boundaries often need something else: "return any useful error with
text and context." Ciel's standard `Error` is that owned error box.

The box is built from one capability:

```ciel
interface<T> []const char format_error(*const T error);
interface ErrorTrait = format_error;
```

Any concrete error type that implements `format_error` can be erased into
`Error`. Internally, `Error` stores an `ErrorTrait` dynamic interface value plus
optional context and source information. In plain terms, the box owns enough
data to format the original concrete error later.

The `?` operator has one targeted convenience rule:

- `Result<T, E>` can propagate into `Result<U, E>` as usual.
- `Result<T, E>` can also propagate into `Result<U, Error>` when `E:
  ErrorTrait`.
- That second case conceptually calls `error_box(error)`.

No general conversion graph is searched. If the concrete error does not
implement `format_error`, boxing is rejected.

```ciel
import /std/lib;
import /std/format as format;

enum ParseError {
    Empty,
    Bad(i64),
}

// A concrete error type becomes boxable by implementing ErrorTrait.
impl format_error(*const ParseError error) {
    switch (*error) {
        case Empty:
            return "empty";
        case Bad(code):
            return format::i64_to_string(code);
    }
}

Result<i64, ParseError> parse(bool ok) {
    if (ok) {
        return Ok(40);
    }
    return Err(Bad(7));
}

Result<i64, Error> load(bool ok) {
    // `parse` returns ParseError, but this function returns the standard Error.
    // `?` is accepted because ParseError implements format_error.
    i64 value = parse(ok)?;
    return Ok(value + 2);
}

Result<i64, Error> load_with_context(bool ok) {
    // Context wraps the boxed error with a higher-level message.
    i64 value = error_context(parse(ok), "load config")?;
    return Ok(value);
}

i32 main() {
    Result<i64, Error> failed = load(false);
    switch (failed) {
        case Ok(_):
            return 1;
        case Err(error):
            must(print("{}", [error_message(&error)]));
    }

    Result<i64, Error> contextual = load_with_context(false);
    switch (contextual) {
        case Ok(_):
            return 2;
        case Err(error):
            must(print(" {}", [error_message(&error)]));
    }

    return 0;
}
```

Use precise error enums inside libraries. Box into `Error` at application,
task, or actor boundaries where the caller mostly needs reporting and context.
