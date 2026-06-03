# Async/Await TODO

This checklist breaks `proposal/async-await.md` into executable vertical
slices. Each phase must land behavior on the final architecture path, with
fixtures and docs for behavior that actually exists in `src/`, `std/`, or
`tutorial/`.

Keep the current flow API working until the final migration phase. For every
phase, run:

```text
cargo test -q --test ciel_cases ciel_case_metadata_is_valid
cargo test -q --test ciel_cases discovered_ciel_cases_pass_their_declared_expectations
git diff --check
```

Implementation guardrails:

- Do not add public transitional APIs such as `sleep_ms_future`.
- Do not add a dummy ready future just to satisfy a phase test.
- Do not land parser-only async syntax without runnable lowering in the same
  phase.
- Any temporary helper must either be internal and survive on the final lowering
  path, or be test-only and removed in the same phase.
- Runtime callbacks must route through operation/task identity. They must not
  capture async frame pointers or task-state pointers.

## Phase 1: Minimal Async/Await Timer Slice

- [x] Land the first user-visible async/await path.
      Scope: add the final `/std/async` future surface, contextual async syntax,
      generated future type for one-await async functions/closures,
      `async::block_on`, and awaitable `/std/async_time::sleep_ms` in one
      vertical slice.
      Tests: `async Result<usize, Error> f() { await async_time::sleep_ms(1)?;
      return Ok(7); }` runs through `async::block_on(f())` and prints `7`.

- [x] Use final-shaped wake routing from the first slice.
      Scope: `sleep_ms` wakeups go through runtime-owned operation/task routing
      state, with room for task id, operation id, and generation even if the
      first test only has one operation.
      Tests: two async timer functions driven through the runtime complete
      independently and do not expose frame pointers to callbacks.

- [x] Type-check the initial final model.
      Scope: async calls return opaque generated futures implementing
      `Awaitable<Out>`; standard `Future<Out>` and `Task<T>` implement
      Awaitable through `/std/async`; `await` yields `Out`; `?` works after
      awaiting `Result<T, Error>`.
      Tests: storing a future in a local before `block_on` compiles; awaiting a
      non-future is rejected; using an async call where an ordinary
      `Result<T, Error>` is expected is rejected.

- [x] Preserve compatibility while adding contextual keywords.
      Scope: `async` remains usable as the `/std/async` module-path segment;
      `await` outside async is rejected; async functions cannot be exported as C
      ABI functions; old flow timer helpers keep working.
      Tests: `/std/async as flow` still imports; focused diagnostics cover
      invalid `await` and exported C ABI async functions; existing
      `std_async_time` flow fixtures still pass.

## Phase 2: Frames, Cleanup, And Trampoline

- [x] Add multi-await frame lowering.
      Scope: generate program counters, frame storage, nested future storage,
      source-order evaluation, and resume code for multiple await points.
      Tests: a run fixture performs two awaits, calls a nested async function,
      and prints the final value.

- [x] Add live-local and async-frame-safety analysis.
      Scope: permit owned frame-safe locals and direct local static read-only
      slices across await; reject raw pointers, nullable pointers, mutable
      slices, non-static borrowed slices, `ThreadLocal` handles, forbidden
      closure captures, and compound values with slice/reference-view fields.
      Tests: one positive fixture for a frame-safe non-`Message` local and one
      positive fixture for a string-literal slice; focused error fixtures for
      each rejected category.

- [x] Add deterministic async-frame cleanup and trampoline scheduling.
      Scope: initialized frame fields are cleaned up on return, `Err`, panic,
      cancel, or abort; immediate completions resume through a trampoline with a
      fairness budget instead of recursive C calls.
      Tests: `defer` cleanup runs on async early return and `Err`; a loop of
      immediately ready awaits does not grow the native stack; two ready-loop
      tasks both make progress.

## Phase 3: Task Ownership Boundary

- [x] Add `Task<T>`, `async::spawn`, and task awaiting.
      Scope: lower directly spawned async closures to actor-owned task
      initialization and generated dispatch; task handles store completion
      results and wake awaiters.
      Tests: a spawned task awaits a timer and returns a value; a task can await
      another task.

- [x] Add task boundary policy.
      Scope: task results and spawned-task captures get hidden `Message`
      obligations; direct async closure captures are analyzed without requiring
      retained `: Message` closure syntax.
      Tests: a structurally messageable config capture passes; a captured
      non-`Message` value fails with a boundary diagnostic that points at the
      capture or nested field; a non-`Message` local created inside the task can
      live across await if frame-safe.

- [x] Add task status and cancellation entry points.
      Scope: add `async::cancel` and `async::is_finished` or explicitly drop
      them from the public surface before this phase closes.
      Tests: fixtures observe finished state and a stable cancellation result.

## Phase 4: Awaitable File And TCP I/O

- [x] Add awaitable `/std/async_io` file operations.
      Scope: add awaitable `read_bytes` and `write_bytes` over the current async
      fd operation backend.
      Tests: a sequential async file-copy fixture uses `await` instead of
      `flow::then`; facade fixtures compile; old flow file-I/O helpers still
      pass.

- [x] Add awaitable `/std/async_net` TCP operations.
      Scope: add awaitable `accept`, `connect`, `read`, `read_into`, `write`,
      and `write_all`, preserving zero-length `Bytes` EOF behavior and reusable
      buffer semantics for `read_into`.
      Tests: a sequential async TCP echo fixture awaits accept, connect, read,
      and write; a `read_into` loop reuses `Bytes` capacity; existing
      `std_async_net` flow fixtures still pass.

- [x] Settle the public `Bytes` location.
      Scope: move or re-export `Bytes` as the chosen general byte-buffer
      surface without breaking existing `aio::Bytes` and `anet::Bytes` users
      during migration.
      Tests: compatibility and `/std/lib` facade fixtures compile.

## Phase 5: Cancellation, Abort, And Timeout

- [x] Complete generation-routed operation abort.
      Scope: external completions route by actor mailbox id, task id, operation
      id, and generation; callbacks enqueue routed events and never dereference
      async frames or task-state pointers.
      Tests: aborting a suspended libdispatch-backed operation drops the async
      frame while a queued callback later posts only a stale completion event.

- [x] Implement trusted `CancelSafe` and `Abortable`.
      Scope: primitive and generated futures implement these capabilities only
      when their behavior actually preserves protocol state or supports bounded
      abort cleanup; do not infer `CancelSafe` merely because child awaits are
      `CancelSafe`.
      Tests: a multi-await frame reader is not inferred `CancelSafe`; a
      non-`Abortable` future is rejected in a cancellable task.

- [x] Add task abort behavior.
      Scope: cancelling a task aborts the currently suspended operation,
      detaches the frame from operation tokens, and runs deterministic cleanup.
      Tests: cancelling a task with a pending raw TCP read closes or poisons the
      stream and terminates without leaking its task actor.

- [x] Add `async::timeout`.
      Scope: timeout uses the same internal registration machinery that later
      backs `select`; `timeout(task)` cancels only the waiter, not the running
      task's internal protocol state.
      Tests: pending connect timeout succeeds; raw TCP read, `read_into`, and
      write are rejected by timeout as not `CancelSafe`; a non-`CancelSafe`
      protocol reader isolated in a task survives waiter timeout.

## Phase 6: Select And Buffered TCP Reads

- [x] Add compiler-level `select` and `biased select`.
      Scope: type-check arms as a flat list of futures, lower to internal
      `SelectSet<R>`, poll every arm once before parking, use fair tie handling
      for default `select`, and source-order priority for `biased select`.
      Tests: `select` races timer, task completion, and cancellable TCP
      connect/accept futures; flat-list fair tie behavior and biased tie
      behavior are both tested. Channel receive select coverage lands with
      Phase 7 channels.

- [x] Enforce selectable future bounds.
      Scope: every losing future must implement `CancelSafe + Abortable`; stale
      completions are discarded only when the contract permits it.
      Tests: raw TCP read, `read_into`, and write are rejected in `select`; a
      cancelled losing `CancelSafe` future does not resume user code; a stale
      completion after cancellation is discarded.

- [x] Add cancellation-safe buffered TCP reads.
      Scope: add `split`, `AsyncTcpReadHalf`, `AsyncTcpWriteHalf`,
      `BufferedStreamReader`, `buffered_reader`, `read_buffered`, and
      `into_read_half`; `read_buffered` checks its private buffer before
      registering socket readiness and serializes or rejects overlapping reads.
      Tests: buffered read fixtures cover normal reads, EOF, residual private
      buffer bytes winning `select` immediately, and overlapping read policy.

## Phase 7: Async Communication

- [x] Add bounded async channels.
      Scope: add `Sender<T>`, `Receiver<T>`, `SendPermit<T>`, `ChannelPair<T>`,
      `channel`, async `send`, sync `try_send`, async `reserve`, sync
      `permit_send`, async `recv`, `close`, and `close_receiver`.
      Tests: task send/recv works; full channel suspends `send`; `try_send`
      reports full or closed without suspension; `select` can race channel
      receive with timers and task completion.

- [x] Add channel lifecycle and cleanup semantics.
      Scope: track sender and receiver counts; last sender wakes receivers;
      last receiver wakes senders and reservations; task-frame cleanup releases
      endpoints before relying on GC finalization.
      Tests: cancelled `reserve` does not send or lose a value; `permit_send`
      commits after capacity reservation; dropping, failing, or aborting the
      last endpoint wakes waiters with `channel_closed_error()`.

- [x] Attach channel payload boundary policy.
      Scope: channel payloads get hidden `Message` obligations at send/recv
      boundaries.
      Tests: messageable payloads pass; non-messageable payloads fail with a
      boundary diagnostic.

- [x] Add task groups.
      Scope: add `TaskGroup<T>`, `task_group`, `group_add`, `group_next`,
      `group_cancel_all`, and `group_close` for dynamic concurrency.
      Tests: `group_next` returns completed tasks in completion order without
      cancelling unfinished tasks; `group_cancel_all` aborts unfinished tasks
      through `Abortable`; closing a group releases remaining handles.

## Phase 8: Migration And Flow Removal

- [ ] Rewrite tutorial chapter 9 around task async/await.
      Scope: teach tasks, async channels, awaitable I/O, timeout/select safety
      boundaries, and only then the low-level actor compatibility story.
      Tests: tutorial fixtures compile, run, and assert observable output.

- [x] Migrate the intranet tunnel.
      Scope: replace public flow chains with tasks, async channels, awaitable
      TCP, and awaitable timers.
      Tests: intranet tunnel integration tests pass on the async/await API.

- [ ] Move operation-token adapters to an internal namespace.
      Scope: keep implementation tests possible but remove the adapter layer
      from the public teaching surface.
      Tests: public facade no longer exports `/std/async/adapter`.

- [ ] Delete the public flow API.
      Scope: remove public `AsyncRunner<S>`, `AsyncTask<S, Out>`,
      `Completion<S, Out>`, `spawn_runner`, `from_completion`, `then`, `start`,
      `stop_runner`, and `join_runner`.
      Tests: a negative fixture proves public `flow::then` and `AsyncRunner`
      names are no longer available; low-level actor fixtures still pass.

- [ ] Update docs and archive the proposal.
      Scope: update `design.md` for the landed async/await surface, update
      tutorial references, and move or mark `proposal/async-await.md` according
      to the final accepted state.
      Tests: `proposal/README.md` remains consistent; final discovered fixture
      suite and `git diff --check` pass.
