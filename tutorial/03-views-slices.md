# 3. Views and Slices

Ciel separates owning a value from viewing it.

- `*T` is a writable non-null pointer.
- `*const T` is a read-only non-null pointer.
- `[]T` is a writable slice view.
- `[]const T` is a read-only slice view.

The motivation is permission. A function that only reads should not receive a
writable view. Read-only means "this path cannot write"; it does not mean the
original storage is frozen forever.

```ciel
import /std/lib;

i64 read_value(*const i64 value) {
    // A read-only pointer can be dereferenced for reading.
    return *value;
}

i64 bump_once(i64 value) {
    // The local slot is mutable because this function replaces it.
    i64 @slot = value;
    slot = slot + 1;

    // Passing `&slot` to a `*const i64` parameter gives read-only access.
    return read_value(&slot);
}

i32 main() {
    // Taking the address of an immutable binding produces a read-only pointer.
    i64 immutable = 9;
    *const i64 immutable_view = &immutable;

    i64 @mutable = bump_once(*immutable_view);

    // A mutable binding can also be viewed through a read-only pointer.
    *const i64 read_only_view = &mutable;
    mutable = mutable + *read_only_view;

    // Slice literals create owned backing storage and a slice view.
    []i64 numbers = [1, 2, 3, 4, 5];

    // A writable slice can be weakened to a read-only slice.
    []const i64 tail = numbers[2..];

    // String literals are read-only char slices.
    []const char label = "views";

    must(print("{} {} {}", [mutable, tail[1], label.len]));
    return 0;
}
```

String literals are `[]const char`. Passing text to C APIs usually means passing
`text.ptr`, which has a read-only pointer type.
