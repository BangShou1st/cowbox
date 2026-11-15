# cowbox benchmarks

## Status

The benchmark harness is being built with M1. This document currently records
the methodology; the result table is filled only after a real run on the
development machine. **TBD is intentional; no numbers are invented.**

## Machine profile

Fill this section from the actual run:

```text
Host:        Windows 11 + WSL2
Kernel:      6.6.87.2-microsoft-standard-WSL2 (planning baseline)
Filesystem:  ext4 for state and fixtures; /mnt/e is 9p and not a benchmark target
CPU:         TBD
Memory:      TBD
Rust:        stable (see rust-toolchain.toml)
```

## Fixtures

| Name | Files | Shape | Purpose |
| --- | ---: | --- | --- |
| `small` | 1,000 | nested directories, a few symlinks | quick local/CI smoke |
| `default` | 10,000 | nested directories, symlinks, one SQLite DB | M1 gate |
| `large` | 200,000 | deep directory tree, dependency-like entries | M2 target; not an M1 gate |

Fixtures are generated under `/tmp` on ext4. The generator must not place
fixtures under `/mnt/e`, `/mnt/c`, or the repository.

## Metrics

| Benchmark | Definition | Target (P99, 10k files) |
| --- | --- | ---: |
| `mount_cold` | create a fresh session view and mount OverlayFS | < 5 ms |
| `step_freeze` | freeze one upper layer and prepare the next | < 10 ms |
| `diff_scan` | scan a layer with 10 modified files against a 10k base | < 25 ms |
| `rollback` | validate a target step and update the DAG head | < 5 ms |

Targets are engineering goals, not published results. If a target is missed,
report the measured value and the reason; do not tune the definition after the
fact.

## How to run

```bash
export CARGO_TARGET_DIR="$HOME/.cache/cowbox-target"
cd /mnt/e/project/cowbox

scripts/bench.sh
# or
COWBOX_BENCH_FILES=10000 cargo bench -p cowbox-core --bench engine
```

For a quick check:

```bash
COWBOX_BENCH_FILES=1000 cargo bench -p cowbox-core --bench engine -- --quick
```

## Results

| Benchmark | 1k files | 10k files | 200k files |
| --- | ---: | ---: | ---: |
| `mount_cold` | TBD | TBD | TBD |
| `step_freeze` | TBD | TBD | TBD |
| `diff_scan` | TBD | TBD | TBD |
| `rollback` | TBD | TBD | TBD |

Record the command, date, kernel, filesystem, and CPU alongside a result. Do not
compare against Docker, worktrees, or other tools unless those numbers were
measured on the same machine with a documented script.

## CI policy

GitHub-hosted runners are shared and noisy. CI compiles the benchmark targets
and does not enforce latency thresholds. The 1k and 10k runs are executed on the
development machine before release; a dedicated benchmark runner can be added
later. A red local gate is not waived because CI is green.
