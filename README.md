<div align="center">

# cowbox

### Millisecond Copy-on-Write sandboxes for AI coding agents

<p>
  <a href="https://github.com/BangShou1st/cowbox/actions/workflows/ci.yml">
    <img src="https://github.com/BangShou1st/cowbox/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI" />
  </a>
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License" />
  <img src="https://img.shields.io/badge/rust-1.85%2B-orange.svg" alt="Rust" />
  <img src="https://img.shields.io/badge/platform-Linux%20%2F%20WSL2%20%2F%20Windows-lightgrey.svg" alt="Platform" />
</p>

**Let agents break things. Rewind in milliseconds. Keep the host clean.**

[English](README.md) | [简体中文](README.zh-CN.md)

</div>

> [!IMPORTANT]
> cowbox is an **M3 agent snapshot preview**. The engine targets Linux and WSL2;
> Windows ships a native launcher that delegates into WSL. `mcp-proxy` wraps a
> stdio MCP server so each tool call becomes an inspectable, reversible cowbox
> step. A native Windows file-edit backend and macOS support are later
> milestones. See [docs/ROADMAP.md](docs/ROADMAP.md) for the staged plan.

## Why cowbox

AI coding agents are good at making changes and bad at knowing when to stop.
The usual recovery tools leave gaps:

| Failure mode | `git checkout` / worktree | Docker / VM | cowbox |
| --- | --- | --- | --- |
| Agent rewrites untracked or ignored files | Not covered | Covered, but the environment is different | Covered by the CoW layer |
| Agent corrupts a local SQLite database | Not covered | Needs volume plumbing | Covered as a regular file |
| Agent leaves a dev server or worker running | Not covered | Covered if the container is removed | PID namespace is reaped at step end |
| Agent installs a broken dependency tree | Slow to reset | Slow cold start and lost local state | New upper layer, instant rewind |
| Human wants to inspect side effects before keeping them | Partial | Awkward | `cowbox diff` before `cowbox commit` |

cowbox is not a replacement for containers. It is a **local safety net** for
the workspace you are already editing: your real toolchain, your real caches,
your real project, with a cheap undo button.

## What it does

```bash
cowbox run -- <command>      # run in a CoW view; every invocation is a step
cowbox run --network off ... # also isolate IP connectivity in a network namespace
cowbox demo                  # verify rollback end to end in an isolated fixture
cowbox demo --offline        # run the same real execution under network policy off
cowbox diff                  # inspect net changes and sandbox side effects
cowbox log                   # see the step DAG, including branches
cowbox rollback <step>       # move the head pointer; no host writes
cowbox undo [n]              # shortcut for rolling back n steps
cowbox commit                # atomically apply the net changes to the host
cowbox commit --recover      # reconcile an interrupted commit (restore or finish)
cowbox discard               # throw the sandbox state away
cowbox doctor                # check kernel, OverlayFS, xattr, Landlock
cowbox doctor --check-run --network off # exercise the real startup chain
cowbox stats                 # inspect logical steps, mount layers, disk use
cowbox sessions              # list active and finalized sessions across projects
cowbox gc --dry-run          # preview old session state that can be removed
cowbox mcp-proxy -- <server> # expose a stdio MCP server behind cowbox snapshots
```

All commands support `--json`. `run` also supports `--trace-chrome` for a
Chrome Trace JSON of namespace, mount, diff, and commit spans.

## How it works

```text
┌──────────────────────────────────────────────────────────────┐
│ cowbox CLI (thin)                                            │
│ init · run · diff · log · rollback · commit · doctor · stats │
└───────────────┬───────────────────────────────┬──────────────┘
                │                               │
┌───────────────▼──────────────┐  ┌─────────────▼──────────────┐
│ cowbox-core (Linux engine)   │  │ cowbox-dag (pure state)    │
│ namespaces · OverlayFS       │  │ Step 0 → Step N · branches │
│ Landlock · PID supervisor    │  │ rollback · undo · validate │
│ whiteout diff · commit       │  └────────────────────────────┘
└───────────────┬──────────────┘
                │
┌───────────────▼──────────────────────────────────────────────┐
│ State: $COWBOX_HOME / XDG_STATE_HOME / ~/.local/state/cowbox │
│ base manifest · per-step upperdirs · journal · events        │
└──────────────────────────────────────────────────────────────┘
```

- The real workspace is the read-only **lowerdir**.
- Every step gets a writable **upperdir** on the WSL/Linux ext4 filesystem.
- A step is frozen by dropping the mount; the next step stacks the previous
  upper layers below its own upperdir.
- Long sessions compact old physical mount layers while retaining every logical
  DAG step. Compression is derived from upper layers plus the base manifest,
  validated in a temporary directory, and published atomically. `diff`,
  `commit`, rollback, and branching continue to use the original logical
  layers.
- `rollback` is a DAG head-pointer move. The old layers stay available for
  audit and for branching.
- `commit` computes the net diff from the base, checks for host drift, backs up
  every affected host path into a journal, and applies changes in a safe order.
- Commit keeps SQLite `-wal`/`-shm`/`-journal` sidecars grouped with their
  database, preserves `user.*` xattrs, and keeps hardlinks among files that are
  committed together. The journal's durable `applied` phase is the commit
  point. `cowbox commit --recover` restores the host if the process stopped
  before that point, or finalizes the committed session if it stopped after.
- Landlock denies writes outside the workspace and the session's private
  home/tmp/cache directories. A PID namespace reaps background children when a
  step ends.
- With `--network off`, the helper unshares a Linux network namespace after the
  UID/GID mapping and after OverlayFS setup, but before the user command starts.
  There is no fallback to the host namespace if that step fails.
- `cowbox mcp-proxy` keeps the agent's MCP connection open, discovers upstream
  tools in a one-off sandbox, and launches a fresh sandboxed upstream process
  for every `tools/call`. The outer protocol exposes the upstream tools plus
  read-only `cowbox_diff`, pointer-only `cowbox_rollback`, and dry-run-only
  `cowbox_commit`.

For the full call sequence, whiteout rules, and commit ordering, see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Quick start (Windows)

Download the Windows release zip, extract `cowbox.exe`, and run it from a
terminal:

```powershell
.\cowbox.exe setup
cowbox doctor
cowbox demo
cowbox demo --offline
cowbox install-skill --list
cowbox install-skill --harness codex
```

`setup` installs the Windows launcher and the Linux engine in WSL, but **does
not install a skill**. `install-skill` is the explicit opt-in step. Your project
can stay on `E:\...`; the launcher maps it to `/mnt/e/...` inside WSL.

`cowbox demo` creates an isolated fixture under `/tmp`, performs a destructive
command, previews the commit, rolls back, and verifies that the fixture files
are restored. It never touches the current project. Use `--keep` to inspect the
fixture or `--json` for machine-readable output. `demo --offline` performs the
real execution, diff, and rollback with network policy `off`.

In a project:

```powershell
cd E:\project\your-app
cowbox init --agent
cowbox run -- <command>
```

`init --agent` writes `.cowbox.toml` plus the AGENTS.md, CLAUDE.md and Cursor
rule blocks. The default trigger mode is `ask`; inspect it with
`cowbox policy --json`.

## Quick start (WSL2)

### Prerequisites

- Windows 11 with WSL2 and Ubuntu 24.04.
- Linux kernel 5.11 or newer (WSL2 `6.6.x` is tested).
- Unprivileged user namespaces and OverlayFS:

```bash
unshare -rm echo ok
grep -w overlay /proc/filesystems
```

- The cowbox state directory must live on a filesystem with xattr support
  (ext4 in WSL2). Do not put `COWBOX_HOME` under `/mnt/c` or `/mnt/e`.

### Build

```bash
git clone https://github.com/BangShou1st/cowbox.git
cd cowbox

rustup toolchain install stable --profile minimal
rustup component add rustfmt clippy
export CARGO_TARGET_DIR="$HOME/.cache/cowbox-target"
cargo install --path crates/cowbox-cli

cowbox doctor
cowbox demo
```

If the source checkout is on `/mnt/e`, keep `CARGO_TARGET_DIR` on the WSL
filesystem. Compiling into NTFS through 9p is slow and unnecessary.

### Try it

```bash
cd /path/to/your/project
cowbox init

# Deliberately destructive command, contained in the sandbox.
cowbox run -- sh -c 'rm -rf src data/dev.sqlite'

cowbox diff --stat

# Keep the experiment if it is useful:
# cowbox commit
#
# Otherwise rewind and leave the host untouched:
cowbox rollback 0
```

`cowbox run` executes the command from the workspace root by default. Use
`--` before the command so cowbox never tries to parse its flags. A failed
command still becomes a step, so you can inspect or roll it back.

Committed and discarded sessions remain available for audit. `cowbox sessions`
shows their disk use, and `cowbox gc` removes finalized sessions older than
seven days. Active sessions are never deleted. Use `--all` to ignore the age
threshold, `--dry-run` to preview, and `--yes` for non-interactive execution.

### Long sessions

Sessions that cross the physical layer or lowerdir-length limit compact the
oldest part of the active ancestor chain automatically and keep the most recent
layers uncompressed. `cowbox stats` reports both logical steps and the physical
mount count, plus compacted-layer storage. A compaction cache is reused only
when its recorded source list is an exact prefix of the current branch.

### Network policy

`cowbox run --network host|off` selects the network policy for a step. The
precedence is CLI, then project configuration, then `host`:

```toml
[sandbox]
network = "off"
```

`off` uses a private Linux network namespace. The namespace has no host
interfaces and its loopback interface remains down. IPv4, IPv6, TCP, and UDP
connections to host or external addresses therefore fail. This is IP
connectivity isolation, not a firewall or a seccomp network policy.

Filesystem-backed Unix-domain sockets are an IPC boundary: a path that the
Landlock policy allows remains reachable across the network namespace. Use
Landlock path rules when an agent must not reach a particular Unix socket.

The effective policy is recorded per logical step. `run --json` reports it in
`data.policy`, and `stats --json` reports the policy for the current snapshot.
Older sessions without a record remain readable and report
`"recorded": false` with network `"unknown"`.

```bash
cowbox run --json --network off -- python3 ./offline_job.py
cowbox stats --json
cowbox doctor --check-run --network off --json
```

`doctor --check-run` keeps the static capability report and also runs the real
helper, namespace, OverlayFS, state-disk, Landlock, and user-command path in a
temporary fixture. Its `run_check.stages` object distinguishes user-namespace,
network-namespace, OverlayFS, state-disk, and Landlock failures. A network
namespace setup failure returns `NETWORK_NAMESPACE_FAILED` with exit code 125
and reports `command_started: false`.

### MCP agent snapshots

`mcp-proxy` is a stdio-to-stdio protocol bridge with a real cowbox step behind
each mutating tool call:

```bash
cowbox mcp-proxy --network off -- python3 /path/to/your/mcp-server.py
```

Configure it as the agent harness's MCP command, for example:

```json
{
  "mcpServers": {
    "cowbox": {
      "command": "/home/you/.cargo/bin/cowbox",
      "args": [
        "mcp-proxy",
        "--network", "off",
        "--",
        "python3", "/path/to/your/mcp-server.py"
      ]
    }
  }
}
```

The first `tools/list` starts a one-off sandbox session under `/tmp`, runs the
upstream `initialize` handshake and paginated `tools/list`, then removes the
temporary state. It never adds a step to the project's active session.

Every upstream `tools/call` starts a new cowbox step in the workspace session,
launches a fresh sandboxed upstream process, performs one initialize/call
cycle, and exits. Upstream servers that need state across calls must persist it
in workspace files. Calls are serialized, the effective network policy and
Landlock rules apply, and the result metadata reports `head_before`, the actual
`step`, `head_after`, timeout state, and a rollback command.

Three reserved control tools are always present:

| Tool | Behavior |
| --- | --- |
| `cowbox_diff` | Read-only net diff for a step; defaults to the current head |
| `cowbox_rollback` | Move the session head; never writes the host |
| `cowbox_commit` | Existing commit dry-run; returns the target step and terminal command |

`cowbox_commit` never writes the host. After human review, the user runs
`cowbox commit --step <n>` in a terminal in the same workspace and completes the
normal interactive confirmation.

Control commands are bound to the proxy's session: the CLI rejects a selected
session if it is no longer the workspace's active pointer. An explicit
`--network host` also overrides a project-level `network = "off"`.

The first MCP version supports stdio tool servers only. Cross-call state lives
in the workspace, not in a long-lived upstream process. HTTP/SSE upstreams,
resources, prompts, and persistent MCP subprocesses are not implemented.
`--timeout`, `--discovery-timeout`, `--max-response-bytes`, `--session`,
`--workspace`, and repeatable `--allow-read` bound the proxy's behavior.

### Host cache reuse

Cargo, npm, and pip can explicitly reuse a host cache as a read-only data
source. Add paths to `.cowbox.toml`:

```toml
[sandbox.host_caches]
cargo = "/home/you/.cargo"     # mounts only registry/ and git/
npm = "/home/you/.npm"         # mounts only _cacache/
pip = "/home/you/.cache/pip"   # mounts cache data, including wheelhouse/
```

`true` selects the default cache under the host home; `false` keeps the original
isolated-only behavior. cowbox mounts only the tool's data directories as
read-only lower layers over a session-specific writable cache. Lock/index
updates, new downloads, and extracted packages are written to the session
cache, not the host. Credentials and tool configuration stay in the isolated
session home. `cowbox doctor` lists each configured source and whether its data
directories exist.

For pip, a `wheelhouse/` directory under the configured cache root is exposed
through `PIP_FIND_LINKS`, so pinned, hashed requirements can install with
`--no-index` without network access. Other downloads still use the normal index
and land in the session cache.

### Offline workflow

`scripts/offline_workflow.py` generates only local fixtures. It verifies:

- a cold Cargo directory-source miss and a host-cache hit through a local
  vendored registry;
- a cold npm `ENOTCACHED` miss and a host `_cacache` hit from a local tarball
  served only while seeding;
- a cold pip miss and a host wheelhouse hit through `PIP_FIND_LINKS`;
- session-local cache writes persist between steps while host cache hashes stay
  unchanged;
- the real diff contains the workspace outputs and rollback restores the host
  fixture.

All commands run with `--network off`; no public package registry is used:

```bash
python3 scripts/offline_workflow.py \
  --binary "$COWBOX_BIN" \
  --json-output "$HOME/cowbox-bench-results/offline.json"
```

## Safety model

- cowbox uses **Landlock ABI 3** for path-based read/write fencing and a
  **PID namespace** plus a PID 1 supervisor for process-tree cleanup.
- The host workspace is treated as **read-only during an active session**. The
  lowerdir is the live workspace, so external edits are detected at commit time
  through a base manifest; `cowbox commit` refuses by default and `--force`
  still takes a backup.
- The default network policy is `host`: a process can bind host ports and make
  network connections. `run --network off` removes IP connectivity by placing
  the command in a fresh Linux network namespace. The namespace is not a
  firewall, cannot filter selected destinations, and does not isolate
  filesystem-backed Unix sockets.
- cowbox is a guard against **accidental damage and agent mistakes**, not a
  hardened sandbox for hostile code. Kernel exploits and deliberately malicious
  binaries are out of scope.
- `cowbox commit` asks for confirmation in a terminal. Non-interactive callers
  must use `cowbox commit --yes` after obtaining explicit human approval.
- Hardlinks are preserved among files in the same commit set. After a
  successful commit, a hardlink to a host file that is not being committed
  becomes a separate inode. Failed-commit rollback uses hardlink backups when
  the workspace and state filesystems allow it.
- Commit backups are retained until the restored host manifest and journal
  phase, or the committed session record and cleared active pointer, have been
  persisted. Recovery refuses to overwrite externally edited paths and keeps
  its backup material for inspection. This is a process-interruption guarantee,
  not a power-loss or storage-failure guarantee.
- Only `user.*` xattrs are preserved. Non-user xattr namespaces are kernel or
  security-managed and are left untouched.
- Configured host caches are mounted read-only and only their data directories
  are exposed. Session writes copy up into the session cache.
- `.git` is never committed, even with `--include-excluded`. Device nodes,
  FIFOs, and sockets are refused. setuid/setgid bits are cleared on commit.
- cowbox never silently falls back to a full directory copy when the OS CoW
  primitive is unavailable. It fails with an actionable error instead.

See [docs/SECURITY-MODEL.md](docs/SECURITY-MODEL.md) for the full threat model.

## Benchmarks

We only publish numbers that were measured on a real machine. The harness and
methodology live in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

The final M2 offline-execution round compared the merge baseline `29af7e7` with
the network-policy/offline candidate `fb4dad0` on WSL2 ext4. Each scenario used
20 samples, one excluded warm-up, and a fresh session for every sample.
This code is shipped in the `v0.5.0-m2` preview.

| Scenario | 10,000 files | 200,000 files |
| --- | ---: | ---: |
| First `cowbox run -- true` (includes manifest capture) | 29.92 / 31.19 ms | 124.45 / 129.62 ms |
| Subsequent run with 1 parent layer | 23.17 / 25.90 ms | 23.86 / 25.65 ms |
| `cowbox diff` with 10 changed files | 5.63 / 5.80 ms | 89.98 / 94.12 ms |

Cells are median / P95 of complete CLI wall-clock durations. The 10,000-file
and 200,000-file first-run limits of 60/80 ms and 150/200 ms pass, as do the
10-file diff medians of 20 ms and 200 ms. A real 300-step session completed
with 279 `host` and 21 `off` steps, automatic compaction at 151 and 268,
rollback, branch, commit, and interruption recovery. Network namespace setup
added 0.47 ms to the 10k first-run median and 0.38 ms to the one-parent median.

The `v0.6.0-m3` candidate repeated the gates against the published
`v0.5.0-m2` Linux release with 20 samples and one warm-up. First-run
median / P95 was 33.70 / 40.41 ms at 10k files and 120.55 / 142.02 ms at
200k files. Candidate 10-file diff median / P95 was 7.17 / 14.54 ms at 10k
and 88.60 / 97.21 ms at 200k. The MCP first handshake was 83.855 / 107.503 ms
median / P95; continuous same-session tool calls were 103.028 / 164.157 ms,
including a fresh sandbox step and upstream process handshake per call.

See [docs/BENCHMARKS.md](docs/BENCHMARKS.md) for full statistics, fixtures,
machine details, caveats, and the exact rerun command. P99 is not separately
instrumented yet.

## Status and roadmap

| Milestone | Scope | Status |
| --- | --- | --- |
| **M1** | Linux/WSL2 CLI, snapshot DAG, OverlayFS, Landlock, commit journal, tests, benchmarks | Preview available |
| **M1.5** | Windows launcher, `setup`, multi-harness skill installer, project rules and policy | Preview available |
| **M2** | Long sessions, network policy, offline diagnostics/workflows, read-only host caches, 200k-file benchmarks | Preview available |
| **M3** | MCP snapshot proxy, synthetic diff/rollback/commit tools, real-harness workflow | Preview available |
| **M4** | Native Windows ReFS + Job Object backend, macOS backend, eval suite and public leaderboard | Planned |

M1 explicitly excludes MCP, native Windows, macOS, network isolation, and
multi-agent racing. M2 adds opt-in network-namespace isolation without claiming
packet filtering or hostile-code isolation. M3 adds stdio MCP snapshot
interception for tool servers, not protocol-level containment of an in-process
or HTTP upstream. The
[roadmap](docs/ROADMAP.md) keeps the boundaries honest.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md). The short
version: keep the engine deterministic, do not add silent fallbacks, run the
adversarial suite in WSL ext4, and never publish unmeasured benchmark numbers.

Security reports: use GitHub's private vulnerability reporting, not a public
issue. See [SECURITY.md](SECURITY.md).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
