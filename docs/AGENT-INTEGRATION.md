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
Read-only commands do not use cowbox. cowbox does not isolate the network or
reverse remote database/service changes unless the caller explicitly selects
`run --network off`, which isolates IP connectivity but not filesystem-backed
Unix sockets. Do not present cowbox as a malware sandbox.

`cowbox commit` requires explicit human approval. Agents use
`cowbox commit --yes` only after receiving that approval; non-interactive calls
without `--yes` fail with `CONFIRMATION_REQUIRED`.

## Project rules

`install-rules` and `init --agent` write idempotent marker blocks to
`AGENTS.md`, `CLAUDE.md`, and `.cursor/rules/cowbox.mdc`. Existing text outside
the markers is preserved; `--remove` deletes only the managed blocks.

## Not in this milestone

`cowbox mcp` is not implemented yet. Codex Desktop built-in edits are outside
the WSL sandbox; only commands routed through cowbox are protected until the
native Windows backend lands.
