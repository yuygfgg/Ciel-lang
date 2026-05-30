# Intranet Tunnel Demo PRD

## 1. Purpose

This document defines a small but realistic intranet tunneling demo for Ciel and
the standard-library work required to support it. The demo is a product-shaped
program. Its main goal is to drive missing compiler, runtime, and
standard-library work through one coherent application.

The demo exposes a TCP service running inside a private network through a public
relay server. A local agent connects outward to the relay server, authenticates,
and forwards traffic between public clients and the private target service.

The implementation must stay small enough to audit. The target application code
budget is roughly 3000 lines of Ciel, excluding standard-library additions and
C shim code.

## 2. Goals

- Build a working TCP-only intranet tunnel with a public server and a private
  agent.
- Exercise Ciel modules, errors, enums, pattern matching, slices, buffers,
  resource cleanup, actors or channels, unsafe FFI, and C-backed runtime hooks.
- Establish reusable standard-library APIs for sockets, monotonic time, async
  timers, byte buffers, keyed maps, binary codecs, command-line arguments, and
  cryptography.
- Use Botan as the first `/std/crypto` backend through its C FFI, while keeping
  the public Ciel API backend-neutral.
- Sequence the work standard-library first and application second, so the demo
  validates stable library surfaces instead of inventing them in place.
- Keep the protocol small enough that compiler and library defects remain
  visible.
- Produce deterministic integration tests that run on local loopback services.

## 3. Scope Boundaries

- TCP forwarding is the initial transport.
- The control protocol is Ciel-specific binary framing.
- The first tunnel topology is one public server, one private agent, and one
  configured route.
- `/std/crypto` uses Botan through its C FFI as the first backend.
- The first Botan-backed crypto surface covers application-grade RNG, hash, MAC,
  constant-time comparison, and later AEAD.
- Async work focuses on the TCP and timer operations needed by the tunnel and
  actor integration.
- CLI polish is limited to the options required to run local tests and manual
  demos.

## 4. Product Shape

The demo contains two executables:

- `tunnel-server`: runs on a publicly reachable host.
- `tunnel-agent`: runs inside the private network.

The server listens on two TCP ports:

- control port: accepts one or more authenticated agents.
- public port: accepts external client connections.

The agent connects to the server control port and also connects to one private
target address, for example `127.0.0.1:8080`.

When an external client connects to the server public port, the server opens a
logical stream over the agent control connection. The agent then dials the local
target and relays bytes in both directions until either side closes.

## 5. User Stories

### 5.1 Expose A Local Echo Service

As a developer, I can run a local TCP echo service, start `tunnel-agent`, start
`tunnel-server`, connect to the server public port, and observe bytes being
echoed from the private service.

Acceptance criteria:

- A client connecting to the server public port can send bytes and receive the
  same bytes back through the tunnel.
- Multiple sequential client connections work without restarting either
  process.
- At least two concurrent client connections can transfer data independently.

### 5.2 Reject Wrong Authentication

As an operator, I can configure a pre-shared key. Agents with the wrong key are
rejected before any public client traffic is forwarded.

Acceptance criteria:

- The server closes the control connection when authentication fails.
- No public stream is assigned to an unauthenticated agent.
- The server reports a structured authentication error.

### 5.3 Handle Target Failure

As a developer, I can start the agent while the private target is unavailable
and still receive a clear error path instead of a hang or crash.

Acceptance criteria:

- A failed target dial produces a stream close frame with an error code.
- The public client connection is closed promptly.
- The control connection remains usable for later streams.

### 5.4 Handle Public Client Disconnect

As a server, I can detect when the public client closes early and notify the
agent so that the corresponding private target connection is closed.

Acceptance criteria:

- Client EOF maps to a stream close frame.
- The agent closes the private socket for that stream.
- Other streams on the same control connection continue running.

## 6. MVP Scope

The MVP implements:

- TCP-only forwarding.
- One configured tunnel route per agent.
- One server process and one agent process.
- Binary length-prefixed frames over a single control TCP connection.
- Multiplexed logical streams identified by `u32 stream_id`.
- Reusable stream tables keyed by `u32 stream_id`, built on `/std/map` instead
  of application-local ad hoc arrays.
- Pre-shared-key authentication.
- Actor-friendly TCP operations that compose with `/std/async`.
- Actor-friendly timer operations that compose with `/std/async`.
- Clean shutdown of sockets and stream state.
- Local integration tests using loopback addresses.

Deferred product work:

- Persistent config files.
- Multiple routes per agent.
- Agent pools with load balancing.
- Stream-level flow control beyond bounded read chunks.
- Payload encryption before the Botan-backed authentication API is stable.

## 7. Protocol Overview

All control traffic uses binary frames. The first implementation should prefer a
fixed header with explicit big-endian integer encoding.

Frame header:

```text
magic      u32   constant, protects against accidental plaintext mismatch
version    u16   protocol version, initially 1
kind       u16   frame kind
stream_id  u32   logical stream id, 0 for control-only frames
length     u32   payload byte count
```

The maximum payload length is `65536` bytes in the MVP. Receivers must reject
larger frames before allocating payload storage.

Frame kinds:

```text
1  Hello
2  HelloOk
3  OpenStream
4  OpenResult
5  Data
6  CloseWrite
7  CloseStream
8  Ping
9  Pong
10 Error
```

Rules:

- `Hello`, `HelloOk`, `Ping`, `Pong`, and authentication errors use
  `stream_id = 0`.
- `OpenStream`, `OpenResult`, `Data`, `CloseWrite`, and `CloseStream` use a
  nonzero stream id.
- The server allocates stream ids for public client connections.
- A receiver must reject a frame kind that is invalid for the current stream
  state.
- A malformed frame closes the control connection.

## 8. Authentication

The MVP uses pre-shared-key authentication through `/std/crypto`. Botan is the
chosen first backend. The tunnel application imports the standard-library
crypto API.

Handshake:

1. The agent sends `Hello` with:
   - agent protocol version
   - agent nonce
   - route name
   - authentication tag
2. The server validates the route name and authentication tag.
3. The server replies with `HelloOk` containing:
   - server nonce
   - selected protocol version
4. Both sides mark the control connection authenticated.

The authentication tag is computed through the Botan-backed standard library.
`/std/crypto` selects the primitive policy for application code. The expected
first primitive is HMAC with a SHA-2 hash, exposed through a general MAC API.

Required high-level Ciel-facing crypto functions:

```text
random_bytes(out: []u8) -> Result<void, Error>
mac_once(algorithm: []const char, key: []const u8, data: []const u8, out: []u8)
    -> Result<usize, Error>
constant_time_eq(left: []const u8, right: []const u8) -> bool
```

The tunnel authentication code should call `mac_once("HMAC(SHA-256)", ...)`.
The same API is intended for other application protocols.

The MVP may leave stream payloads unencrypted after authentication if the demo
goal is compiler and standard-library pressure. A later phase may add
authenticated encryption around `Data` payloads.

## 9. Stream State Machine

Each public client connection maps to one logical stream.

Server-side stream states:

```text
PendingAgentOpen
Open
ClientWriteClosed
AgentWriteClosed
Closed
```

Agent-side stream states:

```text
DialingTarget
Open
ServerWriteClosed
TargetWriteClosed
Closed
```

Transitions:

- Public client accepted -> server creates `PendingAgentOpen`.
- Server sends `OpenStream`.
- Agent target dial succeeds -> agent sends `OpenResult(success)`.
- Agent target dial fails -> agent sends `OpenResult(error)` and closes stream.
- Either side reads bytes -> send `Data`.
- Either side reads EOF -> send `CloseWrite`.
- Either side sees fatal socket error -> send `CloseStream`.
- Both write directions closed -> close local socket and remove stream state.

Invalid transitions are protocol errors for the stream. They should close only
that stream when possible. Header parse failures, oversized frames, and
authentication failures close the control connection.

## 10. Standard Library Product Requirements

The standard-library additions are part of this PRD. Each new module must be
documented, covered by focused tests, and usable by other Ciel programs.

Required modules:

- `/std/net`: core TCP handles, addresses, shutdown, and optional blocking
  wrappers.
- `/std/async_net`: actor-friendly TCP operations aligned with the existing
  `/std/async_io` design.
- `/std/async_time`: actor-friendly timers and deadline building blocks aligned
  with `/std/async`.
- `/std/buf`: reusable byte buffers.
- `/std/map`: reusable keyed tables for stream state and other actor-local
  resource registries.
- `/std/codec`: endian-aware binary encoding helpers.
- `/std/time`: monotonic clock and sleep.
- `/std/env`: command-line argument access for the final product path.
- `/std/crypto`: Botan-backed cryptographic primitives behind a Ciel facade.

The initial implementation may stage these modules over several PRs. The
accepted demo requires the relevant public APIs to live in `std/`.

### 10.1 `/std/net`

`/std/net` is the core networking layer. It owns socket addresses, safe handle
wrappers, lifecycle operations, and optional blocking convenience APIs.
The standard-library surface is intentionally broader than the tunnel MVP:
numeric IPv4, numeric bracketed IPv6, and explicit DNS resolution are supported.

Minimum public API:

```text
enum AddressFamily
struct TcpListener
struct TcpStream
struct SocketAddr

parse_addr(text: []const char) -> Result<SocketAddr, Error>
resolve_tcp(host: []const char, port: u16) -> Result<SocketAddr, Error>
addr_family(addr: SocketAddr) -> Result<AddressFamily, Error>
addr_port(addr: SocketAddr) -> Result<u16, Error>
addr_to_string(addr: SocketAddr) -> Result<[]const char, Error>
tcp_listen(addr: SocketAddr) -> Result<TcpListener, Error>
tcp_accept(listener: TcpListener) -> Result<TcpStream, Error>
tcp_connect(addr: SocketAddr) -> Result<TcpStream, Error>
tcp_connect_host(host: []const char, port: u16) -> Result<TcpStream, Error>
tcp_read(stream: TcpStream, out: []u8) -> Result<usize, Error>
tcp_write(stream: TcpStream, data: []const u8) -> Result<usize, Error>
tcp_write_all(stream: TcpStream, data: []const u8) -> Result<void, Error>
tcp_shutdown_read(stream: TcpStream) -> Result<void, Error>
tcp_shutdown_write(stream: TcpStream) -> Result<void, Error>
tcp_shutdown(stream: TcpStream) -> Result<void, Error>
tcp_close(stream: TcpStream) -> Result<void, Error>
listener_close(listener: TcpListener) -> Result<void, Error>
listener_addr(listener: TcpListener) -> Result<SocketAddr, Error>
```

`parse_addr` is deterministic and does not perform DNS. Domain-name lookup is
explicit through `resolve_tcp` and `tcp_connect_host`.

These blocking calls are acceptable for:

- bootstrap code,
- direct tests,
- simple utilities,
- compatibility wrappers around the async layer.

Actor-driven tunnel code should prefer `/std/async_net`.

The API may use scoped helpers if the existing file-handle pattern is a better
fit:

```text
with_tcp_connect<R: Message>(addr: SocketAddr, Result<R, Error> |(TcpStream)| body)
with_tcp_connect_host<R: Message>(host: []const char, port: u16, Result<R, Error> |(TcpStream)| body)
with_tcp_listen<R: Message>(addr: SocketAddr, Result<R, Error> |(TcpListener)| body)
```

### 10.2 `/std/async_net`

`/std/async_net` should follow the same conceptual pattern as the existing
`/std/async_io`:

- a long-lived handle type,
- explicit async operation tokens,
- `notify_done` and `finish` integration through `/std/async/adapter`,
- `Completion<S, Out>` helpers for actor-driven continuations.

The tunnel demo should be written against this shape wherever a socket
operation might block.

Minimum public API:

```text
struct AsyncTcpListener
struct AsyncTcpStream
struct AsyncAccept
struct AsyncConnect
struct AsyncTcpRead
struct AsyncTcpWrite

listen_async(addr: net::SocketAddr) -> Result<AsyncTcpListener, Error>
listener_addr(listener: AsyncTcpListener) -> Result<net::SocketAddr, Error>
accept_async(listener: AsyncTcpListener) -> Result<AsyncAccept, Error>
connect_async(addr: net::SocketAddr) -> Result<AsyncConnect, Error>
close_listener(listener: AsyncTcpListener) -> Result<void, Error>
close_stream(stream: AsyncTcpStream) -> Result<void, Error>
shutdown_read(stream: AsyncTcpStream) -> Result<void, Error>
shutdown_write(stream: AsyncTcpStream) -> Result<void, Error>
stream_local_addr(stream: AsyncTcpStream) -> Result<net::SocketAddr, Error>
stream_peer_addr(stream: AsyncTcpStream) -> Result<net::SocketAddr, Error>

read_bytes(stream: AsyncTcpStream, max_len: usize) -> Result<AsyncTcpRead, Error>
write_bytes(stream: AsyncTcpStream, data: Bytes) -> Result<AsyncTcpWrite, Error>
```

Required `/std/async` integration:

```text
accept_completion<S: Message>(
    listener: AsyncTcpListener
) -> Result<flow::Completion<S, AsyncTcpStream>, Error>

connect_completion<S: Message>(
    addr: net::SocketAddr
) -> Result<flow::Completion<S, AsyncTcpStream>, Error>

read_bytes_completion<S: Message>(
    stream: AsyncTcpStream,
    max_len: usize
) -> Result<flow::Completion<S, Bytes>, Error>

write_bytes_completion<S: Message>(
    stream: AsyncTcpStream,
    data: Bytes
) -> Result<flow::Completion<S, usize>, Error>
```

Async networking reuses the shared async `Bytes` ownership model from
`/std/async/bytes`, re-exported by both async I/O and async networking. This
keeps async file reads and async TCP reads compatible with the same
`flow::Completion<S, Bytes>` shape.

### 10.3 `/std/buf`

Minimum public API:

```text
struct ByteBuf

byte_buf_new(capacity: usize) -> Result<ByteBuf, Error>
byte_buf_len(*const ByteBuf) -> usize
byte_buf_clear(*ByteBuf) -> void
byte_buf_slice(*const ByteBuf) -> []const u8
byte_buf_mut_slice(*ByteBuf) -> []u8
byte_buf_reserve(*ByteBuf, additional: usize) -> Result<void, Error>
byte_buf_push_slice(*ByteBuf, data: []const u8) -> Result<void, Error>
```

The first implementation may use fixed-size stack buffers inside the standard
library while the public API is stabilized. Application code should use the
intended `std/` surface.

### 10.4 `/std/map`

`/std/map` is the reusable keyed-table layer. The tunnel server and agent need
stream tables keyed by `u32 stream_id`; that requirement should be met through a
standard-library map rather than a demo-local data structure.

The first implementation should stay small and auditable, but it should be
generic rather than `u32`-specific. A key type participates through ordinary
policy interfaces. `/std/map` provides primitive key policies for the built-in
scalar types needed by the tunnel, and reusable structural policies over
`/std/meta` product/sum nodes so visible user structs and enums can opt in by
projecting to `meta::RefRepr<T>` and delegating to library code.

`HashMap<K, V>` is an actor-local mutable resource by default. It should not
implement `Message` unless a later wrapper explicitly proves that the key,
value, and storage semantics are safe to clone or transfer. Application code
should pass keys, values, snapshots, or completed messages across actor
boundaries, not live map storage that owns socket or async operation handles.

Minimum public API:

```text
struct HashMap<K, V>

interface<T> u64 hash_key(*const T value, u64 seed)
interface<T> bool key_eq(*const T left, *const T right)
interface map_key = hash_key + key_eq

enum InsertResult<V> {
    Inserted,
    Replaced(V),
}

enum RemoveResult<V> {
    Removed(V),
    Missing,
}

hash_map_new<K: map_key, V>() -> Result<HashMap<K, V>, Error>
hash_map_len<K: map_key, V>(*const HashMap<K, V>) -> usize
hash_map_clear<K: map_key, V>(*HashMap<K, V>) -> void
hash_map_contains_key<K: map_key, V>(
    *const HashMap<K, V>,
    key: K
) -> Result<bool, Error>
hash_map_insert<K: map_key, V>(
    *HashMap<K, V>,
    key: K,
    value: V
) -> Result<InsertResult<V>, Error>
hash_map_remove<K: map_key, V>(
    *HashMap<K, V>,
    key: K
) -> Result<RemoveResult<V>, Error>
hash_map_with<K: map_key, V, R: Message>(
    *HashMap<K, V>,
    key: K,
    Result<R, Error> |(*V)| body
) -> Result<R, Error>
```

Application code should usually write the key/value types at construction and
let generic inference handle later operations:

```text
_ @streams = must(hash_map_new<u32, ServerStream>())
must(hash_map_insert(&streams, stream_id, stream))
hash_map_len(&streams)
```

`hash_map_with` provides scoped mutable access to an existing entry without
returning a long-lived pointer into the table. Missing keys should report a
stable map error. Insertions may rehash and invalidate any internal storage, so
borrowed entry access must stay scoped to the callback.

The required first key-policy coverage is:

- primitive scalar keys: `bool`, `char`, signed integers, unsigned integers, and
  `usize`;
- `/std/meta` structural product/sum nodes used by `meta::RefRepr<T>`, so a
  nominal struct or enum can opt in with an explicit `hash_key` and `key_eq`
  wrapper that projects through `meta::as_ref_repr`;
- byte-slice or string-like keys only if an owned key type with stable storage
  is available; borrowed slices must not be stored as map keys without an
  explicit lifetime-owning wrapper policy.

### 10.5 `/std/codec`

Minimum public API:

```text
struct meta::Type<T>

encoded_len(value: T) -> usize
put_be(out: []u8, value: T) -> Result<void, Error>
put_le(out: []u8, value: T) -> Result<void, Error>
get_be(tag: meta::Type<T>, data: []const u8) -> Result<T, Error>
get_le(tag: meta::Type<T>, data: []const u8) -> Result<T, Error>
encode_be(value: T) -> Result<[]u8, Error>
encode_le(value: T) -> Result<[]u8, Error>
```

The first implementation supports unsigned integer types only: `u8`, `u16`,
`u32`, `u64`, and `usize`. The `meta::Type<T>` tag gives `get_be` and `get_le` an
input receiver so interface dispatch remains explicit while the return type is
still `Result<T, Error>`.

### 10.6 `/std/time`

Minimum public API:

```text
sleep_ms(ms: u64) -> Result<void, Error>
monotonic_ms() -> Result<u64, Error>
```

`/std/time` is the synchronous clock layer. It is useful for direct utilities
and low-level tests. Actor-driven code should use `/std/async_time` instead of
blocking a runner thread with `sleep_ms`.

### 10.7 `/std/async_time`

`/std/async_time` is the standard timer layer for actor-oriented code. It
provides generic sleep/deadline building blocks; it does not define tunnel
heartbeat frames or missed-pong policy.

Minimum public API:

```text
struct AsyncSleep

sleep_ms_async(ms: u64) -> Result<AsyncSleep, Error>
notify_sleep_done<M: Message>(
    op: *const AsyncSleep,
    actor: *const actor::Actor<M>,
    message: M
) -> Result<void, Error>
finish_sleep(op: AsyncSleep) -> Result<void, Error>
cancel_sleep(op: AsyncSleep) -> Result<void, Error>

sleep_ms_completion<S: Message>(
    ms: u64
) -> Result<flow::Completion<S, void>, Error>

sleep_ms_task<S: Message>(
    ms: u64
) -> Result<flow::AsyncTask<S, void>, Error>
```

Required behavior:

- Timers use monotonic time, not wall-clock time.
- A zero-delay sleep completes asynchronously but promptly.
- Canceling a pending timer prevents its completion message from being sent.
- Finishing a sleep consumes the operation exactly once.
- Multiple timers may be active concurrently and complete independently.

Timeout policy is layered above this primitive. A generic timeout combinator may
be added to `/std/async` or `/std/async_time` only if the losing operation can
be canceled without leaving a late completion in the actor mailbox. The tunnel
application should implement Ping/Pong and idle-deadline policy in its protocol
code using these timer primitives.

### 10.8 `/std/env`

Minimum public API:

```text
args_len() -> Result<usize, Error>
arg(index: usize) -> Result<[]const char, Error>
```

The first implementation only needs process argument access. Environment
variables, current directory, process spawning, and path search are future
`/std/env` extensions.

### 10.9 `/std/crypto`

`/std/crypto` is a general standard-library module. Botan is the selected first
backend because it provides a stable C FFI, opaque handles, broad algorithm
coverage, and algorithm-name based construction.

The public Ciel API is backend-neutral. Application code imports portable names
such as `Rng`, `SystemRng`, `Hash`, `Mac`, `random_bytes`, and `mac_once`.
Backend-specific names live in private standard-library implementation modules
or C shims.

Backend decision and release shape:

- Use Botan through its C FFI as the first backend.
- Keep Botan details behind private standard-library modules or C shims.
- Expose a practical first surface: RNG, hash, MAC, constant-time comparison,
  and streaming hash/MAC contexts.
- Add AEAD after the first surface is stable.

Minimum public API:

```text
random_bytes(out: []u8) -> Result<void, Error>

struct SystemRng

system_rng() -> Result<SystemRng, Error>
rng_random_bytes(rng: SystemRng, out: []u8) -> Result<void, Error>

hash_once(algorithm: []const char, data: []const u8, out: []u8)
    -> Result<usize, Error>

mac_once(algorithm: []const char, key: []const u8, data: []const u8, out: []u8)
    -> Result<usize, Error>

constant_time_eq(left: []const u8, right: []const u8) -> bool
```

Required streaming API:

```text
struct Hash
struct Mac

hash_new(algorithm: []const char) -> Result<Hash, Error>
hash_update(hash: Hash, data: []const u8) -> Result<void, Error>
hash_finish(hash: Hash, out: []u8) -> Result<usize, Error>
hash_clear(hash: Hash) -> Result<void, Error>

mac_new(algorithm: []const char, key: []const u8) -> Result<Mac, Error>
mac_update(mac: Mac, data: []const u8) -> Result<void, Error>
mac_finish(mac: Mac, out: []u8) -> Result<usize, Error>
mac_clear(mac: Mac) -> Result<void, Error>
```

Actor and thread-safety policy:

- `random_bytes`, `system_rng`, `rng_random_bytes`, `hash_once`, `mac_once`, and
  `constant_time_eq` are safe concurrent entry points. The implementation may
  use thread-local Botan objects, short-lived Botan objects, or Botan-backed
  shared RNG facilities.
- `random_bytes` is the convenience CSPRNG entry point. `SystemRng` is the
  reusable CSPRNG handle for code that needs an explicit random source.
- `Hash` and `Mac` are stateful mutable contexts. They are actor-local resources
  and should be used by one actor or worker at a time.
- `Hash`, `Mac`, and future stateful RNG contexts do not implement `Message`.
- Application code should pass bytes, keys, algorithm names, and completed
  digest/MAC values across actor boundaries. It should keep live crypto contexts
  inside the actor that owns the ongoing operation.
- Shared crypto services should be modeled as actors that own their contexts and
  process request messages serially.
- Future `Aead` stateful contexts follow the same actor-local rule. Tunnel data
  encryption contexts belong to the stream actor or stream worker that owns the
  corresponding stream state.

Target first-version algorithm names:

```text
SHA-256
SHA-384
SHA-512
HMAC(SHA-256)
HMAC(SHA-384)
HMAC(SHA-512)
```

Recommended follow-up API:

```text
aead_seal_into(
    algorithm: []const char,
    key: []const u8,
    nonce: []const u8,
    aad: []const u8,
    plaintext: []const u8,
    out: []u8
) -> Result<usize, Error>

aead_open_into(
    algorithm: []const char,
    key: []const u8,
    nonce: []const u8,
    aad: []const u8,
    ciphertext: []const u8,
    out: []u8
) -> Result<usize, Error>

aead_seal(
    algorithm: []const char,
    key: []const u8,
    nonce: []const u8,
    aad: []const u8,
    plaintext: []const u8
) -> Result<ByteBuf, Error>

aead_open(
    algorithm: []const char,
    key: []const u8,
    nonce: []const u8,
    aad: []const u8,
    ciphertext: []const u8
) -> Result<ByteBuf, Error>
```

The `*_into` APIs are the data-plane path for tunnel frames and let callers
reuse buffers. The `ByteBuf` returning APIs are convenience wrappers for
control-plane code and tests.

Target follow-up algorithm names:

```text
AES-256/GCM
ChaCha20Poly1305
```

The first crypto release must document output-size requirements and return a
standard error when the output buffer is too small. It may use fixed-size
caller-provided output buffers before `/std/buf` is mature.

## 11. C FFI Requirements

The socket and crypto layers may use C shims to hide platform and backend
differences.

Required C shim responsibilities:

- create, bind, listen, accept, connect, read, write, shutdown, and close TCP
  sockets.
- convert OS errors into stable integer error codes.
- parse numeric IPv4 and bracketed numeric IPv6 endpoints, and perform explicit
  DNS resolution for host/port APIs.
- keep raw OS descriptors private to safe Ciel APIs.

Required async network shim responsibilities:

- integrate TCP accept, connect, read, and write completion with the existing
  actor-oriented async model.
- expose opaque async operation objects for `accept`, `connect`, `read`, and
  `write`.
- support `notify_done` and `finish` style completion handling, matching the
  contract already used by `/std/async_io`.
- keep async socket state private to standard-library handle wrappers.

Required Botan shim responsibilities:

- initialize Botan FFI when needed.
- expose CSPRNG, hash, MAC, and constant-time comparison operations to Ciel.
- keep one-shot RNG/hash/MAC entry points safe for concurrent calls.
- leave explicit RNG, stateful hash, MAC, and future AEAD context ownership to
  the Ciel standard-library handle.
- convert Botan status codes into stable standard-library errors.
- own Botan object lifetime behind private standard-library handles.
- accept algorithm names from Ciel as UTF-8 byte slices converted to C strings.
- keep Botan-specific opaque pointers inside standard-library handles.

Required Ciel safety rules:

- Raw socket handles are private to `/std/net`.
- Raw async socket handles and async operation tokens are private to
  `/std/async_net`.
- Raw crypto handles are private to `/std/crypto`.
- Live `/std/map::HashMap` storage is actor-local unless a future wrapper
  explicitly implements safe transfer semantics.
- Safe code cannot construct a fake `TcpStream` from arbitrary integers.
- All raw-handle adoption, pointer casts, and external C calls stay inside
  `unsafe` blocks or trusted standard-library wrappers.
- Public APIs return `Result<T, Error>` and never expose `errno` directly.

## 12. Application Modules

Suggested layout:

```text
examples/intranet_tunnel/
  main_server.ciel
  main_agent.ciel
  protocol/frame.ciel
  protocol/codec.ciel
  protocol/auth.ciel
  protocol/error.ciel
  server/control.ciel
  server/listener.ciel
  server/streams.ciel
  agent/control.ciel
  agent/dialer.ciel
  relay.ciel
```

Module responsibilities:

- `protocol/frame`: frame structs, enums, size limits, and validation.
- `protocol/codec`: encode/decode frame headers and payload structs.
- `protocol/auth`: handshake payloads and `/std/crypto` calls.
- `protocol/error`: protocol-specific error enum and formatting impl.
- `server/control`: authenticated agent control connection.
- `server/listener`: public listener and stream id allocation through
  `/std/async_net`.
- `server/streams`: server-side stream table built on `/std/map` and
  server-side state transitions.
- `agent/control`: agent-side control loop.
- `agent/dialer`: private target connection management and agent-side stream
  table entries built on `/std/map`.
- `relay`: socket-to-frame and frame-to-socket data movement.

## 13. CLI Requirements

Server:

```text
tunnel-server \
  --control 127.0.0.1:7000 \
  --public 127.0.0.1:7001 \
  --route dev \
  --psk secret
```

Agent:

```text
tunnel-agent \
  --server 127.0.0.1:7000 \
  --target 127.0.0.1:9000 \
  --route dev \
  --psk secret
```

If `/std/env` is not ready, the first checked-in demo may hardcode these values
behind constants. The product target still requires CLI parsing.

## 14. Error Model

Application functions should return `Result<T, Error>` at public boundaries.
Protocol and network internals may use precise enums and box them into
`/std/error::Error` at process or actor boundaries.

Expected error categories:

- invalid command-line arguments.
- address parse failure.
- socket listen/connect/read/write failure.
- authentication failure.
- protocol version mismatch.
- malformed frame.
- oversized frame.
- invalid stream transition.
- private target dial failure.
- unexpected EOF.

Each precise error enum must implement `format_error` so `?` can box it into
standard `Error`.

## 15. Logging And Diagnostics

The MVP should print plain text diagnostics to stderr.

Required events:

- server started with control and public addresses.
- agent connected to server.
- authentication accepted or rejected.
- public client accepted.
- stream opened with stream id.
- target dial failed.
- stream closed.
- malformed frame.
- control connection closed.

Logs must not print the pre-shared key.

## 16. Testing Requirements

### 16.1 Unit-Level Tests

Compiler fixture tests should cover:

- frame header encode/decode round trips.
- frame decode rejection for short input.
- frame decode rejection for oversized length.
- invalid stream state transition rejection.
- `/std/map` insertion, replacement, lookup, removal, collision handling,
  scoped mutable entry access, and primitive plus structural key-policy
  coverage.
- `/std/codec` big-endian round trips and short-buffer rejection.
- `/std/crypto` CSPRNG, hash, MAC, explicit `SystemRng`, and constant-time
  equality tests.
- `/std/crypto` rejects `Message` use for stateful crypto handles.
- `/std/crypto` one-shot APIs are callable from multiple actors in the same
  process.
- `/std/net` address parse and safe close behavior.
- `/std/async_net` completion shape for accept, connect, read, and write.
- `/std/async_time` timer completion, cancellation, and task composition.
- authentication tag success and failure through `/std/crypto`.

### 16.2 Integration Tests

Local integration tests should run only on loopback.

Required scenarios:

- echo service through tunnel.
- two sequential client connections.
- two concurrent client connections.
- wrong pre-shared key rejection.
- target unavailable.
- client closes before target response.
- large payload split across multiple data frames.

Integration tests must not require privileged ports or external network access.
They should allocate localhost ports dynamically when the test harness supports
it.

## 17. Implementation Phases

The sequencing is standard-library first, demo second. Demo code should begin
only after the relevant `std/` surfaces have landed, been documented, and
passed focused tests.

### Phase 1: Core Standard Library Foundation

Deliverables:

- `/std/codec` first public API.
- `/std/buf` first public API or a clearly bounded shared byte-container API.
- `/std/map` first public API, with generic key policies that cover tunnel
  `u32` stream ids and small nominal wrappers.
- `/std/time` first public API.
- `/std/async_time` first public API for actor-friendly timers.
- `/std/env` argument access API.
- Botan-backed `/std/crypto` with RNG, hash, MAC, and constant-time comparison.

Exit criteria:

- each public API lives under `std/`, not under `examples/`.
- focused standard-library tests pass.
- actor code can wait on timers without blocking a runner thread.
- `/std/crypto` application code uses backend-neutral names only.
- stateful crypto handles remain actor-local and are not messageable.

### Phase 2: Networking Standard Library

Deliverables:

- `/std/net` core TCP handle and address API.
- `/std/async_net` actor-friendly async TCP API aligned with `/std/async_io`.
- async accept, connect, read, and write operations integrate with
  `flow::Completion`.
- loopback library tests covering connect, accept, read, write, shutdown, and
  close.

Exit criteria:

- actor-oriented code can drive TCP without blocking on direct socket calls.
- blocking wrappers, if present, are secondary convenience APIs rather than the
  primary actor path.
- async socket handles and operation tokens remain private to `std/`.

### Phase 3: Demo Protocol And Relay Core

Deliverables:

- frame header codec.
- stream id allocation.
- stream tables backed by `/std/map`.
- `OpenStream`, `OpenResult`, `Data`, `CloseWrite`, and `CloseStream`.
- server and agent processes built on the stabilized standard-library surfaces.
- bytes copied through the tunnel for at least one active stream.

Exit criteria:

- loopback echo test passes through the tunnel.
- errors use `Result<T, Error>`.
- stream state transitions are represented as enums and exhaustively switched.

### Phase 4: Authentication And Multiplexed Demo Behavior

Deliverables:

- `Hello` and `HelloOk` handshake.
- pre-shared-key authentication via `/std/crypto`.
- concurrent public streams over one agent control connection.
- wrong-key rejection test.

Exit criteria:

- unauthenticated agents cannot receive public streams.
- at least two concurrent streams work over one control connection.
- secrets are not logged.
- application code imports `/std/crypto`.
- stream table code imports `/std/map` rather than defining a demo-only keyed
  container.

### Phase 5: Product Cleanup

Deliverables:

- final CLI flow through `/std/env`.
- reconnect backoff using `/std/async_time`.
- basic ping/pong keepalive driven by `/std/async_time` idle timers.
- documented run commands.

Exit criteria:

- server and agent can be run manually from documented commands.
- integration tests still pass.

## 18. Compiler Pressure Checklist

The implementation should intentionally exercise:

- nested modules and explicit imports.
- exported protocol types.
- enum payloads and nested pattern switches.
- value copying for frame structs.
- generic standard-library containers such as `/std/map::HashMap`.
- local type holes where they improve readability.
- mutable bindings with `@`.
- `[]u8` and `[]const u8` conversions.
- fixed-size arrays for headers and nonces.
- checked integer casts and bitwise operations in codecs.
- `Result<T, Error>` and `?`.
- `defer` on socket cleanup paths.
- closures passed to scoped resource helpers.
- actor or channel message passing for worker coordination, especially through
  `/std/async`-style completions.
- `unsafe extern "C"` declarations inside standard-library modules.
- opaque C structs or private handle wrappers.
- dynamic formatting through `format_error` or `to_string`.

## 19. Acceptance Criteria

The demo is accepted when:

- `tunnel-server` and `tunnel-agent` compile as Ciel programs.
- A loopback echo service is reachable through the tunnel.
- At least two concurrent streams work over one control connection.
- Wrong authentication is rejected.
- Target dial failure closes only the affected stream.
- The program has no raw socket handle exposure in safe application code.
- The program has no raw async socket operation exposure in safe application
  code.
- The program has no raw Botan handle exposure in safe application code.
- Stateful crypto handles are actor-local; cross-actor crypto work uses values
  or a dedicated crypto actor.
- `/std/net`, `/std/async_net`, `/std/async_time`, `/std/map`, `/std/codec`,
  and the used subset of `/std/crypto` are documented and tested as
  standard-library APIs.
- `/std/crypto` uses Botan as its first backend while exposing a backend-neutral
  Ciel API.
- The demo remains within the intended scale: roughly 3000 lines of Ciel
  application code, excluding standard-library and C shim additions.

## 20. Open Questions

- Should stream payload encryption be included in Phase 3, or should Phase 3
  stop at authenticated cleartext?
- Should route names be part of the authenticated message, or should the server
  choose a single configured route before authentication?
