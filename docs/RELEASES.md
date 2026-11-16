# Release verification

## v0.6.0-m3 MCP agent snapshot preview

- Tag: `v0.6.0-m3`
- Release commit: `7eb2fdee090bac8b63f25c961e07c3f795285d7d`
- Release workflow: https://github.com/BangShou1st/cowbox/actions/runs/43870327668
- Release: https://github.com/BangShou1st/cowbox/releases/tag/v0.6.0-m3
- Type: GitHub pre-release

Published archive hashes:

| Asset | SHA-256 |
| --- | --- |
| `cowbox-v0.6.0-m3-x86_64-unknown-linux-gnu.tar.gz` | `eec5e14804f2f9d0af4ce6997ca8b219ff2338c64ea6f6f24281426d19d18b00` |
| `cowbox-v0.6.0-m3-x86_64-pc-windows-msvc.zip` | `d30e1b14bd2483dd40766788cf230e292fd3f7e90f65b687f1477dac45777366` |

Downloaded assets were re-hashed and matched their published `.sha256` files.
Both checksum files had no CR bytes and no trailing filename whitespace, and
`sha256sum -c` passed for both archives.

Extracted binary hashes:

| Binary | SHA-256 |
| --- | --- |
| Linux `cowbox` | `c73d9d2a331d9676ec5af009aa4f29db424f760cf58d68351a13c66b70728c1d` |
| Windows `cowbox.exe` | `4805df57439286770bcc9b12ca2a4d0ced48c29ce7658ee34e8e674bf292dc97` |

Acceptance used only the downloaded release assets:

1. The Linux archive was extracted under `/tmp` on ext4. `--version` reported
   `cowbox 0.6.0-m3`; `doctor --json` reported `ready=true` with Landlock ABI 3;
   `doctor --check-run --network off --json` reported a ready active-run chain
   and `command_started=true`; `demo --offline --json` reported
   `host_unchanged=true`.
2. The local MCP acceptance wrote `WRONG`, read it back, diffed, rolled back,
   verified host TCP was unreachable under `--network off`, wrote `RIGHT`,
   previewed the commit, and used the returned session-bound command with a
   real PTY confirmation. The host contained exactly `RIGHT\n` and the session
   ended `committed`. The extracted release binary reported MCP first-handshake
   median / P95 of 87.557 / 108.870 ms and continuous same-session tool calls of
   103.716 / 125.825 ms. A separate black-box command-path check confirmed that
   rolling back an MCP step with `cowbox rollback 2 --session <id>` left the
   previously completed terminal step 2 and head at 2. After switching sessions,
   the old session-bound commit command failed with `SESSION_NOT_ACTIVE`, the
   new session stayed at head 1, and the host remained untouched.
3. A real Claude Code 2.1.197 session used a temporary MCP config and only the
   cowbox MCP tools with `marvis/deepseek-v4-1-flash-261017-antidowngrade[1m]`.
   It created steps 1 and 2 for the wrong write and read, diffed, rolled back
   to 0, created step 3 for `RIGHT`, and received
   `cowbox commit --step 3 --session <id>`. The terminal confirmation committed
   step 3, the host file was exactly `RIGHT\n`, and the final session status was
   `committed`.
4. The Windows ZIP was extracted and its `cowbox.exe` reported `0.6.0-m3`.
   With an isolated `%APPDATA%`, `doctor --json` preserved the valid Linux
   result and exit 0; a nonexistent distro returned structured
   `WSL_START_FAILED` with exit 1. A real
   `cowbox.exe -> wsl.exe -> Linux cowbox` MCP session completed discovery,
   write/read, diff, rollback, retry, and commit preview across multiple
   requests. The Windows host file stayed absent, and the returned rollback and
   commit commands carried the bound session.
5. `scripts/acceptance_m2.py` passed on the tagged source with the debug binary.
   It completed the 300-step mixed-policy session, compaction at steps 151 and
   268, rollback, branch, commit, interrupted-commit recovery, host-drift
   refusal, and directory-recovery host protection.

No release asset was rebuilt or modified during acceptance.

## v0.5.0-m2 offline execution preview

- Tag: `v0.5.0-m2`
- Release commit: `b67b83891f1e211c97922b2a920bb6d241833490`
- Release workflow: https://github.com/BangShou1st/cowbox/actions/runs/43823021342
- Release: https://github.com/BangShou1st/cowbox/releases/tag/v0.5.0-m2
- Type: GitHub pre-release

Published archive hashes:

| Asset | SHA-256 |
| --- | --- |
| `cowbox-v0.5.0-m2-x86_64-unknown-linux-gnu.tar.gz` | `7a6e9739e88d883a2d07e9bafa762129e8a7a2a5111f7c3db9e749e1bd1731f3` |
| `cowbox-v0.5.0-m2-x86_64-pc-windows-msvc.zip` | `a7e595139928565807a301c80e04aaa1d21e7f8d737c159419e13b32dd41de5b` |

Downloaded assets were re-hashed and matched their published `.sha256` files.

Extracted binary hashes:

| Binary | SHA-256 |
| --- | --- |
| Linux `cowbox` | `a11a80201a5e23696a899f1b4cc9898183d9791a49ce0e15f1236b5197ec416a` |
| Windows `cowbox.exe` | `a7efa5b937e1360148c52ad9f8a9ef46345a0c5f102cc32b69eb361463416c54` |

Acceptance used only the downloaded release assets:

1. The Linux archive was extracted under `/tmp` on ext4. `--version` reported
   `cowbox 0.5.0-m2`. `doctor --check-run --network off --json` reported
   `ready=true`, `command_started=true`, and true state-disk, user-namespace,
   network-namespace, OverlayFS, and Landlock stages. `demo --offline --json`
   completed with `host_unchanged=true`.
2. A separate Linux workspace completed a real `run --network off`, produced
   the expected one-file `diff`, left the file absent on the host, and
   discarded with `--purge`. The standalone Cargo/npm/pip offline workflow
   passed with all cold misses, warm hits, session-cache persistence, and host
   cache hash checks.
3. The Windows ZIP was extracted and its `cowbox.exe` ran
   `setup --json --force`. Setup installed version `0.5.0-m2`, and the installed
   Windows and WSL binaries matched the extracted hashes above.
4. Through the real `cowbox.exe -> wsl.exe -> Linux cowbox` path, a separate
   Windows project passed `doctor --check-run --network off`, `demo --offline`,
   an offline `run`, and `diff`. The sandbox-only file existed in the diff but
   not on the Windows host. A second command exited 7 and the Windows launcher
   returned 7. `discard --yes --purge` left the host fixture at its original
   `.git` directory and `base.txt`.

No release asset was rebuilt or modified during acceptance.

## v0.4.0-m2 performance preview

- Tag: `v0.4.0-m2`
- Release commit: `e5e1a53f873ea82c0565c4799ad9dc1c0bb36459`
- Release workflow: https://github.com/BangShou1st/cowbox/actions/runs/43791933547
- Release: https://github.com/BangShou1st/cowbox/releases/tag/v0.4.0-m2
- Type: GitHub pre-release

Published archive hashes:

| Asset | SHA-256 |
| --- | --- |
| `cowbox-v0.4.0-m2-x86_64-unknown-linux-gnu.tar.gz` | `a89a372e37072ab00c93f113005a1372784a4737dd201a5427ed41c0678ccfbd` |
| `cowbox-v0.4.0-m2-x86_64-pc-windows-msvc.zip` | `c05f49a8dc725fa43bd65f10b3b9b534c8e9b9e536c6ebdd3a1d65461f5f26ac` |

Downloaded assets were re-hashed and matched their published `.sha256` files.

Extracted binary hashes:

| Binary | SHA-256 |
| --- | --- |
| Linux `cowbox` | `2a8b38b3f7aab1100fd6f1ada8a0ebd4c62236522c918ce868cf8f8d2b7caa29` |
| Windows `cowbox.exe` | `4a9707152a25d26a443f7e4acf318d2ee663972e650b2976f504c90bbdb48c73` |

Acceptance used a fresh audit directory:

1. Extracted and ran the Linux release binary in an independent ext4 workspace.
   `--version` reported `cowbox 0.4.0-m2`; `doctor --json` reported `ready=true`
   with user namespaces, OverlayFS, `userxattr`, Landlock ABI 3, and ext4 state;
   `demo --json` completed two steps with `host_unchanged=true`.
2. Extracted and ran the Windows launcher. `setup --json --force` downloaded and
   checksum-verified the Linux archive, installed version `0.4.0-m2`, and
   reported a ready Linux doctor result. The installed Windows and WSL binaries
   matched the extracted release hashes above.
3. From a separate Windows-to-WSL project directory, the installed launcher
   passed `doctor`, `demo`, `init`, a real sandbox `run`, and `diff`. The run
   created a sandbox-only file, the host path remained absent, and the session
   was discarded with `--purge`.

No release asset was rebuilt or modified during acceptance.
