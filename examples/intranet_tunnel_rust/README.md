# Rust Intranet Tunnel Reference

This crate is a Rust reference implementation for `../intranet-tunnel-prd.md`
and `../todo.md`. It intentionally mirrors Ciel-facing product capabilities:

- TCP listener/stream behavior maps to `/std/net` and `/std/async_net`.
- CLI parsing maps to `/std/env`.
- HMAC(SHA-256), random nonces, and constant-time comparison map to
  `/std/crypto`.
- Binary big-endian frame encoding maps to `/std/codec`.
- Stream close and target-failure behavior maps to the protocol state helpers.

The Rust implementation uses `std` for networking and threading, plus
`hmac`, `sha2`, and `getrandom` for the crypto/RNG capability that Ciel exposes
through `/std/crypto`.

## Run

```sh
cargo run --manifest-path examples/intranet_tunnel_rust/Cargo.toml --bin tunnel-server -- \
  --control 127.0.0.1:7000 \
  --public 127.0.0.1:7001 \
  --route dev \
  --psk secret
```

```sh
cargo run --manifest-path examples/intranet_tunnel_rust/Cargo.toml --bin tunnel-agent -- \
  --server 127.0.0.1:7000 \
  --target 127.0.0.1:9000 \
  --route dev \
  --psk secret
```

The test suite starts loopback echo targets automatically:

```sh
cargo test --manifest-path examples/intranet_tunnel_rust/Cargo.toml
```
