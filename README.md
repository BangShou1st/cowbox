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
> cowbox is an **M1.5 preview**. The engine targets Linux and WSL2; Windows ships
> a native launcher that delegates into WSL. MCP proxying, a native Windows
> file-edit backend, macOS support, and network isolation are later milestones.
> See [docs/ROADMAP.md](docs/ROADMAP.md) for the staged plan.

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
cowbox demo                  # verify rollback end to end in an isolated fixture
cowbox diff                  # inspect net changes and sandbox side effects
cowbox log                   # see the step DAG, including branches
cowbox rollback <step>       # move the head pointer; no host writes
cowbox undo [n]              # shortcut for rolling back n steps
cowbox commit                # atomically apply the net changes to the host
cowbox commit --recover      # reconcile an interrupted commit (restore or finish)
cowbox discard               # throw the sandbox state away
cowbox doctor                # check kernel, OverlayFS, xattr, Landlock
cowbox stats                 # inspect steps, rollbacks, orphans, disk use
cowbox sessions              # list active and finalized sessions across projects
cowbox gc --dry-run          # preview old session state that can be removed
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

For the full call sequence, whiteout rules, and commit ordering, see
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Quick start (Windows)

Download the Windows release zip, extract `cowbox.exe`, and run it from a
terminal:

```powershell
.\cowbox.exe setup
cowbox doctor
cowbox demo
cowbox install-skill --list
cowbox install-skill --harness codex
```

`setup` installs the Windows launcher and the Linux engine in WSL, but **does
not install a skill**. `install-skill` is the explicit opt-in step. Your project
can stay on `E:\...`; the launcher maps it to `/mnt/e/...` inside WSL.

`cowbox demo` creates an isolated fixture under `/tmp`, performs a destructive
command, previews the commit, rolls back, and verifies that the fixture files
are restored. It never touches the current project. Use `--keep` to inspect the
fixture or `--json` for machine-readable output.

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

## Safety model

- cowbox M1 uses **Landlock ABI 3** for path-based read/write fencing and a
  **PID namespace** plus a PID 1 supervisor for process-tree cleanup.
- The host workspace is treated as **read-only during an active session**. The
  lowerdir is the live workspace, so external edits are detected at commit time
  through a base manifest; `cowbox commit` refuses by default and `--force`
  still takes a backup.
- M1 does **not** isolate the network. A process inside the sandbox can bind
  host ports and make network connections. Network policy is planned for M2.
- M1 is a guard against **accidental damage and agent mistakes**, not a
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
- `.git` is never committed, even with `--include-excluded`. Device nodes,
  FIFOs, and sockets are refused. setuid/setgid bits are cleared on commit.
- cowbox never silently falls back to a full directory copy when the OS CoW
  primitive is unavailable. It fails with an actionable error instead.

See [docs/SECURITY-MODEL.md](docs/SECURITY-MODEL.md) for the full threat model.

## Benchmarks

We only publish numbers that were measured on a real machine. The harness and
methodology live in [docs/BENCHMARKS.md](docs/BENCHMARKS.md).

| Scenario | Fixture | Target | Measured (Criterion mean) |
| --- | --- | --- | --- |
| Cold OverlayFS mount | 10,000 files | < 5 ms | TBD (pure mount not separately instrumented) |
| Step freeze | 10,000 files | < 10 ms | TBD (pure freeze not separately instrumented) |
| Diff scan (10 changed files) | 10,000 files | < 25 ms | 13.19 ms (12.96–13.56 ms) |
| Rollback (DAG pointer) | 10,000 files | < 5 ms | 10.7 ns (10.44–11.01 ns) |
| End-to-end `cowbox run -- true` | 10,000 files | no hard target | 56.2 ms (51.76–60.88 ms, includes CLI/namespace/mount) |

These are targets, not claims. The measured column is filled after
`scripts/bench.sh` runs in WSL2 ext4.

M1 reports Criterion means and ranges; P99 is not separately instrumented yet.

## Status and roadmap

| Milestone | Scope | Status |
| --- | --- | --- |
| **M1** | Linux/WSL2 CLI, snapshot DAG, OverlayFS, Landlock, commit journal, tests, benchmarks | Preview available |
| **M1.5** | Windows launcher, `setup`, multi-harness skill installer, project rules and policy | Preview available |
| **M2** | Network policy (seccomp or newer Landlock), cache bind mounts, 200k-file benchmarks, blocked-write auditing | Planned |
| **M3** | MCP proxy with `cowbox_diff` / `cowbox_rollback` / `cowbox_commit` meta-tools | Planned |
| **M4** | Native Windows ReFS + Job Object backend, macOS backend, eval suite and public leaderboard | Planned |

M1 explicitly excludes MCP, native Windows, macOS, network isolation, and
multi-agent racing. The [roadmap](docs/ROADMAP.md) keeps the boundaries honest.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md). The short
version: keep the engine deterministic, do not add silent fallbacks, run the
adversarial suite in WSL ext4, and never publish unmeasured benchmark numbers.

Security reports: use GitHub's private vulnerability reporting, not a public
issue. See [SECURITY.md](SECURITY.md).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
