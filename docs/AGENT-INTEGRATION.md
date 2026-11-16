# Agent integration

cowbox follows the same packaging idea as bsk: a real CLI carries the engine,
and a thin `SKILL.md` teaches an agent when and how to call it.

## Commands

```bash
cowbox setup
cowbox install-skill --list
cowbox install-skill --harness codex --mode ask
cowbox install-rules
cowbox policy --json
cowbox update --check
```

`setup` never installs a skill. The user explicitly opts in with
`install-skill`.

## Harnesses

The supported table mirrors bsk: Codex, Codex internal, Claude Code, Claude
internal, Cursor, OpenClaw, OpenClaw internal, CodeBuddy, WorkBuddy, Qoder,
Qoder CN, Pi, Hermes and Kimi Code. `--list` shows detection and paths;
`--harness` installs into one or more; `--all` installs into every supported
path.

## Trigger policy

- `explicit`: only when the user asks for cowbox.
- `ask`: ask before starting a session when a risk trigger matches (default).
- `auto`: start automatically when a risk trigger matches.

Risk triggers include destructive shell commands, local file-backed database
migrations, dependency upgrades, refactors over five files, and stateful tests.
Read-only commands do not use cowbox. The default network policy is `host`; a
caller can select `run --network off` to isolate IP connectivity without
isolating filesystem-backed Unix sockets. cowbox does not reverse remote
database or service changes. Do not present it as a malware sandbox.

`cowbox commit` requires explicit human approval. Agents use
`cowbox commit --yes` only after receiving that approval; non-interactive calls
without `--yes` fail with `CONFIRMATION_REQUIRED`.

## MCP snapshot proxy

The M3 integration path is:

```text
agent harness <-- newline JSON-RPC over stdio --> cowbox mcp-proxy
                                                     |
                                                     +-- one-off sandbox: upstream initialize + tools/list
                                                     |
                                                     +-- one sandbox step per upstream tools/call
                                                         (fresh upstream process, one call, exit)
```

Start the proxy with a Linux/WSL stdio MCP server after `--`:

```bash
cowbox mcp-proxy \
  --network off \
  --timeout 2m \
  -- /path/to/upstream-mcp-server
```

The proxy keeps the harness's MCP connection open and supports the stdio
transport, initialization, protocol-version negotiation, ping, `tools/list`,
and `tools/call`. It declares tools capability only. stdout is reserved for
JSON-RPC; diagnostics go to stderr.

Tool discovery uses a one-off sandbox with a temporary state root below `/tmp`.
The project's normal active session is not touched. The proxy follows upstream
`tools/list` cursors, rejects duplicate names, and reserves:

- `cowbox_diff`: read-only net diff, optionally for a historical step;
- `cowbox_rollback`: move the session head; no host writes;
- `cowbox_commit`: existing commit dry-run, including host-drift conflicts,
  target step, and change preview.

Each upstream `tools/call` creates a real step in the workspace's active
session. It starts a fresh upstream process inside that step, completes
`initialize` and one tool call, then exits. The proxy serializes calls. All
calls are treated as potentially write-capable and inherit the configured
OverlayFS, Landlock, PID, cache, and network policies. Cross-call state must be
written into workspace files; an upstream process is not kept alive.

Every tool result carries `_meta.cowbox` with the workspace, bound session,
`head_before`, actual `step`, `head_after`, exit status, timeout flag, and
rollback/commit commands. A visible text block repeats the snapshot summary.
Failures after step creation retain that metadata, so an agent can explicitly
call `cowbox_rollback`.

### Harness configuration

Example `.mcp.json` for a Linux harness:

```json
{
  "mcpServers": {
    "cowbox": {
      "command": "cowbox",
      "args": [
        "mcp-proxy",
        "--network", "off",
        "--",
        "python3", "/path/to/workspace/mcp-server.py"
      ]
    }
  }
}
```

On Windows, keep the harness configuration pointed at `cowbox.exe`. The launcher
streams the MCP connection through `wsl.exe`; the upstream command after `--`
must still be a Linux command visible inside WSL. For a server script under the
project, an absolute workspace path is remapped by the adapter to the sandbox's
merged mount. Do not expect a Windows-native upstream executable to work.

Useful bounds:

- `--workspace <path>` pins the project root; otherwise the current directory's
  workspace is used.
- `--session <id>` pins an already active session. Without it, the proxy binds
  to the session created or found by the first tool call.
- `--timeout 2m` bounds one upstream call; `--discovery-timeout 60s` bounds
  discovery.
- `--max-response-bytes` defaults to 768 KiB and is capped below the CLI's
  capture limit.
- Repeatable `--allow-read <path>` exposes a specific upstream installation
  path without opening the rest of the host.

The first MCP version does not support HTTP/SSE upstreams, resources, prompts,
notifications/list-changed subscription, or a persistent upstream process.
Those are roadmap items, not partial behaviors hidden behind a capability
claim.

## Human commit boundary

`cowbox_commit` is deliberately a preview. It calls the existing
`cowbox commit --dry-run` path and returns:

```text
cowbox commit --step <n>
```

The user must run that command in the same workspace terminal and approve the
existing interactive prompt. There is no MCP parameter for `--yes`, force, or
an arbitrary host command. After a successful terminal commit, the active
session ends; start a new proxy for a new session.

## Project rules

`install-rules` and `init --agent` write idempotent marker blocks to
`AGENTS.md`, `CLAUDE.md`, and `.cursor/rules/cowbox.mdc`. Existing text outside
the markers is preserved; `--remove` deletes only the managed blocks.

## Not in this milestone

The proxy supports stdio tool servers only. Codex Desktop built-in edits remain
outside the WSL sandbox until the native Windows backend lands.
