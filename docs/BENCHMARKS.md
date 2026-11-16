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

### Paired first-run A/B

Pass `--baseline-binary` to compare first runs with an explicit baseline. The
paired mode measures only `run` at step 1 and rejects `--depths`. Both binaries
use the same fixture for each file count, the same measured sample count, and
one warm-up pair. Odd pairs run baseline then candidate, while even pairs run
candidate then baseline. Each command receives a fresh isolated state root.

```bash
python3 scripts/bench_m2.py \
  --binary /path/to/candidate-cowbox \
  --baseline-binary /path/to/baseline-cowbox \
  --file-counts 10000 200000 \
  --samples 20 --warmups 1 \
  --json-output /absolute/results/cowbox-m2-paired-first-run-YYYYMMDDTHHMMSSZ.json \
  --markdown-output /absolute/results/cowbox-m2-paired-first-run-YYYYMMDDTHHMMSSZ.md
```

Use a new timestamped output prefix for every run so earlier raw samples remain
intact. JSON records both absolute binary paths and SHA-256 values, every pair's
order and timings, machine load, and filesystem details. Pair samples retain full
`perf_counter` precision internally. Summaries, P95 values, and paired deltas use
those unrounded values, while per-pair display values are rounded to three
decimal places during serialization.

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

## Paired first-run A/B result

One serial paired run compared the first-run experiment candidate at source
commit `5c133ea9c6a4ded777161099ed0c2aaa222f909d` with the diff candidate. Each
file count used one shared ext4 fixture, 20 formal pairs, and one excluded
warm-up pair. Odd pairs ran baseline then candidate; even pairs ran candidate
then baseline. Every formal command ran in a fresh state and returned step 1.
No sample was removed.

| Binary | Path | SHA-256 |
| --- | --- | --- |
| Diff baseline | `/home/bangshou/cowbox-bench-results/m2-diff-final/bin/cowbox-m2-diff-152fd440` | `152fd4407df4047a2765a37c4daa9d6372ac9cb64b643d9f6a71a109c342d451` |
| First-run candidate | `/home/bangshou/cowbox-bench-results/m2-firstrun-final/bin/cowbox-firstrun-final` | `f959b263c1e1c15567fb5a09a71864ecb39584ffc1f9aefa8cb150f793fd1b91` |

Cells contain median (P95) in milliseconds. The paired delta is candidate minus
baseline for each pair.

| Files | Baseline median | Candidate median | Paired delta median | Baseline P95 | Candidate P95 | Paired delta P95 |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 10,000 | 59.567 | **63.074** | -1.262 | 81.352 | 76.774 | 16.449 |
| 200,000 | 164.506 | **138.700** | -26.219 | 432.140 | 144.551 | -15.888 |

The 10,000-file candidate median was 3.074 ms above the 60 ms target, while
its P95 was below 80 ms. The 200,000-file candidate passed the 150 ms median
and 200 ms P95 targets at 138.700 and 144.551 ms. Two baseline 200,000-file
samples produced large tail values, while the candidate remained between
129.040 and 145.102 ms. This is one run and is retained as observed.

The run started at load average `0.222, 0.475, 0.719` and ended at
`0.327, 0.485, 0.719` on the 8-vCPU WSL profile above. Artifacts use the new
timestamp prefix `cowbox-m2-paired-first-run-20261115T203256Z`:

| Artifact | SHA-256 |
| --- | --- |
| Raw JSON | `34d1630a86c96af712ea5d4777ccd524304e9c3de0205f057b2183ebc6ea18b6` |
| Markdown | `0bea0fc91a5c56423847456788683f78fc32d9f4278eec12bbd87f3ec86661fe` |
| Run log | `fe73ad7837ec4b44a4ed55eb74ccc88f473501eedf25c6d5c3f9cdbc015cd593` |
| Verification | `8bef72084b0f40e2be087a7e1713d0ef21093e4305b4b2deac223faa5c928291` |
| Precision check | `b7a32173a57852e29e8cef69a6bb7e588d5e62cba03be57e16e5730f0463ab2a` |
| Compatibility check | `9b1824528f0bb498633fd77d02aeebe14de519d3a51cbd19ec0409d8deedafdc` |

A separate compatibility check rendered an existing raw result with the
pre-change and final default-mode Markdown paths and compared them byte for
byte. It also confirmed default depths `1 60 149`, 20 samples, one warm-up, and
an explicit exit code 1 for a non-executable baseline.

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

## Doctor diagnostics verification

This round changes only capability probing and diagnostic state handling. It
does not change namespace, OverlayFS, Landlock enforcement, or command
execution. Verification used WSL2 kernel `6.6.87.2-microsoft-standard-WSL2`
and ext4 on `/dev/sde`.

Landlock detection now sets `CompatLevel::HardRequirement`, requests the file
access set for ABI 1, 2, or 3, and creates then immediately drops a temporary
ruleset file descriptor without calling `restrict_self`. ABI 2 adds `Refer`
and ABI 3 adds `Truncate` in the landlock 0.4.7 access model. The existing
`landlock_abi` field therefore means the highest ABI in the V1-V3 probe range
that can create a ruleset. A newer kernel is still reported as ABI 3. `doctor`
requires ABI 3 or newer for readiness.

The `doctor` command walks the configured state path without creating it.
Existing symlinks are canonicalized before the workspace overlap check; a
dangling or cyclic symlink returns a structured `STATE_ROOT` error. Relative
paths and `..` components are resolved even when an intermediate directory does
not exist. Observed filesystem and path behavior:

| Scenario | Result |
| --- | --- |
| Missing state path below ext4 | `ready=true`, `state_fs=ext4`, ABI 3; state path not created |
| Missing relative path containing `..` | resolves the prospective path; no missing directory created |
| Resolvable state symlink | canonical target is used; ready when the target passes checks |
| Dangling state symlink | structured `STATE_ROOT` error, exit 2; target not created |
| State symlink into workspace | structured `STATE_ROOT` error, exit 2; no state layout created |
| 9p workspace with ext4 state | `ready=true`, `state_fs=ext4`; state path not created |
| Dedicated 9p state path | `ready=false`, exit 1, actionable xattr-capable `COWBOX_HOME` note; state path not created |
| State path inside workspace | structured `STATE_ROOT` error, exit 2; state path not created |
| Synthetic Landlock capability `None`, ABI 1, ABI 2 | not ready |
| Synthetic Landlock capability ABI 3 | ready when other capabilities pass |

The complete verification exited 0:

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | passed |
| workspace Clippy with `-D warnings` | passed |
| `cargo test --workspace` | 92 passed |
| forced-sandbox `adversarial_suite` | 26 passed, 0 ignored |
| `bash scripts/smoke.sh` | passed |

Criterion was run for the initial engine-probe round, before the follow-up
path-resolver correction. That correction only affects `doctor` path
resolution and is not exercised by benchmark targets, so no performance
sampling was repeated while the exec-step investigation was active. Values are
the reported lower / estimate / upper interval:

| Benchmark | Samples | Measured |
| --- | ---: | --- |
| `diff_scan/10000_files_10_changes` | 20 | 3.8420 / **3.8597** / 3.8801 ms |
| `rollback_dag/100_steps` | 20 | 10.382 / **10.482** / 10.594 ns |
| `run_true/10000_files_end_to_end` | 10 | 51.180 / **55.917** / 60.895 ms |

Criterion reported no statistically significant change for `diff_scan`,
`rollback_dag`, or `run_true`. The `run_true` interval showed host variance and
is reported without a regression or improvement claim. The final fixed release
binary used for the path scenarios had SHA-256
`736367338b2003b9b7f4dcf32be1a8aa9ee2b5c2dfbf7bd21d384e91e5c14ced`.

## M2 execution-step integration round

This round uses `f86b72a` as the performance baseline. It integrates the
first-session manifest capture experiment from `5c133ea` and the supervisor
wait work preserved in
`/home/bangshou/cowbox-bench-results/m2-exec-step-20261115T205234Z/`.
The earlier experiment remains the historical source for two ideas:

- persist the first-session manifest from workers without handing every parsed
  entry back to the main thread;
- wait for command exit with a pidfd instead of a fixed 10 ms poll.

The production integration additionally keeps a polling fallback when pidfd
creation is unavailable, when poll fails, or when an event cannot confirm that
the direct child has exited. It checks the direct child again after a poll
timeout before deciding that the command timed out. The manifest path retains
complete tree coverage, global path ordering, no-follow symlink handling, xattr
digests, hardlink metadata, and JSONL compatibility. The archived `REPORT.md` and
`0001-perf-wait-for-command-exit-with-pidfd.patch` remain the context for the
earlier 46.60/156.65 ms and 46.59/124.07 ms candidate measurements; they are not
relabeled as this round's results.

The measured candidate source was commit
`1dd203a4c995bf667d3b0752be1520c56ba9d2b5` plus `source.patch` from the artifact
directory. Commit `bb3b150090bfdec47385394beb05567f442a60df` records that
source. A later pidfd boundary fix changes only poll-error and non-exit-event
fallback handling; it leaves the measured normal-exit path unchanged. Raw
results, logs, binaries, and the exact patch are retained in
`/home/bangshou/cowbox-bench-results/m2-final-20261116T010646Z/`.
The measured implementation commits are `c7e4b83` for Windows doctor
forwarding, `1dd203a` for the supervisor wait, and `bb3b150` for the manifest
integration and final measurements.

| Artifact | SHA-256 |
| --- | --- |
| Baseline `f86b72a` binary | `90b8f2e8635b967fc2c7af15ae834ded0550cd0ca2804b76c19511710b7ea06b` |
| Final candidate binary | `38cafdb1acb082bf209463e5b2b4ca3c68a97cda0306446ebf5791b831f89955` |
| Measured source patch | `1a7b39a5c00ed11003c47a06485d132f3808c82b5ee7820136842b7e811ab30c` |
| Formal paired A/B JSON | `c060101e76bb37ad07851f0bd55cf345d4031d9d1ae82da1ada1aa59bb0e5647` |
| Full harness JSON | `b05559aa87eb4bedfe2920def158a63d9c977f6f92e5513d949b1088c401d97f` |
| Excluded first paired A/B JSON | `010d5a421805576d1c637e5a63c5e3620cd300019f46a33a76e88c456f808a4d` |

### Measurement protocol

Both binaries were built from separate ext4 target directories with the
workspace release profile. Each file count used one shared fixture generated
under ext4, 20 measured samples, one excluded warm-up, and alternating
baseline/candidate order. The full harness used the same final binary, fixture
generator, sample count, and warm-up count, with `--depths 1`. No sample was
removed or winsorized.

### Paired first-run A/B

Cells are median / P95 in milliseconds. The paired delta is candidate minus
baseline.

| First run | Baseline | Final candidate | Paired delta |
| --- | ---: | ---: | ---: |
| 10,000 files | 49.44 / 62.17 ms | **37.65 / 50.43 ms** | -10.21 / -2.78 ms |
| 200,000 files | 168.67 / 194.73 ms | **135.57 / 170.45 ms** | -36.61 / -20.93 ms |

The formal run started at load average `1.047, 1.071, 0.540` and ended at
`1.107, 1.083, 0.556`. Both first-run median and P95 targets pass:
10,000 files at 60/80 ms and 200,000 files at 150/200 ms.

An earlier paired run was excluded after the WSL storage layer restarted during
the measurement window. Its load average started at `2.696, 1.238, 0.541`;
both sides slowed by roughly the same factor, the candidate measured
187.72/257.00 ms at 200,000 files, and the benchmark's temporary metadata path
was cleared. `logs/wsl-dmesg-after-paired.txt` records p9io cancellation and
ext4 unmount/remount events surrounding that window. The excluded JSON remains
available for audit and is not used in the table above.

### Full harness

| Scenario | 10,000 files | 200,000 files |
| --- | ---: | ---: |
| First run | **37.02 / 43.65 ms** | **143.08 / 169.98 ms** |
| Subsequent run, 1 layer | **30.22 / 32.98 ms** | **27.69 / 32.01 ms** |
| 10-file diff | **7.96 / 12.78 ms** | **105.29 / 113.25 ms** |

The fixed one-parent step measured 28.38 / 31.86 ms after a 36.48 ms untimed
preparation step. The layer-limit probe accepted a step from 150 active layers
in 41.96 ms, then rejected the next invocation from 151 layers with exit 2 after
2.30 ms; head remained 151 and step 152 was not created.

The 10-file diff targets pass at both sizes (10,000 files <=20 ms and 200,000
files <=200 ms). The full harness started at load average
`0.937, 1.046, 0.549` and ended at `1.267, 1.121, 0.591`.

### Verification

The final tree passed:

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | passed |
| workspace Clippy with `-D warnings` | passed |
| `cargo test --workspace` | 97 passed |
| forced-sandbox `adversarial_suite` | 26 passed, 0 ignored |
| `bash scripts/smoke.sh` | passed: init -> run -> diff -> rollback -> commit --dry-run |

The relevant Criterion run completed with these lower / estimate / upper
intervals:

| Benchmark | Samples | Measured |
| --- | ---: | --- |
| `diff_scan/10000_files_10_changes` | 20 | 4.3558 / **4.4126** / 4.4643 ms |
| `rollback_dag/100_steps` | 20 | 10.478 / **10.627** / 10.875 ns |
| `run_true/10000_files_end_to_end` | 10 | 28.952 / **30.334** / 31.727 ms |

Criterion reported a 10.22% increase for `diff_scan` and a 6.44% increase for
`rollback_dag` against its previously stored samples, and a 26.72% improvement
for `run_true`. Those comparisons are across separate Criterion sessions rather
than a fresh `f86b72a` Criterion run. The CLI harness measured the 10-file diff
inside the target and did not reproduce a diff regression in this round.

The Windows release launcher also passed a separate build and real
`cowbox.exe -> wsl.exe -> cowbox-final` validation. Its SHA-256 was
`462459d361b47005d316fba5b7e83f4012befe5f6e6125c8cb9b9636d3d505de`.
The ready capability path exited 0. A 9p `COWBOX_HOME` path exited 1 while
printing every failed check and the migration note, and `doctor --json` retained
`ok=true`, `ready=false`, `state_fs=9p`, and the inspected `state_root`. A
missing WSL distribution produced `WSL_START_FAILED` with exit 1 instead of a
capability diagnostic.

## M2 long-session and cache regression round

This round used the published `v0.4.0-m2` Linux binary
`2a8b38b3f7aab1100fd6f1ada8a0ebd4c62236522c918ce868cf8f8d2b7caa29`
as the fixed baseline and the source at
`ce285ad613fa82e98237868074107b3b3e293f58` as the candidate. The candidate
binary SHA-256 was
`21e05c96bba3f3df027382e8e55045640a02817ebde95e0d6a1b903ad43b19db`.

All measurements used the same WSL2 instance, ext4 benchmark/state filesystems,
release profile, serial execution, 20 samples, and one excluded warm-up per
scenario. No sample was removed or winsorized.

The full-harness baseline run started at load average
`0.841, 0.323, 0.127` and ended at `1.121, 0.438, 0.172`; the candidate run
started at `1.121, 0.438, 0.172` and ended at `1.458, 0.596, 0.236`. The paired
run started at `1.134, 0.566, 0.231` and ended at `1.671, 0.705, 0.282`.
Machine details are unchanged from the profile above.

### Full harness

Cells are median / P95 milliseconds.

| Scenario | Files | `v0.4.0-m2` baseline | Candidate |
| --- | ---: | ---: | ---: |
| First run | 10,000 | 35.58 / 39.54 | **40.40 / 46.98** |
| One parent layer | 10,000 | 30.98 / 36.51 | **30.81 / 35.28** |
| 10-file diff | 10,000 | 13.38 / 17.22 | **14.17 / 17.00** |
| First run | 200,000 | 126.61 / 160.61 | **119.05 / 130.97** |
| One parent layer | 200,000 | 28.58 / 52.26 | **28.60 / 31.53** |
| 10-file diff | 200,000 | 93.25 / 103.26 | **91.69 / 103.25** |

The fixed one-parent candidate step measured 31.91 / 41.65 ms after a 40.22 ms
untimed preparation step. The two first-run limits pass:
10,000 files at 40.40 / 46.98 ms against 60 / 80 ms, and 200,000 files at
119.05 / 130.97 ms against 150 / 200 ms. The 10-file diff also passes:
14.17 ms at 10,000 files against 20 ms, and 91.69 ms at 200,000 files against
200 ms.

### Paired first-run A/B

The paired run alternated baseline/candidate order, used a shared fixture per
file count, 20 measured pairs, and one warm-up pair. Paired delta is candidate
minus baseline.

| Files | Baseline median / P95 | Candidate median / P95 | Paired delta |
| ---: | ---: | ---: | ---: |
| 10,000 | 42.52 / 47.01 ms | **42.42 / 44.99 ms** | -0.91 / +3.81 ms |
| 200,000 | 125.83 / 140.72 ms | **128.51 / 135.40 ms** | +1.99 / +11.17 ms |

The deltas are small relative to host variance and remain well inside the
required limits.

### Long session and compaction boundary

The candidate completed 300 measured steps with a 100-file fixture. Baseline
completed 151 steps, then rejected step 152 with `CONFIG_ERROR`; its rejected
CLI invocation took 1.58 ms.

| Session segment | Median | P95 |
| --- | ---: | ---: |
| Candidate all 300 steps | 41.83 ms | 65.52 ms |
| Candidate steps 1-151 | 39.36 ms | 66.30 ms |
| Candidate steps 152-300 | 45.47 ms | 64.41 ms |

Compaction ran at steps 151 and 268:

| Compaction | Step elapsed | Physical layers after | Stored compact layers | Reported compact bytes |
| ---: | ---: | ---: | ---: | ---: |
| 151 | 65.22 ms | 35 | 1 | 4,096 |
| 268 | 50.79 ms | 35 | 2 | 8,192 |

Disk checkpoints include both logical step directories and compacted artifacts:

| Step | Logical disk bytes | Allocated bytes | Mounted layers | Compact layers |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 1,577 | 81,920 | 2 | 0 |
| 150 | 568,035 | 4,030,464 | 151 | 0 |
| 151 | 571,800 | 4,079,616 | 35 | 1 |
| 200 | 737,763 | 5,308,416 | 84 | 1 |
| 267 | 1,085,158 | 7,241,728 | 151 | 1 |
| 268 | 1,089,976 | 7,278,592 | 35 | 2 |
| 300 | 1,189,928 | 8,077,312 | 67 | 2 |

From step 151 to 300, logical session storage grew by 618,128 bytes and
allocated storage by 3,997,696 bytes. The two compacted layers account for
8,192 reported bytes; the retained logical step directories remain available
for diff, commit, rollback, and branching.

### Criterion

The required Criterion targets were rerun with
`COWBOX_BENCH_FILES=10000` and 20 samples. Values are lower / estimate / upper.

| Benchmark | Measured |
| --- | --- |
| `diff_scan/10000_files_10_changes` | 3.9979 / **4.1281** / 4.2240 ms |
| `rollback_dag/100_steps` | 10.260 / **10.456** / 10.763 ns |
| `run_true/10000_files_end_to_end` | 33.449 / **35.739** / 38.016 ms |

`diff_scan` reported no change (`p=0.25`). `rollback_dag` reported a 3.15%
change, which Criterion classified as within its noise threshold. `run_true`
reported no change against the previous stored run (`p=0.77`). The 20-sample
CLI harness above remains the authoritative end-to-end comparison.

Raw artifacts are in
`/home/bangshou/cowbox-bench-results/m2-final2-20261116T031019Z/`:
`baseline.json`, `candidate.json`, `paired-20261116T0313Z.json`, and
`criterion.log`.

| Artifact | SHA-256 |
| --- | --- |
| `baseline.json` | `b08d80918e3bcefbd47682c414db065875ede58f45f82d396d3ddbca286e48f6` |
| `candidate.json` | `2b66119a14362e2eab994467de04b24a89edc3cd7dd364780645bcb317216f36` |
| `paired-20261116T0313Z.json` | `85b7e88c021d476a0d0dcaaedd21ce5630575d63f626978c5cf6db8ccc25f262` |
| `criterion.log` | `cac68eb8cacd5a4b771895ad37dbb4e2aeaf2d1bb58d124c7915c48098ee37ed` |

## M2 review-hardening round

The fifth review round used the same published Linux binary
`2a8b38b3f7aab1100fd6f1ada8a0ebd4c62236522c918ce868cf8f8d2b7caa29`
and candidate commit `06b82d4c21579deb52a204c11c519e8aa21ee199`. The rebuilt
candidate binary SHA-256 was
`3b1df3bea053a9b4fd3ac04f733fe34e2abcec4990388f232cd7aef677ed0e98`.

This round fixed directory-to-file replacement, read-only xattr copying,
read-only temporary-tree recovery, quadratic compaction scans, and the
benchmark's acceptance of a candidate exiting after the legacy layer limit.

The baseline run started at load average `0.140, 0.337, 0.205` and ended at
`1.297, 0.627, 0.309`; the candidate run started at
`1.297, 0.627, 0.309` and ended at `1.347, 0.724, 0.356`. The paired run
started at `1.048, 0.688, 0.351` and ended at `1.678, 0.837, 0.405`.

### Full harness

Cells are median / P95 milliseconds. The baseline run used
`--allow-legacy-layer-limit`; the candidate run did not.

| Scenario | Files | `v0.4.0-m2` baseline | Candidate |
| --- | ---: | ---: | ---: |
| First run | 10,000 | 32.29 / 35.97 | **34.95 / 39.40** |
| One parent layer | 10,000 | 24.41 / 29.50 | **29.68 / 35.55** |
| 10-file diff | 10,000 | 9.38 / 13.71 | **5.88 / 10.78** |
| First run | 200,000 | 122.48 / 275.97 | **130.59 / 143.40** |
| One parent layer | 200,000 | 27.32 / 35.53 | **28.65 / 31.33** |
| 10-file diff | 200,000 | 87.93 / 99.65 | **89.25 / 93.31** |

The candidate first-run targets pass at 34.95 / 39.40 ms and
130.59 / 143.40 ms. The diff medians pass at 5.88 ms and 89.25 ms. The
baseline's 200,000-file P95 contained a large host-noise outlier and is retained
unchanged rather than trimmed.

### Paired first-run A/B

| Files | Baseline median / P95 | Candidate median / P95 | Paired delta |
| ---: | ---: | ---: | ---: |
| 10,000 | 32.64 / 35.74 ms | **32.38 / 46.91 ms** | +0.32 / +7.36 ms |
| 200,000 | 167.64 / 180.87 ms | **165.74 / 178.36 ms** | +0.40 / +12.88 ms |

The paired run shared a higher system load than the full harness. The paired
deltas remain small, while the full-harness absolute targets are the release
gate.

### Long session and scale

The candidate completed all 300 steps without being allowed to stop at the
legacy limit:

| Session segment | Median | P95 |
| --- | ---: | ---: |
| Candidate all 300 steps | 36.97 ms | 47.88 ms |
| Candidate steps 1-151 | 34.66 ms | 49.51 ms |
| Candidate steps 152-300 | 38.60 ms | 47.17 ms |

| Compaction | Step elapsed | Physical layers after | Stored compact layers | Reported compact bytes |
| ---: | ---: | ---: | ---: | ---: |
| 151 | 42.67 ms | 35 | 1 | 4,096 |
| 268 | 46.05 ms | 35 | 2 | 8,192 |

The baseline with the explicit legacy flag completed step 151 and rejected step
152 with `CONFIG_ERROR` after 1.45 ms. Without the flag, the harness treats that
stop as a failure.

A separate real-change run completed 300 steps with directory replacement,
read-only file/directory xattrs, deletion, opaque recreation, symlink update,
type changes, and hardlinks. It produced 18 diff entries and two compactions;
the diff was identical before and after compaction, rollback before the
compacted prefix and branching preserved the same changes, and host drift was
refused. The post-compaction sandbox view also passed explicit content, mode,
xattr, and hardlink checks.

For the many-entry scaling check, a 10,000-file upper layer was prepared through
150 steps and the state was copied before compaction. The pre-fix candidate
step took 17,544.558 ms; the fixed candidate took 428.554 ms. This is a single
diagnostic run, not a percentile study, but it confirms that whole-map scans
were removed from the hot path.

### Criterion

The first Criterion pass ran under visible host load. A lower-load rerun
recorded:

| Benchmark | Measured |
| --- | --- |
| `diff_scan/10000_files_10_changes` | 4.0189 / **4.1259** / 4.2941 ms |
| `rollback_dag/100_steps` | 11.710 / **12.005** / 12.383 ns |
| `run_true/10000_files_end_to_end` | 35.949 / **39.453** / 43.916 ms |

`run_true` reported no statistically significant change (`p=0.18`) with a wide
interval. Diff and DAG code paths were not modified in this round; the
20-sample CLI harness and the compaction scaling check remain the primary
evidence.

Raw artifacts are in
`/home/bangshou/cowbox-bench-results/m2-review-20261116T035232Z/`.

| Artifact | SHA-256 |
| --- | --- |
| `baseline.json` | `2e89fa3973d5c7742903ba7f6c8ebb9a0c017ba93e3428edf2b40daa45fd8543` |
| `candidate.json` | `a60cd033355d3a21ff786624cf9ea42f7013ebe496a4f56b9bfd3f0225ba9eb0` |
| `paired-20261116T0355Z.json` | `9646010dad6be873f272cf6083139a5dea6997f9aa17769c49207cca710bbaec` |
| `criterion.log` | `da601be23389573068631dfdd2731dfea4a20f673f5f437f5fff747f79cd9880` |
| `criterion-rerun.log` | `d73997c35fb2b652c92ffe1b97f239f5d135fabbb25b0519b50f26eb71281553` |

### Unreadable-file hardening

Candidate commit `62e08524cf6c71aef101eef6549a8cfd6827e26e` added durable
permission journaling for `000` files and directories. Its rebuilt Linux binary
SHA-256 was
`a773bc1dda8cc7117bfccd225963f2db9e58efc088837b7037088505a59360b2`.

A dedicated real-CLI session created a `000` file with a `user.*` xattr in step
1 and then ran to step 300. Compaction occurred at steps 151 and 268. The final
session had 301 logical steps, 68 mounted layers, two compacted layers, and
16,384 reported compact bytes. Both compiled layers and the original step upper
retained mode `000`; execution inside the sandbox verified the original content
and xattr. The permission-restoration journal was also exercised after an
interrupted compaction and restored the source mode before retry.

The 10,000-entry compaction scaling check still completed in 489.094 ms in the
fixed build, versus the pre-range-scan measurement of 17,544.558 ms.

Final Criterion values from the lower-load rerun:

| Benchmark | Measured |
| --- | --- |
| `diff_scan/10000_files_10_changes` | 3.9753 / **4.0430** / 4.1302 ms |
| `rollback_dag/100_steps` | 11.061 / **11.184** / 11.363 ns |
| `run_true/10000_files_end_to_end` | 28.602 / **33.001** / 38.586 ms |

`run_true` reported no statistically significant change (`p=0.89`).

| Artifact | SHA-256 |
| --- | --- |
| Candidate binary | `a773bc1dda8cc7117bfccd225963f2db9e58efc088837b7037088505a59360b2` |
| `criterion.log` | `4506d2cc7fd824ccd49d733e518cc7287dedde33a6cdec64dd1cd0f3d9f921dc` |
| `criterion-rerun.log` | `93629366a87c138a234344b69cb7007d5495152b018840602fb75ea933602d29` |

## M2 permission-transaction hardening

This round measured the release binary built from the working tree based on
`ab14bccc4986a71c72475f20b0988b1dab255ffa`. The binary SHA-256 was
`94e6b077eed2a5b666855736a90c9179d204b80c8510694f0ce2fbcdd0e4a53d`.
All timings used the same WSL2 instance, ext4 filesystem, release profile,
serial execution, 20 measured samples, and one excluded warm-up per scenario.
No sample was removed or winsorized.

### Full harness

Cells are median / P95 milliseconds.

| Scenario | Files | Release candidate |
| --- | ---: | ---: |
| First run | 10,000 | **31.91 / 44.82** |
| One parent layer | 10,000 | **28.50 / 31.53** |
| 10-file diff | 10,000 | **10.81 / 17.89** |
| First run | 200,000 | **120.47 / 134.63** |
| One parent layer | 200,000 | **26.24 / 29.82** |
| 10-file diff | 200,000 | **88.40 / 99.21** |

The fixed one-parent step measured 30.07 / 33.17 ms after a 37.74 ms untimed
preparation step.

### Populated compaction

A fresh session created 10,000 entries in step 1, then advanced through 149
empty steps. Step 151 triggered the first compaction. Preparation, including
all 150 previous steps, took 6,609.68 ms and was excluded from the compaction
measurement.

| Metric | Value |
| --- | ---: |
| Upper entries | 10,000 |
| Compaction step 151 | **471.62 ms** |
| Mounted layers after compaction | 35 |
| Compacted layers | 1 |
| Reported compact bytes | 41,373,696 |

### Real-change acceptance

`scripts/acceptance_m2.py` ran a 300-step real-change session with a debug
binary so deterministic crash injection was available. The fixture covered
whiteouts, an opaque directory, directory-to-file and file-to-directory
changes, a symlink update, session hardlinks, `user.*` xattrs, `000` files and
directories, rollback to step 100, branching from the old snapshot, host-drift
refusal, a real commit, and commit recovery.

| Check | Observed |
| --- | --- |
| Steps | 300 |
| Diff entries before and after two compactions | 16, identical |
| Compactions | Step 151 and step 268, 35 mounted layers after each |
| Final layout | 67 mounted layers, 2 compacted layers, 114,688 reported bytes |
| Rollback / branch | head 100 / next DAG step 301 |
| Host drift | `COMMIT_CONFLICT` |
| Commit | 16 changes applied |
| Commit recovery | `restore_host`, 1 path restored |

The acceptance JSON SHA-256 was
`3def5fa92557937109428b013c2c4a00c1bd39340379ed4519471a4decc61717`.

### Criterion

The final Criterion pass recorded:

| Benchmark | Lower / estimate / upper |
| --- | --- |
| `diff_scan/10000_files_10_changes` | 3.8025 / **3.8260** / 3.8651 ms |
| `rollback_dag/100_steps` | 10.393 / **10.676** / 10.899 ns |
| `run_true/10000_files_end_to_end` | 29.296 / **32.225** / 35.258 ms |

`diff_scan` reported a 7.18% change against the previous stored sample. The
reader permission scan now covers the upper layers before diffing; the
end-to-end CLI diff still measured 10.81 / 17.89 ms at 10,000 files.
`rollback_dag` was within Criterion's noise threshold, and `run_true` improved
by 22.24%. The CLI harness remains the authoritative end-to-end comparison.

### Verification

The final tree passed:

| Check | Result |
| --- | --- |
| `cargo fmt --check` | passed |
| workspace Clippy with `-D warnings` | passed |
| `cargo test --workspace` | 119 passed |
| forced-sandbox `adversarial_suite` | 34 passed, 0 failed, sequentially |
| `bash scripts/smoke.sh` | passed |

### Artifacts

| Artifact | SHA-256 |
| --- | --- |
| Release binary | `94e6b077eed2a5b666855736a90c9179d204b80c8510694f0ce2fbcdd0e4a53d` |
| `bench.json` | `875da2f489f1261a46a7c876c5fba5e8dc150f29f61bf56ce087a9d9a2fb876a` |
| `bench.md` | `75edb5e04386cf06bfae9589961336ab50d8ea48bcffbdf68720f064b9d9ad29` |
| `bench.log` | `0dfc117d059ede8e92bdff76e5b1e47db3a1e4de1cb042a898490dbd0a21c81f` |
| Criterion final log | `fbe7b7f29fd69eb98306c97ea5e03ed2942aeea79fa5c09ea1e98e78d9d10c2a` |
| Acceptance JSON | `3def5fa92557937109428b013c2c4a00c1bd39340379ed4519471a4decc61717` |

Raw files are under
`/home/bangshou/cowbox-bench-results/m2-permission-final-20261116/`.

## M2 commit-recovery hardening

This round starts from `c168afb5c2465311db833e23a0376214a10b3506` and fixes
commit recovery after an interrupted apply. The release binary SHA-256 was
`5ef971717022998a55c2c30c5fac72a014c0529fdf7a544aa321c7f3822aefe0`.
All measurements used the same WSL2/ext4 profile, release settings, 20 samples,
and one excluded warm-up per scenario.

### Full harness

Cells are median / P95 milliseconds.

| Scenario | Files | Release candidate |
| --- | ---: | ---: |
| First run | 10,000 | **34.90 / 110.40** |
| One parent layer | 10,000 | **24.22 / 29.49** |
| 10-file diff | 10,000 | **10.59 / 14.66** |
| First run | 200,000 | **121.26 / 136.85** |
| One parent layer | 200,000 | **25.03 / 29.63** |
| 10-file diff | 200,000 | **96.59 / 104.60** |

The 10,000-file first-run P95 contains a retained host-noise outlier; its
median remains inside the 60 ms limit. The fixed one-parent step measured
29.24 / 33.21 ms after a 37.00 ms untimed preparation step.

### Populated compaction

| Metric | Value |
| --- | ---: |
| Upper entries | 10,000 |
| Compaction step 151 | **461.58 ms** |
| Mounted layers after compaction | 35 |
| Compacted layers | 1 |
| Reported compact bytes | 41,373,696 |

### Commit-recovery acceptance

The 300-step acceptance now has two additional commit-recovery scenarios:

- a file-to-directory type change interrupted after the first host write leaves
  the commit's own `0700` directory; recovery accepts that intermediate state
  and restores the original file;
- after the same interruption, a user file is written inside the prepared
  directory; recovery refuses to delete it, reports the path, and retains the
  journal and backups. Removing the user file allows the same recovery command
  to finish and restore the original file.

The complete 300-step run still produced 16 identical diff entries before and
after compactions at steps 151 and 268, 35 mounted layers after each
compaction, rollback to head 100, branch step 301, `COMMIT_CONFLICT` on host
drift, an applied 16-change commit, and commit recovery with `restore_host`.

The updated acceptance JSON SHA-256 was
`f7a2bd78bb924ae4df9fbffad43460315c78d20b9ad298014e29987fedf5e62d`.

### Criterion

| Benchmark | Lower / estimate / upper |
| --- | --- |
| `diff_scan/10000_files_10_changes` | 3.8705 / **3.9379** / 3.9917 ms |
| `rollback_dag/100_steps` | 9.9934 / **10.038** / 10.091 ns |
| `run_true/10000_files_end_to_end` | 32.887 / **37.847** / 44.281 ms |

`diff_scan` reported no change (`p=0.26`), `rollback_dag` improved by 4.40%,
and `run_true` reported no statistically significant change (`p=0.12`).

### Verification

| Check | Result |
| --- | --- |
| `cargo fmt --check` | passed |
| workspace Clippy with `-D warnings` | passed |
| `cargo test --workspace` | 121 passed |
| forced-sandbox `adversarial_suite` | 36 passed, 0 failed, sequentially |
| `bash scripts/smoke.sh` | passed |

### Artifacts

| Artifact | SHA-256 |
| --- | --- |
| Release binary | `5ef971717022998a55c2c30c5fac72a014c0529fdf7a544aa321c7f3822aefe0` |
| `bench.json` | `9a416243b50791555614e052eb93c264f4df2ab7b673cdf3e202781e4a4abb03` |
| `bench.md` | `c8e1d43a6f15050c25b1f5c751d05acadfcd6050c535feeb5e63f859705f5467` |
| `bench.log` | `f398f31171dfa037b3663347a0cf28010be73dd9f6ad29005510f2b714a9f263` |
| Criterion log | `00153f1fa2a501df4d48b1d875d8668952f18cdbced507ed65ccf1829eee61ef` |
| Acceptance JSON | `f7a2bd78bb924ae4df9fbffad43460315c78d20b9ad298014e29987fedf5e62d` |

Raw files are under
`/home/bangshou/cowbox-bench-results/m2-commit-recovery-20261116/`.

### Commit rollback content verification

Criterion rerun on WSL2/ext4 with 10,000 files. `run_true` used the release
binary with SHA-256
`e991767788c3c0ca73ef390f8758ded544345491086dda3e352156eabe927796`.

| Benchmark | Lower / estimate / upper |
| --- | --- |
| `diff_scan/10000_files_10_changes` | 3.9625 / **3.9828** / 4.0034 ms |
| `rollback_dag/100_steps` | 10.612 / **10.704** / 10.829 ns |
| `run_true/10000_files_end_to_end` | 23.430 / **23.733** / 24.041 ms |

The workspace suite passed 122 tests. The forced-sandbox suite passed 37 tests
and verified that rollback preserves an unreadable file edited on the host and a
user file whose name resembles a commit temporary file. The 300-step CLI
acceptance passed with compaction at steps 151 and 268 and 16 diff entries.
The acceptance JSON and Criterion log are in
`/home/bangshou/cowbox-bench-results/m2-recovery-verify-1Oz2Nh/`.

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
  --long-session-steps 300 \
  --long-session-file-count 100 \
  --compaction-entries 10000 \
  --json-output "$HOME/cowbox-bench-results/cowbox-m2-diff-rerun-$stamp.json" \
  --markdown-output "$HOME/cowbox-bench-results/cowbox-m2-diff-rerun-$stamp.md"
```

For a slower depth-scaling run, pass `--depths 1 60 149`. `scripts/bench-m2.sh`
builds the release CLI once, rejects a 9p target directory, and passes that
binary to the harness. The harness requires an ext4 benchmark root and fails if
`doctor --json` is not ready. A missing CLI, failed sandbox preflight, invalid
output, or unexpected step count fails the run instead of producing a partial
result.

Baseline binaries that predate layer compaction must be run with
`--allow-legacy-layer-limit`. Candidate runs must omit that flag so an
unexpected exit after step 151 fails the benchmark.

Use `--root /tmp/cowbox-m2-run --keep` to retain fixtures and states for
inspection. Keep output files outside the disposable benchmark root.

The real-change acceptance uses a debug binary because it injects a commit
crash:

```bash
root="$(mktemp -d /tmp/cowbox-m2-acceptance-XXXXXX)"
python3 scripts/acceptance_m2.py \
  --binary "$CARGO_TARGET_DIR/debug/cowbox" \
  --root "$root" \
  --json-output "$HOME/cowbox-bench-results/m2-acceptance-$stamp.json"
```

The Criterion microbenchmark remains available:

```bash
COWBOX_BENCH_FILES=10000 scripts/bench.sh --sample-size 20
```
