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
    must(aio::close_async(fd));
    return 0;
}
```

## Trace The Run

Read the program in this order:

1. `main` creates `done`, a `Channel<usize>` for the final answer.
2. `main` spawns `reader`, whose mailbox type is `meta::Repr<ReaderMsg>`.
3. `main` sends `Start(fd, reader)` after sealing it with `meta::into_repr`.
4. `reader` opens the envelope with `meta::from_repr<ReaderMsg>`.
5. `reader` starts the async read and gets an `AsyncRead` token.
6. `reader` registers `ReadDone(op)` as the completion message.
7. `reader` returns to the runtime; no user code is running for that read.
8. `main` waits on `channel_recv(&done)`.
9. The runtime later sends `ReadDone(op)` to `reader`.
10. `reader` calls `finish_read(op)`, measures the byte length, and sends it
    through `done`.
11. `main` receives the length and prints it.

The intentional blocking point in this program is `channel_recv(&done)` in
`main`. The actor is free between the `Start` handler returning and the later
`ReadDone` handler being scheduled.

## What To Remember

Use a channel when a caller needs a reply value.

Use an actor message when a worker should continue a workflow later.

Use `meta::Repr<T>` at the actor boundary for ordinary user-defined message
types. The async I/O APIs use the same actor-message rule; they do not add a
separate coroutine model.
