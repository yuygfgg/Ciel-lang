# 6. Closures

A closure is a callable value. Ciel closures capture by value only.

The motivation is predictable ownership. If a closure sees `box.value`, it
captures a snapshot of `box`, not a hidden reference that can later mutate
behind your back.

Closure parameters use the same mutability rule as other bindings: `value` is
read-only, `@value` can be reassigned inside the closure.

There is one more important rule: erasing a closure to a common callable type can
erase capability evidence too.

`i64 |(i64)|` means "callable from `i64` to `i64`". It says nothing about whether
the closure can be cloned as a message, printed, scored, or used by another
capability-constrained API.

`i64 |(i64): Message + score|` keeps the same callable shape, but also retains
the listed capability witnesses. This is how a collection or field can store an
erased closure while still proving it is safe to send or usable by custom
capability calls.

Retained does not mean "write any capability name and the value magically has
it." At the conversion point, the compiler checks that the concrete closure type
really satisfies each positive capability. For `Message`, that means the closure
capture environment must be messageable. If the closure captures a raw pointer,
slice, dynamic interface value without policy, or plain erased closure, the
retained `: Message` conversion is rejected.

```ciel
import /std/lib;

interface<T> i64 score(*T value);

impl<T: Message> score(*T value) {
    return 7;
}

T clone_value<T: Message>(T value) {
    T local = value;
    return must(clone_message(&local));
}

i64 apply(i64 value, i64 |(i64)| f) {
    return f(value);
}

i32 main() {
    i64 @base = 2;

    // Plain erased closure: callable, but it carries no Message proof.
    i64 |(i64)| plain = |i64 value| value + base;

    // Retained closure: callable, and it carries Message plus score witnesses.
    i64 |(i64): Message + score| retained = |i64 value| value + base;

    // The closure captured the old value of base.
    base = 100;

    // Message was retained, so generic code can clone the erased closure value.
    i64 |(i64): Message| copied = clone_value(retained);

    must(print("{} {} {}", [apply(5, plain), copied(5), score(&retained)]));
    return 0;
}
```

The plain closure remains callable through `apply`. The retained closure is also
callable, but it can do more: it can satisfy `T: Message`, and `score(&retained)`
dispatches through the stored witness.
