# Workflow and recovery

Every `cowbox run -- <command>` records a step. `cowbox rollback <step>` only
moves the session head; it never writes the host. `cowbox commit` applies the
net changes to the host and requires human approval. Non-interactive callers
must use `cowbox commit --yes` only after that approval.

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
# After explicit human approval:
cowbox commit --yes
```

Use `cowbox discard --yes` to throw the session away. Long sessions compact
older physical mount layers automatically at the OverlayFS limit while keeping
the logical step DAG available for diff, commit, rollback, and branching.

cowbox does not isolate the network or reverse remote database/service changes.
It only protects the local workspace file tree.
