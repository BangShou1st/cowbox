# cowbox architecture

## 1. Goals

cowbox makes local workspace mutations by an AI coding agent cheap to inspect
and cheap to reverse. It does this without a VM, without a container image, and
without replacing the developer's toolchain.

The M1 design has four properties:

1. **Millisecond setup.** The workspace is a read-only lowerdir; each step adds
   a writable upperdir on ext4.
2. **Deterministic state.** Every command invocation becomes a numbered DAG
   node. Rollback is a pointer move, not a copy.
3. **Host safety.** Writes outside the workspace are denied by Landlock and
   background processes are reaped in a PID namespace.
4. **Auditable commit.** The host is changed only through a journaled,
   drift-checked, topologically ordered commit.

## 2. Non-goals for M1

- Network isolation or a network namespace.
- A native Windows or macOS backend.
- MCP proxying or agent-specific protocol integration.
- Multi-agent parallel racing.
- Hostile-code containment equivalent to a VM.

## 3. Components

```text
┌─────────────────────────────────────────────────────────────────┐
│ cowbox-cli                                                      │
│ clap commands · session store · JSON/human rendering · helper   │
└───────────────┬─────────────────────────────────┬───────────────┘
                │                                 │
┌───────────────▼─────────────────┐  ┌────────────▼──────────────┐
│ cowbox-core                     │  │ cowbox-dag                │
│ LinuxBackend                    │  │ StepNode / SessionDag     │
│ namespaces · OverlayFS          │  │ rollback · undo · branch  │
│ Landlock · supervisor           │  │ validation · serde        │
│ diff · commit journal           │  └───────────────────────────┘
└───────────────┬─────────────────┘
                │
┌───────────────▼─────────────────────────────────────────────────┐
│ cowbox state                                                    │
│ workspaces/<hash>.json · sessions/<id>/...                      │
└─────────────────────────────────────────────────────────────────┘
```

### `cowbox-dag`

Pure in-memory and serializable state. It knows nothing about mounts, PIDs, or
the filesystem. `SessionDag` owns a `BTreeMap<u64, StepNode>`, a `head_step`, and
the workspace root. `add_step` enforces sequential ids and a parent equal to the
current head. `rollback` validates the target and moves the head. `undo` walks
parent links. `active_layer_chain` returns the root-to-head layer ids for
OverlayFS.

### `cowbox-core`

The engine. It owns the `SandboxBackend` trait, Linux namespace setup, overlay
mounting, Landlock policy, diff, commit, and capability probing. The rest of
the workspace must not call syscalls directly.

### `cowbox-cli`

Argument parsing, workspace discovery, state persistence, locks, rendering, and
the hidden `__sandbox-exec` / `__supervisor` helper entry points. The CLI
orchestrates; it does not reimplement the engine.

## 4. State layout

The state root is selected in this order:

1. `$COWBOX_HOME`
2. `$XDG_STATE_HOME/cowbox`
3. `~/.local/state/cowbox`

It must be outside the workspace and on an xattr-capable Linux filesystem.

```text
<state-root>/
├── workspaces/
│   └── <sha256(workspace)[..16]>.json   # active session pointer
└── sessions/
    └── <8-hex-id>/
        ├── session.json                 # DAG + status + workspace
        ├── base/
        │   └── manifest.jsonl           # metadata snapshot at session start
        ├── steps/
        │   └── <n>/
        │       ├── upper/               # writable overlay layer
        │       └── work/                # overlay workdir
        ├── home/                        # redirected HOME
        ├── tmp/                         # redirected TMPDIR
        ├── cache/                       # redirected caches
        ├── cache-overlays/              # session writable halves of host caches
        ├── layers/
        │   ├── compacted/<key>/         # validated physical mount-layer cache
        │   └── tmp/                     # unpublished compaction attempts
        ├── events.jsonl                 # run/rollback/commit/discard audit
        ├── lock                         # flock for workspace mutation
        └── commit/<journal>/            # phase meta, ops, copy + link backups
```

A session is `Active`, `Committed`, or `Discarded`. Only one active session is
allowed per workspace in M1. After a successful commit the active pointer is
cleared; the next `run` creates a new session whose base is the updated host.

## 5. Run lifecycle

```mermaid
sequenceDiagram
    participant CLI
    participant Helper as __sandbox-exec
    participant Sup as PID 1 supervisor
    participant Cmd as user command
    CLI->>Helper: spawn with step dirs + handshake files
    Helper->>Helper: unshare(CLONE_NEWUSER)
    Helper-->>CLI: ready signal (pre-opened fd)
    CLI->>Helper: write setgroups=deny, uid_map, gid_map
    CLI->>Helper: go signal
    Helper->>Helper: unshare(CLONE_NEWNS)
    Helper->>Helper: mount overlayfs (userxattr)
    Helper->>Helper: unshare(CLONE_NEWPID)
    Helper->>Sup: spawn __supervisor (PID 1)
    Sup->>Sup: mount private procfs
    Sup->>Sup: mount read-only host caches over session caches
    Sup->>Sup: apply Landlock policy
    Sup->>Cmd: spawn command
    Cmd-->>Sup: exit status
    Sup->>Sup: kill/reap remaining namespace processes
    Sup-->>Helper: result JSON
    Helper->>Helper: unmount
    CLI->>CLI: freeze step, append DAG node, persist
```

### Handshake

The child cannot open new paths between `unshare(CLONE_NEWUSER)` and the uid/gid
map write: its uid is unmapped. The helper therefore uses pre-opened file
descriptors for readiness and the "go" signal. The parent writes:

```text
/proc/<child>/setgroups  -> "deny"
/proc/<child>/uid_map    -> "0 <host-uid> 1"
/proc/<child>/gid_map    -> "0 <host-gid> 1"
```

If the maps cannot be written, the helper is killed and the CLI returns an
actionable isolation error. There is no fallback path.

### Overlay mounts

For a chain `[1, 2, 3]` the mount is:

```text
lowerdir=<state>/steps/3/upper:<state>/steps/2/upper:<state>/steps/1/upper:<workspace>
upperdir=<state>/steps/<next>/upper
workdir=<state>/steps/<next>/work
userxattr
```

The lower layers are ordered topmost-first. The base workspace remains the live
host directory. That is why the host must be treated as read-only while a
session is active.

### Layer compaction

The DAG remains entirely logical. Compaction changes only the physical lower
layers passed to OverlayFS when the active branch approaches the 150-layer or
3800-byte `lowerdir` limit.

Before creating a step, core scans existing compaction metadata and reuses a
compacted layer only when its recorded logical source list is an exact prefix of
the active root-to-head chain. When another compaction is required, core:

1. selects the oldest contiguous prefix and keeps up to 32 recent logical
   layers uncompressed;
2. replays only those upper layers, using the base manifest to distinguish
   additions, modifications, type changes, and deletions;
3. materializes the net result in `layers/tmp/`, normalizing whiteouts to
   `user.overlay.whiteout`, preserving opaque directories, modes, symlinks,
   `user.*` xattrs, and hardlink groups;
4. analyzes the temporary result again and compares namespace semantics,
   content hashes, metadata, xattrs, whiteouts, opaque directories, and
   hardlink partitions with the source layers;
5. writes `layer.json` with the source layer ids and base-manifest fingerprint,
   then publishes the directory with an atomic rename.

The session lock serializes compaction with other session mutations. A process
interruption leaves either an unpublished temporary directory or a complete
published layer; the original logical layers are never modified, so the next
run can clean up and retry. `stats` reports logical steps, physical lower layers
(including the live base), compacted-layer count, and allocated compacted bytes.
Finalized sessions include compaction artifacts in normal `gc` removal.

`diff` and `commit` always read `steps/<n>/upper` for the logical chain. They do
not substitute a compacted layer, so historical rollback and branching retain
the original per-step semantics.

### Host cache overlays

When `.cowbox.toml` enables a host cache, the supervisor mounts a second OverlayFS
inside the session mount namespace before applying Landlock:

```text
lowerdir=<host cache data, read-only>
upperdir=<session>/cache-overlays/<tool>/upper
workdir=<session>/cache-overlays/<tool>/work
userxattr
```

The destination is the same path already exported as `CARGO_HOME`, npm's
`NPM_CONFIG_CACHE`, or `PIP_CACHE_DIR`. Only cache data is mounted: Cargo
`registry/` and `git/`, npm `_cacache/`, and the pip cache directory. Host
credentials and configuration files are not part of the mount and remain
outside the sandbox. Any cache write copies up to the session upper layer.
Pip additionally receives a `PIP_FIND_LINKS` path for an optional read-only
`wheelhouse/` under the cache root.

## 6. Diff

The diff engine walks the active upper layers in root-to-head order and applies
their entries to a `BTreeMap<PathBuf, Change>`:

- A regular file, directory, or symlink is an upsert.
- A `0/0` character device is a whiteout.
- A file carrying `user.overlay.whiteout` is a whiteout.
- A directory with `user.overlay.opaque` removes all lower entries below it.
- A later layer always wins over an earlier layer.

At the end, the base manifest decides whether an upsert is `Added` or
`Modified`, and whether a whiteout is a real `Deleted`. Files that were added
and later removed net out. Symlink target changes become `SymlinkUpdated`.
Type changes become `TypeChanged`.

Only upper layers are scanned. The lowerdir is never walked or hashed during a
normal diff.

Text line counts are optional and capped by `max_line_stat_files` and
`max_line_stat_bytes`. Binary files report a size delta instead.

## 7. Commit

`commit` is intentionally conservative:

1. Compute the net diff from base to the target step.
2. Compare every affected host path with the base manifest. If the host
   changed, return `CommitConflict`; `--force` overrides but still backs up.
3. Create a `prepared` journal, copy every affected existing host path into it,
   record pre/post fingerprints, and write an operation manifest. Existing host
   files also get an original-inode hardlink backup when the filesystem allows
   it; the independent copy is the stable recovery source.
4. Apply in topological order:
   - delete removed paths, deepest first;
   - create directories, shallowest first;
   - write regular files to same-directory temporary files, fsync, apply
     `user.*` xattrs, rename;
   - preserve hardlinks among files in the same commit set;
   - recreate or replace symlinks last.
5. Keep SQLite `-wal`/`-shm`/`-journal` sidecars grouped with a changed
   database and delete stale host sidecars that are not present in an upper
   layer.
6. Strip setuid/setgid, preserve mode and mtime where possible, and refuse
   device/FIFO/socket entries.
7. Persist the `applied` phase only after every host write and fsync has
   succeeded. This is the durable commit point.
8. Save the committed session record and clear the active pointer. Only then
   mark the journal `complete` and remove its backup material.

If applying fails before the commit point, the journal moves to `restoring`,
restores the host, refreshes the base manifest, and records `rolled_back` while
keeping backups until those writes are durable. A process interruption at any
point leaves either an incomplete journal or a terminal one. `commit --recover`
checks the recorded operation manifest and pre/post fingerprints:

- `prepared`, `applying`, or `restoring`: restore the host and keep the original
  session active;
- `applied`: leave the committed host content in place, persist the committed
  session state, clear the active pointer, then finalize the journal;
- `complete` or `rolled_back`: no further host action; repeated recovery is a
  no-op.

External edits cause recovery to stop with an explicit error and retain the
journal and backups. The guarantee covers process interruption, not power loss
or storage failure.

`.git` and `.cowbox` are never committed. Default excluded paths such as
`target/`, `node_modules/`, and `.venv/` are ignored unless
`--include-excluded` is explicit.

## 8. Observability

`tracing` spans cover namespace setup, mount, Landlock, diff, and commit.
`--trace-chrome` emits a Chrome Trace JSON file. `doctor` runs static capability
probes (namespace, overlay, `userxattr`, Landlock ABI, state filesystem, disk
space) and reports configured Cargo/npm/pip host cache data sources. The
whiteout kind is confirmed by the first real run, and latency is measured by
`scripts/bench.sh`. `stats` reports logical step count, physical mount layers,
compacted storage, rollback count, reaped orphans, incremental disk use, and
whether network isolation is active.

cowbox does **not** count blocked writes: Landlock denials are silent. The field is
reported as not instrumented rather than invented.

## 9. WSL2 bridge notes

M1 runs as a Linux binary inside WSL2. The Windows GUI integration (`cowbox.exe`
wrapper over `wsl.exe`) is M3. For M1:

- Source may be on `/mnt/e`; upper and work layers must be on ext4.
- `/mnt/e` works as a read-only lowerdir in testing, but 9p has different
  case-folding, symlink, and latency behavior. Acceptance tests use ext4.
- `WSL_INTEROP` must be unset and Windows PATH entries removed before spawning
  the command, otherwise a sandboxed process could invoke Windows binaries.

## 10. Failure modes

| Failure | Behavior |
| --- | --- |
| Kernel lacks user namespaces | `IsolationFailed`; command does not run |
| OverlayFS mount fails | `CowUnsupported`/`IsolationFailed`; no copy fallback |
| Landlock unavailable | `Landlock` error; command does not run |
| State dir on 9p/without xattr | `CowUnsupported` with a path hint |
| Command times out | kill namespace, exit 124, record a step |
| Host file changed during session | commit conflict; `--force` still backs up |
| Commit interrupted | journal restore; manual recovery path if restore fails |
| Whiteout format differs from expectation | both char-device and xattr forms supported |
| Layer compaction interrupted | originals untouched; temporary output is discarded and retried |
| Configured host cache data is missing | actionable config error before the command runs |
