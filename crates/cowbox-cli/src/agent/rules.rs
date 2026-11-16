use crate::error::{CliError, CliResult};
use std::fs;
use std::path::{Path, PathBuf};

const BEGIN: &str = "<!-- cowbox:begin -->";
const END: &str = "<!-- cowbox:end -->";

#[derive(Debug, Clone)]
pub struct RulesResult {
    pub path: PathBuf,
    pub action: &'static str,
}

pub fn install(project: &Path, remove: bool, dry_run: bool) -> CliResult<Vec<RulesResult>> {
    let targets = [
        project.join("AGENTS.md"),
        project.join("CLAUDE.md"),
        project.join(".cursor/rules/cowbox.mdc"),
    ];
    let mut results = Vec::new();
    for path in targets {
        let is_cursor = path.extension().and_then(|value| value.to_str()) == Some("mdc");
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let next = if remove {
            remove_block(&existing)?
        } else if is_cursor {
            upsert_block(&with_cursor_frontmatter(&existing), &rules_block())?
        } else {
            upsert_block(&existing, &rules_block())?
        };
        let action = if remove {
            "removed"
        } else if existing == next {
            "unchanged"
        } else {
            "updated"
        };
        if !dry_run && existing != next {
            write_atomic(&path, &next)?;
        }
        results.push(RulesResult { path, action });
    }
    Ok(results)
}

fn rules_block() -> String {
    format!(
        "{BEGIN}
## cowbox sandbox policy

Use cowbox only when the user explicitly asks for it, or when the task involves
one of these risk triggers:

- destructive shell commands (`rm -rf`, `git reset --hard`, bulk delete);
- local file-backed database migrations or destructive database changes;
- dependency installation or major upgrades;
- refactors touching more than five files.

Before starting, run `cowbox policy --json`. Follow its `mode`:

- `explicit`: use cowbox only when the user explicitly requested it.
- `ask`: ask the user before creating a session.
- `auto`: start a session without asking.

When enabled, route shell commands through `cowbox run -- <command>`. On Windows
the project may stay at `E:\\...`; `cowbox.exe` maps it into WSL, so do not move
the project into WSL. On Linux/WSL use `cowbox` directly.

If a step fails, inspect `cowbox diff` and `cowbox log`, then `cowbox rollback
<step>` instead of stacking fixes on a broken workspace. `cowbox commit` is a
host write and requires explicit human approval. Do not edit the host workspace
while a cowbox session is active. After approval, run `cowbox commit --yes`;
non-interactive calls without `--yes` are refused.
If commit reports `COMMIT_RECOVERY_REQUIRED`, stop editing and run
`cowbox commit --recover --yes`. It restores the host before the durable commit
point or finalizes an already-applied commit; do not force recovery over
reported host drift.

If cowbox MCP tools are configured, use the upstream file tools for edits and
inspect `_meta.cowbox` after each call. Use `cowbox_diff` before retrying and
`cowbox_rollback` to discard a bad step. `cowbox_commit` only previews; the user
must run `cowbox commit --step <n>` in a terminal and approve it.

cowbox protects local workspace files. The default network policy is `host`;
`--network off` isolates IPv4/IPv6 TCP/UDP connectivity but does not reverse
remote database/service changes or prevent readable files from leaving the
machine.

This policy protects commands routed through cowbox. Codex Desktop's built-in
file editor is outside this boundary until the native Windows backend lands.
{END}"
    )
}

fn with_cursor_frontmatter(existing: &str) -> String {
    if existing.trim_start().starts_with("---") {
        existing.to_string()
    } else {
        format!("---\ndescription: cowbox sandbox policy\nalwaysApply: true\n---\n\n{existing}")
    }
}

fn upsert_block(existing: &str, block: &str) -> CliResult<String> {
    let begin = existing.find(BEGIN);
    let end = existing.find(END);
    match (begin, end) {
        (Some(begin), Some(end)) if end > begin => {
            let end = end + END.len();
            let mut result = String::new();
            result.push_str(&existing[..begin]);
            result.push_str(block);
            result.push_str(existing[end..].trim_start_matches('\n'));
            Ok(ensure_trailing_newline(result))
        }
        (None, None) => {
            let mut result = existing.trim_end().to_string();
            if !result.is_empty() {
                result.push_str("\n\n");
            }
            result.push_str(block);
            Ok(ensure_trailing_newline(result))
        }
        _ => Err(CliError::new(
            "RULES_MARKERS_INVALID",
            "found a partial cowbox rules marker block; repair or remove the markers manually",
        )
        .exit(2)),
    }
}

fn remove_block(existing: &str) -> CliResult<String> {
    let begin = existing.find(BEGIN);
    let end = existing.find(END);
    match (begin, end) {
        (Some(begin), Some(end)) if end > begin => {
            let end = end + END.len();
            let mut result = String::new();
            result.push_str(existing[..begin].trim_end());
            let tail = existing[end..].trim_start_matches('\n');
            if !tail.is_empty() {
                if !result.is_empty() {
                    result.push_str("\n\n");
                }
                result.push_str(tail);
            }
            Ok(ensure_trailing_newline(result))
        }
        (None, None) => Ok(existing.to_string()),
        _ => Err(CliError::new(
            "RULES_MARKERS_INVALID",
            "found a partial cowbox rules marker block; repair or remove the markers manually",
        )
        .exit(2)),
    }
}

fn ensure_trailing_newline(mut value: String) -> String {
    if !value.is_empty() && !value.ends_with('\n') {
        value.push('\n');
    }
    value
}

fn write_atomic(path: &Path, contents: &str) -> CliResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temp, contents)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temp, path)?;
    Ok(())
}
