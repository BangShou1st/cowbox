# cowbox Engineering Rules

## Architecture

- Keep the OS-specific engine in `crates/cowbox-core` and keep the CLI thin.
- Do not introduce a silent fallback to full directory copies when OverlayFS fails.
- Use `rustix` and `landlock`; do not add raw `libc` FFI or `unsafe` blocks.
- Always inspect entries with `symlink_metadata` (`lstat`), never follow symlinks during diff or commit.

## Verification

- Run Rust builds with `CARGO_TARGET_DIR` on WSL's ext4 filesystem.
- Integration fixtures must live under `/tmp`, never under `/mnt/e`.
- Do not claim benchmark numbers that were not measured on this machine.
