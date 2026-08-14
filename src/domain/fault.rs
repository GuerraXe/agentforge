//! `FaultKind`, `FaultSpec`, `FaultRef` — SPEC.md §10 Amendment (2026-08-12),
//! docs/ARCHITECTURE.md §9a.
//!
//! Repository fault injection is a second, distinct mechanism from `mutation::MutationRef`/
//! `MutationEngine`: mutation testing injects small logic faults into *tracked source lines* to
//! validate an evaluator's own detection power. Repository fault injection instead simulates a
//! broken *repository/environment state* (a missing file, a corrupted config value, a stale
//! generated artifact, a corrupted dependency pin) as the starting point for a task. The two
//! share git/worktree plumbing but not an operator model or a record type — `FaultRef` is its
//! own standalone-persisted record (via `Store::save_fault`), unlike `MutationRef`, which stays
//! embedded-only in `TaskSpec` (SPEC.md §20 C3): fault injection has no wrapping task to embed
//! into yet, since `experiment`/`ExperimentRunner` doesn't exist yet either.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::experiment::DiffStats;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FaultKind {
    /// Deletes a tracked file from the fault workspace.
    MissingFile,
    /// Rewrites one recognized `key = value` / `key: value` / `"key": value` line in a tracked
    /// file to a fixed, invalid sentinel value.
    BrokenConfigValue,
    /// Overwrites a tracked generated-artifact-like file's entire contents with a fixed,
    /// timestamp-independent stale marker — never touches mtime/ctime, since real-clock
    /// staleness isn't reproducible.
    StaleArtifact,
    /// Corrupts one dependency version pin (a semver-shaped substring) on a tracked line to a
    /// fixed, invalid sentinel version.
    DependencyCorruption,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultSpec {
    pub kind: FaultKind,
    pub target_glob: String,
    pub seed: u64,
    /// Bumped whenever candidate-discovery or transformation behavior changes for `kind` —
    /// mirrors `MutationSpec.operator_version` (SPEC.md §20, R5).
    pub fault_version: u32,
}

/// The candidate site `inject()` actually selected. `line` is `None` for whole-file kinds
/// (`MissingFile`, `StaleArtifact`), which have exactly one candidate per matched file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaultTarget {
    /// Forward-slash-joined, repo-root-relative path — same cross-platform normalization as
    /// `MutationTarget.file` (SPEC.md §20, R4).
    pub file: String,
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaultRef {
    /// Caller-chosen, validated (`[A-Za-z0-9_-]`-only) id — both this record's `Store` key and
    /// the fault workspace's directory name.
    pub id: String,
    pub spec: FaultSpec,
    /// Resolved SHA the fault was injected on top of.
    pub base_commit: String,
    /// The one candidate `inject()` selected out of everything `find_candidates` discovered.
    pub selected_target: FaultTarget,
    /// Exact, human-readable description of what changed — e.g. the before/after line text, or
    /// "deleted tracked file". The auditable record of the fault, independent of reading a diff.
    pub description: String,
    /// The isolated worktree the fault was written into directly — never the source repository
    /// itself. `restore()` and `discard()` both operate against this path. Rust-level name
    /// unified with `ExperimentRecord.worktree_path`/`BisectRecord.worktree_path`/
    /// `MutantRef.worktree_path`; the on-disk/JSON key stays `workspace_path` so the tested
    /// TOML/`--json` contract doesn't change.
    #[serde(rename = "workspace_path")]
    pub worktree_path: PathBuf,
    pub diff_stats: DiffStats,
    /// When `inject()` ran. Deliberately NOT part of the reproducibility contract, mirroring
    /// `MutationRef.applied_at` — replaying the same `(kind, target_glob, seed, fault_version,
    /// base_commit)` must reproduce the same `selected_target`/`description`/`diff_stats`
    /// regardless of when it's replayed or which `id` it's injected under.
    pub applied_at: DateTime<Utc>,
}
