//! `ExperimentRecord`, `ExperimentStatus`, `RawMetrics`, `DiffStats` — SPEC.md §4.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::evaluator::EvaluatorVerdict;
use super::mutation::MutationRef;
use super::policy::{AgentConfig, PermissionPolicy};
use super::scoring::ScoreCard;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExperimentStatus {
    Running,
    Completed,
    Failed,
    TimedOut,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DiffStats {
    pub files_changed: u32,
    pub lines_added: u32,
    pub lines_removed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawMetrics {
    /// From evaluating the agent's patch.
    pub verdict: EvaluatorVerdict,
    pub diff: DiffStats,
    /// The AGENT process itself was killed by the Executor — distinct from `verdict.timed_out`,
    /// which is the evaluator's own timeout.
    pub agent_timed_out: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentRecord {
    pub id: String,
    pub task_id: String,
    pub agent_config: AgentConfig,
    pub policy_snapshot: PermissionPolicy,
    pub base_ref: String,
    pub mutation_ref: Option<MutationRef>,
    /// Set only if launched as part of a race.
    pub race_id: Option<String>,
    /// Deterministic position, fixed before execution starts — SPEC.md §20 (R2/T1).
    pub race_index: Option<u32>,
    pub worktree_path: PathBuf,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub status: ExperimentStatus,
    pub patch_path: PathBuf,
    pub raw_metrics: Option<RawMetrics>,
    pub score: Option<ScoreCard>,
    pub audit_log_path: PathBuf,
}
