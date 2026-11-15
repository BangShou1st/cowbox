# cowbox security model

## 1. Position

cowbox is a **workspace safety net**, not a hardened multi-tenant sandbox. It
is designed for a developer running their own agent against their own project.
It reduces the blast radius of mistakes and gives the developer a deterministic
way to inspect and undo them.

## 2. Assets

- The host workspace: source, untracked files, ignored files, lockfiles, local
  databases, and build outputs.
- The developer's home directory and credentials.
- The integrity of the host after `commit`.
- The audit trail of what the agent did.

## 3. Trust boundaries

| Boundary | Mechanism | M1 strength |
| --- | --- | --- |
| Workspace vs. rest of host | Landlock path rules | Strong for normal file access |
| Sandbox process tree vs. host processes | PID namespace + PID 1 supervisor | Strong for ordinary children |
| Command filesystem view vs. host mutations | OverlayFS mount namespace | Strong while the host is not edited |
| Command environment vs. host environment | env sanitization + redirected HOME/TMPDIR/cache | Best effort; explicit allowlists matter |
| Network | none | **Not isolated** |
| Kernel | none | Out of scope for M1 |
| Host commit vs. partial writes | journal + backups + atomic renames | Strong for ordinary files |

## 4. Landlock policy

The supervisor applies a deny-by-default Landlock ruleset before executing the
command:

- Read/execute: Linux system trees (`/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`,
  `/etc`, `/proc`, `/sys`, `/run`, `/opt`, `/snap` where present).
- Read/write: the overlay merged workspace, session `home`, `tmp`, and `cache`.
- Read/write: a small allowlist of device files such as `/dev/null`,
  `/dev/zero`, `/dev/full`, `/dev/random`, `/dev/urandom`, `/dev/tty`, and
  `/dev/pts`.
- Extra paths come only from explicit `--allow-read` / `--allow-write` flags or
  config. `--allow-write` is a direct host write and is not snapshotted.

If Landlock cannot be applied, cowbox fails closed. It never runs the command
without the fence.

## 5. Namespace and process controls

- New user namespace with the host uid/gid mapped to namespace root.
- New mount namespace so the overlay and private procfs are private.
- New PID namespace; the supervisor is PID 1 and reaps every remaining child
  before the step is frozen.
- The command's `PATH` is rebuilt from Linux directories; Windows entries are
  removed.
- `WSL_INTEROP`, `WSLENV`, dynamic-loader variables, shell startup hooks, and
  Python/Node injection variables are cleared by default.

## 6. Commit safety

The base lowerdir is the live host workspace. cowbox therefore treats the host
as read-only during a session:

- At session start it records path, kind, mode, size, mtime, and symlink target
  in a base manifest.
- At commit it compares the affected host paths against that manifest.
- A mismatch is a conflict. `--force` still copies the current host file into
  the journal before replacing it.
- Every write goes through a same-directory temp file and rename.
- `.git` is never written. Device/FIFO/socket entries are refused. setuid and
  setgid bits are cleared.

Limitations: metadata drift detection is not content hashing for the entire
tree; hardlinks and xattrs are not preserved in M1; a host file edited with
identical size and mtime can evade the check.

## 7. Known gaps

1. **Network access is unrestricted.** A command can connect out, bind host
   ports, or exfiltrate files it is allowed to read. M2 adds a network policy.
2. **Live lowerdir.** External host edits are visible in the sandbox and are
   detected only at commit (metadata-level).
3. **Kernel attack surface.** User namespaces and OverlayFS are kernel code; a
   kernel exploit is outside the M1 threat model.
4. **Secrets in allowed paths.** If a path is explicitly allowed, the command
   can read it. Prefer targeted `--allow-read` over broad home access.
5. **Not a malware sandbox.** Do not run untrusted binaries and assume cowbox
   contains them.
6. **No network audit counts.** Landlock denials are silent; `stats` reports
   blocked writes as not instrumented.

## 8. Reporting

Use GitHub private vulnerability reporting on this repository. Do not disclose
details in a public issue. Include the cowbox version, kernel, filesystem,
`cowbox doctor --json` with secrets removed, and a minimal reproduction.
