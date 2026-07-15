# Typed Task Errors Proposal

## Historical Status

This proposal established `Task<T, E>` and `TaskGroup<T, E>`. The later
`error-downcast` design supersedes only its erased cross-owner carrier policy:
`Error` is local-only and does not implement `Message`; erased task failures use
`Report`, and task carriers use `MessageClone(Report)`. Concrete messageable
error types remain unchanged. The pre-proposal `Task<T>` baseline and references
to `Task<T, Error>` are retained where they explain the migration, but they do
not describe the current API. `design.md` is normative.

This proposal lets spawned async tasks preserve concrete application error
types instead of forcing task bodies through the standard boxed `Error` type.

## Proposal Order

```text
async-await < typed-task-errors
runtime-future-adapters <= typed-task-errors[task result storage]
error-box <= typed-task-errors[explicit erasure boundaries]
pure-library-message <= typed-task-errors[task result transfer policy]
```

`async-await` owns the existing task model, `spawn`, task awaits, and async
frame safety. `runtime-future-adapters` owns the runtime future storage
mechanism that task result storage builds on. `error-box` remains the fallback
for explicit erased process boundaries. `pure-library-message` owns the
cross-task value transfer policy for task results.

## Problem

The current async task API fixes spawned task output to `Result<T, Error>`:

```ciel
export struct Task<T> {
    *void handle;
}

export Result<Task<T>, AsyncError> spawn<
    T,
    A: Awaitable<Result<T, Error>> + Abortable
>(A body);

unsafe impl<T> awaitable_future<Result<T, Error>>(*const Task<T> task);
```

This makes every spawned task an error-erasure boundary. Application code that
otherwise uses a concrete error enum, such as `Result<T, TunnelError>`, must add
thin wrapper futures that convert the concrete error into `/std/error.Error`
only to satisfy `spawn`. That has several costs:

1. It encourages boxed `Error` inside application code.
2. It hides structured error categories from task awaiters.
3. It adds boilerplate wrapper functions at every spawn site.
4. It makes task APIs less expressive than ordinary async functions, which can
   already return `Result<T, E>` for custom `E` when `E` satisfies the async
   runtime carrier rules.

## Goals

1. Let `spawn` accept async bodies whose output is `Result<T, E>` for concrete
   application error types `E`.
2. Let awaiting a task recover the same `Result<T, E>` shape.
3. Preserve the existing `AsyncError` setup failures from `spawn` itself.
4. Keep task result transfer safe across task ownership boundaries.
5. Avoid implicit conversion to boxed `Error` unless user code explicitly asks
   for an erased boundary.
6. Require task handle types to name both the success and error payloads until
   the language supports default generic arguments.

## Non-Goals

1. Adding implicit general-purpose error conversions for `?`.
2. Removing `/std/error.Error`.
3. Changing ordinary async function syntax.
4. Allowing non-transferable task results or error payloads to cross task
   ownership boundaries.
5. Reworking actor APIs such as `spawn_actor_cloned` or `spawn_actor_state`.
6. Adding `spawn_result` as the final model. A helper that erases errors may be
   useful at explicit erasure boundaries, but it should not be the primary API.

## Proposed API

Make the task handle carry both the success and error payload types:

```ciel
export struct Task<T, E> {
    *void handle;
}

export Result<Task<T, E>, AsyncError> spawn<
    T,
    E,
    A: Awaitable<Result<T, E>> + Abortable
>(A body);

unsafe impl<T, E> awaitable_future<Result<T, E>>(*const Task<T, E> task);
```

The long-term API should remain `Task<T, E>`, not `Task<Out>`. Spawned tasks are
specifically `Result`-producing because task await has to surface task runtime
failures, cancellation failures, and task-boundary clone failures through the
awaited output. Keeping the success and error payloads explicit lets the
compiler attach targeted transfer and carrier obligations to both sides of the
result. A fully general `Task<Out>` would need either a second outer result, a
panic path for task failures, or a new carrier protocol for arbitrary `Out`;
that is a broader future design and not the right primary task API.

The API is `Task<T, E>` only until the language grows default generic
arguments. Code that wants erased transferable task errors writes the error
type explicitly as `Task<T, Report>`.

If default generic arguments exist by the time this lands, the standard library
may use a defaulted parameter:

```ciel
export struct Task<T, E = Report> {
    *void handle;
}
```

Without default generic arguments, `Task<T>` is not a valid task handle type in
the standard API.

Task groups should carry the same error type:

```ciel
export struct TaskGroup<T, E> {
    *void handle;
}

export Result<TaskGroup<T, E>, AsyncError> task_group<T, E>();
export Result<void, AsyncError> group_add<T, E>(
    *const TaskGroup<T, E> group,
    Task<T, E> task
);
export async Result<T, E> group_next<T, E>(*const TaskGroup<T, E> group);
```

Heterogeneous task errors should be modeled explicitly by the application, for
example with an application error enum or `TaskGroup<T, Report>`. The standard
library should not provide a separate erased heterogeneous task-group path,
because that would reintroduce the same implicit error-erasure boundary this
proposal removes.

## Resolved Design Choices

This proposal has no remaining design-level open questions. The settled choices
are:

1. Use `Task<T, E>`, not `Task<Out>`. Tasks are specifically
   `Result`-producing because task await has to report task runtime and
   transfer failures through the awaited output. A fully general `Task<Out>`
   needs a broader carrier protocol and should not block this feature.
2. Keep `spawn` as the primary API and do not add `spawn_result` as the model.
   Transferable error erasure uses `Report` through explicit result types or
   helper functions, and remains visible at the call site.
3. Do not provide a `Task<T>` compatibility path before default generic
   arguments exist. All task handle types must be spelled `Task<T, E>`, with
   erased transferable task errors written explicitly as `Task<T, Report>`.
4. Make task groups homogeneous in their error type: `TaskGroup<T, E>` accepts
   only `Task<T, E>`. Heterogeneous groups must use an application enum or an
   explicit erased error type.
5. Reuse generic future result storage and the existing `Message`/
   `clone_message` transfer machinery. The C runtime should remain unaware of
   the semantic shape of `E`; generated code supplies the monomorphized layout
   and transfer hooks.
6. Require the task error type to carry both classes of synthesized failures:
   async runtime failures and task-boundary message-clone failures.
   `async::AsyncError`, or an enum variant wrapping `async::AsyncError`, is the
   preferred carrier because it covers both paths. Separate carrier variants
   are valid only when both checks are satisfied.

## Semantics

`spawn(body)` starts `body` as an independent task. If `body` completes with
`Ok(value)`, awaiting the returned task yields `Ok(value)`. If `body` completes
with `Err(error)`, awaiting the task yields `Err(error)` with the original
concrete error type `E`.

`spawn` itself still returns `Result<_, AsyncError>` because task creation can
fail before the body has started. Cancellation APIs and task status APIs keep
using `AsyncError` for task-runtime operation failures.

Code that wants a transferable erased task error chooses `Report` explicitly:

```ciel
async Result<T, Report> erase_task_error<T, E: ErrorTrait>(
    Future<Result<T, E>> body
) {
    return await body?;
}
```

The enclosing result type makes the report conversion visible. Application code
that needs structured recovery should keep a concrete messageable `E` instead.

## Type Checking

The compiler's special handling for `async::spawn` should infer both `T` and
`E` from the awaitable output `Result<T, E>`.

The spawn boundary must attach transfer obligations to both result payloads:

1. `T` must be safe to transfer from the spawned task to the awaiting task.
2. `E` must be safe to transfer from the spawned task to the awaiting task.
3. Captures moved into the spawned task keep the existing spawn-boundary
   obligations.
4. `E` must be able to represent task await failures synthesized by the
   runtime, including async runtime failures and message-clone failures when
   task results cross owners.

The existing custom async `Result<T, E>` runtime-carrier rule remains relevant:
async functions returning custom `E` must still be representable when async
runtime failures need to be surfaced by that function. This proposal does not
change that rule; it only prevents `spawn` from forcing a second erased error
boundary after the async function already type-checks.

For task await, the carrier rule must also cover message-clone failure. The
task-compatible check is the conjunction of two abilities:

1. `E` can represent async runtime failure, using the existing carriers:
   `/std/error.Report`, `async::AsyncError`, or a concrete enum with a
   `Runtime(i64)`, `Async(async::AsyncError)`,
   `TaskGroupAsync(async::AsyncError)`, or applicable
   `Resource(resource::ResourceError)` variant.
2. `E` can represent task-boundary clone failure, using `/std/error.Report`,
   `async::AsyncError`, or a concrete enum with a `MessageClone(Report)`,
   `Async(async::AsyncError)`, or `TaskGroupAsync(async::AsyncError)` variant.

A single `Async(async::AsyncError)` or
`TaskGroupAsync(async::AsyncError)`-style carrier satisfies both checks because
`AsyncError` already contains `MessageClone(Report)`. A standalone
`MessageClone(Report)` carrier is not enough because it cannot represent runtime
failures. This follows the existing generated error synthesis paths for async
runtime failures and task-boundary clone failures.

## Lowering And Runtime

Task result storage must be parameterized by the full result type
`Result<T, E>`, not one erased carrier such as `Result<T, Report>`.

The runtime task handle may remain an opaque pointer, but the compiler and
stdlib must agree on the result layout for each `Task<T, E>` instance. Awaiting
a task copies or moves the stored `Result<T, E>` out of the completed task using
the same result-transfer policy used by the original `Task<T>` implementation.

The C runtime does not need to understand `E` semantically. It only needs enough
typed size, alignment, run, cleanup, and result-copy/move hooks from generated
code to store and deliver the monomorphized result.

The storage mechanism should be shared with generic `Future<Out>` storage:
tasks should continue to hold an opaque runtime future with the output size and
alignment supplied by generated code. The task-specific part is only the
cross-owner transfer on await. That transfer should reuse the standard
`Message`/`clone_message` machinery for `Result<T, E>` rather than adding a
separate task-only clone hook to the C runtime.

## Diagnostics

When a task body output is not `Result<T, E>`, keep the current diagnostic that
`spawn` requires an awaitable task result.

When `T` or `E` cannot cross the task boundary, report which side failed:

```text
task result value cannot cross task boundary
task error value cannot cross task boundary
```

When type inference cannot determine `E`, diagnostics should suggest adding an
expected `Task<T, E>` type or annotating the async function result.

## Testing Strategy

Add fixtures that cover:

1. spawning `async Result<T, LocalError>` and awaiting `Task<T, LocalError>`;
2. propagating the awaited task's concrete `Err(LocalError)` with `?`;
3. rejecting non-transferable success payloads;
4. rejecting non-transferable error payloads;
5. accepting `Task<T, Report>` and rejecting `Task<T, Error>` with a targeted
   replacement diagnostic;
6. task cancellation and runtime setup failures still reporting `AsyncError`;
7. `TaskGroup<T, E>` preserving typed task errors;
8. rejecting `group_add` when the task error type differs from the group error
   type;
9. rejecting spawned task error types that cannot carry task await runtime or
   message-clone failures.
