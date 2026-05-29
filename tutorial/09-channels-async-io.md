# 9. Channels and Async I/O

This chapter builds one workflow:

1. `main` asks a reader actor to read a file.
2. The reader actor starts the I/O operation and stops running.
3. The runtime sends the reader actor a completion message later.
4. The reader actor sends the final length back to `main`.

There are three moving parts:

- the actor mailbox receives commands and completion messages
- a channel carries the final answer back to the caller
- `meta::Repr<ReaderMsg>` is the safe envelope used at the actor boundary

The example reads a small file so the output is predictable, but the structure is
the same for a slow descriptor. The actor does not sit on a CPU while the read is
in flight.

## The Two Queues

An actor mailbox and a channel are both queues, but they are used for different
jobs.

The actor mailbox is for work that should be handled by that actor. In this
chapter the reader actor receives two messages:

```ciel
enum ReaderMsg {
    Start(aio::AsyncFd, Actor<meta::Repr<ReaderMsg>>),
    ReadDone(aio::AsyncRead),
}
```

`Start` means "begin the read". `ReadDone` means "the runtime observed that the
read finished; consume the result now".

A channel is a typed reply path. It is useful when some code needs one value
back from an actor:

```ciel
Channel<usize> done = must(make_channel<usize>());
```

The reader actor will eventually call:

```ciel
channel_send(&done, len)?;
```

`main` waits with:

```ciel
usize len = must(channel_recv(&done));
```

That call blocks `main` until a value arrives. It does not mean the reader actor
is blocked on the file read. `main` is simply waiting for the final reply.

## Close Handles With `defer`

The async file handle is a runtime resource. Open it, then register the close
next to the open:

```ciel
aio::AsyncFd fd = must(aio::open_async_read("/tmp/ciel_tutorial_async_read.txt"));
defer aio::close_async(fd);
```

`defer` runs the direct function call when the current block exits through
normal control flow. Its return value is ignored, so it is a cleanup tool, not
an error-handling tool.

Putting `defer` next to the open keeps the lifetime visible. The rest of `main`
can focus on the actor workflow instead of remembering to close the handle at
the bottom of the function.

## Why The Actor Starts I/O

The example lets `main` create the input file and open the async file handle so
the setup stays boring. The actual async read starts inside the actor handler:

```ciel
aio::AsyncRead op = aio::read_bytes(fd, 16)?;
```

`read_bytes` does not return the bytes. It returns an `AsyncRead` operation
token. The token says: "this read has started; use this handle when the
completion arrives."

The actor then registers the completion message:

```ciel
ReaderMsg completed = ReadDone(op);
meta::Repr<ReaderMsg> completed_envelope = meta::into_repr(&completed);
aio::notify_read_done(&op, &self, completed_envelope)?;
```

After `notify_read_done`, the handler returns:

```ciel
return Ok(done);
```

At that point the actor job is over. There is no hidden suspended stack frame.
There is no `await` or `yield` keyword. The continuation is the next mailbox
message: `ReadDone(op)`.

When the OS reports that the read finished, the runtime sends the registered
message to the registered actor. The actor runs again only when it handles that
normal mailbox message.

## Why The Message Contains `self`

`notify_read_done` needs to know which actor should receive the completion. The
runtime cannot guess that target from the file descriptor.

That is why the command message is:

```ciel
Start(aio::AsyncFd, Actor<meta::Repr<ReaderMsg>>)
```

The second field is the actor handle. In the example, `main` sends the reader
actor its own handle:

```ciel
ReaderMsg start = Start(fd, reader);
must(send(&reader, meta::into_repr(&start)));
```

Inside the handler, that handle is named `self`:

```ciel
case Start(fd, self):
```

The actor uses it to say: "when this read finishes, send `ReadDone(op)` back to
this actor."

## Why The Mailbox Stores `meta::Repr<ReaderMsg>`

Actor messages must satisfy `Message`. Chapter 8 showed the recommended path for
ordinary user-defined structs and enums:

```ciel
meta::Repr<T>
```

Use `meta::into_repr(&value)` before sending. Use `meta::from_repr<T>(envelope)`
after receiving.

That is why the actor type is:

```ciel
Actor<meta::Repr<ReaderMsg>>
```

The mailbox stores the safe envelope, not a borrowed local `ReaderMsg`. This is
not an async-I/O special case. Async completion is delivered as a normal actor
message, so it follows the same message rule as any other actor send.

The envelope is still checked by the compiler. SOP does not make every type a
valid message. `meta::Repr<ReaderMsg>` implements `Message` only when the
compiler can prove that the owned structural representation is safe to move
across the actor boundary. The async handle types used here are standard-library
handle values with explicit message policy.

## Chaining Operations With `then`

The explicit message-enum style above is the most general actor shape. For a
linear workflow, `/std/async` can keep the same runtime model while hiding the
completion-message enum.

`flow::Step<S>` is a messageable closure that takes the actor state and returns
the next state:

```ciel
import /std/async as flow;

flow::Step<Channel<usize>> step = |Channel<usize> done| {
    return Ok(done);
};
```

`flow::spawn_step_actor<S>(state)` starts an actor whose mailbox receives those
steps. Its handler is only:

```ciel
return step(state);
```

`flow::then` connects one completion to the next step. Its second argument is a
`flow::Completion<S, Out>`:

```ciel
flow::Completion<Channel<usize>, aio::Bytes> read =
    aio::read_bytes_completion<Channel<usize>>(input, 64)?;
```

`Completion<S, Out>` hides the operation token from application code. It says:
"this operation can notify a `Step<S>` actor when ready, and later produce an
`Out` value." `then` is the reusable glue that builds the completion `Step<S>`
and calls the completion in the right phases.

The raw operation layer still exists for libraries. A library that introduces a
new async operation type writes an adapter once. For example, `/std/async_io`
connects `AsyncRead` to the generic adapter like this:

```ciel
impl<M: Message> notify_done(
    AsyncRead op,
    actor::Actor<M> actor_handle,
    M message
) {
    return notify_read_done(&op, &actor_handle, message);
}

impl finish<Bytes>(AsyncRead op) {
    return finish_read(op);
}
```

The first impl says how to register a typed completion message with the runtime.
The second impl says how to consume the completed operation token and get the
result value. The output type is part of the interface instance:
`AsyncRead` finishes as `Bytes`, while `AsyncWrite` finishes as `usize`.

The public constructor wraps that raw `AsyncRead` token into a `Completion`:

```ciel
export Result<flow::Completion<S, Bytes>, Error> read_bytes_completion<S: Message>(
    AsyncFd fd,
    usize max_len
) {
    AsyncRead op = read_bytes(fd, max_len)?;
    return Ok(flow::completion_from_op<S, Bytes, AsyncRead>(op));
}
```

That split keeps application code on one high-level path. Use
`read_bytes_completion` and `write_bytes_completion` with `then` for linear
flows. Drop to `read_bytes`, `notify_read_done`, and `finish_read` only when you
need a custom actor protocol, cancellation branch, or a message shape that
`then` should not hide.

With completions, a file-copy pipeline can say "after the read finishes, write
the bytes; after the write finishes, report the byte count" without exposing a
state-machine enum or raw operation token:

```ciel
flow::Step<Channel<usize>> start = |Channel<usize> done| {
    return flow::then(
        done,
        aio::read_bytes_completion<Channel<usize>>(input, 64)?,
        worker,
        |Channel<usize> done, aio::Bytes bytes| {
            return flow::then(
                done,
                aio::write_bytes_completion<Channel<usize>>(output, bytes)?,
                worker,
                |Channel<usize> done, usize written| {
                    channel_send(&done, written)?;
                    return Ok(done);
                }
            );
        }
    );
};
```

The call to `then` returns immediately with the unchanged actor state. Later,
the runtime sends a generated `Step<S>` message to the same actor. That step
finishes the completion and then runs the continuation.

The continuation is still an actor message. Captured values must therefore be
messageable. Capturing actor handles, channels, async file handles, byte buffers,
and completion internals works because those handle types have explicit
standard library message policy. Capturing a raw pointer or actor-local slice is
rejected for the same reason it is rejected in a manually written actor message.

The complete `then` example is in
`tutorial/examples/09_async_then_file_copy.ciel`. It copies a small file through
async read and async write, then reports the number of bytes written.

## Full Program

The complete example is also in `tutorial/examples/09_channels_async_io.ciel`.

```ciel
import /std/lib;
import /std/async_io as aio;
import /std/meta as meta;

enum ReaderMsg {
    // Start a read on this async file handle. The actor handle tells the runtime
    // where the completion message should be sent later.
    Start(aio::AsyncFd, Actor<meta::Repr<ReaderMsg>>),

    // The runtime sends this after it observes that the read operation finished.
    ReadDone(aio::AsyncRead),
}

Result<Channel<usize>, Error> handle(Channel<usize> done, meta::Repr<ReaderMsg> envelope) {
    // The mailbox stores the safe envelope. Open it before normal pattern matching.
    ReaderMsg msg = meta::from_repr<ReaderMsg>(envelope);

    switch (msg) {
        case Start(fd, self):
            // Start one async read. This returns immediately with an operation token.
            aio::AsyncRead op = aio::read_bytes(fd, 16)?;

            // Build the completion message, then seal it into the same safe envelope type.
            ReaderMsg completed = ReadDone(op);
            meta::Repr<ReaderMsg> completed_envelope = meta::into_repr(&completed);

            // Ask the runtime to send ReadDone(op) back to this actor later.
            aio::notify_read_done(&op, &self, completed_envelope)?;

            // Return now. The actor is not blocked while the OS is reading.
            return Ok(done);

        case ReadDone(op):
            // The OS has finished. Now consume the completed operation token.
            aio::Bytes bytes = aio::finish_read(op)?;
            usize len = aio::bytes_len(bytes);

            // Send the final answer back to whoever is waiting on the channel.
            channel_send(&done, len)?;
            return Ok(done);
    }
}

i32 main() {
    // Prepare stable input for the example.
    must(with_create<void>("/tmp/ciel_tutorial_async_read.txt", |file| {
        write_text(file, "hello")?;
        return Ok;
    }));

    // The channel is only the reply path back to main.
    Channel<usize> done = must(make_channel<usize>());

    // The actor receives envelopes around the real application message.
    Actor<meta::Repr<ReaderMsg>> reader = must(spawn_actor<Channel<usize>, meta::Repr<ReaderMsg>>(
        done,
        handle
    ));

    // Open an async file handle. The read itself starts inside the actor handler.
    aio::AsyncFd fd = must(aio::open_async_read("/tmp/ciel_tutorial_async_read.txt"));
    defer aio::close_async(fd);

    // Tell the actor to begin the read. The actor handle is part of the command so
    // the actor can ask the runtime to send the completion message back to itself.
    ReaderMsg start = Start(fd, reader);
    must(send(&reader, meta::into_repr(&start)));

    // Main blocks here waiting for the final answer.
    // The reader actor itself is not blocked while the read is pending.
    usize len = must(channel_recv(&done));
    must(print_value(len));

    must(join(&reader));
    must(channel_close(&done));
    return 0;
}
```

## Trace The Run

Read the program in this order:

1. `main` creates `done`, a `Channel<usize>` for the final answer.
2. `main` spawns `reader`, whose mailbox type is `meta::Repr<ReaderMsg>`.
3. `main` opens the async handle and registers `defer aio::close_async(fd)`.
4. `main` sends `Start(fd, reader)` after sealing it with `meta::into_repr`.
5. `reader` opens the envelope with `meta::from_repr<ReaderMsg>`.
6. `reader` starts the async read and gets an `AsyncRead` token.
7. `reader` registers `ReadDone(op)` as the completion message.
8. `reader` returns to the runtime; no user code is running for that read.
9. `main` waits on `channel_recv(&done)`.
10. The runtime later sends `ReadDone(op)` to `reader`.
11. `reader` calls `finish_read(op)`, measures the byte length, and sends it
    through `done`.
12. `main` receives the length and prints it.
13. `main` returns, then the deferred async close runs.

The intentional blocking point in this program is `channel_recv(&done)` in
`main`. The actor is free between the `Start` handler returning and the later
`ReadDone` handler being scheduled.

## What To Remember

Use a channel when a caller needs a reply value.

Use an actor message when a worker should continue a workflow later.

Use `meta::Repr<T>` at the actor boundary for ordinary user-defined message
types. The async I/O APIs use the same actor-message rule; they do not add a
separate coroutine model.
