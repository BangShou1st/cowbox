# Workflow and recovery

Every `cowbox run -- <command>` records a step. `cowbox rollback <step>` only
moves the session head; it never writes the host. `cowbox commit` applies the
net changes to the host and requires human approval.

Recommended loop:

```bash
cowbox run -- <risky command>
cowbox diff --stat
cowbox diff
# If the result is broken:
cowbox log
cowbox rollback <last-clean-step>
# Otherwise:
cowbox commit --dry-run
cowbox commit
```

Use `cowbox discard --yes` to throw the session away. M1 supports up to 150
layers per session; commit or discard and start a new session when the limit is
reached.
