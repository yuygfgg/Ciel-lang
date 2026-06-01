# Async/Await TODO

This checklist breaks `proposal/async-await.md` into executable vertical slices.
Each phase should land runnable behavior, fixtures, and any `design.md` updates
for behavior that actually exists in `src/`, `std/`, or `tutorial/`.

Keep the current flow API working until the final migration phase. For every
phase, run:

```text
cargo test -q --test ciel_cases ciel_case_metadata_is_valid
cargo test -q --test ciel_cases discovered_ciel_cases_pass_their_declared_expectations
git diff --check
```

## Phase 1: Runtime Future Driver

- [ ] Add the core future surface and primitive driver.
      Scope: add `/std/async` `Poll<T>`, `FutureContext`, `Future<T>`,
      `CancelSafe`, `Abortable`, async error constructors, and a runtime task
      driver that can poll a primitive future to completion through
      `async::block_on`.
      Tests: a run fixture drives a primitive ready future through
      `async::block_on` and prints the result.

- [ ] Add a sleep future over the existing async operation token.
      Scope: expose a transitional `sleep_ms_future` backed by the existing
      `CielAsyncOp` timer path, and keep `sleep_ms_async`,
      `sleep_ms_completion`, and `sleep_ms_task` working.
      Tests: one fixture drives `sleep_ms_future(1)` through `block_on`; the
      existing `std_async_time` flow fixtures still pass.

- [ ] Route primitive future wakeups without exposing frame pointers.
      Scope: `FutureContext` registers wakeups through runtime-owned routing
      state; callbacks must not receive pointers into user async frames.
      Tests: two primitive sleep futures complete independently through the
      task driver.

## Phase 2: First Language Await

- [ ] Add contextual async syntax and first lowering in one slice.
      Scope: parse `async` functions, async closures, and `await` as contextual
      keywords; preserve `/std/async` module-path compatibility; lower an async
      function with one `await` of `sleep_ms_future`.
      Tests: `async Result<usize, Error> f() { await sleep_ms_future(1)?;
      return Ok(7); }` runs through `async::block_on(f())` and prints `7`.

- [ ] Type-check the initial future model.
      Scope: an async call returns an opaque generated future implementing
      `Future<Out>`; `await` requires `Future<Out>` and yields `Out`; `?` works
      immediately after an await of `Result<T, Error>`.
      Tests: storing a future in a local before `block_on` compiles; awaiting a
      non-future is rejected; using an async call where an ordinary
      `Result<T, Error>` is expected is rejected.

- [ ] Reject invalid async entry points.
      Scope: `await` outside async is rejected, and async functions cannot be
      exported as C ABI functions.
      Tests: focused error fixtures for both diagnostics.

## Phase 3: Async Frames And Cleanup

- [ ] Add multi-await frame lowering.
      Scope: generate program counters, frame storage, nested future storage,
      source-order evaluation, and resume code for multiple await points.
      Tests: a run fixture performs two awaits, calls a nested async function,
      and prints the final value.

- [ ] Add live-local and async-frame-safety analysis.
      Scope: permit owned frame-safe locals and direct local static read-only
      slices across await; reject raw pointers, nullable pointers, mutable
      slices, non-static borrowed slices, `ThreadLocal` handles, forbidden
      closure captures, and compound values with slice/reference-view fields.
      Tests: one positive fixture for a frame-safe non-`Message` local and one
      positive fixture for a string-literal slice; focused error fixtures for
      each rejected category.

- [ ] Add deterministic async-frame cleanup and trampoline scheduling.
      Scope: initialized frame fields are cleaned up on return, `Err`, panic,
      cancel, or abort; immediate completions resume through a trampoline with a
      fairness budget instead of recursive C calls.
      Tests: `defer` cleanup runs on async early return and `Err`; a loop of
      immediately ready awaits does not grow the native stack; two ready-loop
      tasks both make progress.

## Phase 4: Task Ownership Boundary

- [ ] Add `Task<T>`, `async::spawn`, and task awaiting.
      Scope: lower directly spawned async closures to actor-owned task
      initialization and generated dispatch; task handles store completion
      results and wake awaiters.
      Tests: a spawned task awaits a timer and returns a value; a task can await
      another task.

- [ ] Add task boundary policy.
      Scope: task results and spawned-task captures get hidden `Message`
      obligations; direct async closure captures are analyzed without requiring
      retained `: Message` closure syntax.
      Tests: a structurally messageable config capture passes; a captured
      non-`Message` value fails with a boundary diagnostic that points at the
      capture or nested field; a non-`Message` local created inside the task can
      live across await if frame-safe.

- [ ] Add task status and cancellation entry points.
      Scope: add `async::cancel` and `async::is_finished` or explicitly drop
      them from the public surface before this phase closes.
      Tests: fixtures observe finished state and a stable cancellation result.

## Phase 5: Awaitable Standard Library I/O

- [ ] Replace transitional timer futures with awaitable `/std/async_time`.
      Scope: add `async_time::sleep_ms` as the user-facing awaitable timer and
      keep old flow helpers until final migration.
      Tests: an async function awaits `sleep_ms` directly; old timer flow
      fixtures still pass.

- [ ] Add awaitable `/std/async_io` file operations.
      Scope: add awaitable `read_bytes` and `write_bytes` over the current async
      fd operation backend.
      Tests: a sequential async file-copy fixture uses `await` instead of
      `flow::then`; facade fixtures compile.

- [ ] Add awaitable `/std/async_net` TCP operations.
      Scope: add awaitable `accept`, `connect`, `read`, `read_into`, `write`,
      and `write_all`, preserving zero-length `Bytes` EOF behavior and reusable
      buffer semantics for `read_into`.
      Tests: a sequential async TCP echo fixture awaits accept, connect, read,
      and write; a `read_into` loop reuses `Bytes` capacity; existing
      `std_async_net` flow fixtures still pass.

- [ ] Settle the public `Bytes` location.
      Scope: move or re-export `Bytes` as the chosen general byte-buffer
      surface without breaking existing `aio::Bytes` and `anet::Bytes` users
      during migration.
      Tests: compatibility and `/std/lib` facade fixtures compile.

## Phase 6: Cancellation, Abort, And Timeout

- [ ] Add operation generation routing for abort-safe callbacks.
      Scope: external completions route by actor mailbox id, task id, operation
      id, and generation; callbacks enqueue routed events and never dereference
      async frames or task-state pointers.
      Tests: aborting a suspended libdispatch-backed operation drops the async
      frame while a queued callback later posts only a stale completion event.

- [ ] Implement trusted `CancelSafe` and `Abortable`.
      Scope: primitive and generated futures implement these capabilities only
      when their behavior actually preserves protocol state or supports bounded
      abort cleanup; do not infer `CancelSafe` merely because child awaits are
      `CancelSafe`.
      Tests: a multi-await frame reader is not inferred `CancelSafe`; a
      non-`Abortable` future is rejected in a cancellable task.

- [ ] Add task abort behavior.
      Scope: cancelling a task aborts the currently suspended operation,
      detaches the frame from operation tokens, and runs deterministic cleanup.
      Tests: cancelling a task with a pending raw TCP read closes or poisons the
      stream and terminates without leaking its task actor.

- [ ] Add `async::timeout`.
      Scope: timeout uses the same internal registration machinery that later
      backs `select`; `timeout(task)` cancels only the waiter, not the running
      task's internal protocol state.
      Tests: pending connect timeout succeeds; raw TCP read, `read_into`, and
      write are rejected by timeout as not `CancelSafe`; a non-`CancelSafe`
      protocol reader isolated in a task survives waiter timeout.

## Phase 7: Async Communication

- [ ] Add bounded async channels.
      Scope: add `Sender<T>`, `Receiver<T>`, `SendPermit<T>`, `ChannelPair<T>`,
      `channel`, async `send`, sync `try_send`, async `reserve`, sync
      `permit_send`, async `recv`, `close`, and `close_receiver`.
      Tests: task send/recv works; full channel suspends `send`; `try_send`
      reports full or closed without suspension.

- [ ] Add channel lifecycle and cleanup semantics.
      Scope: track sender and receiver counts; last sender wakes receivers;
      last receiver wakes senders and reservations; task-frame cleanup releases
      endpoints before relying on GC finalization.
      Tests: cancelled `reserve` does not send or lose a value; `permit_send`
      commits after capacity reservation; dropping, failing, or aborting the
      last endpoint wakes waiters with `channel_closed_error()`.

- [ ] Attach channel payload boundary policy.
      Scope: channel payloads get hidden `Message` obligations at send/recv
      boundaries.
      Tests: messageable payloads pass; non-messageable payloads fail with a
      boundary diagnostic.

- [ ] Add task groups.
      Scope: add `TaskGroup<T>`, `task_group`, `group_add`, `group_next`,
      `group_cancel_all`, and `group_close` for dynamic concurrency.
      Tests: `group_next` returns completed tasks in completion order without
      cancelling unfinished tasks; `group_cancel_all` aborts unfinished tasks
      through `Abortable`; closing a group releases remaining handles.

## Phase 8: Select And Buffered TCP Reads

- [ ] Add compiler-level `select` and `biased select`.
      Scope: type-check arms as a flat list of futures, lower to internal
      `SelectSet<R>`, poll every arm once before parking, use fair tie handling
      for default `select`, and source-order priority for `biased select`.
      Tests: `select` races timer, channel receive, and task completion;
      flat-list fair tie behavior and biased tie behavior are both tested.

- [ ] Enforce selectable future bounds.
      Scope: every losing future must implement `CancelSafe + Abortable`; stale
      completions are discarded only when the contract permits it.
      Tests: raw TCP read, `read_into`, and write are rejected in `select`; a
      cancelled losing `CancelSafe` future does not resume user code; a stale
      completion after cancellation is discarded.

- [ ] Add cancellation-safe buffered TCP reads.
      Scope: add `split`, `AsyncTcpReadHalf`, `AsyncTcpWriteHalf`,
      `BufferedStreamReader`, `buffered_reader`, `read_buffered`, and
      `into_read_half`; `read_buffered` checks its private buffer before
      registering socket readiness and serializes or rejects overlapping reads.
      Tests: buffered read fixtures cover normal reads, EOF, residual private
      buffer bytes winning `select` immediately, and overlapping read policy.

## Phase 9: Migration And Flow Removal

- [ ] Rewrite tutorial chapter 9 around task async/await.
      Scope: teach tasks, async channels, awaitable I/O, timeout/select safety
      boundaries, and only then the low-level actor compatibility story.
      Tests: tutorial fixtures compile, run, and assert observable output.

- [ ] Migrate the intranet tunnel.
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
