# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [0.6.0-m3] - 2026-11-16

### Added

- `cowbox mcp-proxy -- <upstream MCP server>` for stdio tool servers. The proxy
  keeps the harness connection open, negotiates protocol versions, handles
  request IDs, paginated tool discovery, timeouts, abnormal exits, response
  limits, and client disconnects, and reserves stdout for JSON-RPC.
- One-off sandbox tool discovery plus one real cowbox step for every upstream
  `tools/call`. A sandboxed adapter starts a fresh upstream process, performs
  initialization and one call, and exits; cross-call state is stored in
  workspace files.
- Reserved `cowbox_diff`, `cowbox_rollback`, and `cowbox_commit` MCP tools.
  They expose read-only diff, head-only rollback, and the existing commit
  dry-run with a terminal command. Upstream name collisions are rejected.
- Snapshot metadata on every MCP result, including the bound session,
  `head_before`, actual `step`, `head_after`, timeout state, and rollback
  command.
- Persistent Windows launcher stdio forwarding for `cowbox.exe mcp-proxy`.
- MCP fixture integration coverage for normal calls, consecutive writes,
  rollback/retry, upstream errors and timeouts, client disconnect, pagination,
  name conflicts, response limits, and network policy `off`.

### Changed

- `doctor --check-run` now verifies the configured state path or nearest
  existing parent is writable and searchable, and reports the inspected path,
  filesystem, and access result. Plain static `doctor` still performs no write
  probe.
- Windows `doctor --json` preserves a valid Linux JSON envelope and exit code,
  and returns structured `WSL_START_FAILED` when the configured distro cannot
  start.
- The release workflow writes LF-only Windows checksum files without trailing
  whitespace and verifies every downloaded checksum before publishing.
- The cowbox skill documents MCP snapshot use, rollback-first recovery, and the
  terminal commit boundary.

### Fixed

- MCP control tools now pass the proxy's bound session to `diff`, `rollback`,
  and `commit --dry-run`; the CLI rejects a selected session that is no longer
  the active workspace session, so a pointer change cannot redirect a control
  operation into another session.
- An explicit `mcp-proxy --network host` is now forwarded to every sandbox
  step, so a project `[sandbox] network = "off"` cannot override the caller's
  explicit choice.

### Known limitations

- MCP upstream transport is stdio only; HTTP/SSE, resources, prompts, and a
  persistent upstream process are not implemented.
- The native Windows file editor path still bypasses the WSL engine.
- Cross-call upstream state must live in workspace files.

## [0.5.0-m2] - 2026-11-16

### Added

- `cowbox run --network host|off` and `[sandbox] network` with CLI, project,
  and default precedence. `off` creates a Linux network namespace after UID/GID
  mapping and before the user command; setup failure is structured and never
  falls back to the host network.
- Per-step policy snapshots. `run --json` reports the effective policy,
  `stats --json` reports the current DAG-head policy, rollback and branching
  preserve step history, and old sessions deserialize with an unknown/legacy
  policy.
- `cowbox doctor --check-run [--network off]`, which runs the real CLI/helper
  startup chain in a temporary fixture and distinguishes state-disk, user
  namespace, network namespace, OverlayFS, and Landlock failures.
- `cowbox demo --offline` for real execution, diff, and rollback under network
  policy `off`.
- `scripts/offline_workflow.py` with local Cargo, npm, and pip cold-miss/hit
  fixtures, session cache writes, host-cache hash checks, diff, and undo.
- Automatic physical layer compaction for long sessions. The logical snapshot
  DAG remains unchanged while older contiguous ancestor layers are merged at
  the 150-layer or lowerdir-length boundary. Compaction preserves char-device
  and xattr whiteouts, opaque directories, type changes, symlinks, `user.*`
  xattrs, and hardlink groups; validates in a temporary directory; and publishes
  under an atomic rename. Reuse requires an exact source-prefix match on the
  active branch. `stats` now reports logical steps, mounted layers, compacted
  layer count, and compacted storage.
- Explicit read-only host cache reuse for Cargo, npm, and pip through
  `[sandbox.host_caches]`. Only cache data directories are mounted; index
  refreshes, downloads, and other writes copy up into the session cache.
  Credentials and configuration remain in the isolated session home. `doctor`
  reports configured sources and data-directory availability.
- Pip wheelhouse reuse through `PIP_FIND_LINKS` under the configured cache root,
  supporting pinned, hashed, offline installs.

### Changed

- `run` JSON adds `data.policy`; `stats` JSON adds `data.policy` and derives the
  existing `network_isolation` field from the recorded snapshot.
- The Windows launcher forwards all `doctor` arguments and preserves Linux
  stdout, stderr, and exit codes through `wsl.exe`.
- The M2 acceptance script now records source commit, binary SHA-256, machine
  and tool environment, network/policy/offline results, and the full 300-step
  mixed-policy session report.
- The M2 benchmark harness measures host-vs-off network overhead with the same
  10k-file fixture, 20 samples, and one warm-up.
- `cowbox stats` preserves the existing `steps` field and adds `logical_steps`,
  `mounted_layers`, `compact_layers`, and `compacted_bytes`.
- Session disk accounting includes compaction artifacts and host-cache overlay
  storage. Finalized sessions include both in normal `gc` removal.
- The M2 wall-clock harness now follows a 300-step session across repeated
  compaction boundaries, records every step, compaction events, mounted-layer
  counts, and logical/allocated disk growth. The fixed-binary regression round
  is recorded in `docs/BENCHMARKS.md`.
- The M2 harness can measure a compaction step after populating an upper layer
  with a deterministic entry count, and `scripts/acceptance_m2.py` records a
  300-step real-change session through two compactions, rollback, branching,
  host drift, commit, and commit recovery.

### Fixed

- Network and OverlayFS setup failures now return distinct
  `NETWORK_NAMESPACE_FAILED` and `OVERLAYFS_FAILED` errors instead of a generic
  isolation error.
- Session discard now makes parent directories writable before descending, so
  read-only host-cache overlay work directories cannot block cleanup.
- Layer compaction now clears descendants when a higher layer replaces a
  directory with a file or symlink.
- Compaction copies `user.*` xattrs before restoring read-only file or
  directory modes.
- Interrupted compaction can remove stale temporary trees that contain
  read-only entries before retrying.
- State cleanup and shape validation use path-range operations instead of
  scanning every upper entry for every change, removing the previous quadratic
  compaction cost.
- The benchmark harness rejects unexpected candidate `CONFIG_ERROR` exits;
  only an explicit `--allow-legacy-layer-limit` accepts the pre-compaction
  release binary stopping after step 151.
- Compaction temporarily records and elevates source-layer read permissions for
  `000` files and directories, then restores the original modes after validation
  or from a durable journal after interruption.
- Nested `000` directories are now fully readable during compaction, and the
  compacted layer is measured before temporary modes are restored.
- Permission journals live outside the disposable compilation tree, are removed
  only after every mode is restored, and are replayed by `diff`, `commit`
  preview, and subsequent runs under the session lock.
- Permission recovery restores non-directories first and directories
  deepest-first, reopens parents that an earlier interruption restored to
  `000`, and accepts a truncated final journal line while preserving errors for
  genuinely malformed complete records.
- Commit ordering now supports restrictive `000` directories and files,
  including directory mode finalization after child writes and recovery checks
  that cannot traverse a confirmed `000` parent.
- Commit recovery now accepts its own `0700` directory preparation state,
  verifies every host entry in affected directory trees against the journal and
  base manifest, and refuses to delete or overwrite user content added after an
  interruption; the journal and backups stay available for retry.
- The adversarial long-session view check now runs with `set -e`, so every file,
  mode, xattr, symlink, and hardlink assertion can fail CI independently.

## [0.4.0-m2] - 2026-11-16

### Added

- Reproducible M2 CLI wall-clock harness for 10k/200k-file sessions, fixed
  shallow runs, 10-file diffs, layer-depth scaling, and the layer-limit
  boundary. The first measured M2 results and rerun protocol are recorded in
  `docs/BENCHMARKS.md`.
- An adversarial regression fixture covering non-empty `user.*` xattrs,
  hardlink identity and link count, no-follow symlink targets, xattr-only diff
  detection, and host-drift commit refusal.

### Changed

- First-session base-manifest capture now schedules directory tasks in
  parallel, uses no-follow directory-entry metadata, serializes JSONL in the
  workers, and pipelines buffered writes. The JSONL schema remains compatible
  with old manifests; deterministic line ordering remains globally path-sorted.
- Empty user-xattr sets reuse the known SHA-256 digest instead of hashing an
  empty byte stream for every file and directory.
- Windows `doctor` now preserves the Linux diagnostic JSON and human-readable
  capability report when launched through the WSL bridge.

## [0.3.0-m1] - 2026-11-15

### Added

- `cowbox demo` with isolated fixture, rollback verification, JSON output, and
  an opt-in `--keep` mode.
- `cowbox sessions` for global session inventory and `cowbox gc` for safely
  removing finalized session state, including read-only OverlayFS layers.
- `cowbox commit --recover` for reconciling an interrupted commit: it restores
  the host before the durable `applied` point and finalizes the session after
  that point.
- SQLite sidecar grouping, `user.*` xattr preservation, hardlink preservation
  within a commit set, hardlink-backed rollback where the filesystems allow it,
  and phased commit journals with drift guards.
- Deterministic process-interruption tests that kill the real CLI after a
  partial host write, during recovery, after the durable commit point but
  before session finalization, with a stale active pointer after the committed
  session record was saved, and after the active pointer was cleared but before
  journal finalization.

### Changed

- `cowbox commit` now requires terminal confirmation. Non-interactive callers
  must pass `--yes` after obtaining human approval.
- Successful commits retain their journal and backups until the CLI has saved
  the committed session and cleared the active pointer. Journal finalization is
  idempotent, and `gc` skips sessions with unfinished commit recovery.
- Commit backups use an independent content copy plus an original-inode
  hardlink where possible, so a host edit cannot mutate the recovery copy while
  still preserving hardlinks to untouched host paths during rollback.
- Recovery documentation now states the guarantee precisely: process
  interruption recovery only, not power-loss, kernel-crash, or storage-failure
  durability.
- New agent policies no longer advertise untrusted scripts or MCP tools as
  handled scenarios. Documentation now states that network isolation and
  remote database/service rollback are outside M1.
- Base manifests now record inode, link count, and xattr digests; old manifests
  remain readable.

## [0.2.0-m1] - 2026-11-15

### Added

- Native Windows `cowbox.exe` launcher that delegates workspace commands to the
  WSL2 engine; Windows projects can stay on `E:\...`.
- `cowbox setup` installer: downloads and verifies the Linux release, installs
  the WSL binary and Windows launcher, writes global config, and runs `doctor`.
- `cowbox install-skill` for 14 agent harnesses, including Codex, Claude Code,
  Cursor, CodeBuddy, WorkBuddy, Hermes and Kimi Code.
- `cowbox install-rules` and `cowbox init --agent` write idempotent
  AGENTS.md/CLAUDE.md/Cursor rule blocks.
- `cowbox policy --json` with `ask`, `explicit` and `auto` trigger modes.
- `cowbox update --check` and Windows-to-WSL `cowbox path` conversion.

### Changed

- Workspace version is now `0.2.0-m1`; `setup` intentionally does not install a
  skill. Skill installation is an explicit opt-in step.

### Known limitations

- No MCP server yet.
- Codex Desktop built-in file editing is not intercepted; only commands routed
  through cowbox are protected.
- Native Windows ReFS/Job Object backend remains future work.

## [0.1.0-m1] - 2026-11-15

### Added

- Linux/WSL2 CLI: `init`, `run`, `diff`, `log`, `rollback`, `undo`, `commit`,
  `discard`, `doctor`, and `stats`.
- Unprivileged user/mount/PID namespace setup with OverlayFS and `userxattr`.
- Snapshot DAG with pointer rollback, undo, branching, and persisted metadata.
- Whiteout-aware net diff that supports both `0/0` character devices and
  `user.overlay.whiteout` xattrs.
- Landlock path fencing and environment sanitization.
- PID 1 supervisor with orphan reaping and per-step timeouts.
- Commit journal with host-drift detection, backup, and rollback on failure.
- JSON output, Chrome Trace export, benchmark harness, and adversarial tests.

### Known limitations

- No network isolation; Landlock ABI 3 cannot enforce network rules.
- No MCP proxy, native Windows backend, macOS backend, or multi-agent race.
- The base lowerdir is the live workspace; the host must be treated as
  read-only during a session.
- Hardlinks and xattrs are not preserved on commit in M1.

[0.4.0-m2]: https://github.com/BangShou1st/cowbox/releases/tag/v0.4.0-m2
[0.5.0-m2]: https://github.com/BangShou1st/cowbox/releases/tag/v0.5.0-m2
[0.6.0-m3]: https://github.com/BangShou1st/cowbox/releases/tag/v0.6.0-m3
[0.3.0-m1]: https://github.com/BangShou1st/cowbox/releases/tag/v0.3.0-m1
[0.1.0-m1]: https://github.com/BangShou1st/cowbox/releases/tag/v0.1.0-m1
