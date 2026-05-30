# Ciel Tutorial

This tutorial is the guided path. `design.md` is the language contract; these
chapters teach the parts a working programmer needs in order.

Each chapter has one or more complete programs under `examples/`. Explanatory
snippets inside the prose may show only the relevant lines.

```sh
for file in tutorial/examples/*.ciel; do
  cargo run -q -- "$file" -o "/tmp/$(basename "$file" .ciel)"
done
```

## Stage 1: Basics

1. [Variables and Mutability](01-variables-mutability.md)
2. [Functions and Control Flow](02-functions-control-flow.md)
3. [Views and Slices](03-views-slices.md)

## Stage 2: Data and Capabilities

4. [Structs and Enums](04-structs-enums.md)
5. [Interfaces and Capabilities](05-interfaces-capabilities.md)
6. [Closures](06-closures.md)

## Stage 3: Concurrency

7. [Actor Basics](07-actor-basics.md)
8. [Sending Complex Data Across Actors](08-actor-envelopes.md)
9. [Channels and Async Flows](09-channels-async-io.md)

## Stage 4: Advanced

10. [Errors, Defer, and Panic](10-errors-defer-panic.md)
11. [C Interop](11-c-interop.md)
12. [Inside SOP](12-inside-sop.md)
13. [Error Boxes](13-error-boxes.md)
