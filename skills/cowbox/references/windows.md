# Windows and WSL

`0.7.0-m4` has two Windows paths:

- `cowbox win ...` runs natively on Windows for editor saves.
- `cowbox run|diff|log|rollback|commit|mcp-proxy ...` still runs the engine
  inside WSL2. The project can stay on `E:\project\app`; `cowbox.exe` invokes
  the Linux binary through `wsl.exe --cd`, mapping it to `/mnt/e/project/app`.

## Native Windows snapshot editing

Use this path when the user edits a project in Codex Desktop or another Windows
editor and wants checkpoint/diff/rollback/commit protection:

```powershell
cowbox win doctor --workspace E:\project\app --state-root D:\cowbox-state\app --json
cowbox win init --workspace E:\project\app --state-root D:\cowbox-state\app --json
# Open data.managed_workspace in the editor.
cowbox win checkpoint --session <id>
cowbox win diff --session <id>
cowbox win rollback <step> --session <id>
cowbox win checkpoint --session <id>
cowbox win commit --dry-run --session <id>
# After explicit human approval:
cowbox win commit --session <id>
```

Rules:

1. Edit only the returned managed workspace. Editing the original
   `E:\project\app` directly bypasses the snapshot.
2. `win rollback` returns the live managed path. Reopen that path if it changed
   from `current` to `workspace-<n>`.
3. `win commit` is a host write. Inspect `win diff` and `win commit --dry-run`;
   never run `--yes` without explicit human approval.
4. If commit reports `WIN_COMMIT_RECOVERY_REQUIRED`, stop editing and run
   `cowbox win commit --recover --session <id> --yes`. Inspect any `conflicts/`
   files before retrying.
5. Hardlink modification, reparse points, and explicit ACL replacement are
   refused. Do not try to force them through cowbox; fix the file or keep it
   outside the managed workspace.

`win doctor` reports `ntfs_materialized_copy`; the initial import uses roughly
twice the source bytes. Checkpoints store only changed objects and a small DAG
delta.

## WSL2 notes

- Keep `upperdir` and state on WSL ext4; never put `COWBOX_HOME` on 9p.
- Use `cowbox path E:\project\app` when a tool needs an explicit WSL path.
- SQLite and heavy small-file workloads can behave differently over 9p; run a
  small `cowbox run -- true` and `doctor` before a large migration.
- Codex Desktop's built-in editor is protected only when it is opened on the
  `cowbox win` managed workspace. The WSL engine still protects only commands
  routed through cowbox.
- `cowbox mcp-proxy` must be launched through the real `cowbox.exe`; the
  launcher forwards the persistent stdio stream to WSL. The upstream command
  after `--` must be a Linux command. Windows-native MCP executables are not
  supported by the M3 proxy.
