# 9. Channels and Async I/O

This chapter uses one file-copy workflow to teach the async flow API:

1. Create a channel for the final answer.
2. Spawn a flow runner and give it that channel.
3. Build a read task.
4. Chain a write task after the read with `flow::then`.
5. Start the task chain on the runner.
6. Receive the byte count from the channel.

The high-level API lives in `/std/async`:

- `flow::AsyncRunner<S>` runs a flow that carries a value of type `S`.
- `flow::AsyncTask<S, Out>` is a one-shot async flow that eventually produces
  `Out` and runs on a runner carrying `S`.
- `flow::then` chains a task to the next task.
- `flow::start` runs a task chain on a runner.

Async I/O helpers in `/std/async_io` return tasks directly. For a linear flow,
application code should not need a custom completion-message enum.

## What Is `Channel<usize>`?

`Channel<T>` is a typed queue for sending values of type `T` between pieces of
concurrent code.

`Channel<usize>` means this channel carries `usize` values. In this chapter the
single value is the number of bytes written by the copy flow.

The channel is needed because `main` starts work that finishes later. The final
flow callback cannot return the byte count directly to the original call stack;
that stack has already moved on. Instead, the callback sends the result into
the channel:

```ciel
channel_send(&done, written)?;
```

`main` waits for the value from the other end:

```ciel
usize written = must(channel_recv(&done));
```

The channel is not the file data. It is only the reply path. The file bytes move
through `aio::Bytes`; the final byte count moves through `Channel<usize>`.

## The Shape

The complete high-level example is in
`tutorial/examples/09_async_then_file_copy.ciel`. The core of it is short:

```ciel
Channel<usize> done = must(make_channel<usize>());
flow::AsyncRunner<Channel<usize>> worker =
    must(flow::spawn_runner<Channel<usize>>(done));

flow::AsyncTask<Channel<usize>, aio::Bytes> read =
    must(aio::read_bytes_task<Channel<usize>>(input, 64));
flow::AsyncTask<Channel<usize>, usize> copy = flow::then(read, |aio::Bytes bytes| {
    return aio::write_bytes_task<Channel<usize>>(output, bytes);
});

must(flow::start(worker, copy, |Channel<usize> done, usize written| {
    channel_send(&done, written)?;
    return Ok(done);
}));
```

Read the types from left to right:

```ciel
flow::AsyncRunner<Channel<usize>>
```

The runner carries one value while the flow is alive. Here that value is
`Channel<usize>`, the reply path back to `main`.

```ciel
flow::AsyncTask<Channel<usize>, aio::Bytes>
```

The first type argument matches the value carried by the runner. The second
type argument is the task output. This task runs on a runner carrying
`Channel<usize>`, and it eventually produces `aio::Bytes`.

```ciel
flow::AsyncTask<Channel<usize>, usize>
```

After the write task is chained, the whole copy task eventually produces a
`usize`: the number of bytes written.

## The Carried Value

Every runner has a carried type `S`. The carried value is available to the final
callback, where the flow reports its result.

In this chapter:

```ciel
S = Channel<usize>
```

The runner is created with the initial carried value:

```ciel
flow::AsyncRunner<Channel<usize>> worker =
    must(flow::spawn_runner<Channel<usize>>(done));
```

The final callback receives that carried value as its first parameter:

```ciel
|Channel<usize> done, usize written| {
    channel_send(&done, written)?;
    return Ok(done);
}
```

It returns `Ok(done)` because the runner should keep the same channel as its
carried value. A different flow could return a changed value.

The intermediate `then` closure does not receive the carried value:

```ciel
|aio::Bytes bytes| {
    return aio::write_bytes_task<Channel<usize>>(output, bytes);
}
```

That closure only decides what task comes next. The runner keeps the carried
value out of the middle of the chain until the final callback needs it.

## Open Handles Near Their Cleanup

The async file handles are runtime resources. Open each handle and put its
cleanup next to it:

```ciel
aio::AsyncFd input = must(aio::open_async_read("/tmp/ciel_tutorial_then_input.txt"));
defer aio::close_async(input);

aio::AsyncFd output = must(aio::create_async("/tmp/ciel_tutorial_then_output.txt"));
defer aio::close_async(output);
```

`defer` runs the direct function call when the current block exits through
normal control flow. Its return value is ignored. Use it for cleanup, not for
recovering from a close error.

## Build The First Task

Create the read task after the input handle is open:

```ciel
flow::AsyncTask<Channel<usize>, aio::Bytes> read =
    must(aio::read_bytes_task<Channel<usize>>(input, 64));
```

`read_bytes_task` creates a one-shot read task. The task is tied to the runner's
carried type, `Channel<usize>`, and will produce `aio::Bytes`.

For async I/O tasks, creating the task also creates the underlying runtime
operation. `flow::start` does not start a second read. It connects this task to
the runner so the completion can resume the flow later.

The type argument appears on the constructor because the task must know which
runner carried type it belongs to. The read operation itself does not use the
channel. The flow machinery uses that type to route continuations through the
right runner.

Do not reuse a task value. A task owns one async operation chain.

## Chain The Next Task

Use `flow::then` when the next operation depends on the previous result:

```ciel
flow::AsyncTask<Channel<usize>, usize> copy = flow::then(read, |aio::Bytes bytes| {
    return aio::write_bytes_task<Channel<usize>>(output, bytes);
});
```

The `then` closure receives `bytes`, the output of the read task. It returns the
next task, which writes those bytes to `output`.

The output type changes across the chain:

```text
read: AsyncTask<Channel<usize>, aio::Bytes>
copy: AsyncTask<Channel<usize>, usize>
```

The carried type does not change. The runner still carries `Channel<usize>`.

`flow::then` builds a new task value. It does not block. It does not finish the
read. It does not start the write. The write task is created when the read task
has produced bytes and the continuation runs.

## Start The Flow

`flow::start` attaches the task chain to the runner:

```ciel
must(flow::start(worker, copy, |Channel<usize> done, usize written| {
    channel_send(&done, written)?;
    return Ok(done);
}));
```

The final callback receives:

- the runner's carried value, `done`
- the output of the whole task chain, `written`

It sends the answer through the channel and returns the next carried value.

After starting the flow, `main` can wait for the answer:

```ciel
usize written = must(channel_recv(&done));
must(print_value(written));

must(flow::join_runner(&worker));
must(channel_close(&done));
```

Join the runner after the result is received. Close the channel when no more
values will be sent.

## Execution Timeline

Read the program in this order:

1. `main` writes stable input text.
2. `main` creates `done`, a `Channel<usize>`.
3. `main` spawns `worker`, an `AsyncRunner<Channel<usize>>`.
4. `main` opens `input` and `output`, with `defer` cleanup for both.
5. `main` creates the read task and its underlying read operation.
6. `main` calls `flow::then` and gets the copy task.
7. `main` calls `flow::start(worker, copy, final_callback)`.
8. The runner registers the read completion and keeps carrying `done`.
9. `main` waits on `channel_recv(&done)`.
10. The runtime later sends the read resume step to the runner.
11. The runner finishes the read and gives `aio::Bytes` to the `then` closure.
12. The `then` closure creates the write task.
13. The runner registers the write completion and keeps carrying `done`.
14. The runtime later sends the write resume step to the runner.
15. The runner finishes the write and calls the final callback.
16. The final callback sends `written` through `done`.
17. `main` receives the value, prints it, joins the runner, and closes the
    channel.

There is no suspended Ciel stack between steps.

## The Lower-Level State Machine

The task API is built on ordinary actor messages. Use the lower-level shape
when the protocol matters: custom routing, cancellation, several completion
targets, or a workflow that is not a linear chain.

The carried value from the high-level API is implemented as state inside this
hidden runner actor. You do not need that detail to write the normal flow code,
but it explains why the final callback returns the carried value.

At that level, the mailbox receives both commands and completion messages:

```ciel
enum ReaderMsg {
    Start(aio::AsyncFd, Actor<meta::Repr<ReaderMsg>>),
    ReadDone(aio::AsyncRead),
}
```

`Start` begins the read. `ReadDone` consumes the completed operation.

The actor starts the operation:

```ciel
aio::AsyncRead op = aio::read_bytes(fd, 16)?;
```

That returns an operation token, not the bytes. The actor then registers a
message for the runtime to send back later:

```ciel
ReaderMsg completed = ReadDone(op);
meta::Repr<ReaderMsg> completed_envelope = meta::into_repr(&completed);
aio::notify_read_done(&op, &self, completed_envelope)?;
```

The handler returns immediately. When the OS reports that the read finished,
the runtime sends `ReadDone(op)` to the actor:

```ciel
aio::Bytes bytes = aio::finish_read(op)?;
usize len = aio::bytes_len(bytes);
channel_send(&done, len)?;
```

The complete lower-level example is in
`tutorial/examples/09_channels_async_io.ciel`.

## Why The Mailbox Stores `meta::Repr<ReaderMsg>`

Actor messages must satisfy `Message`. Chapter 8 showed the recommended path
for ordinary user-defined structs and enums:

```ciel
meta::Repr<T>
```

Use `meta::into_repr(&value)` before sending. Use `meta::from_repr<T>(envelope)`
after receiving.

That is why the lower-level actor type is:

```ciel
Actor<meta::Repr<ReaderMsg>>
```

The mailbox stores the safe envelope, not a borrowed local `ReaderMsg`. Async
completion is delivered as a normal actor message, so it follows the same rule
as any other actor send.

## Safety Rule Shared By Both APIs

The flow API hides the completion-message enum, but it does not remove message
safety. Values captured by task closures still cross a concurrency boundary and
must be messageable.

These captures work in the example:

- `input` and `output`, because async file handles have explicit message policy
- `bytes`, because `aio::Bytes` is an owned runtime-backed byte buffer
- `done`, because `Channel<T>` is a messageable handle when `T: Message`

These captures would be rejected:

- a raw pointer into actor-local memory
- a borrowed slice of a local buffer
- an erased closure value without a retained `Message` witness

## What To Remember

Use `AsyncRunner<S>` to run a flow that carries a value of type `S`.

Use `AsyncTask<S, Out>` for a one-shot async flow that produces `Out`.

Use `flow::then` when the next task depends on the previous task's output.

Use `flow::start` to attach a task chain to a runner and provide the final
callback that returns the carried value.

Use a manual actor state machine when the workflow is not a linear chain.
