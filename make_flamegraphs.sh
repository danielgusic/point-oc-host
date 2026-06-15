#!/usr/bin/env bash
#
# Regenerate the four point-oc flamegraphs in example_flamegraphs/.
#
# Measures the point-oc guest under the point-oc-host embedder, AN-encoding off
# vs on, for both a debug and a release guest build. Uses perf via cargo
# flamegraph with frame-pointer unwinding (so the JIT'd wasm/AN frames symbolize
# across the wasm boundary) and the host's --perfmap flag.
#
# Requires: rustup, perf, cargo-flamegraph, wasm32-unknown-unknown target.
# Run from anywhere; paths are resolved relative to this script.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUEST_DIR="$HERE/../wasm-point-oc"
OUT="$HERE/example_flamegraphs"

# Telegram counts are picked so each run takes a few wall-clock seconds:
# AN-off is ~100-500x faster, so it needs far more telegrams than AN-on.
BENCH_OFF_DEBUG=4000000
BENCH_OFF_RELEASE=14000000
BENCH_ON=20000

PERF_ARGS="record -F 997 --call-graph fp -g"

echo ">> building guest (debug + release)"
( cd "$GUEST_DIR" && cargo build && cargo build --release )

echo ">> building host (profiling profile)"
( cd "$HERE" && cargo build --profile profiling )

REL_WASM="$GUEST_DIR/target/wasm32-unknown-unknown/release/point-oc.wasm"
mkdir -p "$OUT"
cd "$HERE"

# flame <output.svg> <host-args...>
flame() {
  local svg="$1"; shift
  echo ">> $svg"
  cargo flamegraph --profile profiling -c "$PERF_ARGS" -o "$OUT/$svg" -- "$@" --perfmap
}

flame an-off.svg                  --bench "$BENCH_OFF_DEBUG"
flame an-on.svg                   --an --bench "$BENCH_ON"
flame an-off-release.svg "$REL_WASM"      --bench "$BENCH_OFF_RELEASE"
flame an-on-release.svg  "$REL_WASM" --an --bench "$BENCH_ON"

echo ">> done — wrote 4 flamegraphs to $OUT"
