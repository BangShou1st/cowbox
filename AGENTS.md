# cowbox Engineering Constitution

This file is the highest-priority engineering contract for anyone (human or
agent) changing cowbox. If a shortcut conflicts with these rules, the rules win.

## 1. Project shape

cowbox is a local, millisecond-latency Copy-on-Write sandbox for AI coding
agents. The architecture is **Thick Engine, Thin Protocol**:

- `crates/cowbox-core`: Linux namespaces, OverlayFS, Landlock, process
  supervision, whiteout-aware diff, commit journal. This is the engine.
- `crates/cowbox-dag`: pure snapshot DAG. No filesystem or process access.
- `crates/cowbox-cli`: argument parsing, session persistence, JSON/human
  rendering, hidden helper subcommands. Keep it thin.

M1 is Linux/WSL2 only. MCP proxying, native Windows, macOS, network isolation,
and multi-agent racing are later milestones and must not leak into M1 code.

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
   failure.
10. **Host read-only invariant.** During a session the host workspace is
    treated as read-only. The base lowerdir is live; `commit` must detect host
    drift against the base manifest and refuse unless `--force` is explicit.
11. **Never commit `.git`.** Even `--include-excluded` must not write VCS
    metadata. Refuse device nodes, FIFOs, and sockets. Clear setuid/setgid bits.
12. **No fake data.** Benchmarks, test results, and demo output must be
    reproducible. "TBD" is acceptable; invented numbers are not.

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

## 4. Repository and build rules

- Source may live under `/mnt/e/project/cowbox` while explicitly requested, but
  **`CARGO_TARGET_DIR` must be on ext4**, for example
  `$HOME/.cache/cowbox-target`.
- Integration fixtures must be created under `/tmp` (ext4), never under
  `/mnt/e` or another 9p mount.
- Keep the state root outside the workspace and on an xattr-capable filesystem.
- All source files use LF. `.gitattributes` enforces it.
- Public APIs live in `cowbox-core/src/model.rs`, `cowbox-core/src/lib.rs`, and
  `cowbox-dag/src/lib.rs`. Change them deliberately, update all call sites, and
  record the reason in the commit or a design note.

## 5. Verification protocol

Before claiming a task is complete:

1. `cargo fmt --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `COWBOX_REQUIRE_SANDBOX=1 cargo test -p cowbox-core --test adversarial_suite -- --test-threads=1`
5. Run the real CLI smoke path in a `/tmp` fixture:
   `init → run → diff → rollback → commit --dry-run`.
6. If you touched the engine, run the relevant Criterion benchmark and update
   `docs/BENCHMARKS.md` with measured values only.

Tests that require namespaces must fail loudly in CI when the capability is
missing. A skipped security test is not a passing security test.

## 6. Communication

Code, comments, commit messages, and public documentation are English. When
reporting to the maintainer in chat, use Chinese. Keep reports factual: files
changed, commands run, tests observed, risks remaining.

## 7. M1 boundaries

- No MCP proxy or meta-tools in M1.
- No native Windows backend or `cowbox.exe` bridge in M1.
- No macOS backend in M1.
- No network namespace or seccomp network policy in M1.
- No multi-agent `race` command in M1.
- No Eval leaderboard in M1.

Document these as roadmap items rather than implementing partial versions that
look finished.
