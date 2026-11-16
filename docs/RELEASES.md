# Release verification

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
