# Contributing to cowbox

Thanks for helping make local agent workflows safer. cowbox is deliberately
small at the protocol layer and strict at the engine layer; please read
[AGENTS.md](AGENTS.md) before touching `cowbox-core`. By participating you
agree to follow [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## Development environment

M1 is developed and tested on Linux. On Windows, use WSL2 with Ubuntu 24.04 and
keep build artifacts on the Linux filesystem:

```bash
rustup toolchain install stable --profile minimal --component rustfmt clippy
export CARGO_TARGET_DIR="$HOME/.cache/cowbox-target"
```

If the checkout lives under `/mnt/e`, do not let Cargo write `target/` there.
Integration fixtures must be created under `/tmp`.

Verify the kernel capabilities:

```bash
unshare -rm echo ok
grep -w overlay /proc/filesystems
```

## Build and test

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
COWBOX_REQUIRE_SANDBOX=1 cargo test -p cowbox-cli --test adversarial_suite -- --test-threads=1
bash scripts/smoke.sh
```

Benchmarks:

```bash
export CARGO_TARGET_DIR="$HOME/.cache/cowbox-target"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"

bash scripts/bench-m2.sh \
  --samples 20 \
  --warmups 1 \
  --file-counts 10000 200000 \
  --depths 1 \
  --json-output "$HOME/cowbox-bench-results/cowbox-m2-rerun-$stamp.json" \
  --markdown-output "$HOME/cowbox-bench-results/cowbox-m2-rerun-$stamp.md"

# Pure library microbenchmarks:
scripts/bench.sh
```

Only publish numbers produced by a real run on a known machine. Update
[docs/BENCHMARKS.md](docs/BENCHMARKS.md) with the command, fixture size, kernel,
and filesystem.

## Commit style

Use Conventional Commits:

- `feat(core): ...`
- `fix(cli): ...`
- `test: ...`
- `docs: ...`
- `ci: ...`
- `chore: ...`

Keep commits focused. Do not mix generated artifacts, benchmark output, or
unrelated formatting with behavior changes.

## Pull requests

A PR should include:

- The problem and the chosen approach.
- The exact commands used to verify it.
- Any new limitation or compatibility requirement.
- A note if a public type or CLI flag changed.

Before requesting review:

- [ ] `cargo fmt --check` passes.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes.
- [ ] `cargo test --workspace` passes.
- [ ] The adversarial suite passes in WSL2 ext4.
- [ ] No silent CoW fallback, raw `libc`, or `unsafe` was introduced.
- [ ] Benchmark or demo output is real, or explicitly marked TBD.

## Scope discipline

M1 does not accept MCP proxy, native Windows, macOS, network isolation, or
multi-agent race features. Those belong to later milestones. Small, focused
fixes that make M1's existing behavior more deterministic are always welcome.

## Security

Do not open a public issue for a vulnerability. Follow
[SECURITY.md](SECURITY.md) and use GitHub private vulnerability reporting.
