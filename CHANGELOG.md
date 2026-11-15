# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

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

[0.1.0-m1]: https://github.com/BangShou1st/cowbox/releases/tag/v0.1.0-m1
