# Windows

cowbox M3 ships a native `cowbox.exe` launcher. The engine still runs in
WSL2; the project does not need to move out of `E:\...`.

## Install

```powershell
.\cowbox.exe setup
cowbox doctor
cowbox demo
cowbox demo --offline
```

`setup` downloads the matching Linux release into WSL `~/.local/bin/cowbox`,
copies the Windows launcher into `%USERPROFILE%\.local\bin`, writes
`%APPDATA%\cowbox\config.toml`, and verifies `doctor`. It does **not** install
any agent skill.

`cowbox demo` delegates to the Linux engine, creates its fixture under WSL
`/tmp`, and leaves the current Windows project untouched.

Add `%USERPROFILE%\.local\bin` to `PATH` if `cowbox` is not found. Re-run
`cowbox setup --force` to reinstall.

## Use a real Windows project

```powershell
cd E:\project\your-app
cowbox init --agent
cowbox run -- npm test
cowbox run --network off -- cargo test
cowbox diff
cowbox commit
```

`cowbox.exe` calls `wsl.exe --cd <cwd>`, so the project is visible as
`/mnt/e/project/your-app` inside WSL. State remains on WSL ext4.

## MCP stdio forwarding

`cowbox.exe mcp-proxy ...` uses inherited stdin, stdout, and stderr instead of
the buffered command path. The MCP JSON-RPC connection stays open across many
requests and is forwarded through `wsl.exe` to the Linux engine:

```powershell
cowbox mcp-proxy --network off -- `
  python3 E:\project\your-app\mcp-server.py
```

The upstream command after `--` must run inside WSL. A Windows-native MCP server
executable is not supported by this first version. When a Linux argument is an
absolute path under the workspace, the sandbox adapter maps it to the merged
OverlayFS mount; for a Windows path configure its WSL form, for example
`/mnt/e/project/your-app/mcp-server.py`.

The agent harness should launch the real Windows launcher. It should not wrap
`cowbox.exe` in a helper that buffers stdout: MCP requires a persistent,
line-delimited JSON-RPC stream.

`cowbox.exe` forwards every `doctor` argument, including
`--check-run --network off`, through the same `wsl.exe` command shape used by
`run`, `diff`, and the other engine commands. Linux stdout, stderr, and the exit
code are preserved. The Windows launcher does not reinterpret the active-run
JSON or its stage-specific failure codes.

`cowbox demo --offline` runs the real demo command, diff, and rollback with
network policy `off`. A host/IP connection is unavailable inside that step;
filesystem-backed Unix sockets remain an IPC boundary.

## Agent skill

The setup is intentionally two-step:

```powershell
cowbox install-skill --list
cowbox install-skill --harness codex
```

`--all` installs to all supported harnesses; `--mode ask|explicit|auto`
controls the default trigger policy. `cowbox install-rules` (or
`cowbox init --agent`) writes the project AGENTS/CLAUDE/Cursor rules.

## Boundary

Codex Desktop's built-in file editor is still outside the WSL sandbox in M3.
Commands routed through `cowbox` are protected; native Windows file-edit
interception is a later milestone.
