# Proposal Dependency Notation

Proposal files may include a `Proposal Order` section. The section uses these
small ordering marks:

- `A < B`: hard prerequisite. `B` follows after `A` is settled.
- `A <= B`: soft baseline. `B` can be specified without `A`, but discussion and
  examples should assume `A` once both proposals are active.
- `A :> B[topic]`: ownership edge. `A` owns `topic`; `B` routes that topic
  through `A`.
- `A || B[topic]`: independence edge. `A` and `B` can proceed in either order
  for `topic`.

Proposal names are the file stem under `proposal/`, for example
`local-type-holes` for `proposal/local-type-holes.md`.
The name may also reserve a future proposal that is being used as an ordering
anchor before its file exists.

Current proposal order:

```text
local-type-holes <= metaprogramming

metaprogramming :> capability-erased-closures[structural capability proofs]
capability-erased-closures || metaprogramming[retained closure witness storage]

metaprogramming :> error-box[derived format_error]
error-box || metaprogramming[owned error erasure and ? propagation]
```

The main consequence is that structural capability derivation belongs to
`metaprogramming`. Other proposals consume ordinary capability impls or retained
witnesses produced through that route.
