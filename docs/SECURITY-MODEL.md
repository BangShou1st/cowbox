# cowbox security model

## 1. Position

cowbox is a **workspace safety net**, not a hardened multi-tenant sandbox. It
is designed for a developer running their own agent against their own project.
It reduces the blast radius of mistakes and gives the developer a deterministic
way to inspect and undo them.

The M4 native Windows preview extends this position to editor saves: the editor
is pointed at a managed NTFS copy, and only `cowbox win commit` writes the
original project. It does not turn the Windows editor process into a sandboxed
process.

## 2. Assets

- The host workspace: source, untracked files, ignored files, lockfiles, local
  databases, and build outputs.
- The developer's home directory and credentials.
- The integrity of the host after `commit`.
- The audit trail of what the agent did.

## 3. Trust boundaries

| Boundary | Mechanism | Strength |
| --- | --- | --- |
| Workspace vs. rest of host | Landlock path rules | Strong for normal file access |
| Sandbox process tree vs. host processes | PID namespace + PID 1 supervisor | Strong for ordinary children |
| Command filesystem view vs. host mutations | OverlayFS mount namespace | Strong while the host is not edited |
| Command environment vs. host environment | env sanitization + redirected HOME/TMPDIR/cache | Best effort; explicit allowlists matter |
| Host cache data vs. session cache writes | read-only OverlayFS lower + session upper | Strong for configured data directories |
| Network IP connectivity | network namespace when policy is `off` | Isolated for IPv4/IPv6 TCP/UDP; `host` remains unrestricted |
| Filesystem Unix sockets | Landlock path rules | IPC boundary remains visible when its path is allowed |
| MCP agent vs. upstream server | child sandbox process + stdio JSON-RPC proxy | Snapshot and process boundary; protocol content is not audited |
| Kernel | none | Out of scope for M3 |
| Host commit vs. partial writes | journal + backups + atomic renames | Strong for ordinary files |
| Windows editor vs. host project | managed NTFS copy + journaled `win commit` | Strong for file write-back inside the managed workspace |
| Windows process/network vs. host | none | Out of scope for the preview |
| Windows ACL/hardlink/reparse fidelity | explicit refusal or preservation path | Fail-closed for unsupported cases |

The MCP proxy starts a fresh upstream process inside a cowbox step for every
`tools/call`. It does not keep a privileged or unsandboxed upstream alive.
Control tools can inspect, roll back, or preview a commit, but cannot write the
host. The upstream server's tool descriptions, returned content, and any data it
sends to the agent are not security-reviewed by cowbox; install only servers
you would run as local project tooling.

## 4. Landlock policy

The supervisor applies a deny-by-default Landlock ruleset before executing the
command:

- Read/execute: Linux system trees (`/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`,
  `/etc`, `/proc`, `/sys`, `/run`, `/opt`, `/snap` where present).
- Read/write: the overlay merged workspace, session `home`, `tmp`, and `cache`.
- Read-only: configured Cargo `registry/`/`git/`, npm `_cacache/`, and pip cache
  data directories, each mounted below the session cache.
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
- For `--network off`, a new network namespace created after UID/GID mapping
  and OverlayFS setup. It has no host interfaces and leaves loopback down.
- New PID namespace; the supervisor is PID 1 and reaps every remaining child
  before the step is frozen.
- The command's `PATH` is rebuilt from Linux directories; Windows entries are
  removed.
- `WSL_INTEROP`, `WSLENV`, dynamic-loader variables, shell startup hooks, and
  Python/Node injection variables are cleared by default.

## 6. Host cache controls

Host cache reuse is opt-in per tool in `.cowbox.toml`. cowbox limits each mount
to the tool's cache data, never the whole home or tool configuration:

- Cargo: `registry/` and `git/` under the selected `CARGO_HOME`.
- npm: `_cacache/` under the selected npm cache root.
- pip: the selected pip cache root; an optional `wheelhouse/` is exposed through
  `PIP_FIND_LINKS`.

Each source is a read-only OverlayFS lower layer. The writable upper and work
directories live under the session state root, so index refreshes, downloads,
and extracted cache entries copy up into session-owned storage. Landlock grants
the underlying source paths read-only access. Credentials and configuration
files such as `credentials.toml`, `.npmrc`, and pip config are not mounted and
remain outside the redirected session home.

The host cache is a trusted local data source. A malicious cache entry can
influence the tool just as a normal host cache could; cowbox verifies package
hashes only when the underlying tool is asked to do so.

## 6.1 Network policy

The default policy is `host`. The CLI override precedes project configuration,
which precedes the default. `off` removes IP connectivity by entering a new
Linux network namespace and does not fall back to the host namespace on error.

The policy isolates IP traffic, not all IPC. A filesystem-backed Unix-domain
socket remains reachable when its path is readable or writable under the
Landlock policy. This is intentional: build daemons and local tooling often use
filesystem sockets. Use narrower Landlock paths when that IPC channel must be
blocked.

The policy is not a firewall. It cannot allow a specific destination, filter
packets, or provide per-protocol rules. IPv6 and IPv4 are both unavailable in
the private namespace because only a down loopback interface exists.

## 7. Commit safety

The base lowerdir is the live host workspace. cowbox therefore treats the host
as read-only during a session:

- At session start it records path, kind, mode, size, mtime, inode, link count,
  `user.*` xattr digest, and symlink target in a base manifest.
- At commit it compares the affected host paths against that manifest.
- A mismatch is a conflict. `--force` still copies the current host file into
  the journal before replacing it.
- The CLI asks for confirmation in a terminal. Non-interactive callers must
  use `--yes` only after explicit human approval.
- SQLite databases are committed with their `-wal`, `-shm`, and `-journal`
  sidecars when a standard SQLite header is detected. Stale host sidecars are
  removed when the sandbox has no matching upper-layer sidecar.
- Hardlinks are preserved among files in the same commit set. `user.*` xattrs
  are preserved; other xattr namespaces are left untouched.
- Commit journals record `prepared`, `applying`, `restoring`, `applied`,
  `complete`, and `rolled_back` phases. The durable `applied` marker is the
  commit point. Recovery before that point restores the host and leaves the
  original session active; recovery at or after it verifies the post-state,
  marks the session committed, clears the active pointer, and only then marks
  the journal complete.
- Journal backups combine an independent content copy with an original-inode
  hardlink where the filesystem allows it. The copy remains stable if the host
  is edited, while the hardlink lets rollback reconnect pre-existing host
  links. Backups are retained until the restored manifest and journal phase,
  or the committed session record and cleared active pointer, are persisted.
- `commit --recover` validates journal metadata, operation paths, backup
  placement, and every affected path against its recorded pre- or post-state.
  It refuses to overwrite external edits and preserves the journal and backups
  for inspection. `gc` skips sessions with an unfinished commit journal.
- Every write goes through a same-directory temp file and rename.
- `.git` is never written. Device/FIFO/socket entries are refused. setuid and
  setgid bits are cleared.

Limitations: metadata drift detection is not content hashing for the entire
tree; a host file edited with identical size, mtime, inode, and xattrs can
evade the check. After a successful commit, hardlinks to host files outside the
commit set become separate inodes; rollback uses hardlink backups when the
workspace and state filesystems allow it. Non-`user.*` xattrs are not copied.
Recovery guards individual paths; external edits inside a directory restored as
a whole may still require manual inspection. The journal is fsynced, but the
recovery contract covers process interruption only. It is not a power-loss,
kernel-crash, or storage-failure guarantee.

### 7.1 Native Windows commit

`cowbox win commit` binds the session id and target step to one journal. The
session lock covers the host baseline scan, preview, backup, apply, and session
write. The persisted `applied` phase is the commit point:

- before `applied`, recovery restores the pre-commit host from journal backups;
- at or after `applied`, recovery verifies the target and finalizes the session;
- recovery is idempotent and may itself be interrupted;
- a mid-commit host edit is copied to `conflicts/`, already-applied operations
  are restored, and the user's file is left in place;
- file-busy, drift, ACL, hardlink, and verification failures retain the journal
  and backups.

The preview is a file-integrity mechanism, not a power-loss guarantee. NTFS
metadata operations and same-directory renames are used to keep readers from
seeing torn file content, but the product does not claim ReFS block cloning,
Job Object process control, or hostile-code containment.

## 8. Native Windows boundary

The managed workspace is a normal NTFS directory. Codex Desktop, another
editor, or a shell can read and write it. cowbox protects the **write-back**:

- `.git` is never imported, diffed, or committed.
- Reparse points are refused during traversal.
- Modified hardlinks and explicit ACL replacement are refused before write.
- Read-only attributes and ADS are preserved for replaced files.
- Host drift is detected against the base manifest; `--force` still backs up
  the actual host bytes.

Reads outside the managed directory, process behavior, network access,
credentials, and edits made directly to the original host project are outside
this boundary. Use the Linux/WSL2 engine when process and network isolation are
required.

## 9. Known gaps

1. **Network access defaults to the host.** A command can connect out, bind host
   ports, or exfiltrate files it is allowed to read unless the caller selects
   `run --network off`. The `off` policy isolates IP connectivity but does not
   filter destinations or isolate filesystem-backed Unix sockets.
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

## 10. Reporting

Use GitHub private vulnerability reporting on this repository. Do not disclose
details in a public issue. Include the cowbox version, kernel, filesystem,
`cowbox doctor --json` with secrets removed, and a minimal reproduction.
