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
- [ ] MCP server (`cowbox mcp`) for MCP-native agents.

## M2: stronger containment and scale

Status: performance preview `0.4.0-m2` is published. Long-session layer
compaction, read-only host cache reuse, and the regression round are on the
pending merge candidate.

- [ ] Network policy via seccomp user notification or Landlock ABI 4+ where the
  kernel supports it.
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
- [ ] Better `doctor` diagnostics for AppArmor, WSL config, and filesystem quirks.

## M3: agent integration

- `cowbox mcp-proxy -- <upstream MCP server>` with transparent stdio JSON-RPC
  forwarding.
- Synthetic MCP tools: `cowbox_diff`, `cowbox_rollback`, `cowbox_commit`.
- Auto-checkpoint on mutating `tools/call` requests.
- A `skills/cowbox/SKILL.md` workflow that prefers rollback over stacking
  speculative fixes.

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
