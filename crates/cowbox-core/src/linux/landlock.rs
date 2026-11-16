use super::spec::RunSpec;
use crate::error::{CoreError, Result};
use landlock::{
    ABI, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus, path_beneath_rules,
};
use std::path::PathBuf;

const READ_ONLY_PATHS: &[&str] = &[
    "/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/proc", "/sys", "/run", "/opt", "/snap",
];

const DEVICE_RW_PATHS: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];

pub fn apply(spec: &RunSpec) -> Result<()> {
    let abi = ABI::V3;
    let mut read_only: Vec<PathBuf> = READ_ONLY_PATHS
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    read_only.extend(spec.extra_ro_paths.iter().cloned());
    read_only.extend(spec.cache_mounts.iter().map(|cache| cache.source.clone()));

    let mut read_write: Vec<PathBuf> = vec![
        spec.merged_dir.clone(),
        spec.home_dir.clone(),
        spec.tmp_dir.clone(),
        spec.cache_dir.clone(),
    ];
    if let Some(result_parent) = spec.result_path.parent() {
        read_write.push(result_parent.to_path_buf());
    }
    if std::path::Path::new("/dev/pts").exists() {
        read_write.push(PathBuf::from("/dev/pts"));
    }
    read_write.extend(spec.extra_rw_paths.iter().cloned());
    let devices: Vec<PathBuf> = DEVICE_RW_PATHS
        .iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect();

    let ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(|error| {
            CoreError::Landlock(format!("kernel does not support Landlock ABI 3: {error}"))
        })?
        .create()
        .map_err(|error| {
            CoreError::Landlock(format!("creating Landlock ruleset failed: {error}"))
        })?;
    let ruleset = ruleset
        .add_rules(path_beneath_rules(&read_only, AccessFs::from_read(abi)))
        .map_err(|error| {
            CoreError::Landlock(format!("adding read-only Landlock rules failed: {error}"))
        })?;
    let ruleset = ruleset
        .add_rules(path_beneath_rules(&read_write, AccessFs::from_all(abi)))
        .map_err(|error| {
            CoreError::Landlock(format!("adding read-write Landlock rules failed: {error}"))
        })?;
    let ruleset = ruleset
        .add_rules(path_beneath_rules(
            &devices,
            AccessFs::ReadFile | AccessFs::WriteFile,
        ))
        .map_err(|error| {
            CoreError::Landlock(format!("adding device Landlock rules failed: {error}"))
        })?;

    let status = ruleset
        .set_compatibility(CompatLevel::HardRequirement)
        .restrict_self()
        .map_err(|error| {
            CoreError::Landlock(format!("enforcing Landlock rules failed: {error}"))
        })?;
    if status.ruleset != RulesetStatus::FullyEnforced {
        return Err(CoreError::Landlock(format!(
            "Landlock was not fully enforced: {:?}",
            status.ruleset
        )));
    }
    Ok(())
}
