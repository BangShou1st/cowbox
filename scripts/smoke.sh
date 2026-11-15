#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/cowbox-target}"

if [[ -z "${COWBOX_BIN:-}" ]]; then
    cd "$ROOT"
    cargo build -p cowbox-cli
    COWBOX_BIN="$CARGO_TARGET_DIR/debug/cowbox"
fi

if [[ ! -x "$COWBOX_BIN" ]]; then
    echo "missing cowbox binary: $COWBOX_BIN" >&2
    exit 1
fi

WORK="$(mktemp -d /tmp/cowbox-smoke-XXXXXX)"
cleanup() {
    chmod -R u+rwX "$WORK" 2>/dev/null || true
    rm -rf "$WORK" 2>/dev/null || true
}
trap cleanup EXIT
mkdir -p "$WORK/project" "$WORK/state"
cd "$WORK/project"

printf 'base\n' > smoke.txt
COWBOX_HOME="$WORK/state" "$COWBOX_BIN" init --json > init.json
COWBOX_HOME="$WORK/state" "$COWBOX_BIN" run --json -- sh -c 'printf changed > smoke.txt' > run.json
COWBOX_HOME="$WORK/state" "$COWBOX_BIN" diff --json > diff.json
COWBOX_HOME="$WORK/state" "$COWBOX_BIN" rollback 0 --json > rollback.json
COWBOX_HOME="$WORK/state" "$COWBOX_BIN" commit --dry-run --json > dry-run.json
COWBOX_HOME="$WORK/state" "$COWBOX_BIN" discard --yes --purge --json > discard.json

python3 - <<'PY'
import json

run = json.load(open("run.json"))
assert run["ok"] is True, run
assert run["data"]["exit_code"] == 0, run

diff = json.load(open("diff.json"))
assert diff["ok"] is True, diff
paths = {change["rel_path"] for change in diff["data"]["changes"]}
assert "smoke.txt" in paths, diff

rollback = json.load(open("rollback.json"))
assert rollback["ok"] is True, rollback
assert rollback["data"]["head"] == 0, rollback

dry = json.load(open("dry-run.json"))
assert dry["ok"] is True, dry
assert dry["data"]["dry_run"] is True, dry
assert dry["data"]["changes"] == [], dry
assert dry["data"]["conflicts"] == [], dry

discard = json.load(open("discard.json"))
assert discard["ok"] is True, discard
assert discard["data"]["discarded"] is True, discard
PY

test "$(cat smoke.txt)" = "base"
echo "CLI smoke passed: init -> run -> diff -> rollback -> commit --dry-run"
