# Intranet Tunnel Benchmark

This benchmark compares the Go reference tunnel against the Ciel tunnel using a
Cargo-built Rust load tool based on Tokio.

From the repository root:

```sh
python3 examples/benchmark/intranet_tunnel/stress.py --ceiling 128
```

For a high-priority run with a prebuilt Ciel compiler:

```sh
cargo build --quiet --release
sudo env CIELC="$PWD/target/release/cielc" \
  nice -n -20 \
  python3 examples/benchmark/intranet_tunnel/stress.py \
    --ceiling 4096 \
    --fd-limit 65536 \
    --trial-gap-ms 15000 \
    --trial-gap-threshold 512
```

The Rust load tool lives in `stress_tool/` as a complete Cargo project. The
Python driver builds it with `cargo build --release` into the benchmark work
directory for each run.
