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

Proposal names are the file stem under `proposal/` or `proposal/done/`, for
example `local-type-holes` for `proposal/done/local-type-holes.md`.
The name may also reserve a future proposal that is being used as an ordering
anchor before its file exists.

Current active proposal order:

```text
binding-mutability <= error-box
binding-mutability < monomorphized-c-callbacks
binding-mutability || metaprogramming[borrowed representation pointers]
binding-mutability || pure-library-message[read-only clone source]

unsafe <= monomorphized-c-callbacks[C callback declarations]
pure-library-message < unsafe[manual policy impls become unsafe]

unsafe <= generic-growable-storage[trusted raw storage construction]
metaprogramming <= generic-growable-storage[type size and alignment]

capability-erased-closures < monomorphized-c-callbacks
pure-library-message <= monomorphized-c-callbacks
monomorphized-c-callbacks :> actor-stdlib-lowering[dispatch callback]
actor-owned-state < actor-stdlib-lowering[spawn_actor_state semantics]

metaprogramming :> error-box[structural representation]
pure-library-message || error-box[structural formatting policy]
error-box || metaprogramming[owned error erasure and ? propagation]

binding-mutability <= actor-owned-state[consumed state locals]
dispatch-actor-io-runtime <= actor-owned-state[actor runtime state storage]
pure-library-message || actor-owned-state[message payload policy]
monomorphized-c-callbacks || actor-owned-state[runtime callback ABI]

dispatch-actor-io-runtime <= actor-reactor-effects[async operation backend]
actor-owned-state <= actor-reactor-effects[safe actor-owned state]
pure-library-message <= actor-reactor-effects[event payload policy]
monomorphized-c-callbacks || actor-reactor-effects[runtime callback ABI]
```

The main consequence is that SOP structural representation belongs to
`metaprogramming`. Structural message policy and witness production belong to
`pure-library-message`. Other proposals consume the representation or ordinary
capability impls produced through those routes.

`dispatch-actor-io-runtime` is implemented and moved to `proposal/done/`. Its
runtime ABI still matters for `monomorphized-c-callbacks`, which owns the later
stdlib-lowering step that removes actor-specific compiler builtins.

`unsafe` owns the source marker for imported C calls and raw handle adoption.
For message policy, it follows `pure-library-message`: that proposal owns
`Message` semantics, and `unsafe` later marks manual policy impls as trusted
implementation sites.
