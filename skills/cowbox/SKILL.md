---
name: cowbox-sandbox-workflow
description: Use when the user explicitly asks for cowbox, or when a task involves multi-file refactors, dependency upgrades, destructive shell commands, or local file-backed database changes. Checkpoints each command, inspects net changes, and rolls back instead of stacking speculative fixes.
---

# Safe cowbox workflow

cowbox is a local Copy-on-Write sandbox for a developer workspace. Every
`cowbox run -- <command>` becomes a numbered step. The host workspace is only
changed by `cowbox commit`.

## First check

1. Check that `cowbox` (Windows: `cowbox.exe`) exists and run `cowbox --version`.
2. Run `cowbox policy --json` and follow its `mode`:
   - `explicit`: use cowbox only when the user explicitly requested it.
   - `ask`: ask the user before starting a session.
   - `auto`: start a session without asking.
3. Run `cowbox doctor` once per project/environment. On Windows this checks the
   WSL distro and Linux binary; on Linux it checks namespaces, OverlayFS and
   Landlock.

If the CLI is missing on Windows, tell the user to run `cowbox setup`; the
installer does not install this skill automatically. The user must explicitly
run `cowbox install-skill` when they want skill files copied into agent
harnesses.

## Rules

1. **Do not patch over a broken state.** If a refactor, dependency upgrade, or
   migration fails and leaves the workspace broken, stop editing. Inspect
   `cowbox log` and call `cowbox rollback <last-clean-step>` before trying a
   different approach.
2. **Inspect before keeping changes.** Run `cowbox diff --stat` and
   `cowbox diff` before `cowbox commit`. Confirm there are no stray temporary
   files, lockfile corruption, or local database changes you did not intend.
3. **Commit only clean work.** `cowbox commit` is a host write. Run it only
   after tests pass and the diff is minimal. Use `--dry-run` first when unsure.
   After explicit human approval, run `cowbox commit --yes`; non-interactive
   calls without `--yes` are refused.
   If commit reports `COMMIT_RECOVERY_REQUIRED`, stop editing and run
   `cowbox commit --recover --yes`. It restores the host before the durable
   commit point or finalizes an already-applied commit; inspect any reported
   host-drift conflicts instead of forcing recovery.
4. **Do not commit `.git`.** cowbox never commits VCS metadata. Do not try to
   force it.
5. **Respect the host read-only invariant.** Do not edit the host workspace
   while a cowbox session is active. If an external edit is unavoidable, expect
   `commit` to report a conflict.
6. **Treat extra write paths as host writes.** `--allow-write` and
   `extra_rw_paths` bypass the snapshot. Use them only for caches or toolchain
   paths you understand.

## When to use

Use cowbox for:

- `rm -rf`, `git reset --hard`, bulk deletes and destructive scripts;
- local file-backed database migrations and resets (for example SQLite);
- dependency installation or major upgrades;
- refactors touching more than five files;
- tests that create local databases, caches, or background processes.

Do not use cowbox for `ls`, `cat`, `rg`, `git status`, or other read-only
commands. Do not use it for a single-file edit that does not touch local state.
Do not use it as a malware sandbox: M1 does not isolate the network, undo
remote database changes, or stop a process from exfiltrating readable files.

## Typical loop

```bash
cd /path/to/project
cowbox init --agent

cowbox run -- cargo test
cowbox diff --stat

# If tests fail, inspect the step tree and rewind.
cowbox log
cowbox rollback <clean-step>

# Try a different approach, then inspect and apply.
cowbox run -- cargo test
cowbox diff
cowbox commit --dry-run
# After explicit human approval:
cowbox commit --yes
```

On Windows the project can stay at `E:\...`; `cowbox.exe` maps it into WSL. Do
not ask the user to move the project into WSL. Run `cowbox path <path>` when a
command needs an explicit WSL path.

## Windows and agent boundary

- `cowbox.exe setup` installs the Windows launcher and the Linux engine in WSL,
  but **does not install this skill**.
- `cowbox install-skill --list` shows supported harnesses; `--harness <id>`
  installs explicitly; `--all` installs into every supported harness.
- `cowbox init --agent` writes the project rule blocks for AGENTS.md,
  CLAUDE.md and Cursor.
- Codex Desktop's built-in file editor is not intercepted in M1.5. Only commands
  routed through cowbox are protected; the native Windows backend is a later
  milestone.

## JSON mode

Use `--json` when another tool needs to parse results:

```bash
cowbox run --json -- cargo test
cowbox diff --json
cowbox log --json
cowbox stats --json
```

## References

- [Installation and harnesses](references/installation.md)
- [Windows/WSL behavior](references/windows.md)
- [Workflow and recovery](references/workflow.md)
- [Troubleshooting](references/troubleshooting.md)

## Boundary

This skill uses the CLI. The MCP proxy and synthetic `cowbox_diff` /
`cowbox_rollback` / `cowbox_commit` tools are not part of M1.5; do not assume
they exist. Codex Desktop built-in edits are outside this boundary until the
native Windows backend lands. Network isolation is also outside this boundary;
cowbox protects workspace files, not remote services or exfiltrated data.
