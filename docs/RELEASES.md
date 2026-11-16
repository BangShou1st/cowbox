# Release verification

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
