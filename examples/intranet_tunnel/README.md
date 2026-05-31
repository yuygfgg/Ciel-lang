# Ciel Intranet Tunnel Demo

This demo implements the multiplexed TCP tunnel from
`../intranet-tunnel-prd.md`.

It builds two Ciel programs:

- `tunnel-server`: listens for one authenticated agent on a control port and
  accepts public clients on a public port.
- `tunnel-agent`: connects to the server control port, authenticates with a
  pre-shared key, and dials one configured private target per logical stream.

The control connection carries framed multiplexed streams. Each public client
gets a `u32` stream id, and server/agent stream tables are backed by
`/std/map::HashMap`.

## Build

From the repository root:

```sh
cargo run --quiet -- \
  --project-root examples \
  --std-path . \
  examples/intranet_tunnel/main_server.ciel \
  -o /tmp/tunnel-server

cargo run --quiet -- \
  --project-root examples \
  --std-path . \
  examples/intranet_tunnel/main_agent.ciel \
  -o /tmp/tunnel-agent
```

## Run

Start a local echo target:

```sh
python3 -c 'import socketserver
class H(socketserver.BaseRequestHandler):
    def handle(self):
        while True:
            data = self.request.recv(32768)
            if not data:
                return
            self.request.sendall(data)
socketserver.ThreadingTCPServer(("127.0.0.1", 9000), H).serve_forever()'
```

Start the public relay:

```sh
/tmp/tunnel-server \
  --control 127.0.0.1:7000 \
  --public 127.0.0.1:7001 \
  --route dev \
  --psk secret-tunnel-key
```

Start the private agent:

```sh
/tmp/tunnel-agent \
  --server 127.0.0.1:7000 \
  --target 127.0.0.1:9000 \
  --route dev \
  --psk secret-tunnel-key
```

Connect a public client to `127.0.0.1:7001`. Bytes sent there are relayed to
the private target and echoed back over the same public client connection.

## Tests

The demo-local test script compiles protocol tests, compiles both executables,
and runs a loopback integration test covering:

- wrong PSK rejection
- sequential clients
- concurrent clients over one control connection
- a large payload split across multiple data frames
- early public client close
- target unavailable and later recovery

```sh
sh examples/intranet_tunnel/test/test.sh
```

Set `CIEL_TUNNEL_SKIP_LOOPBACK=1` to run only the Ciel compile/unit portion in
an environment that blocks loopback sockets.

## Stress Comparison

The benchmark script under `examples/benchmark/intranet_tunnel` compares the Go
reference implementation against the Ciel implementation. It builds release
binaries, builds the Cargo-based Rust load tool, verifies that the direct
echo/load path can handle the configured ceiling, then binary-searches the
highest concurrent public-client count that each tunnel can complete. The
summary reports both boundary throughput at the highest passing concurrency and
peak throughput among successful trials.

```sh
python3 examples/benchmark/intranet_tunnel/stress.py --ceiling 128
```

The script raises the soft file-descriptor limit before it starts the echo
server, load generator, and tunnel processes. Use `--fd-limit` to request a
higher limit, or `--fd-limit 0` to leave the current limit unchanged.

It also waits 15 seconds between high-concurrency load-generator trials by
default so short connections from the previous large trial have time to leave
`TIME_WAIT`. The cooldown applies only at or above concurrency 512 by default;
use `--trial-gap-threshold` to tune that cutoff, or `--trial-gap-ms 0` to
disable it.

For a high-priority run with a prebuilt compiler:

```sh
cargo build --quiet --release
sudo env CIELC="$PWD/target/release/cielc" \
  nice -n -20 \
  python3 examples/benchmark/intranet_tunnel/stress.py --ceiling 3072 --fd-limit 65536
```

Use `--payload-bytes`, `--round-trips`, and `--trial-timeout-ms` to change the
per-client workload. Use `--trial-gap-ms` to adjust the cooldown between
high-concurrency load-generator trials. Set `CIELC=/path/to/cielc` to reuse an
existing compiler binary instead of running it through Cargo.
