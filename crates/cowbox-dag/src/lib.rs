#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use thiserror::Error;

pub const BASE_STEP: u64 = 0;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DagError {
    #[error("step {0} does not exist in session DAG")]
    UnknownStep(u64),
    #[error("step {0} already exists in session DAG")]
    DuplicateStep(u64),
    #[error("step {0} is not the next id for this session")]
    NonSequentialStep(u64),
    #[error("parent step {0} does not exist")]
    InvalidParent(u64),
    #[error("new step must branch from current head {head}, got parent {parent}")]
    ParentIsNotHead { parent: u64, head: u64 },
    #[error("session DAG is missing its base step")]
    MissingBase,
    #[error("session DAG contains a cycle at step {0}")]
    Cycle(u64),
    #[error("head step {0} does not exist")]
    InvalidHead(u64),
    #[error("cannot undo {requested} step(s); only {available} available")]
    UndoTooDeep { requested: usize, available: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepNode {
    pub id: u64,
    pub parent_id: Option<u64>,
    pub label: String,
    pub timestamp_unix_ms: u64,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub duration_ms: u64,
    pub orphans_reaped: usize,
    pub timed_out: bool,
}

impl StepNode {
    pub fn new(id: u64, parent_id: Option<u64>, label: impl Into<String>) -> Self {
        Self {
            id,
            parent_id,
            label: label.into(),
            timestamp_unix_ms: 0,
            exit_code: None,
            signal: None,
            duration_ms: 0,
            orphans_reaped: 0,
            timed_out: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDag {
    pub session_id: String,
    pub workspace_root: PathBuf,
    pub head_step: u64,
    pub nodes: BTreeMap<u64, StepNode>,
}

impl SessionDag {
    pub fn new(session_id: impl Into<String>, workspace_root: impl Into<PathBuf>) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            BASE_STEP,
            StepNode {
                id: BASE_STEP,
                parent_id: None,
                label: "base".to_string(),
                timestamp_unix_ms: 0,
                exit_code: None,
                signal: None,
                duration_ms: 0,
                orphans_reaped: 0,
                timed_out: false,
            },
        );
        Self {
            session_id: session_id.into(),
            workspace_root: workspace_root.into(),
            head_step: BASE_STEP,
            nodes,
        }
    }

    pub fn next_step_id(&self) -> u64 {
        self.nodes.keys().next_back().copied().unwrap_or(BASE_STEP) + 1
    }

    pub fn add_step(&mut self, node: StepNode) -> Result<(), DagError> {
        if node.id == BASE_STEP {
            return Err(DagError::DuplicateStep(BASE_STEP));
        }
        if self.nodes.contains_key(&node.id) {
            return Err(DagError::DuplicateStep(node.id));
        }
        if node.id != self.next_step_id() {
            return Err(DagError::NonSequentialStep(node.id));
        }
        let parent = node.parent_id.ok_or(DagError::InvalidParent(node.id))?;
        if !self.nodes.contains_key(&parent) {
            return Err(DagError::InvalidParent(parent));
        }
        if parent != self.head_step {
            return Err(DagError::ParentIsNotHead {
                parent,
                head: self.head_step,
            });
        }
        let id = node.id;
        self.nodes.insert(id, node);
        self.head_step = id;
        Ok(())
    }

    pub fn rollback(&mut self, target: u64) -> Result<(), DagError> {
        if !self.nodes.contains_key(&target) {
            return Err(DagError::UnknownStep(target));
        }
        self.head_step = target;
        Ok(())
    }

    pub fn undo(&mut self, steps: usize) -> Result<u64, DagError> {
        if steps == 0 {
            return Ok(self.head_step);
        }
        let available = self.depth_to_base()?;
        if steps > available {
            return Err(DagError::UndoTooDeep {
                requested: steps,
                available,
            });
        }
        let mut current = self.head_step;
        for _ in 0..steps {
            let node = self
                .nodes
                .get(&current)
                .ok_or(DagError::UnknownStep(current))?;
            current = node.parent_id.ok_or(DagError::MissingBase)?;
        }
        self.head_step = current;
        Ok(current)
    }

    pub fn active_layer_chain(&self) -> Vec<u64> {
        let mut chain = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = Some(self.head_step);
        while let Some(id) = current {
            if !seen.insert(id) {
                break;
            }
            if id != BASE_STEP {
                chain.push(id);
            }
            current = self.nodes.get(&id).and_then(|node| node.parent_id);
        }
        chain.reverse();
        chain
    }

    pub fn path_to_base(&self, step: u64) -> Result<Vec<u64>, DagError> {
        if !self.nodes.contains_key(&step) {
            return Err(DagError::UnknownStep(step));
        }
        let mut path = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = Some(step);
        while let Some(id) = current {
            if !seen.insert(id) {
                return Err(DagError::Cycle(id));
            }
            path.push(id);
            current = self.nodes.get(&id).and_then(|node| node.parent_id);
        }
        if path.last().copied() != Some(BASE_STEP) {
            return Err(DagError::MissingBase);
        }
        Ok(path)
    }

    pub fn validate(&self) -> Result<(), DagError> {
        if !self.nodes.contains_key(&BASE_STEP) {
            return Err(DagError::MissingBase);
        }
        if !self.nodes.contains_key(&self.head_step) {
            return Err(DagError::InvalidHead(self.head_step));
        }
        for (id, node) in &self.nodes {
            if *id == BASE_STEP {
                if node.parent_id.is_some() {
                    return Err(DagError::InvalidParent(*id));
                }
                continue;
            }
            let parent = node.parent_id.ok_or(DagError::InvalidParent(*id))?;
            if !self.nodes.contains_key(&parent) {
                return Err(DagError::InvalidParent(parent));
            }
        }
        for id in self.nodes.keys() {
            let mut seen = BTreeSet::new();
            let mut current = Some(*id);
            while let Some(step) = current {
                if !seen.insert(step) {
                    return Err(DagError::Cycle(step));
                }
                current = self.nodes.get(&step).and_then(|node| node.parent_id);
            }
        }
        Ok(())
    }

    fn depth_to_base(&self) -> Result<usize, DagError> {
        let mut depth = 0;
        let mut current = self.head_step;
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(current) {
                return Err(DagError::Cycle(current));
            }
            let node = self
                .nodes
                .get(&current)
                .ok_or(DagError::UnknownStep(current))?;
            match node.parent_id {
                Some(parent) => {
                    depth += 1;
                    current = parent;
                }
                None => {
                    if current != BASE_STEP {
                        return Err(DagError::MissingBase);
                    }
                    break;
                }
            }
        }
        Ok(depth)
    }
}
