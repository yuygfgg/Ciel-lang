# Runtime Future Adapters Proposal

This proposal lets the standard library define small runtime-backed future
adapters in Ciel instead of routing each adapter through compiler special cases.
It keeps async function and `await` lowering as compiler-owned behavior, but
moves one-shot adapter futures such as async channels, task-group completion,
timers, and operation-token futures toward ordinary stdlib code.

## Proposal Order

```text
monomorphized-c-callbacks < runtime-future-adapters
async-await :> runtime-future-adapters[async function and await lowering]
unsafe <= runtime-future-adapters[unsafe runtime future construction]
resource-management <= runtime-future-adapters[context cleanup]
```

`monomorphized-c-callbacks` supplies generic internal C ABI function items such
as `task_group_next_run::<T>`. `async-await` continues to own async state-machine
lowering, `await`, `select`, task spawning, and compiler-generated async
function frames. This proposal only defines the trusted bridge that lets stdlib
code allocate a runtime `Future<T>` from a typed context plus C ABI callbacks.

## Problem

The async runtime already has a small future ABI: a `CielFuture` stores a
context pointer, a run callback, result storage, and a cleanup callback. Runtime
operations such as channel send, channel receive, task-group next, and raw
operation polling need the current `CielFuture *` so they can bind the pending
source that will later wake the task or select arm.

Today the standard-library surface hides this behind declarations such as:

```ciel
export async Result<void, AsyncError> send<T: Message>(Sender<T> sender, T value) = .send;
async Result<Task<T>, AsyncError> group_next_task<T>(*const TaskGroup<T> group) = .next_task;
export Future<Result<Out, AsyncError>> future_from_op<Op: OperationFuture<Out = _>>(Op op);
```

Those functions are not ordinary calls. Type checking recognizes their module
and name, produces dedicated THIR nodes, and codegen emits specialized C context
structs plus run and cleanup callbacks. This creates several problems:

1. The standard library cannot add a new runtime-backed future primitive without
   compiler changes.
2. Third-party packages cannot provide primitives with the same integration
   quality.
3. The compiler must know detailed stdlib API names and payload shapes.
4. The codegen layer owns adapter code that is conceptually stdlib policy, such
   as converting channel errors into `AsyncError`.
5. `GeneratedFuture` carries per-adapter `CancelSafe` and `Abortable` facts, so
   simply returning plain `Future<T>` from stdlib would lose useful diagnostics.

The existing runtime callback ABI also makes a pure stdlib implementation
awkward because the callback receives only `ctx` and `out`. Compiler-generated
contexts currently store a back-pointer to their own `CielFuture`. A source
level API should not depend on that self-referential context convention.

## Goals

1. Change the runtime future callback ABI so the current `CielFuture *` is
   passed explicitly to run and cleanup callbacks.
2. Provide an unsafe std-internal API for constructing a `Future<Out>` from a
   typed context and monomorphized C ABI callbacks.
3. Let stdlib define runtime-backed future adapter newtypes with precise
   `Awaitable`, `CancelSafe`, and `Abortable` implementations.
4. Allow `send`, `recv`, `reserve`, `group_next_task`, `future_from_op`, and
   similar one-shot adapters to be implemented as normal stdlib functions.
5. Preserve compiler ownership of async function frames, `await` suspension,
   `select`, task spawning, task cancellation, and frame cleanup.
6. Keep the new constructor unsafe and internal-facing; ordinary async code
   should use stable stdlib APIs.

## Non-Goals

1. Adding a public user-level `Future` or `Poll` trait.
2. Replacing compiler lowering for async functions or async closures.
3. Passing Ciel closures directly as C callbacks.
4. Supporting arbitrary foreign event loops or callbacks from unattached C
   threads.
5. Making all `Future<T>` values cancel-safe.
6. Removing every async compiler builtin in one step.
7. Changing the semantics of `await`, `select`, `timeout`, `spawn`, or
   `block_on`.

## Runtime ABI

Change the runtime callback typedefs from:

```c
typedef int32_t (*CielFutureRunFn)(void *ctx, void *out);
typedef void (*CielFutureCleanupFn)(void *ctx, int32_t reason);
```

to:

```c
typedef int32_t (*CielFutureRunFn)(CielFuture *future, void *ctx, void *out);
typedef void (*CielFutureCleanupFn)(CielFuture *future, void *ctx, int32_t reason);
```

`ciel_future_new` keeps the same ownership model and arguments, but its callback
types use the new signatures:

```c
CielFuture *ciel_future_new(
    size_t result_size,
    size_t result_align,
    CielFutureRunFn run,
    void *ctx,
    CielFutureCleanupFn cleanup
);
```

The runtime must pass the current future to both callbacks:

```c
int32_t rc = future->run(future, future->ctx, future->result);
cleanup(future, ctx, reason);
```

Callbacks may pass this pointer to runtime helper functions such as
`ciel_future_bind_operation`, `ciel_future_clear_operation`,
`ciel_async_channel_send_poll`, and `ciel_task_group_next_task_poll`. They must
not run or poll the same future recursively, and they should not store the
future pointer in their context. The pointer is supplied on every callback
invocation specifically to avoid self-referential context layouts.

Cleanup remains the abort/cancellation cleanup hook. A run callback that returns
anything other than `EAGAIN` must consume, release, or otherwise settle context
state needed by that path before returning. The cleanup callback handles the
case where the runtime aborts a pending future before the run callback reaches a
terminal result.

## Stdlib Runtime Future API

Add an internal module such as `/std/async/internal/runtime_future`:

```ciel
export import /std/async/core;
import /std/c as c;
import /std/meta;

unsafe extern "C" {
    opaque struct CielFuture;
}

type RuntimeRun =
    extern "C" c::c_int fn(*CielFuture future, *void ctx, *void out);
type RuntimeCleanup =
    extern "C" void fn(*CielFuture future, *void ctx, c::c_int reason);

unsafe extern "C" {
    *CielFuture ciel_future_new(
        usize result_size,
        usize result_align,
        RuntimeRun run,
        *void ctx,
        RuntimeCleanup cleanup
    );
}

export unsafe Future<Out> new<Out, Ctx>(
    Ctx @ctx,
    RuntimeRun run,
    RuntimeCleanup cleanup
);
```

`new<Out, Ctx>` transfers ownership of `ctx` into GC-managed runtime future
context storage, calls `ciel_future_new(type_size<Out>(), type_align<Out>(),
run, ctx_raw, cleanup)`, and returns `Future<Out>`. Allocation failure should
panic, matching compiler-generated async function allocation behavior.

The constructor is unsafe because the caller must uphold all of these
contracts:

1. `run` and `cleanup` must cast `ctx_raw` back to the exact `Ctx` type used at
   construction.
2. `out_raw` must be written only as `Out`.
3. `run` must return `0` only after fully initializing `out_raw` when `Out` is
   non-erased.
4. `run` may return `EAGAIN` only after binding a pending source to `future`, or
   after arranging another wakeup path.
5. `cleanup` must make cancellation idempotent for any state that may be
   pending when the runtime aborts the future.
6. Context fields that own resources must be consumed or closed exactly once
   across ready, error, and cancellation paths.

This API should be re-exported only from an internal stdlib namespace. Normal
application code should keep using `/std/async`, `/std/async_io`,
`/std/async_net`, and `/std/async_time`.

## Adapter Newtypes

Stdlib adapters should not expose every `Future<T>` as cancel-safe. Instead,
each adapter that needs capability facts should define a narrow wrapper:

```ciel
struct TaskGroupNextFuture<T> {
    Future<Result<Task<T>, AsyncError>> inner;
}

unsafe impl<T> awaitable_future<Result<Task<T>, AsyncError>>(
    *const TaskGroupNextFuture<T> future
) {
    return awaitable_future<Result<Task<T>, AsyncError>>(&future->inner);
}

unsafe derive<T> cancel_safe_marker<TaskGroupNextFuture<T>>;

unsafe impl<T> abort_future(*TaskGroupNextFuture<T> future) {
    abort_future(&future->inner)?;
    return Ok;
}
```

The helper can then be ordinary Ciel code that returns an awaitable adapter
value:

```ciel
TaskGroupNextFuture<T> group_next_task<T>(
    *const TaskGroup<T> group
) {
    TaskGroupNextCtx<T> ctx = { group: group->handle };
    return TaskGroupNextFuture<T> {
        inner: unsafe {
            runtime_future::new<Result<Task<T>, AsyncError>, TaskGroupNextCtx<T>>(
                ctx,
                task_group_next_run::<T>,
                task_group_next_cleanup::<T>
            )
        }
    };
}
```

The exported `group_next<T>` helper can then await this value and await the
completed task result. The important point is that the adapter future is a
normal value with normal capability impls.

## Example Callback

With `monomorphized-c-callbacks`, the task-group next adapter can be expressed
as a generic internal C ABI callback whose visible C signature is concrete:

```ciel
struct TaskGroupNextCtx<T> {
    *void group;
}

extern "C" c::c_int task_group_next_run<T>(
    *runtime_future::CielFuture future,
    *void ctx_raw,
    *void out_raw
) {
    *TaskGroupNextCtx<T> ctx = unsafe { ctx_raw as *TaskGroupNextCtx<T> };
    *Result<Task<T>, AsyncError> out =
        unsafe { out_raw as *Result<Task<T>, AsyncError> };

    ?*void @raw_task = null;
    c::c_int rc = unsafe {
        ciel_task_group_next_task_poll(
            future,
            ctx->group as *CielTaskGroup,
            &raw_task
        )
    };
    if ((rc as i64) == EAGAIN) {
        return rc;
    }
    if ((rc as i64) == 0) {
        *out = Ok({ handle: raw_task as *void });
        return 0;
    }
    *out = Err(async_error_from_runtime_rc(rc));
    return 0;
}

extern "C" void task_group_next_cleanup<T>(
    *runtime_future::CielFuture future,
    *void ctx_raw,
    c::c_int reason
) {
    future;
    ctx_raw;
    reason;
}
```

This example avoids a context `future` field entirely. The runtime future is an
explicit callback argument.

## Type Checking

The unsafe constructor is an ordinary generic function from the type checker's
point of view. The type checker must still validate:

1. The callback values have `extern "C"` ABI and the exact runtime callback
   signatures.
2. Type-applied callback items such as `task_group_next_run::<T>` satisfy the
   rules from `monomorphized-c-callbacks`.
3. `Awaitable`, `CancelSafe`, and `Abortable` facts come from normal capability
   impls on adapter newtypes.
4. Hidden `Message` constraints for task and channel boundaries remain enforced
   by the public stdlib entry points or by existing compiler-owned task
   boundary checks.

After migration, the type checker should no longer need dedicated THIR nodes
for `AsyncChannelSend`, `AsyncChannelReserve`, `AsyncChannelRecv`,
`AsyncTaskGroupNext`, or `AsyncOpFuture`. Those operations become calls that
return awaitable wrapper values. The compiler may keep transitional recognition
while each adapter is moved.

## Lowering And Codegen

Required compiler and runtime changes:

1. Update generated async function and async closure run callbacks to accept
   `CielFuture *future` as the first parameter.
2. Remove the generated `future` field from compiler async contexts where it is
   needed only as a self-pointer; use the callback argument instead.
3. Update cleanup callbacks to accept `CielFuture *future` and use that argument
   when clearing pending sources or aborting active children.
4. Update runtime invocation sites to call `run(future, ctx, out)` and
   `cleanup(future, ctx, reason)`.
5. Teach codegen for monomorphized C callback items to produce internal C ABI
   symbols usable as `CielFutureRunFn` and `CielFutureCleanupFn`.
6. Keep compiler-generated async function and closure futures using
   `ciel_future_new`; only their callback signatures change.
7. Once stdlib adapters are migrated, remove the corresponding
   `async_gen` context/prototype/run generation for those adapters.

## Diagnostics

Suggested diagnostics:

```text
runtime future constructor requires an extern "C" run callback with type
`c_int fn(*CielFuture, *void, *void)`
```

```text
runtime future constructor requires an extern "C" cleanup callback with type
`void fn(*CielFuture, *void, c_int)`
```

```text
adapter future `{T}` is not cancel-safe; losing select and timeout require
`CancelSafe`
```

```text
generic C ABI callback `{name}` must be type-applied before it can be passed to
`runtime_future::new`
```

Most user-facing diagnostics should remain capability diagnostics on the public
adapter type, not raw runtime constructor errors.

## Migration Plan

1. Implement the runtime callback ABI change and update compiler-generated async
   function and closure callbacks.
2. Implement `monomorphized-c-callbacks`.
3. Add `/std/async/internal/runtime_future`.
4. Migrate `group_next_task<T>` first. It has the smallest context and validates
   the new wakeup path through `ciel_task_group_next_task_poll`.
5. Migrate channel `reserve`, `recv`, and `send`. Preserve payload `Message`
   requirements as standard-library generic constraints and preserve channel
   closed error mapping.
6. Migrate `future_from_op<Op>`. Preserve `raw_operation`, `poll_done`, operation
   cleanup, and cancellation behavior.
7. Decide whether `sleep_ms` remains a small compiler-recognized convenience or
   moves to the same adapter model.
8. Remove obsolete THIR variants, mono planning fields, and `async_gen` adapter
   emission after each migrated adapter has equivalent fixture coverage.

## Tests

1. A stdlib-defined `TaskGroupNextFuture<T>` waits for task completion and wakes
   through the task-group pending source.
2. `group_next<T>` preserves completion order and cancellation behavior after
   the compiler special case is removed.
3. Channel `send`, `reserve`, and `recv` preserve blocking, wakeup, closed-side
   errors, and `Message` diagnostics.
4. `future_from_op<Op>` infers `Out` through `OperationFuture<Out = _>` and
   preserves operation cancellation on abort.
5. `select` and `timeout` continue to accept cancel-safe adapter newtypes and
   reject non-cancel-safe operation futures.
6. Multiple payload types instantiate distinct C ABI callback symbols and emit
   them even when they are used only as callback values.
7. Runtime callback ABI tests cover run completion, pending wakeup, cancellation
   cleanup, and allocation failure panic behavior.
8. Generated async function and closure tests still pass after their callback
   signatures change.

## Open Questions

1. Should `/std/async/internal/runtime_future::new` return `Future<Out>` and
   panic on allocation failure, or return `Result<Future<Out>, AsyncError>` and
   make stdlib callers map allocation errors explicitly?
2. Should the runtime cleanup callback remain cancellation-only, or should a
   separate terminal destructor hook be added for all completed and failed
   futures?
3. Should the unsafe constructor be enforced as std-only by package visibility,
   or is an internal namespace plus `unsafe` sufficient?
4. Should `sleep_ms` move to this adapter model immediately, or remain a
   compiler primitive until timers need additional stdlib policy?
5. How much of `block_on`, task cancellation, and task spawning should remain
   compiler-recognized after adapter futures move into stdlib?
