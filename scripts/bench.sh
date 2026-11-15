#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cowbox-target}"
export COWBOX_BENCH_FILES="${COWBOX_BENCH_FILES:-10000}"

cd "$ROOT"

if [[ ! -f crates/cowbox-core/benches/engine.rs ]]; then
    echo "missing benchmark target: crates/cowbox-core/benches/engine.rs" >&2
    exit 1
fi

echo "running cowbox benchmarks with COWBOX_BENCH_FILES=$COWBOX_BENCH_FILES"
cargo bench -p cowbox-core --bench engine -- "$@"
