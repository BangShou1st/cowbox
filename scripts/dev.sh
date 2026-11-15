#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cowbox-target}"

if ! command -v rustup >/dev/null 2>&1; then
    echo "rustup is required: https://rustup.rs" >&2
    exit 1
fi

rustup toolchain install stable --profile minimal
rustup component add rustfmt clippy --toolchain stable

cd "$ROOT"
cargo fmt --check
cargo check --workspace
echo "cowbox dev environment ready; CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
