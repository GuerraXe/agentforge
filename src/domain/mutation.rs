//! `MutationOperator`, `MutationSpec`, `MutationRef` — SPEC.md §4, §10.
//!
//! `MutationRef` is the one reproducible record used by both fault injection (selecting and
//! applying a mutation) and mutation testing (the sanity-gate evaluation of it) — SPEC.md §10
//! treats these as a single feature, not two. It stays embedded-only in `TaskSpec` (SPEC.md §10,
//! §20 C3 — "no standalone, task-less mutation record"); the fields below are additive to that
//! existing contract, not a new parallel record type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::experiment::DiffStats;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MutationOperator {
    NegateCondition,
    OffByOne,
    BooleanFlip,
    DeleteStatement,
    SwapOperator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationSpec {
    pub operator: MutationOperator,
    pub target_glob: String,
    pub seed: u64,
    /// Bumped whenever candidate-discovery behavior changes — SPEC.md §20 (R5).
    pub operator_version: u32,
}

/// The candidate site `apply()` actually selected — `candidates[seed % candidates.len()]`,
/// SPEC.md §10. Recorded so a persisted `MutationRef` says *what* was mutated, not just *how*.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutationTarget {
    /// Forward-slash-joined, repo-root-relative path — SPEC.md §10, §20 (R4).
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationRef {
    pub spec: MutationSpec,
    /// Resolved SHA the mutation was applied on top of.
    pub base_commit: String,
    /// Resulting commit, on `refs/agentforge/mutants/<task-id>` (or, if the mutation is later
    /// rejected by the sanity gate, `refs/agentforge/mutants/rejected/<id>` — see `mutant_ref`).
    pub mutant_commit: String,
    pub sanity_checked: bool,
    /// The one candidate `apply()` selected out of everything `find_candidates` discovered.
    pub selected_target: MutationTarget,
    /// The exact git ref `mutant_commit` is reachable from — restore/cleanup information: nothing
    /// needs restoring (pure plumbing never touches a working tree or `HEAD`), and deleting this
    /// ref (`GitRepo::delete_ref`) is the entire cleanup surface for a mutant no longer needed.
    pub mutant_ref: String,
    /// Structured diff of `mutant_commit` against `base_commit` — SPEC.md §4's `DiffStats`,
    /// reused rather than duplicated.
    pub diff_stats: DiffStats,
    /// When `apply()` ran. Deliberately NOT part of the reproducibility/determinism contract —
    /// replaying the same `(operator, target_glob, seed, operator_version, base_commit)` must
    /// reproduce the same `mutant_commit`/`selected_target`/`diff_stats` regardless of when it's
    /// replayed; only this field is expected to differ between two applications of the same spec.
    pub applied_at: DateTime<Utc>,
}
