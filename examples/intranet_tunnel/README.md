# Ciel Intranet Tunnel Demo

This demo implements the multiplexed TCP tunnel from
`../intranet-tunnel-prd.md`.

It builds two Ciel programs:

- `tunnel-server`: listens for one authenticated agent on a control port and
  accepts public clients on a public port.
- `tunnel-agent`: connects to the server control port, authenticates with a
  pre-shared key, and dials one configured private target per logical stream.

## Build

From the repository root:

```sh
cargo run --quiet -- \
  --project-root "$PWD/examples" \
  --std-path "$PWD" \
  "$PWD/examples/intranet_tunnel/main_server.ciel" \
  -o /tmp/tunnel-server

cargo run --quiet -- \
  --project-root "$PWD/examples" \
  --std-path "$PWD" \
  "$PWD/examples/intranet_tunnel/main_agent.ciel" \
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
