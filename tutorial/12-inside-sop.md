# 12. Inside SOP

Earlier, `meta::Repr<Event>` was a safe envelope for actor messages. This
chapter opens that envelope.

SOP means "sum of products". In everyday terms:

- a struct becomes a list of fields
- an enum becomes a choice between variants
- each variant payload becomes a list of payload fields

The standard meta library represents struct field lists with `HCons` and
`HNil`. You do not need these names for ordinary actor code, but they let
generic code inspect visible data shapes.

For a two-field struct, the borrowed representation behaves like a field list:

```ciel
HCons<FieldRef<i64>, HCons<FieldRef<bool>, HNil>>
```

The exact type is normalized by the compiler. The useful idea is simpler: match
the empty list with `HNil`, and match a non-empty list with `HCons<head, tail>`.

```ciel
import /std/lib;
import /std/meta as meta;

struct Event {
    i64 amount;
    bool important;
}

interface<T> i64 count_fields(*const T value);

// Empty product: no more fields.
impl count_fields(*const meta::HNil value) {
    return 0;
}

// Non-empty product: count the head field, then recurse into the tail.
impl<V, Tail: count_fields> count_fields(*const meta::HCons<meta::FieldRef<V>, Tail> value) {
    return 1 + count_fields(&value->tail);
}

i32 main() {
    Event event = { amount: 7, important: true };

    // Borrow the event as a structural field list.
    meta::RefRepr<Event> view = meta::as_ref_repr(&event);

    i64 fields = count_fields(&view);
    must(print("{}", [fields]));
    return 0;
}
```

`meta::RefRepr<T>` borrows the original value. `meta::Repr<T>` owns a copy-like
structural value. Actor and channel examples use the owned form because crossing
a concurrency boundary needs owned data.

SOP does not automatically make every type messageable. `/std/message` has
ordinary `Message` impls for owned SOP nodes such as fields, products, variants,
and coproducts. Those impls recursively require every owned leaf to implement
`Message` and to avoid capabilities forbidden by `Message`, such as
`ThreadLocal`. If a leaf is not safe to clone across actors, the compiler
rejects the boundary where `meta::Repr<T>: Message` is required.
