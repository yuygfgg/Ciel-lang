# 5. Interfaces and Capabilities

Ciel does not organize behavior around classes. It uses capabilities.

A capability answers a practical question: "can this value do the operation I
need?" For printing, the operation is converting a value to text.

The standard printing API is the easiest place to see the first motivation.
Format strings can receive heterogeneous values: text-like values, integers,
booleans, and user-defined types can all appear in one call. They do not share a
class. They share the `printable` capability, backed by `to_string`.

For a custom type, the important line looks like this:

```ciel
impl to_string(*const Label value) {
    return value->text;
}
```

After that, `Label` can flow through APIs that require printable values.

The second motivation is views over behavior. Sometimes a value supports many
operations, but an API should expose only some of them. Interface algebra gives
names to those views:

- `a + b` requires both capabilities.
- `a - b` removes a capability from the visible view.
- `!a` in a constraint rejects types that have a capability.

The `-` form is not deletion from the concrete type. It is a narrowed view. If
an `Item` can be reset internally, a `read_only_active` view can still hide
`reset` from callers that should only read.

```ciel
import /std/lib;

// Two small capabilities. A value may support either one or both.
interface<T> i64 value(*T item);
interface<T> bool active(*T item);

// Algebra builds a named view that requires both capabilities.
interface active_value = value + active;

// Algebra can also hide a capability from a dynamic view.
interface<T> void reset(*T item);
interface read_only_active = active_value - reset;

struct Item {
    i64 n;
    bool ok;
}

impl value(*Item item) {
    return item->n;
}

impl active(*Item item) {
    return item->ok;
}

impl reset(*Item item) {
    item->n = 0;
    return;
}

i32 main() {
    Item item = { n: 33, ok: true };
    read_only_active view = &item;

    // The view can call `value` and `active`.
    if (active(view)) {
        must(print("{}", [value(view)]));
    }

    // `reset(view)` would be rejected because the view masked out reset.
    return 0;
}
```

An `impl` is not inheritance. It is evidence that a type supports a capability.
Generic APIs can require that evidence, and dynamic interface values can carry a
receiver together with the operations needed for that capability.
