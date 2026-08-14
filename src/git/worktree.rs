//! Worktree lifecycle policy layered on top of `GitRepo` — SPEC.md §7, docs/ARCHITECTURE.md §6.
//!
//! Five flavors: experiment (one per `run`), bisect (one per bisect session, revisited via
//! checkout), evaluation (throwaway, no agent involved — used by `task add`'s baseline capture,
//! `mutate`'s sanity gate, and `eval`), fault (one per `fault inject` — a materialized,
//! addressable workspace a repository fault is written into directly, SPEC.md §10 Amendment),
//! and mutant (one per `mutant apply` — the same materialized-and-addressable shape as fault,
//! for standalone source mutation testing, SPEC.md §10 Amendment (standalone mutation testing
//! pass)).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{Error, GitRepo, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeKind {
    Experiment,
    Bisect,
    Evaluation,
    Fault,
    Mutant,
}

#[derive(Debug, Clone)]
pub struct WorktreeHandle {
    pub path: PathBuf,
    pub kind: WorktreeKind,
}

pub struct WorktreeManager {
    state_root: PathBuf,
    git: Arc<GitRepo>,
    /// `git worktree add`/`remove` calls are serialized behind this even when experiment
    /// *execution* runs in parallel — `.git/worktrees` metadata isn't safe for concurrent
    /// mutation — SPEC.md §7.
    worktree_lock: Mutex<()>,
}

impl WorktreeManager {
    pub fn new(state_root: PathBuf, git: Arc<GitRepo>) -> Self {
        WorktreeManager {
            state_root,
            git,
            worktree_lock: Mutex::new(()),
        }
    }

    /// Platform data dir + a stable hash of the repo's own canonical path — SPEC.md §5, §20
    /// (U1). Deterministic without needing `config.toml` to exist, and computed with no
    /// dependency on an `Executor` (this associated function has none available), which is why
    /// it hashes the canonicalized `repo_root` itself rather than shelling out to
    /// `git rev-parse --git-common-dir` — for the one caller that matters (resolving state root
    /// for the original repo, never from inside an already-created worktree) they identify the
    /// same repo.
    pub fn resolve_state_root(repo_root: &Path) -> Result<PathBuf> {
        let canonical = repo_root.canonicalize().map_err(Error::Io)?;
        let hash = stable_hash(&canonical.to_string_lossy());
        Ok(platform_data_dir()
            .join("agentforge")
            .join("state")
            .join(hash))
    }

    pub fn create_experiment_worktree(
        &self,
        experiment_id: &str,
        base_ref: &str,
    ) -> Result<WorktreeHandle> {
        let path = self.state_root.join("worktrees").join(experiment_id);
        self.add_worktree(&path, base_ref)?;
        Ok(WorktreeHandle {
            path,
            kind: WorktreeKind::Experiment,
        })
    }

    pub fn create_bisect_worktree(
        &self,
        bisect_id: &str,
        base_ref: &str,
    ) -> Result<WorktreeHandle> {
        let path = self.state_root.join("bisect-worktrees").join(bisect_id);
        self.add_worktree(&path, base_ref)?;
        Ok(WorktreeHandle {
            path,
            kind: WorktreeKind::Bisect,
        })
    }

    /// No keep-on-fail concept for this flavor — no agent involved.
    pub fn create_evaluation_worktree(&self, base_ref: &str) -> Result<WorktreeHandle> {
        let id = crate::domain::ids::new_id(chrono::Utc::now());
        let path = self.state_root.join("tmp").join(format!("eval-{id}"));
        self.add_worktree(&path, base_ref)?;
        Ok(WorktreeHandle {
            path,
            kind: WorktreeKind::Evaluation,
        })
    }

    /// A materialized, addressable workspace for `fault::FaultInjector` — unlike
    /// `mutation::MutationEngine::apply` (pure git plumbing, no worktree, SPEC.md §20 U4),
    /// repository fault injection needs real files on disk: "this file is missing" or "this
    /// generated artifact is stale" are working-tree states, not things a git blob/tree/commit
    /// can express on their own. Caller-provided `id` is trusted to already be validated (the
    /// same `[A-Za-z0-9_-]`-only rule `workspace::validate_id` enforces) — this method itself
    /// does no validation, mirroring `create_experiment_worktree`.
    pub fn create_fault_worktree(&self, id: &str, base_ref: &str) -> Result<WorktreeHandle> {
        let path = self.state_root.join("fault-worktrees").join(id);
        self.add_worktree(&path, base_ref)?;
        Ok(WorktreeHandle {
            path,
            kind: WorktreeKind::Fault,
        })
    }

    /// A materialized, addressable workspace for `mutant::MutantTester` — identical shape to
    /// `create_fault_worktree` (same reasoning: `evaluate` needs a real, still-checked-out
    /// worktree to run a real test command against later, unlike
    /// `mutation::MutationEngine::apply`'s pure git plumbing). Caller-provided `id` is trusted to
    /// already be validated, mirroring `create_fault_worktree`.
    pub fn create_mutant_worktree(&self, id: &str, base_ref: &str) -> Result<WorktreeHandle> {
        let path = self.state_root.join("mutant-worktrees").join(id);
        self.add_worktree(&path, base_ref)?;
        Ok(WorktreeHandle {
            path,
            kind: WorktreeKind::Mutant,
        })
    }

    /// Idempotent: if `handle.path` is already gone (e.g. a second `remove` call, or the
    /// directory was already cleaned up some other way), this is a no-op success rather than an
    /// error — cleanup callers should never have to special-case "already removed."
    pub fn remove(&self, handle: &WorktreeHandle) -> Result<()> {
        let _guard = self.lock();
        if !handle.path.exists() {
            let _ = self.git.prune_worktrees();
            return Ok(());
        }
        self.git.worktree_remove(&handle.path)
    }

    /// The external state root this manager places worktrees under — read-only, so callers
    /// (e.g. `workspace::WorkspaceManager`) can enumerate or validate paths without duplicating
    /// this manager's own copy of the value.
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// Writes `experiments/<experiment_id>/RUNNING.lock` — SPEC.md §7, §20 (F1/M1). The lock
    /// file's *presence*, not the experiment record's own `status` field, is what `clean` uses
    /// to tell a genuinely in-progress experiment apart from one orphaned by a crash.
    pub fn mark_running(&self, experiment_id: &str) -> Result<()> {
        let path = self.lock_file_path(experiment_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        std::fs::write(&path, std::process::id().to_string()).map_err(Error::Io)
    }

    pub fn clear_running(&self, experiment_id: &str) -> Result<()> {
        let path = self.lock_file_path(experiment_id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io(e)),
        }
    }

    pub fn is_locked(&self, experiment_id: &str) -> bool {
        self.lock_file_path(experiment_id).exists()
    }

    fn add_worktree(&self, path: &Path, base_ref: &str) -> Result<()> {
        let _guard = self.lock();
        self.git.worktree_add(path, base_ref)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.worktree_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_file_path(&self, experiment_id: &str) -> PathBuf {
        self.state_root
            .join("experiments")
            .join(experiment_id)
            .join("RUNNING.lock")
    }
}

/// A fixed-seed hash (not `RandomState`, which is randomized per-instance) so the same input
/// always produces the same output, across calls and across process restarts — required for
/// `resolve_state_root` to be deterministic (SPEC.md §20, R1-adjacent guarantee for state
/// discoverability).
fn stable_hash(input: &str) -> String {
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn platform_data_dir() -> PathBuf {
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(local_app_data);
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local").join("share");
    }
    std::env::temp_dir()
}
