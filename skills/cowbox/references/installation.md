# Installation and harnesses

## Windows

1. Download the Windows release zip and run `cowbox.exe setup`.
2. `setup` installs the Linux engine into WSL and the Windows launcher into
   `%USERPROFILE%\.local\bin`. It does not install skills.
3. Add `%USERPROFILE%\.local\bin` to `PATH` if `cowbox` is not found.
4. Run `cowbox doctor`.
5. Run `cowbox install-skill --list`, then install the harness you use:
   `cowbox install-skill --harness codex` (or `--all`).
6. In each project, run `cowbox init --agent` to write project rules.

## Linux/WSL

Use the release tarball or build from source, then install the skill into the
harnesses you use:

```bash
cowbox install-skill --list
cowbox install-skill --harness codex --mode ask
cowbox init --agent
```

## Trigger policy

`cowbox install-skill --mode ask` is the default. Other modes are `explicit`
(only when the user asks) and `auto` (start automatically for a detected risk).
The skill calls `cowbox policy --json` before acting.
