# Windows

cowbox M1.5 ships a native `cowbox.exe` launcher. The engine still runs in
WSL2; the project does not need to move out of `E:\...`.

## Install

```powershell
.\cowbox.exe setup
cowbox doctor
cowbox demo
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
cowbox diff
cowbox commit
```

`cowbox.exe` calls `wsl.exe --cd <cwd>`, so the project is visible as
`/mnt/e/project/your-app` inside WSL. State remains on WSL ext4.

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

Codex Desktop's built-in file editor is still outside the WSL sandbox in M1.5.
Commands routed through `cowbox` are protected; native Windows file-edit
interception is a later milestone.
