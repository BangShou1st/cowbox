# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [0.1.0-m1] - Unreleased

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
