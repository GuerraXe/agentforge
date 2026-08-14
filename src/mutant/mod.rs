//! Standalone, reproducible source mutation testing — SPEC.md §10 Amendment (standalone
//! mutation testing pass), docs/ARCHITECTURE.md §9b.
//!
//! A sibling to `fault::FaultInjector`, built the same way: `apply` always materializes an
//! isolated `WorktreeKind::Mutant` worktree at `base_commit` and writes the mutation directly
//! into it via `std::fs` (never the source repository), the resulting `MutantRef` is
//! standalone-persisted (own `Store` collection, own id), and `restore`/`discard` reuse the exact
//! same `GitRepo::restore_path`/`WorktreeManager::remove` cleanup surface `FaultInjector` uses.
//! Candidate discovery and the mutation transform itself are NOT duplicated from
//! `mutation::MutationEngine` — they're called directly (`mutation::scan_line`,
//! `mutation::mutate_file_contents`, `mutation::is_comment_line`, reusing `mutation::Candidate`
//! and `MutationOperator`), so both mechanisms share one set of operator regexes.
//!
//! The one real difference from `mutation::MutationEngine::sanity_check`: evaluation is a
//! separate, later, non-gating step. `apply` never runs the evaluator and never rejects — the
//! mutant workspace stays alive afterward so `evaluate` can run against it independently,
//! whenever a caller asks, writing a real audit trail (unlike `sanity_check`'s
//! `NullAuditSink`) and recording the outcome on the persisted record rather than discarding it.

use std::sync::Arc;

use thiserror::Error;

use crate::audit::AuditSink;
use crate::domain::{EvaluatorSpec, EvaluatorVerdict, MutantRef, MutantSpec, MutantTarget};
use crate::evaluator::Evaluator;
use crate::fault::{IdError, PathEscape};
use crate::git::worktree::{WorktreeHandle, WorktreeKind, WorktreeManager};
use crate::git::GitRepo;
use crate::mutation::Candidate;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git error: {0}")]
    Git(#[from] crate::git::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("evaluator error: {0}")]
    Evaluator(#[from] crate::evaluator::Error),
    #[error("invalid target_glob {glob:?}: {reason}")]
    InvalidGlob { glob: String, reason: String },
    #[error("no candidate sites found for operator {operator:?} matching {glob}")]
    NoCandidates {
        operator: crate::domain::MutationOperator,
        glob: String,
    },
    #[error("mutant id must not be empty")]
    EmptyId,
    #[error("mutant id {0:?} is invalid — only ASCII letters, digits, '-', and '_' are allowed")]
    InvalidId(String),
    #[error("candidate path {0:?} escapes the mutant workspace root — refusing to touch it")]
    PathEscapesWorkspace(String),
    #[error(
        "candidate path {0:?} is a symlink inside the mutant workspace — refusing to follow it \
         off the isolated worktree"
    )]
    TargetIsSymlink(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct MutantTester {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    evaluator: Arc<Evaluator>,
}

impl MutantTester {
    pub fn new(
        git: Arc<GitRepo>,
        worktrees: Arc<WorktreeManager>,
        evaluator: Arc<Evaluator>,
    ) -> Self {
        MutantTester {
            git,
            worktrees,
            evaluator,
        }
    }

    /// Deterministic candidate discovery — identical shape to
    /// `mutation::MutationEngine::find_candidates` (same sort/scan order, same
    /// `mutation::scan_line` regexes), reading blobs at `base_commit` rather than a worktree.
    pub fn find_candidates(&self, base_commit: &str, spec: &MutantSpec) -> Result<Vec<Candidate>> {
        let files =
            crate::mutation::matching_tracked_files(&self.git, base_commit, &spec.target_glob)
                .map_err(|e| match e {
                    crate::mutation::MatchError::InvalidGlob(reason) => Error::InvalidGlob {
                        glob: spec.target_glob.clone(),
                        reason,
                    },
                    crate::mutation::MatchError::Git(e) => Error::Git(e),
                })?;

        let mut candidates = Vec::new();
        for file in &files {
            if !crate::fault::is_safe_relative_path(file) {
                return Err(Error::PathEscapesWorkspace(file.clone()));
            }
            let blob = self.git.read_blob(base_commit, file)?;
            let text = String::from_utf8_lossy(&blob).into_owned();
            for (idx, line) in text.split('\n').enumerate() {
                if crate::mutation::is_comment_line(line) {
                    continue;
                }
                for m in crate::mutation::scan_line(spec.operator, line) {
                    candidates.push(Candidate {
                        file: file.clone(),
                        line: (idx + 1) as u32,
                        column: m.column as u32,
                    });
                }
            }
        }
        Ok(candidates)
    }

    /// Selects `candidates[seed % candidates.len()]`, materializes a fresh `WorktreeKind::Mutant`
    /// worktree checked out at `base_commit`, and writes the mutation directly into it via
    /// `std::fs` — mirrors `FaultInjector::inject` exactly, unlike
    /// `mutation::MutationEngine::apply` (pure git plumbing, no worktree, SPEC.md §20 U4): a
    /// mutant here needs real files on disk so `evaluate` can run a real test command against it
    /// later. The workspace is left materialized on return — never removed until `discard`.
    pub fn apply(&self, base_commit: &str, spec: &MutantSpec, id: &str) -> Result<MutantRef> {
        crate::fault::validate_id(id).map_err(|e| match e {
            IdError::Empty => Error::EmptyId,
            IdError::Invalid(id) => Error::InvalidId(id),
        })?;
        let candidates = self.find_candidates(base_commit, spec)?;
        if candidates.is_empty() {
            return Err(Error::NoCandidates {
                operator: spec.operator,
                glob: spec.target_glob.clone(),
            });
        }
        let chosen = &candidates[(spec.seed % candidates.len() as u64) as usize];

        let handle = self.worktrees.create_mutant_worktree(id, base_commit)?;
        let target_path = crate::fault::safe_join(&handle.path, &chosen.file)
            .map_err(|PathEscape(rel)| Error::PathEscapesWorkspace(rel))?;
        crate::fault::reject_symlink(&target_path)
            .map_err(|_| Error::TargetIsSymlink(chosen.file.clone()))?;

        let original = std::fs::read_to_string(&target_path)?;
        // `chosen` came from `find_candidates` scanning this exact blob (checked out unmodified
        // by `worktree_add`), so a matching site at (line, column) is always found.
        let mutated = crate::mutation::mutate_file_contents(
            &original,
            spec.operator,
            chosen.line,
            chosen.column,
        )
        .expect("candidate selected by find_candidates must be reproducible by apply");
        std::fs::write(&target_path, &mutated)?;

        let description = format!(
            "{}:{}:{}: applied {:?}",
            chosen.file, chosen.line, chosen.column, spec.operator
        );
        let diff_stats = self.git.diff_stats(&handle.path, base_commit)?;

        Ok(MutantRef {
            id: id.to_string(),
            spec: spec.clone(),
            base_commit: base_commit.to_string(),
            selected_target: MutantTarget {
                file: chosen.file.clone(),
                line: chosen.line,
                column: chosen.column,
            },
            description,
            worktree_path: handle.path,
            diff_stats,
            applied_at: chrono::Utc::now(),
            evaluation: None,
        })
    }

    /// Runs the shared `Evaluator::evaluate` directly against `mutant_ref`'s already-materialized
    /// workspace — no worktree is created or removed here, unlike
    /// `mutation::MutationEngine::sanity_check`'s throwaway evaluation worktree. This is what
    /// makes evaluation independent and deferrable: `apply` and `evaluate` are two separate calls
    /// a caller can run minutes or days apart, against the same still-alive workspace. Does not
    /// mutate or persist `mutant_ref` itself — the caller is responsible for recording the
    /// returned verdict onto a `MutantEvaluation` and re-saving the record.
    pub fn evaluate(
        &self,
        mutant_ref: &MutantRef,
        evaluator_spec: &EvaluatorSpec,
        audit: &dyn AuditSink,
    ) -> Result<EvaluatorVerdict> {
        Ok(self
            .evaluator
            .evaluate(&mutant_ref.worktree_path, evaluator_spec, audit)?)
    }

    /// Reverses the mutation in place: `git checkout <base_commit> -- <file>` inside the mutant
    /// workspace — the workspace itself stays alive for reuse. Identical mechanism to
    /// `FaultInjector::restore`.
    pub fn restore(&self, mutant_ref: &MutantRef) -> Result<()> {
        Ok(self.git.restore_path(
            &mutant_ref.worktree_path,
            &mutant_ref.base_commit,
            &mutant_ref.selected_target.file,
        )?)
    }

    /// Removes the entire mutant workspace — the alternative to `restore` when it's no longer
    /// needed. Idempotent, identical mechanism to `FaultInjector::discard`.
    pub fn discard(&self, mutant_ref: &MutantRef) -> Result<()> {
        let handle = WorktreeHandle {
            path: mutant_ref.worktree_path.clone(),
            kind: WorktreeKind::Mutant,
        };
        Ok(self.worktrees.remove(&handle)?)
    }
}
