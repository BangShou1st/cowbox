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
- `commit` returns `CONFIRMATION_REQUIRED`: review the diff and obtain human
  approval, then run `cowbox commit --yes`.
- `commit` returns `COMMIT_RECOVERY_REQUIRED`: an earlier commit was
  interrupted; inspect the journal and run `cowbox commit --recover --yes`.
  Before the durable commit point this restores the host; after it, recovery
  keeps the committed content and finishes the session.
- `commit --recover` refuses with changed paths: do not force it blindly;
  inspect the listed host edits and the journal before deciding what to keep.
- State grows across many sessions: inspect with `cowbox sessions`, then preview
  cleanup with `cowbox gc --dry-run`.
- Skill install conflicts: use `--force` only if the target is a cowbox-managed
  or disposable skill directory; `--list` shows detected harnesses.
