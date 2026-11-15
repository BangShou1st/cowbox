# cowbox benchmarks

## Status

These are M1 measurements from the development machine below. The numbers are
reproducible with the commands in this document; no synthetic or guessed values
are published.

## Machine profile

```text
Host:        Windows 11 + WSL2 (Ubuntu 24.04)
Kernel:      6.6.87.2-microsoft-standard-WSL2
Filesystem:  ext4 for state and fixtures; /mnt/e is 9p and is not a benchmark target
CPU:         Intel(R) Core(TM) i9-14900HX, 8 vCPU visible to WSL2
Memory:      1.9 GiB visible to WSL2
Rust:        1.100.0 (2026-11-05)
Measured:    2026-11-15
```

## Fixtures

| Name | Files | Shape | Purpose |
| --- | ---: | --- | --- |
| `small` | 1,000 | 10 directory shards, 10 symlinks | quick local check |
| `default` | 10,000 | 100 directory shards, 10 symlinks | M1 gate |
| `large` | 200,000 | deep directory tree, dependency-like entries | M2 target; not an M1 gate |

Fixtures are generated under `/tmp` on ext4. The generator never places
fixtures under `/mnt/e`, `/mnt/c`, or the repository.

## Metrics

| Benchmark | Definition | Target |
| --- | --- | ---: |
| `run_true_end_to_end` | `cowbox run --json -- true` on the fixture: CLI process, session state, namespace handshake, OverlayFS mount, supervisor, cleanup. **Not a pure mount measurement.** | no hard M1 target |
| `diff_scan` | `compute_diff` over a 10-file upper layer against a full base manifest | < 25 ms at 10k |
| `rollback_dag` | two pointer moves (base and head) in a 100-step DAG | < 5 ms |
| `mount_cold` | isolated OverlayFS mount, excluding process startup | TBD; not separately instrumented in M1 |
| `step_freeze` | freeze one upper and prepare the next step | TBD; not separately instrumented in M1 |

The original M1 target for a cold mount was < 5 ms. `run_true_end_to_end` is
reported instead of pretending it is a pure mount: it includes spawning the
CLI, the helper handshake, the supervisor, overlay setup, and cleanup.

## Results

Command:

```bash
export CARGO_TARGET_DIR="$HOME/.cache/cowbox-target"
cd /mnt/e/project/cowbox

COWBOX_BENCH_FILES=1000  cargo bench -p cowbox-core --bench engine -- --sample-size 10
COWBOX_BENCH_FILES=10000 cargo bench -p cowbox-core --bench engine -- --sample-size 10
```

| Benchmark | 1k files | 10k files | 200k files |
| --- | ---: | ---: | ---: |
| `run_true_end_to_end` | 43.5 ms | 56.2 ms | TBD |
| `diff_scan` | 1.26 ms | 13.19 ms | TBD |
| `rollback_dag` | 11.3 ns | 10.7 ns | TBD |
| `mount_cold` | TBD | TBD | TBD |
| `step_freeze` | TBD | TBD | TBD |

Criterion reported these ranges in the same runs:

| Benchmark | 1k range | 10k range |
| --- | ---: | ---: |
| `run_true_end_to_end` | 41.857–45.528 ms | 51.764–60.876 ms |
| `diff_scan` | 1.233–1.280 ms | 12.957–13.558 ms |
| `rollback_dag` | 11.185–11.625 ns | 10.438–11.011 ns |

Process, helper, and mount noise still dominates the end-to-end benchmark.
Treat the end-to-end number as a range, not a precise constant. These values
were measured after the commit-recovery changes, but the benchmark does not
exercise the commit path and does not isolate a single cause for run-to-run
variation.

## Known issues observed during measurement

- M1 mounts each active layer through a short `/tmp/cw<session><step>/lN`
  symlink alias, so the `lowerdir=` option no longer grows with the full state
  path. The engine supports up to 150 layers per session and returns an explicit
  `Config` error beyond that; layer compaction is an M2 item. A 60-step session
  was verified end-to-end after this change.
- `doctor` reports the whiteout kind as unknown until the first real run. The
  first run confirms the local kernel uses a `0/0` character-device whiteout.
- `blocked_writes` is reported as not instrumented; Landlock denies writes but
  does not emit an audit event in M1.
- Base manifest capture now records inode, link count, and `user.*` xattr
  digests. `run_true` does not include manifest capture because `init` is set
  up outside the timed loop; init cost is not separately benchmarked yet.

## How to run

```bash
export CARGO_TARGET_DIR="$HOME/.cache/cowbox-target"
cd /mnt/e/project/cowbox

scripts/bench.sh
# or
COWBOX_BENCH_FILES=10000 cargo bench -p cowbox-core --bench engine -- --sample-size 10
```

The benchmark target must compile in CI; latency thresholds are not enforced on
shared GitHub runners. A dedicated benchmark runner can be added later.
