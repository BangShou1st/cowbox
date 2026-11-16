# MCP proxy setup

`cowbox mcp-proxy` wraps a stdio MCP tool server. The agent connection stays
open while each upstream `tools/call` runs in a fresh cowbox sandbox step.

```bash
cowbox mcp-proxy --network off -- python3 /path/to/mcp-server.py
```

Configure the harness with `cowbox` as the command. Example:

```json
{
  "mcpServers": {
    "cowbox": {
      "command": "cowbox",
      "args": [
        "mcp-proxy",
        "--network", "off",
        "--",
        "python3", "/path/to/mcp-server.py"
      ]
    }
  }
}
```

On Windows use `cowbox.exe` as the command. The upstream after `--` is still a
Linux command inside WSL.

The proxy exposes the upstream tools plus:

- `cowbox_diff` — show the net snapshot;
- `cowbox_rollback` — move the session head;
- `cowbox_commit` — dry-run preview only.

Each upstream result includes `_meta.cowbox` with `session`, `head_before`,
`step`, `head_after`, and rollback metadata. If a result is wrong, inspect the
diff and roll back to the prior head before retrying. A commit is always a
separate terminal action performed by the user with the returned
`cowbox commit --step <n> --session <id>` command.

Cross-call state must be stored in workspace files because the upstream process
is started anew for every call. HTTP/SSE upstreams, resources, prompts, and
persistent upstream processes are outside the M3 boundary.
