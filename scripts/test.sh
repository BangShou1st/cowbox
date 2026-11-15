#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cowbox-target}"

cd "$ROOT"
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

if [[ ! -f crates/cowbox-cli/tests/adversarial_suite.rs ]]; then
    echo "missing adversarial suite: crates/cowbox-cli/tests/adversarial_suite.rs" >&2
    exit 1
fi

COWBOX_REQUIRE_SANDBOX=1 cargo test -p cowbox-cli --test adversarial_suite -- --test-threads=1
bash scripts/smoke.sh

echo "all cowbox tests passed; CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
