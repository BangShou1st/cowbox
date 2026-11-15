use crate::error::{CliError, CliResult};
use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

static SKILL_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../skills/cowbox");

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SkillSource {
    version: String,
    source: String,
    files: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub path: PathBuf,
    pub status: &'static str,
}

pub fn install_paths(
    paths: &[PathBuf],
    force: bool,
    dry_run: bool,
) -> CliResult<Vec<InstallResult>> {
    let mut results = Vec::new();
    for path in paths {
        if dry_run {
            results.push(InstallResult {
                path: path.clone(),
                status: "planned",
            });
            continue;
        }
        let managed = path.join(".cowbox-source").exists();
        if path.exists() && !managed && !force {
            return Err(CliError::new(
                "SKILL_EXISTS",
                format!(
                    "{} exists and is not cowbox-managed; use --force to replace it",
                    path.display()
                ),
            )
            .exit(2));
        }
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("cowbox-tmp-{}", std::process::id()));
        if temp.exists() {
            fs::remove_dir_all(&temp)?;
        }
        fs::create_dir_all(&temp)?;
        copy_embedded(&SKILL_DIR, &temp)?;
        write_source_manifest(&temp)?;
        fs::rename(&temp, path)?;
        results.push(InstallResult {
            path: path.clone(),
            status: "installed",
        });
    }
    Ok(results)
}

fn copy_embedded(dir: &Dir<'_>, destination: &Path) -> CliResult<()> {
    for file in dir.files() {
        let target = destination.join(file.path());
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, file.contents())?;
    }
    for child in dir.dirs() {
        copy_embedded(child, destination)?;
    }
    Ok(())
}

fn write_source_manifest(root: &Path) -> CliResult<()> {
    let mut files = BTreeMap::new();
    collect_hashes(&SKILL_DIR, &mut files)?;
    let source = SkillSource {
        version: env!("CARGO_PKG_VERSION").to_string(),
        source: "embedded".to_string(),
        files,
    };
    let text = serde_json::to_vec_pretty(&source)?;
    fs::write(root.join(".cowbox-source"), text)?;
    Ok(())
}

fn collect_hashes(dir: &Dir<'_>, files: &mut BTreeMap<String, String>) -> CliResult<()> {
    for file in dir.files() {
        let mut hasher = Sha256::new();
        hasher.update(file.contents());
        files.insert(
            file.path().to_string_lossy().replace('\\', "/"),
            hex::encode(hasher.finalize()),
        );
    }
    for child in dir.dirs() {
        collect_hashes(child, files)?;
    }
    Ok(())
}
