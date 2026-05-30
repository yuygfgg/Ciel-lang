#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)
EXAMPLES_ROOT="$ROOT/examples"
OUT_DIR="${TMPDIR:-/tmp}/ciel_intranet_tunnel_protocol_tests"

mkdir -p "$OUT_DIR"
cd "$ROOT"

for case in "$SCRIPT_DIR"/*_test.ciel; do
    name=$(basename "$case" .ciel)
    exe="$OUT_DIR/$name"
    if [ -n "${CIELC:-}" ]; then
        "$CIELC" --project-root "$EXAMPLES_ROOT" --std-path "$ROOT" "$case" -o "$exe"
    else
        cargo run --quiet --manifest-path "$ROOT/Cargo.toml" -- \
            --project-root "$EXAMPLES_ROOT" \
            --std-path "$ROOT" \
            "$case" \
            -o "$exe"
    fi
    "$exe"
done
