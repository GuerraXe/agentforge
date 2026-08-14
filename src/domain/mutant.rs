//! `MutantSpec`, `MutantTarget`, `MutantEvaluation`, `MutantRef` — SPEC.md §10 Amendment
//! (standalone mutation testing pass), docs/ARCHITECTURE.md §9b.
//!
//! A second, standalone-persisted mutation-testing mechanism, sibling to `mutation::MutationRef`
//! (embedded-only in `TaskSpec`, §20 C3, sanity-checked immediately with a reject-on-good-verdict
//! gate) the same way `fault::FaultRef` is a standalone-persisted sibling of it. `MutantRef` has
//! no wrapping task to embed into and no immediate gate: `apply()` always materializes and
//! persists the record, and evaluation is a separate, later, non-gating step — the record exists
//! (and is inspectable) whether or not it has ever been evaluated.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::evaluator::EvaluatorVerdict;
use super::experiment::DiffStats;
use super::mutation::MutationOperator;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutantSpec {
    /// Reuses `mutation::MutationOperator`'s five text-pattern operators (SPEC.md §10) rather
    /// than a second operator vocabulary — one set of "mutations whose intent can be clearly
    /// reported," shared by both mechanisms.
    pub operator: MutationOperator,
    pub target_glob: String,
    pub seed: u64,
    /// Bumped whenever candidate-discovery/transform behavior changes — mirrors
    /// `MutationSpec.operator_version` (SPEC.md §20, R5).
    pub operator_version: u32,
}

/// The candidate site `apply()` actually selected — `candidates[seed % candidates.len()]`, the
/// same selection rule as `mutation::MutationEngine`/`fault::FaultInjector`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutantTarget {
    /// Forward-slash-joined, repo-root-relative path — SPEC.md §20 (R4).
    pub file: String,
    pub line: u32,
    pub column: u32,
}

/// The result of running `MutantTester::evaluate` against a mutant's still-materialized
/// workspace — recorded on `MutantRef.evaluation`, not just returned and discarded, so "was this
/// mutant killed" survives past the call that answered it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutantEvaluation {
    pub verdict: EvaluatorVerdict,
    pub evaluated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutantRef {
    /// Caller-chosen, validated (`[A-Za-z0-9_-]`-only) id — both this record's `Store` key and
    /// the mutant workspace's directory name, mirroring `FaultRef.id`.
    pub id: String,
    pub spec: MutantSpec,
    /// Resolved SHA the mutation was applied on top of.
    pub base_commit: String,
    /// The one candidate `apply()` selected out of everything `find_candidates` discovered.
    pub selected_target: MutantTarget,
    /// Exact, human-readable description of what changed — the auditable record of the mutation,
    /// independent of reading a diff. Mirrors `FaultRef.description`.
    pub description: String,
    /// The isolated worktree the mutation was written into directly — never the source
    /// repository itself. `evaluate()`, `restore()`, and `discard()` all operate against this
    /// path; unlike `mutation::MutationEngine::sanity_check`'s throwaway evaluation worktree,
    /// this one stays alive after `apply()` so evaluation can happen independently, later.
    /// Rust-level name unified with `ExperimentRecord.worktree_path`/`BisectRecord.worktree_path`/
    /// `FaultRef.worktree_path`; the on-disk/JSON key stays `workspace_path` so the tested
    /// TOML/`--json` contract doesn't change.
    #[serde(rename = "workspace_path")]
    pub worktree_path: PathBuf,
    pub diff_stats: DiffStats,
    /// When `apply()` ran. Deliberately NOT part of the reproducibility/determinism contract —
    /// replaying the same `(operator, target_glob, seed, operator_version, base_commit)` must
    /// reproduce the same `selected_target`/`description`/`diff_stats` regardless of when it's
    /// replayed or which `id` it's applied under.
    pub applied_at: DateTime<Utc>,
    /// `None` until `MutantTester::evaluate` runs. Unlike `mutation::MutationRef.sanity_checked`
    /// (a bool gate that causes rejection), this is a full recorded outcome with no gating
    /// behavior — `apply()` always persists the record regardless of whether or how it's later
    /// evaluated.
    pub evaluation: Option<MutantEvaluation>,
}
