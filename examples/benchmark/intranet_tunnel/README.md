# Intranet Tunnel Benchmark

This benchmark compares the Go reference tunnel against the Ciel tunnel using a
Cargo-built Rust load tool based on Tokio.

## Benchmark result

```
sudo env CIELC="$PWD/target/release/cielc" \
    nice -n -20 \
    python3 examples/benchmark/intranet_tunnel/stress.py --ceiling 4096

fd limit: soft=65536 hard=9223372036854775807

=== go ===
go: concurrency=1 ok mibps=338.1 rps=2704.7 elapsed_ms=3
go: concurrency=2 ok mibps=597.0 rps=4776.3 elapsed_ms=3
go: concurrency=4 ok mibps=201.3 rps=1610.7 elapsed_ms=20
go: concurrency=8 ok mibps=875.5 rps=7004.2 elapsed_ms=9
go: concurrency=16 ok mibps=884.0 rps=7072.1 elapsed_ms=18
go: concurrency=32 ok mibps=874.5 rps=6996.0 elapsed_ms=37
go: concurrency=64 ok mibps=918.2 rps=7345.4 elapsed_ms=70
go: concurrency=128 ok mibps=950.1 rps=7601.1 elapsed_ms=135
go: concurrency=256 ok mibps=853.3 rps=6826.7 elapsed_ms=300
cooldown: waiting 14.3s before high-concurrency go concurrency=512
go: concurrency=512 ok mibps=569.0 rps=4552.0 elapsed_ms=900
cooldown: waiting 15.0s before high-concurrency go concurrency=1024
go: concurrency=1024 ok mibps=763.9 rps=6111.3 elapsed_ms=1340
cooldown: waiting 15.0s before high-concurrency go concurrency=2048
go: concurrency=2048 ok mibps=689.2 rps=5513.3 elapsed_ms=2972
cooldown: waiting 15.0s before high-concurrency go concurrency=4096
go: concurrency=4096 ok mibps=562.6 rps=4501.0 elapsed_ms=7280

=== ciel ===
ciel: concurrency=1 ok mibps=369.1 rps=2952.9 elapsed_ms=3
ciel: concurrency=2 ok mibps=656.5 rps=5252.3 elapsed_ms=3
ciel: concurrency=4 ok mibps=643.9 rps=5151.0 elapsed_ms=6
ciel: concurrency=8 ok mibps=879.8 rps=7038.2 elapsed_ms=9
ciel: concurrency=16 ok mibps=926.7 rps=7413.6 elapsed_ms=17
ciel: concurrency=32 ok mibps=905.2 rps=7241.3 elapsed_ms=35
ciel: concurrency=64 ok mibps=907.5 rps=7259.6 elapsed_ms=71
ciel: concurrency=128 ok mibps=637.2 rps=5097.9 elapsed_ms=201
ciel: concurrency=256 ok mibps=778.0 rps=6223.9 elapsed_ms=329
cooldown: waiting 14.2s before high-concurrency ciel concurrency=512
ciel: concurrency=512 ok mibps=673.3 rps=5386.2 elapsed_ms=760
cooldown: waiting 15.0s before high-concurrency ciel concurrency=1024
ciel: concurrency=1024 ok mibps=603.4 rps=4827.4 elapsed_ms=1697
cooldown: waiting 15.0s before high-concurrency ciel concurrency=2048
ciel: concurrency=2048 ok mibps=740.1 rps=5921.2 elapsed_ms=2767
cooldown: waiting 15.0s before high-concurrency ciel concurrency=4096
ciel: concurrency=4096 ok mibps=654.3 rps=5234.7 elapsed_ms=6260

=== Summary ===
implementation max_concurrency first_failure capped boundary_mibps boundary_rps peak_concurrency peak_mibps peak_rps
go 4096 - yes 562.6 4501.0 128 950.1 7601.1
ciel 4096 - yes 654.3 5234.7 16 926.7 7413.6
ciel/go concurrency ratio: 1.000
go: reached --ceiling; raise --ceiling for a higher bound
ciel: reached --ceiling; raise --ceiling for a higher bound
```