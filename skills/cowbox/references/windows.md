# Windows and WSL

cowbox M1.5 runs its engine inside WSL2. The project can stay on a Windows drive
such as `E:\project\app`; `cowbox.exe` invokes the Linux binary through
`wsl.exe --cd`, which maps the project to `/mnt/e/project/app`.

- Keep `upperdir` and state on WSL ext4; never put `COWBOX_HOME` on 9p.
- Use `cowbox path E:\project\app` when a tool needs an explicit WSL path.
- SQLite and heavy small-file workloads can behave differently over 9p; run a
  small `cowbox run -- true` and `doctor` before a large migration.
- Native Windows file-edit interception is not part of M1.5. Codex Desktop's
  built-in editor still bypasses the WSL sandbox; use Codex CLI inside
  `cowbox run -- codex`, or wait for the native Windows backend.
