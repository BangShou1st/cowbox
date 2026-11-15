# Troubleshooting

- `cowbox: command not found`: add `%USERPROFILE%\.local\bin` to `PATH` and
  reopen the agent shell.
- WSL not found: install WSL2 and Ubuntu, then run `cowbox setup`.
- Linux binary missing: run `cowbox setup` again; do not point `COWBOX_HOME` at
  a Windows drive.
- `doctor` reports no user namespace/OverlayFS: do not run the command without
  isolation; fix the kernel/WSL capability first.
- `commit` reports host drift: inspect the listed paths; use `--force` only
  after confirming the host edits should be overwritten.
- Skill install conflicts: use `--force` only if the target is a cowbox-managed
  or disposable skill directory; `--list` shows detected harnesses.
