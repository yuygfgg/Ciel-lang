#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)
PROJECT_DIR="$ROOT/examples/intranet_tunnel"
PROJECT_MANIFEST="$PROJECT_DIR/ciel.toml"
OUT_DIR="${TMPDIR:-/tmp}/ciel_intranet_tunnel_protocol_tests"

mkdir -p "$OUT_DIR"
cd "$ROOT"

compile_ciel_entry() {
    entry="$1"
    exe="$2"
    if [ -n "${CIELC:-}" ]; then
        "$CIELC" --manifest-path "$PROJECT_MANIFEST" --std-path "$ROOT" --entry "$entry" -o "$exe"
    else
        cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -- \
            --manifest-path "$PROJECT_MANIFEST" \
            --std-path "$ROOT" \
            --entry "$entry" \
            -o "$exe"
    fi
}

for case in "$SCRIPT_DIR"/*_test.ciel; do
    name=$(basename "$case" .ciel)
    exe="$OUT_DIR/$name"
    compile_ciel_entry "$name" "$exe"
    "$exe"
done

SERVER_EXE="$OUT_DIR/tunnel-server"
AGENT_EXE="$OUT_DIR/tunnel-agent"
compile_ciel_entry server "$SERVER_EXE"
compile_ciel_entry agent "$AGENT_EXE"

if [ "${CIEL_TUNNEL_SKIP_LOOPBACK:-0}" != "1" ]; then
    if ! command -v python3 >/dev/null 2>&1; then
        echo "python3 is required for tunnel loopback integration tests" >&2
        exit 1
    fi
    python3 "$SCRIPT_DIR/integration.py" "$SERVER_EXE" "$AGENT_EXE"
fi
