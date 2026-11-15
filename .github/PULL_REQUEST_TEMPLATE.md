## Summary

What problem does this change solve, and what is the chosen approach?

## Verification

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Adversarial suite in WSL2 ext4 (`COWBOX_REQUIRE_SANDBOX=1`)
- [ ] CLI smoke: `init → run → diff → rollback → commit --dry-run`
- [ ] Benchmark/demo output is measured, or explicitly marked TBD

Commands and observed results:

```text
paste exact commands and results
```

## Risk and compatibility

- Does this change a public type, CLI flag, config field, or state format?
- Does it affect Landlock, namespace setup, commit ordering, or symlink handling?
- Does it add a platform-specific dependency?

## Checklist

- [ ] No silent CoW fallback.
- [ ] No raw `libc` FFI or `unsafe`.
- [ ] `.git` is never committed.
- [ ] No benchmark or test result was fabricated.
