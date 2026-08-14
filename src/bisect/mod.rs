//! In-process binary search over `evaluate()` calls — SPEC.md §13, docs/ARCHITECTURE.md §10.
//!
//! Deliberately does not depend on `experiment` — a bisect step produces an `EvaluatorVerdict`,
//! not an `ExperimentRecord`. This is the concrete, checkable form of "race and bisect share
//! evaluator infrastructure": they share the layer below `experiment`, not `experiment` itself.

use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use thiserror::Error;

use crate::audit::JsonlAuditSink;
use crate::domain::{BisectRecord, BisectStep, TaskSpec};
use crate::evaluator::Evaluator;
use crate::git::worktree::WorktreeManager;
use crate::git::GitRepo;
use crate::store::Store;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git error: {0}")]
    Git(#[from] crate::git::Error),
    #[error("evaluator error: {0}")]
    Evaluator(#[from] crate::evaluator::Error),
    #[error("store error: {0}")]
    Store(#[from] crate::store::Error),
    #[error("audit error: {0}")]
    Audit(#[from] crate::audit::Error),
    #[error("`good` ({good}) is not an ancestor of `bad` ({bad})")]
    NotLinear { good: String, bad: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// SPEC.md §13, ARCHITECTURE.md §10 — carries one field beyond ARCHITECTURE.md's original
/// sketch, `store`, for the same reason `experiment::ExperimentRunner` carries one: `TaskSpec`
/// only names an evaluator *id* (`task.evaluator`), so turning it into a real `EvaluatorSpec`
/// needs `Store::load_evaluator`. `store` is also what makes SPEC.md §13 point 6 real
/// ("`steps.jsonl` gets one entry per commit actually tested... appended as it happens"):
/// `Store::save_bisect` always persists a full snapshot (see its own doc comment, which assigns
/// the live append-as-tested behavior to this runner, not to `Store`), so `run_bisect` re-saves
/// after every step rather than once at the end — repeated full-snapshot writes are what turn
/// into an as-it-happens append from an outside reader's view.
pub struct BisectRunner {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    evaluator: Arc<Evaluator>,
    store: Arc<Store>,
}

impl BisectRunner {
    pub fn new(
        git: Arc<GitRepo>,
        worktrees: Arc<WorktreeManager>,
        evaluator: Arc<Evaluator>,
        store: Arc<Store>,
    ) -> Self {
        BisectRunner {
            git,
            worktrees,
            evaluator,
            store,
        }
    }

    /// Resolve `good`/`bad` → require `good` is an ancestor of `bad` (`Error::NotLinear`
    /// otherwise, before any worktree is created — SPEC.md §3.1's linear-range-only limitation)
    /// → build the ordered candidate list via `rev_list_ancestry_path` (excludes `good`,
    /// includes `bad`) → one dedicated worktree for the whole session → binary search over the
    /// candidates, checking out and `evaluate()`-ing only the O(log n) midpoints a textbook
    /// binary search actually visits, recording a `BisectStep` (commit, verdict, `is_good`) for
    /// each — SPEC.md §13.
    ///
    /// A "no flip found" range (every tested candidate is good, so the whole range shares one
    /// verdict — SPEC.md §13 point 5's exit-3 case) comes back as `culprit: None` on an
    /// `Ok(BisectRecord)`, not an `Err` — same convention as `experiment`'s "a judgment about a
    /// patch is a normal result, not an internal failure" (SPEC.md §12 F3), and `mutate`'s
    /// undetectable-mutation verdict, which the caller inspects rather than receiving as an
    /// error. Only a genuine precondition failure (non-linear range) or an infrastructure error
    /// (git/evaluator/store/audit) is `Err`.
    ///
    /// The dedicated worktree is unconditionally removed before returning, on every path once
    /// it's been created — the same "AgentForge-managed isolated Git state, primary checkout
    /// never touched" guarantee `experiment::ExperimentRunner::run` gives (SPEC.md §7, §17);
    /// nothing here ever runs a git command against the caller's own working tree.
    pub fn run_bisect(&self, task: &TaskSpec, good: &str, bad: &str) -> Result<BisectRecord> {
        let good_sha = self.git.resolve_commit(good)?;
        let bad_sha = self.git.resolve_commit(bad)?;
        if !self.git.is_ancestor(&good_sha, &bad_sha)? {
            return Err(Error::NotLinear {
                good: good_sha,
                bad: bad_sha,
            });
        }
        let candidates = self.git.rev_list_ancestry_path(&good_sha, &bad_sha)?;

        let bisect_id = crate::domain::ids::new_id(Utc::now());
        let handle = self
            .worktrees
            .create_bisect_worktree(&bisect_id, &good_sha)?;

        let result = self.search(
            &bisect_id,
            &handle.path,
            task,
            (good_sha, bad_sha),
            candidates,
        );
        let _ = self.worktrees.remove(&handle);
        result
    }

    /// The binary search itself, factored out of `run_bisect` so the worktree cleanup there is
    /// unconditional regardless of which `?` inside this method returns early.
    fn search(
        &self,
        bisect_id: &str,
        worktree_path: &Path,
        task: &TaskSpec,
        range: (String, String),
        candidates: Vec<String>,
    ) -> Result<BisectRecord> {
        let evaluator_spec = self.store.load_evaluator(&task.evaluator)?;
        let audit_path = self
            .worktrees
            .state_root()
            .join("bisects")
            .join(bisect_id)
            .join("audit.jsonl");
        let audit = JsonlAuditSink::open(audit_path)?;

        let mut record = BisectRecord {
            id: bisect_id.to_string(),
            task_id: task.id.clone(),
            evaluator_id: task.evaluator.clone(),
            range,
            worktree_path: worktree_path.to_path_buf(),
            steps: Vec::new(),
            culprit: None,
        };
        self.store.save_bisect(&record)?;

        let mut lo = 0usize;
        let mut hi = candidates.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let commit = candidates[mid].clone();
            self.git.worktree_checkout(worktree_path, &commit)?;
            let verdict = self
                .evaluator
                .evaluate(worktree_path, &evaluator_spec, &audit)?;
            let is_good = verdict.is_good();
            record.steps.push(BisectStep {
                commit,
                verdict,
                is_good,
            });
            self.store.save_bisect(&record)?;
            if is_good {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }

        record.culprit = (lo < candidates.len()).then(|| candidates[lo].clone());
        self.store.save_bisect(&record)?;
        Ok(record)
    }
}
