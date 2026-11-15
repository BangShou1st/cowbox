---
name: cowbox-sandbox-workflow
description: Use when making multi-file changes, dependency upgrades, database migrations, or destructive shell commands inside a cowbox-protected workspace. Checkpoints each step, inspects net changes, and rolls back cleanly instead of stacking speculative fixes.
---

# Safe cowbox workflow

cowbox is a local Copy-on-Write sandbox for a developer workspace. Every
`cowbox run -- <command>` becomes a numbered step. The host workspace is only
changed by `cowbox commit`.

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
4. **Do not commit `.git`.** cowbox never commits VCS metadata. Do not try to
   force it.
5. **Respect the host read-only invariant.** Do not edit the host workspace
   while a cowbox session is active. If an external edit is unavoidable, expect
   `commit` to report a conflict.
6. **Treat extra write paths as host writes.** `--allow-write` and
   `extra_rw_paths` bypass the snapshot. Use them only for caches or toolchain
   paths you understand.

## Typical loop

```bash
cd /path/to/project
cowbox init

cowbox run -- cargo test
cowbox diff --stat

# If tests fail, inspect the step tree and rewind.
cowbox log
cowbox rollback <clean-step>

# Try a different approach, then inspect and apply.
cowbox run -- cargo test
cowbox diff
cowbox commit --dry-run
cowbox commit
```

## JSON mode

Use `--json` when another tool needs to parse results:

```bash
cowbox run --json -- cargo test
cowbox diff --json
cowbox log --json
cowbox stats --json
```

## M1 boundary

This skill uses the CLI. The MCP proxy and synthetic `cowbox_diff` /
`cowbox_rollback` / `cowbox_commit` tools arrive in M3; do not assume they exist
in an M1 installation.
