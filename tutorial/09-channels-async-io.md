# 9. Channels and Async Flows

This chapter uses one route-and-send workflow to teach async flow composition:

1. Start a loopback TCP listener.
2. Write the listener address to a small target file.
3. Read a payload file with `/std/async_io`.
4. Read the target file, parse the address, and connect with `/std/async_net`.
5. Wait briefly with `/std/async_time`.
6. Send the payload to the connected stream.
7. Read the payload on the listener side and report the byte count through a
   channel.

The complete program is in `tutorial/examples/09_async_file_delay_tcp.ciel`.
It uses three operation libraries through one flow API:

- `/std/async_io` for file reads
- `/std/async_net` for TCP accept, connect, read, and write
- `/std/async_time` for a non-blocking delay

The high-level composition API lives in `/std/async`:

- `flow::AsyncRunner<S>` runs a flow that carries a value of type `S`.
- `flow::AsyncTask<S, Out>` is a one-shot async flow that eventually produces
  `Out` and runs on a runner carrying `S`.
- `flow::then` chains a task to the next task.
- `flow::start` runs a task chain on the runner.

Each operation-specific module returns task constructors that plug into this
same shape. Application code does not need a custom completion-message enum for
a linear workflow.

## The Data Flow

The example has two files:

```ciel
must(with_create<void>("/tmp/ciel_tutorial_payload.txt", |file| {
    write_text(file, "ping")?;
    return Ok;
}));
```

The target file is written after the listener chooses an available loopback
port:

```ciel
anet::AsyncTcpListener listener =
    must(anet::listen_async(must(net::parse_addr("127.0.0.1:0"))));
net::SocketAddr bound = must(anet::listener_addr(listener));

[80]char @target_text = ['\0';];
usize target_len = must(net::addr_write(bound, target_text[..]));
must(with_create<void>("/tmp/ciel_tutorial_target.txt", |file| {
    write_text(file, target_text[..target_len])?;
    return Ok;
}));
```

The payload file is the data to send. The target file is routing data. Reading
both asynchronously makes the flow more realistic than a single file copy: one
async result becomes the bytes to write, and another async result decides where
to connect.

## Channels

`Channel<T>` is a typed queue for sending values between pieces of concurrent
code.

The sender flow cannot return the byte count to the original call stack because
the original stack has already moved on. Instead, the final callback sends the
answer into a channel:

```ciel
channel_send(&ch, written)?;
```

`main` waits for that value:

```ciel
usize written = must(channel_recv(&sent));
```

The server side uses another channel to report how many bytes arrived.

## The Main Chain

Open the files near their cleanup:

```ciel
aio::AsyncFd payload_file =
    must(aio::open_async_read("/tmp/ciel_tutorial_payload.txt"));
defer aio::close_async(payload_file);

aio::AsyncFd target_file =
    must(aio::open_async_read("/tmp/ciel_tutorial_target.txt"));
defer aio::close_async(target_file);
```

Then build the sender flow:

```ciel
flow::AsyncTask<Channel<usize>, aio::Bytes> read_payload =
    must(aio::read_bytes_task<Channel<usize>>(payload_file, 64));
flow::AsyncTask<Channel<usize>, usize> send_payload =
    flow::then(read_payload, |aio::Bytes payload| {
        return route_and_send_task<Channel<usize>>(target_file, payload);
    });
```

Read the types from left to right:

```text
read_payload: AsyncTask<Channel<usize>, aio::Bytes>
send_payload: AsyncTask<Channel<usize>, usize>
```

The runner carries `Channel<usize>` the whole time. The task output changes as
the workflow learns more: payload bytes first, then a final write count.

## Routing From A File

`route_and_send_task` reads the target file and uses the result to create the
network operation:

```ciel
Result<flow::AsyncTask<S, usize>, Error> route_and_send_task<S: Message>(
    aio::AsyncFd target_file,
    aio::Bytes payload
) {
    flow::AsyncTask<S, aio::Bytes> read_target =
        aio::read_bytes_task<S>(target_file, 80)?;
    return Ok(flow::then(read_target, |aio::Bytes target_bytes| {
        net::SocketAddr target = address_from_bytes(target_bytes)?;
        flow::AsyncTask<S, anet::AsyncTcpStream> connect =
            anet::connect_task<S>(target)?;
        return Ok(flow::then(connect, |anet::AsyncTcpStream stream| {
            return delayed_write_task<S>(stream, payload);
        }));
    }));
}
```

The continuation does synchronous parsing inside the actor runner, then returns
the next async task. It still does not block on the socket connection. The
connect operation completes later through the same flow machinery.

## Delay Then Send

`delayed_write_task` waits without blocking the runner thread, then writes the
payload to the connected stream:

```ciel
Result<flow::AsyncTask<S, usize>, Error> delayed_write_task<S: Message>(
    anet::AsyncTcpStream stream,
    aio::Bytes payload
) {
    flow::AsyncTask<S, void> wait = atime::sleep_ms_task<S>(1)?;
    return Ok(flow::then(wait, |void marker| {
        marker;
        return anet::write_bytes_task<S>(stream, payload);
    }));
}
```

The `marker` binding is the `void` result from the timer. Evaluating it marks
that result as intentionally consumed before the continuation starts the write.

## Start The Flow

`flow::start` attaches the chain to the runner:

```ciel
must(flow::start(sender, send_payload, |Channel<usize> ch, usize written| {
    channel_send(&ch, written)?;
    return Ok(ch);
}));
```

The final callback receives:

- the runner's carried value, `ch`
- the output of the whole task chain, `written`

It sends the answer through the channel and returns the next carried value.

## The Server Side

The example also starts an async accept before the sender connects:

```ciel
flow::AsyncTask<Channel<anet::AsyncTcpStream>, anet::AsyncTcpStream> accept =
    must(anet::accept_task<Channel<anet::AsyncTcpStream>>(listener));
```

After `main` receives the accepted stream, it starts one async read:

```ciel
flow::AsyncTask<Channel<usize>, anet::Bytes> read_socket =
    must(anet::read_bytes_task<Channel<usize>>(server, 64));
```

The server flow reports the length of the bytes it received. The program prints
that count, so the tutorial output is observable:

```text
4
```

## Execution Timeline

Read the program in this order:

1. `main` writes the payload file.
2. `main` starts a listener and writes its chosen address to the target file.
3. `main` starts an accept task on one runner.
4. `main` opens the payload and target files as async file handles.
5. `main` starts the sender flow: read payload, read target, connect, wait,
   write.
6. The accept task completes and gives `main` the server stream.
7. `main` starts a server read task.
8. The sender flow finishes the payload read.
9. The sender flow reads the target file and parses the address.
10. The sender flow connects to the parsed address.
11. The sender flow waits through `/std/async_time`.
12. The sender flow writes the payload through `/std/async_net`.
13. The server read completes and reports the received byte count.

There is no suspended Ciel stack between these steps. Every async operation
resumes the runner by sending an actor message.

## The Lower-Level State Machine

The task API is built on ordinary actor messages. Use the lower-level shape
when the protocol matters: custom routing, cancellation, several completion
targets, or a workflow that is not a linear chain.

At that level, the mailbox receives both commands and completion messages:

```ciel
enum ReaderMsg {
    Start(aio::AsyncFd),
    ReadDone(aio::AsyncRead),
}
```

`Start` begins the read. `ReadDone` consumes the completed operation.
The actor is spawned with `spawn_actor_state`, so the handler receives the
actor's own handle directly:

```ciel
Result<void, Error> handle(
    *Channel<usize> done,
    Actor<meta::Repr<ReaderMsg>> self,
    meta::Repr<ReaderMsg> envelope
)
```

The actor starts the operation:

```ciel
aio::AsyncRead op = aio::read_bytes_async(fd, 16)?;
```

That returns an operation token, not the bytes. The actor then registers a
message for the runtime to send back later:

```ciel
ReaderMsg completed = ReadDone(op);
meta::Repr<ReaderMsg> completed_envelope = meta::into_repr(&completed);
aio::notify_read_done(&op, &self, completed_envelope)?;
```

When the OS reports that the read finished, the runtime sends `ReadDone(op)` to
the actor:

```ciel
aio::Bytes bytes = aio::finish_read(op)?;
usize len = aio::bytes_len(bytes);
channel_send(done, len)?;
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

Manual actors have the same rule for their initializer and handler values.
`spawn_actor_state` lets the private state `S` be non-messageable, but the
initializer closure and handler value still need retained `Message` capability.

These captures work in the example:

- `payload_file` and `target_file`, because async file handles have explicit
  message policy
- `payload`, because `Bytes` is an owned runtime-backed byte buffer
- `stream`, because async TCP streams have explicit message policy
- `sent` and `received`, because `Channel<T>` is a messageable handle when
  `T: Message`

These captures would be rejected:

- a raw pointer into actor-local memory
- a borrowed slice of a local buffer
- an erased closure value without a retained `Message` witness

## What To Remember

Use `AsyncRunner<S>` to run a flow that carries a value of type `S`.

Use `AsyncTask<S, Out>` for a one-shot async flow that produces `Out`.

Use `flow::then` when the next operation depends on the previous task's output.

Operation-specific tasks from `/std/async_io`, `/std/async_time`, and
`/std/async_net` compose through the same `flow::then` API.

Use a manual actor state machine when the workflow is not a linear chain.

Use `spawn_actor_state` for a manual actor that owns mutable resources in place
or needs its own `Actor<M>` handle while handling a message.
