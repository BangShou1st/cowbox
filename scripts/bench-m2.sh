#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cowbox-target}"

mkdir -p "$CARGO_TARGET_DIR"
fs_type="$(stat -f -c %T "$CARGO_TARGET_DIR")"
if [[ "$fs_type" == "9p" || "$CARGO_TARGET_DIR" == /mnt/* ]]; then
    echo "CARGO_TARGET_DIR must be on the WSL Linux filesystem, not $CARGO_TARGET_DIR" >&2
    exit 1
fi

if [[ ! -f "$ROOT/scripts/bench_m2.py" ]]; then
    echo "missing benchmark harness: $ROOT/scripts/bench_m2.py" >&2
    exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
    echo "python3 is required to run the M2 benchmark harness" >&2
    exit 1
fi

cd "$ROOT"
cargo build --release -p cowbox-cli

binary="$CARGO_TARGET_DIR/release/cowbox"
if [[ ! -x "$binary" ]]; then
    echo "missing release binary: $binary" >&2
    exit 1
fi

exec python3 "$ROOT/scripts/bench_m2.py" --binary "$binary" "$@"
