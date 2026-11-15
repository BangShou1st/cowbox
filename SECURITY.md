# Security policy

## Supported versions

cowbox is an M1 preview. Security fixes target the `main` branch. There is no
long-term-support release yet.

## What cowbox is trying to protect

M1 is a safety net for a developer's own workspace while an AI agent edits it.
It aims to prevent:

- Accidental deletion or corruption of tracked, untracked, or ignored files.
- Corruption of local state files such as SQLite databases.
- Background dev servers and workers surviving a step.
- Writes outside the workspace and the session's private directories.
- A partial `commit` leaving the host in a half-written state.

## What M1 does not protect against

- **Network access.** M1 has no network namespace or seccomp network policy.
  Sandboxed code can connect to the network and bind host ports.
- **Hostile code or kernel exploits.** Landlock and user namespaces are not a
  substitute for a VM. A deliberately malicious binary may exploit the kernel.
- **External edits during a session.** The host workspace is treated as
  read-only by convention. `commit` detects metadata drift, but this is not a
  filesystem snapshot and cannot recover a host file edited outside cowbox.
- **Secrets deliberately exposed to the agent.** If a path is added to the
  read allowlist, the agent can read it.
- **All filesystem semantics.** Hardlinks and xattrs are not preserved in M1;
  device nodes, FIFOs, and sockets are refused on commit.

Report a vulnerability through GitHub's **private vulnerability reporting** on
this repository. Do not open a public issue. Include:

- Affected commit or version.
- WSL/Linux distribution, kernel version, and filesystem type.
- The smallest reproduction you can produce.
- The output of `cowbox doctor --json` with secrets removed.
- Impact and any suggested mitigation.

We aim to acknowledge reports within 7 days and to agree on a disclosure plan
within 30 days. Please give us a reasonable window to ship a fix before public
disclosure.

## Security-sensitive code

Changes to namespace setup, uid/gid mapping, OverlayFS options, Landlock rules,
environment sanitization, commit journaling, or symlink handling require an
explicit security review in the pull request.
