# cowbox Engineering Constitution

This file is the highest-priority engineering contract for anyone (human or
agent) changing cowbox. If a shortcut conflicts with these rules, the rules win.

## 1. Project shape

cowbox is a local, millisecond-latency Copy-on-Write sandbox for AI coding
agents. The architecture is **Thick Engine, Thin Protocol**:

- `crates/cowbox-core`: Linux namespaces, OverlayFS, Landlock, process
  supervision, whiteout-aware diff, commit journal, and the native Windows
  NTFS snapshot store. This is the engine.
- `crates/cowbox-dag`: pure snapshot DAG. No filesystem or process access.
- `crates/cowbox-cli`: argument parsing, session persistence, JSON/human
  rendering, hidden helper subcommands. Keep it thin.

M1 is Linux/WSL2 only. M4 adds the native Windows snapshot-editing preview in
`cowbox-core/src/win`; MCP proxying, macOS, ReFS block cloning, network
isolation, and multi-agent racing remain separate milestones.

## 2. Non-negotiable invariants

1. **No silent fallbacks.** If OverlayFS, `userxattr`, user namespaces, or
   Landlock are unavailable, return a structured error with the exact missing
   capability. Never copy the whole tree as a "best effort" substitute.
2. **No raw `libc` FFI and no `unsafe` blocks in project code.** Use `rustix`,
   the `landlock` crate, and safe standard-library APIs. If a primitive is
   missing, stop and escalate; do not hand-write syscall bindings.
3. **`lstat` everywhere.** Walk and diff with `symlink_metadata`; never
   dereference symlinks inside `node_modules`, `.venv`, or user projects.
4. **Only scan the upper layers.** Diff must inspect the writable layers and
   the base manifest. Never walk or hash the read-only lowerdir as a shortcut.
5. **Whiteout compatibility.** On this WSL2 kernel the whiteout is a `0/0`
   character device. Some environments use `user.overlay.whiteout` xattrs.
   Support both. Also honor `user.overlay.opaque`.
6. **Unprivileged OverlayFS.** Use a user namespace plus mount namespace, write
   `deny` to `/proc/self/setgroups`, map uid/gid to the host user, and mount
   with `userxattr`. The writable upper/work layers must live on ext4 (or
   another xattr-capable Linux filesystem), never on 9p.
7. **PID isolation.** The supervisor is PID 1 in a new PID namespace, mounts a
   private procfs, and reaps all remaining children before the step is frozen.
   Background processes must not escape into the host process table.
8. **Landlock fail-closed.** Apply a deny-by-default path policy after mounting
   procfs and before spawning the command. Landlock ABI 3 has no network rules;
   do not pretend network isolation exists.
9. **Atomic commit ordering.** Delete removed paths in reverse depth order,
   create directories top-down, write regular files through same-directory temp
   files plus rename, and recreate symlinks last. Keep a journal and restore on
   failure. The persisted `applied` phase is the commit point: recovery before
   it restores the host, while recovery at or after it finalizes the commit.
   Retain backups until the host restore or CLI session finalization has been
   persisted.
10. **Host read-only invariant.** During a session the host workspace is
    treated as read-only. The base lowerdir is live; `commit` must detect host
    drift against the base manifest and refuse unless `--force` is explicit.
11. **Never commit `.git`.** Even `--include-excluded` must not write VCS
    metadata. Refuse device nodes, FIFOs, and sockets. Clear setuid/setgid bits.
12. **No fake data.** Benchmarks, test results, and demo output must be
    reproducible. "TBD" is acceptable; invented numbers are not.
13. **Windows storage is explicit.** The native backend uses
    `ntfs_materialized_copy`. Report that mode in doctor/init JSON; never imply
    NTFS block cloning. Refuse non-NTFS source or state volumes.
14. **Windows traversal is no-follow.** Walk with `symlink_metadata` and refuse
    symlinks, junctions, and reparse points. Verify every write path stays
    inside the declared workspace or session root.
15. **Windows commit is journaled.** Bind session and step, re-check the host
    baseline under the session lock, write same-directory temp files, use a
    persisted `applied` phase as the commit point, and retain backups until the
    host restore or session finalization is persisted. Preserve mid-commit user
    bytes under the journal's `conflicts/` directory.
16. **Windows snapshots are incremental.** Import once into a content-addressed
    object store; checkpoints use `cowbox-dag` and store only changed entries.
    Diff reads the DAG. Do not rescan the host as a shortcut for snapshot
    content.
17. **Windows file fidelity is explicit.** Preserve read-only attributes and
    ADS on modified files. Refuse modified hardlinks, explicit ACL replacement,
    device files, and reparse points before write-back. `.git` is never
    imported or committed.
18. **Windows recovery is testable.** Keep deterministic fault injection for
    interrupted apply, interrupted recovery, mid-commit host edits, and wrong
    sessions. A skipped recovery test is not a passing recovery test.

## 3. Linux implementation notes

- `cowbox run` re-execs the binary as a hidden helper. The helper unshares the
  user namespace, signals readiness through pre-opened file descriptors, waits
  for the parent to write uid/gid maps, then unshares mount and PID namespaces.
- Do not try to open new paths between `unshare(CLONE_NEWUSER)` and the uid/gid
  map write; the process has an unmapped uid at that point.
- The overlay option string is
  `lowerdir=<head>:...:<base>,upperdir=<step upper>,workdir=<step work>,userxattr`.
  Reject paths containing `:` or `,` with an actionable error.
- The CLI must never silently run without Landlock. If the policy cannot be
  applied, the command does not run.
- The command's environment must be sanitized: unset `WSL_INTEROP`, `WSLENV`,
  `LD_PRELOAD`, `LD_LIBRARY_PATH`, `BASH_ENV`, `ENV`, `PYTHONPATH`, and
  `NODE_OPTIONS`; strip `/mnt/c` and other Windows entries from `PATH`.
- Redirect `HOME`, `TMPDIR`, `XDG_CACHE_HOME`, `CARGO_HOME`, and cache variables
  into the session directory when cache isolation is enabled.

### Windows native notes

- `cowbox win` runs natively and must not delegate to `wsl.exe`. Existing
  `run|diff|log|rollback|commit|mcp-proxy` commands keep their WSL delegation.
- The session lives under `<state-root>/win/sessions/<id>` and contains a
  `current` junction, generation directories, `base.json`, per-step deltas,
  content-addressed objects, a session lock, and commit journals.
- Use `junction::delete` followed by removing the leftover empty directory
  before recreating a junction. A busy `current` path may fall back to a
  returned `workspace-<n>` path; never report a stale path as live.
- Replace existing files by copying the host file into the same-directory temp
  first (preserves ADS), truncating and writing the object bytes, verifying the
  hash, then renaming. Clear read-only only around the replacement and restore
  the target attribute.
- Explicit DACL entries are detected with `windows-acl`; inherited ACLs are
  allowed. Hardlink counts come from `winapi-util` file information.

## 4. Repository and build rules

- Source may live under `/mnt/e/project/cowbox` while explicitly requested, but
  **`CARGO_TARGET_DIR` must be on ext4**, for example
  `$HOME/.cache/cowbox-target`.
- Integration fixtures must be created under `/tmp` (ext4), never under
  `/mnt/e` or another 9p mount.
- Keep the state root outside the workspace and on an xattr-capable filesystem.
- Windows builds and tests use one NTFS target directory on D: and disposable
  fixtures outside the source tree. Do not create Windows target directories
  under `E:\project`.
- All source files use LF. `.gitattributes` enforces it.
- Public APIs live in `cowbox-core/src/model.rs`, `cowbox-core/src/lib.rs`, and
  `cowbox-dag/src/lib.rs`. Change them deliberately, update all call sites, and
  record the reason in the commit or a design note.

## 5. Verification protocol

Before claiming a task is complete:

1. `cargo fmt --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `COWBOX_REQUIRE_SANDBOX=1 cargo test -p cowbox-cli --test adversarial_suite -- --test-threads=1`
5. Run the real CLI smoke path in a `/tmp` fixture:
   `bash scripts/smoke.sh` (`init → run → diff → rollback → commit --dry-run`).
6. If you touched the engine, run the relevant Criterion benchmark and update
   `docs/BENCHMARKS.md` with measured values only.
7. On Windows, also run:
   `cargo clippy -p cowbox-cli --bin cowbox -- -D warnings`,
   `cargo test -p cowbox-cli --test win_native -- --test-threads=1`, and
   `py -3 scripts/acceptance_win_native.py ...` for 1k/10k and cross-volume
   acceptance.

Tests that require namespaces must fail loudly in CI when the capability is
missing. A skipped security test is not a passing security test.

## 6. Communication

Code, comments, commit messages, and public documentation are English. When
reporting to the maintainer in chat, use Chinese. Keep reports factual: files
changed, commands run, tests observed, risks remaining.

## 7. Milestone boundaries

- M1 excludes MCP proxy, native Windows, macOS, network namespace policy, the
  multi-agent `race` command, and the eval leaderboard.
- M4 currently ships only the native Windows NTFS materialized-copy preview.
  ReFS block cloning, Job Object process tracking, macOS, and the eval
  leaderboard remain roadmap items.

Document these as roadmap items rather than implementing partial versions that
look finished.

## 8. Agent integration

- `cowbox setup` installs only the Windows launcher and WSL engine. It must not
  install a skill or modify agent harness directories.
- `cowbox install-skill` is the explicit opt-in path. It must write into
  `skills/cowbox/` under each harness root, never the harness root itself.
- Default trigger policy is `ask`; `explicit` and `auto` are optional modes.
  `commit` always requires human approval.
- Project rules are idempotent marker blocks in `AGENTS.md`, `CLAUDE.md`, and
  `.cursor/rules/cowbox.mdc`; preserve content outside the markers.
- Commands routed through cowbox are covered by the Linux/WSL2 engine. Codex
  Desktop's built-in editor is covered only when it is opened on the managed
  workspace returned by `cowbox win init`/`rollback`. Editing the original host
  project directly is outside the preview boundary.
