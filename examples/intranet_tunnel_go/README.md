# Go Intranet Tunnel Reference

This module is the Go reference implementation for `../intranet-tunnel-prd.md`
and mirrors the Ciel demo's public behavior:

- `tunnel-server` listens on a control port and a public client port.
- `tunnel-agent` connects to the control port, authenticates with the
  pre-shared key, and forwards public streams to one private target.
- The wire protocol, frame header, stream ids, HMAC(SHA-256) authentication,
  open-result payloads, data frames, and close frames match the Ciel
  implementation.

## Build

```sh
go build -C examples/intranet_tunnel_go -o /tmp/tunnel-server ./cmd/tunnel-server
go build -C examples/intranet_tunnel_go -o /tmp/tunnel-agent ./cmd/tunnel-agent
```

## Run

```sh
/tmp/tunnel-server \
  --control 127.0.0.1:7000 \
  --public 127.0.0.1:7001 \
  --route dev \
  --psk secret
```

```sh
/tmp/tunnel-agent \
  --server 127.0.0.1:7000 \
  --target 127.0.0.1:9000 \
  --route dev \
  --psk secret
```

The Ciel demo's loopback integration driver can run against these binaries:

```sh
python3 examples/intranet_tunnel/test/integration.py /tmp/tunnel-server /tmp/tunnel-agent
```
