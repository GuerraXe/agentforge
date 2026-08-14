//! `TaskSpec` — SPEC.md §4.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::evaluator::EvaluatorVerdict;
use super::mutation::MutationRef;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub repo_path: std::path::PathBuf,
    /// Always a resolved 40-hex commit SHA, never a mutable ref — SPEC.md §20 (R1).
    pub base_ref: String,
    pub mutation: Option<MutationRef>,
    /// Id of an `EvaluatorSpec`. MANDATORY, never optional — SPEC.md §4.
    pub evaluator: String,
    pub agent_timeout_secs: u64,
    /// Captured once, at registration time, against `base_ref` — SPEC.md §20 (S1).
    /// Not required to be a good verdict (a mutation task's baseline is expected to be bad).
    pub baseline: EvaluatorVerdict,
    pub created_at: DateTime<Utc>,
}
