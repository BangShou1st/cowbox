# Windows and WSL

cowbox M3 runs its engine inside WSL2. The project can stay on a Windows drive
such as `E:\project\app`; `cowbox.exe` invokes the Linux binary through
`wsl.exe --cd`, which maps the project to `/mnt/e/project/app`.

- Keep `upperdir` and state on WSL ext4; never put `COWBOX_HOME` on 9p.
- Use `cowbox path E:\project\app` when a tool needs an explicit WSL path.
- SQLite and heavy small-file workloads can behave differently over 9p; run a
  small `cowbox run -- true` and `doctor` before a large migration.
- Native Windows file-edit interception is not part of M3. Codex Desktop's
  built-in editor still bypasses the WSL sandbox; only commands routed through
  cowbox are protected until the native Windows backend lands.
- `cowbox mcp-proxy` must be launched through the real `cowbox.exe`; the
  launcher forwards the persistent stdio stream to WSL. The upstream command
  after `--` must be a Linux command. Windows-native MCP executables are not
  supported by the M3 proxy.
