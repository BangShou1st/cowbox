use super::spec::RunSpec;
use std::collections::BTreeMap;

const DENY: &[&str] = &[
    "WSL_INTEROP",
    "WSLENV",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "BASH_ENV",
    "ENV",
    "PYTHONPATH",
    "NODE_OPTIONS",
];

pub fn build_env(spec: &RunSpec) -> Vec<(String, String)> {
    let mut env: BTreeMap<String, String> = if spec.clean_env {
        BTreeMap::new()
    } else {
        std::env::vars().collect()
    };
    for key in DENY {
        env.remove(*key);
    }

    env.insert(
        "HOME".to_string(),
        spec.home_dir.to_string_lossy().into_owned(),
    );
    env.insert(
        "TMPDIR".to_string(),
        spec.tmp_dir.to_string_lossy().into_owned(),
    );
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        spec.home_dir.join(".config").to_string_lossy().into_owned(),
    );
    env.insert(
        "XDG_DATA_HOME".to_string(),
        spec.home_dir
            .join(".local/share")
            .to_string_lossy()
            .into_owned(),
    );
    env.insert(
        "XDG_STATE_HOME".to_string(),
        spec.home_dir
            .join(".local/state")
            .to_string_lossy()
            .into_owned(),
    );
    env.insert(
        "PATH".to_string(),
        sanitized_path(env.get("PATH").map(String::as_str).unwrap_or_default()),
    );

    if spec.isolate_caches {
        env.insert(
            "XDG_CACHE_HOME".to_string(),
            spec.cache_dir.to_string_lossy().into_owned(),
        );
        env.insert(
            "CARGO_HOME".to_string(),
            spec.cache_dir.join("cargo").to_string_lossy().into_owned(),
        );
        env.insert(
            "NPM_CONFIG_CACHE".to_string(),
            spec.cache_dir.join("npm").to_string_lossy().into_owned(),
        );
        env.insert(
            "PIP_CACHE_DIR".to_string(),
            spec.cache_dir.join("pip").to_string_lossy().into_owned(),
        );
    }

    for (key, value) in &spec.env {
        env.insert(key.clone(), value.clone());
    }
    env.into_iter().collect()
}

fn sanitized_path(original: &str) -> String {
    let mut parts = vec![
        "/usr/local/bin".to_string(),
        "/usr/bin".to_string(),
        "/bin".to_string(),
        "/usr/local/sbin".to_string(),
        "/usr/sbin".to_string(),
        "/sbin".to_string(),
    ];
    for part in original.split(':') {
        if part.is_empty()
            || part.starts_with("/mnt/")
            || part.contains('\\')
            || part.contains("Windows")
        {
            continue;
        }
        if !parts.iter().any(|existing| existing == part) {
            parts.push(part.to_string());
        }
    }
    parts.join(":")
}
