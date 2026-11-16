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

The default network policy is `host`. Add `--network off` when a command should
run without IPv4/IPv6 TCP/UDP connectivity; filesystem-backed Unix sockets
remain an IPC boundary. cowbox does not reverse remote database or service
changes. It protects the local workspace file tree, not remote state.

If the harness uses `cowbox mcp-proxy`, the MCP control tools follow the same
model:

- `cowbox_diff` inspects the net snapshot;
- `cowbox_rollback` moves the session head and never writes the host;
- `cowbox_commit` previews the existing dry-run and returns
  `cowbox commit --step <n>`.

The user runs the commit command in the same workspace terminal and approves
the prompt. Do not ask for or invent an MCP force/yes parameter.
