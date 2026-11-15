#!/usr/bin/env bash
set -euo pipefail

COWBOX="${COWBOX:-cowbox}"
DEMO_DIR="${DEMO_DIR:-/tmp/cowbox-demo}"

if ! command -v "$COWBOX" >/dev/null 2>&1; then
    echo "cowbox binary not found: $COWBOX" >&2
    echo "build it with: cargo build --release -p cowbox-cli" >&2
    exit 1
fi

rm -rf "$DEMO_DIR"
mkdir -p "$DEMO_DIR/src" "$DEMO_DIR/data"

cat > "$DEMO_DIR/src/main.rs" <<'RS'
fn main() {
    println!("demo");
}
RS

python3 - "$DEMO_DIR/data/app.sqlite" <<'PY'
import sqlite3
import sys

conn = sqlite3.connect(sys.argv[1])
conn.execute("create table users (id integer primary key, email text not null unique)")
conn.execute("insert into users (email) values ('demo@example.com')")
conn.commit()
conn.close()
PY

cd "$DEMO_DIR"
"$COWBOX" init

echo
echo "== destructive command inside cowbox =="
"$COWBOX" run -- sh -c 'rm -rf src data/app.sqlite'

echo
echo "== net changes =="
"$COWBOX" diff --stat

echo
echo "== rewind to base =="
"$COWBOX" rollback 0

echo
echo "== verify the sandbox view is intact =="
"$COWBOX" run -- sh -c 'test -f src/main.rs && test -f data/app.sqlite && echo files-restored'

echo
echo "== review before host write =="
"$COWBOX" commit --dry-run

echo
echo "== clean up the demo session =="
"$COWBOX" discard --yes

echo
echo "demo complete: $DEMO_DIR"
