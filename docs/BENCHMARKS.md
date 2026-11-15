# Benchmarks

## Status

This document records the M2 first-run capture round and the subsequent diff
optimization round. The first-run baseline is `v0.3.0-m1` at commit `339a539`.
The diff baseline is the validated first-run candidate, which preserves the
existing JSONL schema, global path order, and compatibility with older
manifests.

Each reported comparison used the same machine, release build settings, fixture
generator, 20 measured samples, and one excluded warm-up per scenario. Raw
results are retained outside the repository in the WSL home directory:

| Run | Raw JSON SHA-256 | Release binary SHA-256 | Raw sample |
| --- | --- | --- | --- |
| Baseline `339a539` | `35b090d308a447feaf04a43e419497170f457248d81932989448a0f9efee1b03` | `63bb6ca279ecd2eca77769ad5a00feb5bd0ceb03d47d59ffb37c2a20b9304b3c` | `/home/bangshou/cowbox-bench-results/cowbox-m2-baseline-339a539.json` |
| Final candidate | `fdc67a9ec143767855c2849f67a7dc076e2079c263968a2c70d147aecbd52e9f` | `e41316faf710f624428c63400e8e37200dbe23882f9999f9b9fae2d6bde53818` | `/home/bangshou/cowbox-bench-results/cowbox-m2-candidate-fixed.json` |

The final candidate JSON was generated at `2026-11-15T18:08:10Z`. A separate
`candidate-fixed-final.json` report was also found (raw SHA-256
`8181c2da178e2467f492a126f59c40aa73052126e961e5b98fe2fdb0404888d5`, binary
SHA-256
`7b1ef3661df7cb5b30e80730e80808d29b7d5c1653820e28d75e863bc5056a8a`). It is
excluded because its binary does not match the release binary rebuilt from the
retained first-run source snapshot. That snapshot's build matches
`candidate-fixed.json` at
`e41316faf710f624428c63400e8e37200dbe23882f9999f9b9fae2d6bde53818`. Its rendered summary is
`/home/bangshou/cowbox-bench-results/cowbox-m2-candidate-fixed.md`, with SHA-256
`ab9a2adcd4862a1505f4b60596d2ff76d4aa957a98d7ed390592c7f7ab387961`.
All values below are measured output, not targets or estimates.

INVALID candidate (do not use for performance claims) at
`/home/bangshou/cowbox-bench-results/cowbox-m2-candidate-final.json`
(raw SHA-256
`4b4493e3ba02fdd1e22bc9c008a70c498fe65f79c04259d1a8a1107038573b62`,
binary SHA-256
`50778006d1d35211b1d07789a47fae038de6c602dc72f626ca40356a1a412fa5`)
stopped manifest task discovery at the first 32-directory frontier. It did not
reveal the omission with the shallow benchmark fixture, but missed descendants
below directories in that frontier. Its numbers are retained only as invalid
historical data and are not mixed with the final candidate.

## Machine profile

```text
Host:        Windows 11 Home 10.0.26200, Redmi G Pro 2024
Host CPU:    Intel Core i9-14900HX, 24 cores / 32 threads
WSL distro:  Ubuntu 24.04.3 LTS
WSL kernel:  6.6.87.2-microsoft-standard-WSL2
WSL CPU:     8 logical CPUs visible
WSL memory:  1.9 GiB visible (1,995,669,504 bytes)
Filesystem:  ext4 on /dev/sde for /tmp, state, and build target
Rust:        1.100.0 (2026-11-09)
Cargo:       1.100.0 (2026-11-05)
Build:       release, CARGO_TARGET_DIR=$HOME/.cache/cowbox-target (ext4)
```

The benchmarks do not pin CPUs. Windows and WSL share the host, so short-window
tail values include scheduler noise. The final candidate run started at load
average `1.041, 0.720, 0.322` and ended at `1.674, 0.908, 0.404`.

## Optimization

The baseline walks the tree once with `WalkDir`, calls `symlink_metadata` by full
path, queries `user.*` xattrs for every file and directory, globally sorts all
entries, and serializes JSONL on one thread.

The final candidate:

- walks the complete directory tree in breadth-first waves, scheduling every
  discovered directory exactly once and never stopping at a worker-count
  frontier;
- captures metadata, symlink targets, xattr digests, and per-directory JSONL in
  parallel workers;
- uses no-follow `symlink_metadata` for every entry and never enters a symlink
  target;
- derives global path order by depth-first traversal of the captured directory
  tree and per-entry JSONL offsets, avoiding a full manifest sort while keeping
  the historical path-sorted line order;
- writes those ordered JSONL fragments through a buffered writer;
- returns the known SHA-256 digest for an empty user-xattr set instead of
  hashing an empty byte stream for every entry.

The first-run round preserved the JSONL fields, defaults, exclusion rules,
symlink behavior, returned `Vec<ManifestEntry>` order, and full-loader behavior.
The later diff round retains those compatibility properties while adding an
internal filtered loader after the upper-layer analysis.

## Fixtures

Fixtures are generated once per file-count group under `/tmp` on ext4. Fixture
generation, `cowbox init`, warm-up samples, state cleanup, and fixture cleanup
are outside timed intervals.

| Files | Shape | Added for diff |
| ---: | --- | --- |
| 10,000 | 100 leaf directories with 100 files each, plus 10 symlinks | 10 modified files |
| 200,000 | 2,000 leaf directories in 20 groups with 100 files each, plus 10 symlinks | 10 modified files |

Every regular file is 66 bytes with deterministic content. An empty `.git`
directory marks the workspace root and is excluded from the manifest.
`.cowbox.toml` is created before measurement and is part of the base manifest.

## Measurement protocol

All durations are wall-clock milliseconds around one complete CLI subprocess.
The parent harness measures process spawn, CLI argument/config handling, JSON
output, helper handshake, namespace setup, OverlayFS, supervisor execution,
teardown, and process exit.

| Scenario | Definition |
| --- | --- |
| First run | Fresh `COWBOX_HOME`; `cowbox run --json -- true` creates the session, captures the base manifest, and executes step 1. |
| Subsequent run, 1 layer | A fresh session is prepared by one untimed run. The timed command is the second `cowbox run --json -- true`, with exactly one parent layer. |
| 10-file diff | A fresh session is prepared by one run plus one untimed step that modifies 10 files in its upper layer. The timed command is `cowbox diff --json`. |
| Timed step at depth 1 | A fresh session runs once without timing. The timer covers only the next run, which creates step 2. |

Each scenario uses `n=20` and one excluded warm-up from a fresh session.
Samples use warm page cache. P95 uses nearest rank, which is the 19th sorted
sample for `n=20`. No sample is removed or winsorized. The final formal run used
`--depths 1`; it still executed the full layer-limit probe.

## File-count results

| Scenario | Files | Baseline median | Final median | Baseline P95 | Final P95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| First run | 10,000 | 68.52 ms | **43.29 ms** | 83.89 ms | **47.235 ms** |
| Subsequent run, 1 layer | 10,000 | 39.90 ms | **38.43 ms** | 42.66 ms | 50.37 ms |
| 10-file diff | 10,000 | 19.47 ms | 24.71 ms | 30.12 ms | **32.12 ms** |
| First run | 200,000 | 612.92 ms | **163.14 ms** | 763.23 ms | **166.94 ms** |
| Subsequent run, 1 layer | 200,000 | 40.66 ms | **38.55 ms** | 48.07 ms | **45.02 ms** |
| 10-file diff | 200,000 | 372.84 ms | 388.55 ms | 390.15 ms | 401.20 ms |

Final candidate distribution:

| Scenario | Files | Mean | Min | Max | Std. dev. |
| --- | ---: | ---: | ---: | ---: | ---: |
| First run | 10,000 | 43.78 ms | 41.42 ms | 48.14 ms | 1.80 ms |
| Subsequent run, 1 layer | 10,000 | 40.37 ms | 33.44 ms | 51.50 ms | 6.19 ms |
| 10-file diff | 10,000 | 24.60 ms | 16.86 ms | 33.55 ms | 4.77 ms |
| First run | 200,000 | 161.75 ms | 154.70 ms | 168.05 ms | 3.89 ms |
| Subsequent run, 1 layer | 200,000 | 39.48 ms | 33.96 ms | 56.68 ms | 5.49 ms |
| 10-file diff | 200,000 | 389.78 ms | 370.45 ms | 403.28 ms | 8.49 ms |

The complete final candidate reduces first-run median latency by 25.23 ms
(36.82%) at 10,000 files and 449.78 ms (73.38%) at 200,000 files.

The 10,000-file first-run target of 60 ms median / 80 ms P95 passed. The
200,000-file target of 150 ms median / 200 ms P95 was only partly met: P95
passed, while median exceeded the target by 13.14 ms. Both shallow-run medians
passed the 55 ms target. The 10,000-file diff median of 24.71 ms missed its
20 ms target by 4.71 ms. The 200,000-file diff median of 388.55 ms passed its
400 ms target, while P95 was 401.20 ms.

## Layer-depth results

These are 10,000-file sessions with no file changes in each pre-fill step.

| Parent layers | Baseline step median | Final step median | Baseline P95 | Final P95 |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 41.48 ms | **37.63 ms** | 48.47 ms | **45.59 ms** |

The final candidate's single preparation run had a 50.86 ms median.

The previously measured depths 60 and 149 below belong to the superseded
defective candidate binary
`50778006d1d35211b1d07789a47fae038de6c602dc72f626ca40356a1a412fa5`.
They are historical only and are not final-candidate results:

| Parent layers | Baseline step median | Superseded candidate median | Baseline P95 | Superseded candidate P95 |
| ---: | ---: | ---: | ---: | ---: |
| 60 | 46.23 ms | 42.01 ms | 55.28 ms | 45.50 ms |
| 149 | 52.12 ms | 52.88 ms | 66.51 ms | 62.03 ms |

## Layer limit

Starting from a session with 150 active layers, the next step completed in
58.00 ms and created head step 151. The following invocation, now starting with
151 parent layers, failed in 2.94 ms with exit code 2:

```text
CONFIG_ERROR: session has 151 layers; M1 supports at most 150. Commit or discard this session and start a new one.
```

The head remained at step 151 and no step 152 directory was created.

## Diff optimization round

This round used the validated first-run candidate as its baseline and the
current dirty tree as its candidate. Both formal runs used the same machine,
fixture generator, release settings, `--depths 1`, 20 measured samples, and one
excluded warm-up per scenario. Raw results and the release binaries are
retained outside the repository:

| Run | Raw JSON SHA-256 | Release binary SHA-256 | Raw sample |
| --- | --- | --- | --- |
| First-run candidate baseline | `88f3920af6f8500733767730a9baa090f6f13de8ffbac3bbbc8b3fd7880ac20a` | `e41316faf710f624428c63400e8e37200dbe23882f9999f9b9fae2d6bde53818` | `/home/bangshou/cowbox-bench-results/cowbox-m2-diff-baseline-e41316fa.json` |
| Diff candidate | `fa924103760e675d43de7f3f13ae1a3716cc5d6b956ef3f4cdc419428f84a0dd` | `152fd4407df4047a2765a37c4daa9d6372ac9cb64b643d9f6a71a109c342d451` | `/home/bangshou/cowbox-bench-results/cowbox-m2-diff-candidate-clean.json` |

The baseline source is reconstructible from
`/home/bangshou/cowbox-bench-results/m2-diff-before/tracked-changes.patch`
(SHA-256 `25429f980897612eb614cb137d4292136b813970d679e3aff09ab3aac0b5bfe6`)
plus the two copied scripts under `m2-diff-before/untracked/`. The preserved
runnable baseline binary is under `m2-diff-before/bin/`. The candidate raw
records `git_head=339a539` and the dirty status used to run it. The baseline raw
contains the literal string `HEAD` in `source.git_head` because its reconstructed
source snapshot had no commit object. The baseline identity is instead fixed by
its preserved binary SHA-256 and the source-patch SHA-256 above, together with
the matching source snapshot under `m2-diff-before/source-tree/`.

### Formal results

Cells contain median (P95) in milliseconds. Every value is from the complete
CLI subprocess measured by the existing harness.

| Scenario | Files | First-run candidate baseline | Diff candidate |
| --- | ---: | ---: | ---: |
| First run | 10,000 | 54.752 (64.107) | **43.628 (54.298)** |
| Subsequent run, 1 layer | 10,000 | 46.637 (49.105) | **40.008 (45.999)** |
| 10-file diff | 10,000 | 25.250 (34.866) | **12.287 (16.139)** |
| First run | 200,000 | 168.203 (185.320) | **162.591 (197.321)** |
| Subsequent run, 1 layer | 200,000 | 39.702 (48.884) | **38.515 (43.034)** |
| 10-file diff | 200,000 | 462.093 (621.130) | **98.453 (106.559)** |

The 10,000-file diff target of 20 ms passed at 12.287 ms. The 200,000-file
diff target of 200 ms passed at 98.453 ms. Median diff latency fell by 51.34%
at 10,000 files and 78.69% at 200,000 files. The 10,000-file diff target is a
hard acceptance target. No baseline or candidate sample was removed.

An earlier full candidate pass ran at load average `1.800, 1.238, 0.728` and
produced a 200,000-file first-run median of 208.537 ms. It remains at
`/home/bangshou/cowbox-bench-results/cowbox-m2-diff-candidate.json` but is not
used for the formal table. A same-fixture alternating A/B confirmation followed
that pass and measured first-run medians of 163.243 ms for the baseline and
162.081 ms for the candidate at 200,000 files. The final formal pass above was
collected after the A/B at load average `0.616, 0.614, 0.654`. The A/B raw
sample is
`/home/bangshou/cowbox-bench-results/cowbox-m2-first-run-ab-200k.json`, SHA-256
`08e8e5fab6c24737459351fb13ee827de30206714af5b35f4f8d3020b59cb2d8`.

### Phase diagnosis

A diagnostic release build added temporary timers to an isolated source copy.
It measured three scored samples per size after one warm-up. Timers inflate
absolute cost, so the formal end-to-end values above are authoritative. The
medians below identified where to optimize:

| Phase | 10,000 files | 200,000 files |
| --- | ---: | ---: |
| Manifest file read | 0.133 ms | 3.944 ms |
| JSON deserialization | 3.121 ms | 91.813 ms |
| Full `BTreeMap` build | 9.542 ms | 363.013 ms |
| Upper-layer scan | 0.223 ms | 0.411 ms |
| Deleted-directory selection | 0.213 ms | 5.362 ms |
| Changed-row classification and content comparison | 0.093 ms | 0.248 ms |
| Deletion traversal | 2.073 ms | 61.670 ms |
| Line statistics | 0.135 ms | 0.295 ms |

The diagnostic raw JSON is
`/home/bangshou/cowbox-bench-results/cowbox-m2-diff-diagnostic-base.json`
(SHA-256 `a53e9adfe462a9e41df21a85102f382172c8a8ed095b735eaf0d62804df66bf2`).
The phase log is
`/home/bangshou/cowbox-bench-results/cowbox-m2-diff-diagnostic-base.profile.log`
(SHA-256 `ac6a1f913f838cf80e1e5385db822359321583ff425343ecd5251f043197ee23`).

### Implementation

The diff now scans the upper layers first and builds the upsert, whiteout, and
opaque marker sets. It then streams the base manifest with one reusable line
buffer and fully deserializes every row, preserving corruption errors for rows
unrelated to the change set. Only exact upsert rows and rows at or below a
deletion or opaque marker are retained. When neither marker exists, the second
base traversal is skipped.

The base manifest remains the only lower-side input. Content comparison and
line statistics keep their existing changed-path access, and no lowerdir walk or
symlink dereference was added. Manifest ordering and JSONL format are unchanged.

### Diff semantics checked

- an old-schema JSONL row without hardlink or xattr fields still participates in
  a modified-file diff;
- a schema-invalid unrelated row still returns a state-corruption error;
- both xattr and real `0/0` character-device whiteouts produce deletions;
- whole-directory whiteouts, opaque replacement, and a later-layer whiteout and
  recreation preserve the net diff;
- file-to-directory and directory-to-file changes are classified as type
  changes, and similar prefixes such as `prefix` and `prefix-extra` remain
  distinct;
- xattr-only modifications, line counts, path sorting, exclusions, commit,
  rollback, and journal recovery retain their existing tests.

A real two-step session created by the preserved first-run baseline was then
read by both binaries. Their `diff --json` change arrays were identical, and the
new binary's `commit --dry-run` reported 11 changes without modifying the host.
The combined output is
`/home/bangshou/cowbox-bench-results/cowbox-m2-diff-old-session-e2e.json`
(SHA-256 `7b942d033edbb1930a831cc87a0b1bf9059624d3631c80415d69146c77417013`).

## First-run diagnostic profile

The baseline instrumented profile remains reusable because it describes the
released baseline, not the defective candidate. Its binary SHA-256 was
`9f4af80a8567ad2f6d12e028153d791e8b0d700c16c63b1be57cb48f29ba929b`;
the raw profile JSON under `$HOME/cowbox-bench-results/` had SHA-256
`61821bb5fd5a9d3e239cb6af241ce8ae31b670933b860824f3bf6c39e6687390`.

Baseline manifest phases at 200,000 files:

| Phase | Median |
| --- | ---: |
| Directory walk | 51.67 ms |
| No-follow metadata | 171.71 ms |
| Symlink targets | 2.52 ms |
| User-xattr query and digest | 114.85 ms |
| Global sort | 90.22 ms |
| JSONL serialization and write | 39.75 ms |
| Total manifest capture | 499.19 ms |

The earlier candidate profile remains in
`$HOME/cowbox-bench-results/cowbox-m2-candidate-profile-v3.json`, but it was
produced by the superseded incomplete traversal and is invalid for performance claims. It is not used to characterize
the final binary.

## Criterion verification

### First-run round

The first-run candidate was checked with the repository Criterion benchmark at
`COWBOX_BENCH_FILES=10000`. These values are the reported
lower/estimate/upper interval from that verification run:

| Benchmark | Samples | Measured |
| --- | ---: | --- |
| `diff_scan/10000_files_10_changes` | 20 | 13.442 / **13.581** / 13.680 ms |
| `rollback_dag/100_steps` | 20 | 10.512 / **10.597** / 10.726 ns |
| `run_true/10000_files_end_to_end` | 10 | 49.114 / **52.488** / 55.761 ms |

`run_true` executes warm subsequent sessions after an untimed warm-up; it does
not measure first-run manifest capture. Criterion found a statistically
significant improvement in `diff_scan` and `rollback_dag`; it reported no
significant change for `run_true`, which had three mild outliers in ten samples.
No Criterion threshold is enforced.

### Diff round

The final diff candidate was checked with the same command and 20 samples for
the scan benchmark:

| Benchmark | Samples | Measured |
| --- | ---: | --- |
| `diff_scan/10000_files_10_changes` | 20 | 4.039 / **4.147** / 4.252 ms |
| `rollback_dag/100_steps` | 20 | 10.683 / **10.854** / 11.001 ns |
| `run_true/10000_files_end_to_end` | 10 | 40.504 / **43.427** / 46.414 ms |

Criterion reported a 68.23% `diff_scan` improvement from the previous stored
sample, no change for `rollback_dag`, and a 17.26% `run_true` improvement. The
run benchmark executes warm subsequent sessions after an untimed warm-up.

## Semantic checks

The complete test suite passed, including:

- a 40-branch wide tree with deep descendants, proving that capture does not
  stop when the frontier first reaches worker-count scale;
- global ordering equality between the returned `Vec<ManifestEntry>` and the
  persisted JSONL;
- directory symlink no-follow behavior and explicit regression fixtures for
  non-empty user xattrs and hardlinks;
- a real first CLI run that changes a pre-existing deep file, followed by a
  `diff` classified as `modified` from the captured base entry;
- xattr-only diff detection and host-drift commit refusal;
- commit recovery, VCS exclusion, whiteout handling, hardlink restoration, and
  related host-drift protections.

The required commands passed:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
COWBOX_REQUIRE_SANDBOX=1 cargo test -p cowbox-cli --test adversarial_suite -- --test-threads=1
bash scripts/smoke.sh
```

## How to run

```bash
export CARGO_TARGET_DIR="$HOME/.cache/cowbox-target"
cd /mnt/e/project/cowbox
stamp="$(date -u +%Y%m%dT%H%M%SZ)"

bash scripts/bench-m2.sh \
  --samples 20 \
  --warmups 1 \
  --file-counts 10000 200000 \
  --depths 1 \
  --json-output "$HOME/cowbox-bench-results/cowbox-m2-diff-rerun-$stamp.json" \
  --markdown-output "$HOME/cowbox-bench-results/cowbox-m2-diff-rerun-$stamp.md"
```

For a slower depth-scaling run, pass `--depths 1 60 149`. `scripts/bench-m2.sh`
builds the release CLI once, rejects a 9p target directory, and passes that
binary to the harness. The harness requires an ext4 benchmark root and fails if
`doctor --json` is not ready. A missing CLI, failed sandbox preflight, invalid
output, or unexpected step count fails the run instead of producing a partial
result.

Use `--root /tmp/cowbox-m2-run --keep` to retain fixtures and states for
inspection. Keep output files outside the disposable benchmark root.

The Criterion microbenchmark remains available:

```bash
COWBOX_BENCH_FILES=10000 scripts/bench.sh --sample-size 20
```
