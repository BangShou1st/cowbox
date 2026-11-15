param(
    [string]$Command = "bash"
)

# Run a command in the Ubuntu WSL distribution with a Linux target directory.
# Examples:
#   .\scripts\wsl.ps1 "cargo test --workspace"
#   .\scripts\wsl.ps1 "cargo run -p cowbox-cli -- doctor --json"
wsl -d Ubuntu -- bash -lc "cd /mnt/e/project/cowbox && export CARGO_TARGET_DIR=`$HOME/.cache/cowbox-target && $Command"
