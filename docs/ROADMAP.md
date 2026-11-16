# cowbox roadmap

cowbox follows a staged path: make the local Linux engine honest and
measurable first, then add agent protocol integration, then native platforms.

## M1: Linux/WSL2 engine preview

Status: preview available (2026-11-15). The public repository and CI are live.

- [x] Cargo workspace and public interfaces.
- [x] Snapshot DAG with rollback, undo, branching, and serialization.
- [x] Unprivileged OverlayFS with `userxattr` and multi-layer lowerdirs.
- [x] Landlock path fencing and environment sanitization.
- [x] PID 1 supervisor, orphan reaping, and timeouts.
- [x] Whiteout-aware diff (char device and xattr forms).
- [x] Commit journal, host-drift detection, and atomic write order.
- [x] `init`, `run`, `diff`, `log`, `rollback`, `undo`, `commit`, `discard`,
  `doctor`, and `stats`.
- [x] 10k-file adversarial suite, 9p smoke, and Criterion benchmarks.
- [x] CI on Ubuntu 22.04/24.04 and a public M1 repository.

M1 mounts at most 150 parent layers when creating a step. A step from a
150-layer head is accepted and creates layer 151; the following step is rejected
with `CONFIG_ERROR`. Longer sessions must commit or discard and start a new one;
layer compaction is an M2 item.

Explicitly excluded from M1: MCP proxy, native Windows, macOS, network
isolation, multi-agent racing, and the eval leaderboard.

## M1.5: Windows launcher and agent packaging

Status: preview released in `0.3.0-m1`.

- [x] Native Windows launcher that delegates to WSL via `wsl.exe --cd`.
- [x] `cowbox setup` CLI installer with release checksum verification.
- [x] `install-skill` for 14 agent harnesses; manual opt-in only.
- [x] `install-rules` and `init --agent` project policy blocks.
- [x] `policy --json` with ask/explicit/auto modes.
- [x] `cowbox demo` self-verification and explicit commit approval.
- [x] Global `sessions` inventory and safe `gc` for finalized state.
- [x] Commit hardening: SQLite sidecars, hardlinks, `user.*` xattrs, and
  restore-or-finalize journals for process interruptions.
- [x] MCP integration via `cowbox mcp-proxy` for MCP-native agents.

## M2: stronger containment and scale

Status: preview `0.5.0-m2` is published. It includes long-session layer
compaction, network-namespace isolation, active diagnostics, offline cache
workflows, read-only host cache reuse, and the regression round.

- [x] `run --network host|off` with CLI, project, and default precedence.
  `off` creates a private Linux network namespace after UID/GID mapping and
  before the user command. IP connectivity is isolated; filesystem-backed Unix
  sockets remain within the Landlock IPC boundary.
- [x] Per-step effective-policy snapshots in `run --json` and `stats --json`,
  including backward-compatible handling for older sessions.
- [x] `doctor --check-run` through the real helper, namespace, OverlayFS,
  state-disk, Landlock, and user-command path, with stage-specific failures.
- [x] Offline `demo --offline` plus local Cargo, npm, and pip cold/hit fixture
  coverage with host-cache hash checks.
- [x] Long-session physical layer compaction with logical DAG, diff, rollback,
  branching, and commit semantics preserved.
- [x] Read-only Cargo/npm/pip host cache reuse with session-local copy-up,
  explicit configuration, and doctor status.
- [x] 200k-file benchmark fixtures, first-round wall-clock measurements, and
  parallel first-session manifest capture.
- [x] Benchmark regression tracking with a fixed release binary, 20 samples,
  one warm-up, long-session boundaries, and measured disk growth.
- [ ] Blocked-write audit counters (requires an interception layer, not Landlock
  alone).
- [ ] Selective destination filtering and packet-level network policy. The M2
  network namespace isolates connectivity but is not a firewall.
- [ ] Additional `doctor` heuristics for AppArmor, WSL configuration, and
  filesystem-specific quirks beyond the active startup chain.

## M3: agent integration

Status: preview available in `0.6.0-m3`.

- [x] `cowbox mcp-proxy -- <upstream MCP server>` for stdio tool servers.
- [x] Initialization, capability negotiation, ping, paginated `tools/list`,
  request IDs, timeouts, abnormal upstream exits, response limits, and client
  disconnect handling.
- [x] One-off sandbox discovery plus one real cowbox step for every upstream
  `tools/call`; calls are serialized and cross-call state lives in workspace
  files.
- [x] Reserved synthetic tools `cowbox_diff`, `cowbox_rollback`, and
  `cowbox_commit`, including upstream name-conflict detection.
- [x] Dry-run-only commit preview. The host write remains a terminal
  `cowbox commit --step <n>` with the existing human confirmation.
- [x] Linux/WSL2 fixture coverage and a real agent-harness workflow.
- [x] Persistent Windows launcher stdio forwarding through the real
  `cowbox.exe -> wsl.exe -> Linux cowbox` path.
- [x] `skills/cowbox` guidance that prefers rollback over stacking speculative
  fixes.
- [ ] HTTP/SSE upstream transport, resources, prompts, and a persistent
  upstream MCP process. The first version starts the upstream per call.

## M4: native platforms and evaluation

- Native Windows 11 backend: ReFS block cloning + Job Objects.
- macOS backend: APFS `clonefile` + `sandbox-exec`/kqueue process tracking.
- A reproducible eval harness for destructive refactor, dependency, migration,
  and prompt-injection tasks.
- Public leaderboard for agent recovery behavior, with published methodology.

## Non-goals

- A Docker or VM fallback. cowbox is explicitly a local CoW tool.
- A web UI or hosted service in the near term.
- Claiming performance or safety properties that are not reproduced by the
  checked-in benchmark and adversarial suites.
