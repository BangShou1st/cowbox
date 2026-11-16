# Windows

`0.7.0-m4` adds a **native Windows snapshot-editing preview**. There are two
independent Windows paths:

- `cowbox.exe win ...` runs entirely on Windows. It materializes a managed
  copy of a project on NTFS and protects edits made in Codex Desktop or another
  editor before a journaled write-back.
- The existing `cowbox.exe run|diff|log|rollback|commit|mcp-proxy ...`
  commands still delegate to the Linux engine through `wsl.exe`.

The native path is a file-write-back guard, not a process or network sandbox.
It is designed for the common local workflow: initialize a managed workspace,
open it in the editor, save changes, checkpoint, inspect, roll back, and commit
only the reviewed state.

## Requirements

- Windows 11 Home or Pro; a non-administrator account is supported.
- NTFS for both the source workspace and the state root. ReFS, FAT, exFAT,
  network shares, and removable media are refused with a structured error.
- A local state root outside the project, for example
  `D:\cowbox-state\project-a`.
- Codex Desktop or any editor that can open a normal directory. No filter
  driver, service, administrator privilege, or Developer Mode is required.

Run the capability check first:

```powershell
cowbox win doctor --workspace E:\project\app --state-root D:\cowbox-state\app --json
```

`win doctor --json` reports the selected storage mode
(`ntfs_materialized_copy`), filesystem type, free space, long-path support,
DACL inspection, and whether the state root is writable. `ready=false` includes
the exact blockers; nothing falls back silently.

## Native workflow

```powershell
cd E:\project\app
cowbox win init --workspace E:\project\app --state-root D:\cowbox-state\app --json
# Open data.managed_workspace in Codex Desktop, then edit and save.

cowbox win checkpoint --session <id>
cowbox win diff --session <id>
# If the edit is wrong:
cowbox win rollback <step> --session <id>
# Edit the returned managed workspace, then:
cowbox win checkpoint --session <id>
cowbox win commit --dry-run --session <id>
cowbox win commit --session <id>          # interactive human confirmation
cowbox win commit --session <id> --yes    # only after explicit approval
cowbox win discard --session <id> --yes
```

`win init` returns:

- `session_id`;
- the original absolute workspace path;
- `managed_workspace`, the path to open in the editor;
- the generation path, storage mode, file counts, source bytes, estimated
  usage, and available bytes.

`win log` and `win diff` can find the single active session registered for the
current project; `--session` is still available for scripts. `win rollback`
returns the actual managed path to continue using. The session keeps a stable
`current` junction when NTFS allows the switch; if the junction is busy, the
returned `workspace-<n>` path is the live one.

## Storage model

The preview uses an **explicit NTFS materialized copy**, visible in every JSON
report:

- `.git`, `.cowbox`, `target`, `node_modules`, `.venv`, `__pycache__`, `.next`,
  and `dist` are excluded from import, diff, and commit.
- The initial import writes a full managed copy plus a content-addressed object
  store. The free-space check estimates approximately twice the source bytes
  plus 64 MiB of overhead.
- Every checkpoint reuses the `cowbox-dag` step graph. It stores only a small
  step delta and the changed file objects; unchanged files are deduplicated by
  SHA-256.
- `win diff` reads the DAG, not the live host. `win rollback` materializes the
  target step into a new generation; old generations remain until `discard`.
- `win discard` removes the session's managed files, objects, journals, and
  index entry, and reports the bytes freed.

This is not a claim of millisecond Windows setup. A 10,000-file import is
dominated by the full copy, hashing, and verification; measured preview numbers
are recorded in [docs/BENCHMARKS.md](BENCHMARKS.md).

## File and path support

| Item | Behavior |
| --- | --- |
| Regular files and directories | Supported, including empty directories. |
| Untracked and ignored project files | Imported unless they match an exclude. `.git` is always excluded. |
| Unicode names | Supported for well-formed UTF-16 names, including non-ASCII and emoji. Invalid surrogate names are refused. |
| Long paths | Supported through Rust's verbatim path handling; the doctor probe verifies a path over 260 characters. |
| Case-only rename | Checkpoint records a delete plus an add; commit deletes the old name before writing the new case. |
| Read-only files | Preserved. A read-only host file is made writable only for replacement, then the target attribute is restored. |
| Alternate data streams | Preserved for modified existing files: the host file is copied into the same-directory temp file before new bytes are written. ADS contents are not diffed. |
| Hardlinks | Imported and materialized as independent copies. A commit that would replace or modify a hardlinked file is refused before write-back with `WIN_HARDLINK_UNSUPPORTED`; deleting one link is allowed. |
| Symlinks, junctions, reparse points | Refused with `WIN_REPARSE_UNSUPPORTED`. Traversal never follows them. |
| Explicit ACLs | A modified file with non-inherited DACL entries is refused with `WIN_ACL_UNSUPPORTED` before write-back. Inherited ACLs are normal. |
| Device files, FIFOs, sockets | Refused as unsupported file types. |
| `.git` | Never imported, diffed, or written, even if a checkpoint tries to add it. |

## Commit and recovery

A commit binds the session, step, host baseline, and preview target:

1. The CLI takes the session lock and scans the host against the base manifest.
   Drift is refused unless `--force` is explicit; `--force` still backs up the
   actual host bytes.
2. A journal is written under the session's `commits/` directory with the
   operation list and target manifest hash. Before every host mutation the
   journal records the active operation, so recovery can reconcile an
   operation whose bytes landed but whose completion counter did not.
3. Existing bytes are copied into a journal backup with SHA-256 verification.
4. Removed paths are deleted in reverse depth order, new directories are
   created top-down, and files are written through a same-directory temp file
   followed by a `MoveFileEx`-backed rename. A busy delete or replace restores
   the original read-only attribute before returning the error.
5. Before each operation the host path is re-checked. A mid-commit edit is
   copied to `conflicts/`, already-applied operations are restored from
   backups, and the user's conflicting file is left in place.
6. After all operations, the host is compared with the target manifest. Only
   then is the journal phase set to `applied`; that persisted phase is the
   commit point.
7. Session finalization is persisted before backup payloads are pruned.

If the process stops before `applied`, `win commit --recover --session <id>`
restores the pre-commit host, including an operation that was written before
its completion count was persisted. Recovery checks the target hash, backup,
and current node type before restoring each operation, so writes, deletes, and
directory creation are all idempotent. If it stops at or after `applied`,
recovery verifies the target and finalizes the session. File-busy, host-drift,
and ACL/hardlink errors keep the journal and recovery material for inspection.

`WIN_FAULT_ABORT` with exit code 70 is reserved for deterministic test
injection; a normal user should run the `--recover` command it prints.

## Existing WSL2 path

The WSL engine and launcher are unchanged:

```powershell
cowbox setup
cowbox doctor
cowbox run -- npm test
cowbox mcp-proxy --network off -- python3 mcp-server.py
```

`setup` installs the Windows launcher and the Linux engine in WSL. It does not
install a skill. The project can stay on `E:\...`; the launcher maps it to
`/mnt/e/...`. State for the WSL engine remains on WSL ext4.

## Boundary

Native `cowbox win` protects edits inside the managed directory and the
journaled write-back to the original project. It does **not**:

- isolate processes, network access, credentials, or reads outside the
  managed directory;
- protect edits made directly to the original host directory while a session
  is active;
- preserve explicit ACLs on replaced files (it refuses instead);
- clone NTFS extents or provide ReFS block cloning;
- support macOS.

Codex Desktop's built-in editor is covered only when it is pointed at the
returned managed workspace. Editing the original project directly bypasses the
snapshot. That is the product boundary for this preview.
