use crate::agent::config::load_global;
use crate::config::{AgentConfig, AgentMode, Config};
use crate::error::CliResult;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct EffectivePolicy {
    pub mode: AgentMode,
    pub require_confirmation: bool,
    pub triggers: Vec<String>,
    pub max_files_without_prompt: usize,
    pub source: &'static str,
}

pub fn effective(project: &Path) -> CliResult<EffectivePolicy> {
    let global = load_global()?.agent;
    let project_config = Config::load(project)?;
    let (agent, source): (AgentConfig, &'static str) = match project_config.agent {
        Some(agent) => (agent, "project"),
        None => (global, "global"),
    };
    Ok(EffectivePolicy {
        mode: agent.mode,
        require_confirmation: agent.require_confirmation,
        triggers: agent.triggers,
        max_files_without_prompt: agent.max_files_without_prompt,
        source,
    })
}
