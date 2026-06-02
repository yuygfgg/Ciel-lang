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
metaprogramming < schema-reflection
schema-reflection :> serialization[instance-free structural decode schema]

metaprogramming :> error-box[structural representation]
pure-library-message || error-box[structural formatting policy]
error-box || metaprogramming[owned error erasure and ? propagation]

binding-mutability <= actor-owned-state[consumed state locals]
dispatch-actor-io-runtime <= actor-owned-state[actor runtime state storage]
pure-library-message || actor-owned-state[message payload policy]

binding-mutability <= async-await[locals live across await]
dispatch-actor-io-runtime <= async-await[async operation backend]
actor-owned-state <= async-await[actor-owned async frame storage]
pure-library-message <= async-await[cross-task payload policy]
async-await :> async-task-lowering[spawn task and await lowering]
```

The main consequence is that SOP structural representation belongs to
`metaprogramming`. Structural message policy and witness production belong to
`pure-library-message`. Other proposals consume the representation or ordinary
capability impls produced through those routes.

`dispatch-actor-io-runtime` is implemented and moved to `proposal/done/`. Async
task lowering now belongs to `async-await`, because stackless await needs
compiler/runtime support for actor-owned async frame storage, hidden resume, and
cancellation. `monomorphized-c-callbacks` remains a separate FFI feature for
generic C ABI callback function items; it is no longer on the actor lowering
path.

`unsafe` owns the source marker for imported C calls and raw handle adoption.
For message policy, it follows `pure-library-message`: that proposal owns
`Message` semantics, and `unsafe` later marks manual policy impls as trusted
implementation sites.
