# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[0.3.0-m1]: https://github.com/BangShou1st/cowbox/releases/tag/v0.3.0-m1
[0.1.0-m1]: https://github.com/BangShou1st/cowbox/releases/tag/v0.1.0-m1
