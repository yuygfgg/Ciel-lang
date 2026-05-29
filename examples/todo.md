# Intranet Tunnel Demo TODO

This checklist breaks `examples/intranet-tunnel-prd.md` into small,
testable steps. Work should land standard-library surfaces first, then the demo
application.

Each task should finish with focused tests. Prefer adding discovered fixtures
under `tests/cases/**` plus any Rust harness coverage needed for runtime or
host integration.

## Phase 1: Core Standard Library Foundation

- [ ] Confirm the standard-library module names and import paths.
      Scope: `/std/codec`, `/std/buf`, `/std/time`, `/std/env`, `/std/crypto`,
      `/std/net`, and `/std/async_net`.
      Tests: documentation-only task; run `git diff --check`.

- [x] Add `/std/codec` endian-aware unsigned integer helpers.
      Implement `put_be`, `put_le`, `get_be`, and `get_le` interfaces for
      unsigned integers, plus encoded length and allocating encode helpers.
      Tests: round-trip success, short output buffer rejection, short input
      rejection, boundary values, little-endian ordering, allocating encode,
      and signed integer rejection.

- [x] Add `/std/buf` fixed-capacity `ByteBuf` shell.
      Implement construction, length, clear, const slice view, mutable slice
      view, and append within current capacity.
      Tests: append success, reserve overflow, clear preserves capacity, const
      and mutable slice lengths match.

- [x] Add `/std/buf` reserve/growth behavior.
      Implement `byte_buf_reserve` and growing append.
      Tests: append across initial capacity, repeated reserve, preserved
      contents after growth.

- [x] Add `/std/time` monotonic clock and sleep wrappers.
      Implement `monotonic_ms` and `sleep_ms`.
      Tests: monotonic values do not go backwards; `sleep_ms(1)` returns
      successfully. Keep timing assertions loose.

- [x] Add `/std/env` process argument access.
      Implement `args_len` and `arg`.
      Tests: compiler/CLI harness passes known arguments and verifies indexing,
      out-of-range error, and UTF-8 byte slice stability.

## Phase 2: Botan-Backed `/std/crypto`

- [x] Add the private Botan C shim skeleton.
      Expose initialization, error-code conversion, and CSPRNG fill to Ciel.
      Tests: build/link with Botan, CSPRNG fill writes a buffer, zero-length
      output succeeds.

- [x] Add `/std/crypto` CSPRNG APIs.
      Implement `random_bytes`, `system_rng`, and `rng_random_bytes`.
      Tests: two calls produce valid filled buffers; explicit `SystemRng` can
      fill multiple buffers; concurrent actor calls complete without shared
      Botan handle exposure.

- [x] Add `/std/crypto::hash_once`.
      Support `SHA-256`, `SHA-384`, and `SHA-512`.
      Tests: known-answer vectors, short output buffer error, unknown algorithm
      error.

- [x] Add streaming `Hash`.
      Implement `hash_new`, `hash_update`, `hash_finish`, and `hash_clear`.
      Tests: streaming and one-shot results match; finish after multiple
      updates; clear releases the handle; use-after-clear reports an error or is
      rejected by the wrapper design.

- [x] Enforce `Hash` actor-local semantics.
      Ensure stateful hash handles do not implement `Message`.
      Tests: a fixture attempting to send `Hash` through an actor or channel is
      rejected; passing digest bytes succeeds.

- [x] Add `/std/crypto::mac_once`.
      Support `HMAC(SHA-256)`, `HMAC(SHA-384)`, and `HMAC(SHA-512)`.
      Tests: known-answer vectors, short output buffer error, wrong key
      produces a different tag.

- [x] Add streaming `Mac`.
      Implement `mac_new`, `mac_update`, `mac_finish`, and `mac_clear`.
      Tests: streaming and one-shot results match; split updates match a single
      update; clear releases the handle.

- [x] Enforce `Mac` actor-local semantics.
      Ensure stateful MAC handles do not implement `Message`.
      Tests: a fixture attempting to send `Mac` through an actor or channel is
      rejected; passing completed MAC bytes succeeds.

- [x] Add `/std/crypto::constant_time_eq`.
      Tests: equal slices, different slices with same length, different lengths,
      and empty slices.

## Phase 3: Blocking TCP Foundation

- [ ] Add `/std/net::SocketAddr` and loopback address parsing.
      Implement `parse_addr` for the demo-required `host:port` shape.
      Tests: valid `127.0.0.1:port`, invalid host, invalid port, and missing
      port.

- [ ] Add private TCP listener and stream handles.
      Implement safe wrappers around raw descriptors or C shim handles.
      Tests: safe code cannot construct fake handles; close is available only
      through standard-library APIs.

- [ ] Add blocking `tcp_listen`, `tcp_accept`, and `tcp_connect`.
      Tests: loopback connect/accept smoke test with dynamic local ports.

- [ ] Add blocking `tcp_read` and `tcp_write`.
      Tests: echo one small payload, handle EOF, handle zero-length write.

- [ ] Add `tcp_shutdown_read`, `tcp_shutdown_write`, `tcp_close`, and
      `listener_close`.
      Tests: close twice reports a stable error or safe no-op policy; shutdown
      write produces EOF on peer; listener close releases the port.

- [ ] Add optional scoped TCP helpers.
      Implement scoped connect/listen helpers only if they match existing
      `/std/io` handle style cleanly.
      Tests: `defer` or scoped body closes streams on success and error paths.

## Phase 4: Actor-Friendly `/std/async_net`

- [ ] Extract shared async `Bytes`.
      Move the existing async I/O byte ownership model into a shared module and
      re-export it from `/std/async_io` and `/std/async_net`.
      Tests: existing async I/O byte tests still pass; async networking can use
      `flow::Completion<S, Bytes>` without defining a second byte type.

- [ ] Add async TCP listener support.
      Define `AsyncTcpListener` and `AsyncAccept`; implement `listen_async`,
      `accept_async`, `close_listener`, `accept_completion`, and the matching
      `notify_done`/`finish` impls.
      Tests: an actor receives an accept completion, finishes to an
      `AsyncTcpStream`, and closing the listener releases the port.

- [ ] Add async TCP connect support.
      Define `AsyncTcpStream` and `AsyncConnect`; implement `connect_async`,
      `close_stream`, `connect_completion`, and the matching
      `notify_done`/`finish` impls.
      Tests: an actor receives a connect completion for a loopback listener;
      refused connection returns a stable `Error`; closing the stream releases
      the connection.

- [ ] Add async TCP stream I/O support.
      Define `AsyncRead` and `AsyncWrite`; implement `read_bytes`,
      `write_bytes`, `shutdown_read`, `shutdown_write`, `read_completion`,
      `write_completion`, and the matching `notify_done`/`finish` impls.
      Tests: a `flow::then` pipeline accepts, connects, reads, writes, shuts
      down write, observes EOF, and closes a loopback connection.

## Phase 5: Tunnel Protocol Library

- [ ] Create `examples/intranet_tunnel/protocol/frame.ciel`.
      Define frame constants, frame kind enum, `FrameHeader`, and maximum
      payload length.
      Tests: enum switches are exhaustive; invalid kind conversion returns a
      protocol error.

- [ ] Create `examples/intranet_tunnel/protocol/codec.ciel`.
      Implement frame header encode/decode using `/std/codec`.
      Tests: header round trip, short header rejection, bad magic rejection,
      unsupported version rejection, oversized payload length rejection.

- [ ] Add protocol error types and formatting.
      Implement precise protocol errors with `format_error`.
      Tests: each error variant formats to a non-empty message; `?` boxes into
      `/std/error::Error`.

- [ ] Add stream state transition helpers.
      Implement server and agent stream-state enums and transition functions.
      Tests: valid transitions succeed; invalid transitions return protocol
      errors; terminal `Closed` transitions are stable.

- [ ] Add authentication payload encoding.
      Encode the Hello fields needed by the PRD: version, nonce, route name, and
      tag.
      Tests: round trip, wrong length rejection, route name length bounds.

- [ ] Add authentication tag computation.
      Use `/std/crypto::mac_once("HMAC(SHA-256)", ...)`.
      Tests: matching PSK verifies; wrong PSK rejects; repeated nonce/message
      computes the same tag.

## Phase 6: Single-Stream Tunnel Demo

- [ ] Add `main_server.ciel` and `main_agent.ciel` skeletons.
      Hardcode loopback addresses if `/std/env` is not ready in the branch.
      Tests: both programs compile and print startup diagnostics.

- [ ] Add server control connection accept path.
      Server accepts one agent, reads Hello, validates authentication, and
      replies HelloOk.
      Tests: good PSK succeeds; wrong PSK closes the control connection.

- [ ] Add agent control connection path.
      Agent connects to server, sends Hello, receives HelloOk, and enters the
      control loop.
      Tests: agent reports success with good PSK; reports authentication error
      with wrong PSK.

- [ ] Add server public listener for one client.
      Server accepts one public client and allocates one stream id.
      Tests: client connection creates `OpenStream`.

- [ ] Add agent target dial for one stream.
      Agent receives `OpenStream`, connects to target, and sends `OpenResult`.
      Tests: target available succeeds; target unavailable sends error result.

- [ ] Add client-to-target data relay for one stream.
      Server reads public client bytes, sends `Data`; agent writes to target.
      Tests: target echo service receives the payload.

- [ ] Add target-to-client data relay for one stream.
      Agent reads target bytes, sends `Data`; server writes to public client.
      Tests: loopback echo through tunnel returns the original payload.

- [ ] Add one-stream close handling.
      Implement `CloseWrite` and `CloseStream` for client EOF, target EOF, and
      fatal socket errors.
      Tests: early public client close, target close, and repeated close frames
      follow the documented state machine.

## Phase 7: Multiplexed Tunnel Demo

- [ ] Add server stream table.
      Track stream id, public client handle, and server-side stream state.
      Tests: two stream ids allocate distinctly and close independently.

- [ ] Add agent stream table.
      Track stream id, target handle, and agent-side stream state.
      Tests: two target connections stay independent.

- [ ] Route incoming `Data` frames by stream id.
      Tests: interleaved frames for two streams deliver to the correct peer.

- [ ] Route close frames by stream id.
      Tests: closing one stream leaves the other stream open.

- [ ] Add concurrent client integration test.
      Start one server, one agent, one echo target, and two public clients.
      Tests: both clients receive their own echoed payloads over one control
      connection.

## Phase 8: Product Cleanup

- [ ] Replace hardcoded options with `/std/env` parsing.
      Support `--control`, `--public`, `--server`, `--target`, `--route`, and
      `--psk`.
      Tests: valid options parse; missing value and unknown flag report errors.

- [ ] Add reconnect backoff for the agent.
      Use `/std/time::sleep_ms` and bounded retry state.
      Tests: failed first connect retries; successful later connect proceeds.

- [ ] Add ping/pong keepalive frames.
      Tests: ping receives pong; missed pong closes or marks the control
      connection according to the documented policy.

- [ ] Add manual run documentation.
      Document local echo target, server command, agent command, and client
      command.
      Tests: documentation-only task; run `git diff --check`.

## Final Acceptance

- [ ] Run focused standard-library tests for codec, buffer, time, env, crypto,
      net, and async_net.
- [ ] Run discovered Ciel fixture tests for all added fixtures.
- [ ] Run loopback tunnel integration tests:
      echo, sequential clients, concurrent clients, wrong PSK, target
      unavailable, early public client close, and large payload split across
      multiple data frames.
- [ ] Confirm application code imports `/std/crypto` portable APIs.
- [ ] Confirm stateful crypto handles and raw socket/async handles stay private
      to their owning standard-library modules.
- [ ] Run `git diff --check`.
