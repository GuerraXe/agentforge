//! `BisectRecord`, `BisectStep` — SPEC.md §4, §13.
//!
//! Bisect steps are direct `evaluate()` calls, not full experiments — no `ExperimentRecord` is
//! manufactured for something that isn't an experiment (SPEC.md §20, D1).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::evaluator::EvaluatorVerdict;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BisectStep {
    pub commit: String,
    pub verdict: EvaluatorVerdict,
    pub is_good: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BisectRecord {
    pub id: String,
    pub task_id: String,
    pub evaluator_id: String,
    /// (good sha, bad sha)
    pub range: (String, String),
    /// Rust-level name unified with `ExperimentRecord.worktree_path`/`FaultRef.worktree_path`/
    /// `MutantRef.worktree_path` (same concept: the one isolated worktree this record's
    /// operations run against) — the on-disk/JSON key stays `bisect_worktree` so the tested
    /// TOML/`--json` contract doesn't change.
    #[serde(rename = "bisect_worktree")]
    pub worktree_path: PathBuf,
    pub steps: Vec<BisectStep>,
    pub culprit: Option<String>,
}
