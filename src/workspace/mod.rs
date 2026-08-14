//! Direct, safe CLI-facing management of isolated task worktrees ("workspaces") — SPEC.md §7,
//! docs/ARCHITECTURE.md §6, extended here with a thin, safety-checked layer the CLI drives
//! directly.
//!
//! A workspace is an Experiment-flavor worktree plus its own audit trail, addressed by a
//! caller-chosen id. It is deliberately independent of `Store`/`Evaluator`/`scoring` — creating,
//! inspecting, running a command in, and removing a workspace never requires a `TaskSpec`, an
//! `EvaluatorSpec`, or a score; those compose on top of this later (`experiment::ExperimentRunner`
//! already uses the same `WorktreeManager` underneath).
//!
//! Every path this module ever deletes is constructed by joining a *validated* id onto the
//! state root — never accepted as a raw path from a caller — which is the structural guarantee
//! behind "never remove paths outside AgentForge-owned workspace roots."

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use crate::audit::{AuditSink, JsonlAuditSink};
use crate::domain::{AuditEvent, ExecutionBudget, PermissionPolicy, ProcessOutcome, ProcessSpec};
use crate::exec::Executor;
use crate::git::worktree::{WorktreeHandle, WorktreeKind, WorktreeManager};
use crate::git::GitRepo;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git error: {0}")]
    Git(#[from] crate::git::Error),
    #[error("execution error: {0}")]
    Exec(#[from] crate::exec::Error),
    #[error("audit error: {0}")]
    Audit(#[from] crate::audit::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("workspace not found: {0}")]
    NotFound(String),
    #[error("workspace already exists: {0}")]
    AlreadyExists(String),
    #[error("refusing to touch a path outside AgentForge-owned workspace roots: {0}")]
    PathOutsideRoots(PathBuf),
    #[error("workspace id must not be empty")]
    EmptyId,
    #[error(
        "workspace id {0:?} is invalid — only ASCII letters, digits, '-', and '_' are allowed"
    )]
    InvalidId(String),
    #[error("workspace {0} is in active use (locked) — pass force to override")]
    Locked(String),
    #[error("command must not be empty")]
    EmptyCommand,
}

pub type Result<T> = std::result::Result<T, Error>;

/// A point-in-time inspection of one workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub id: String,
    pub path: PathBuf,
    /// The commit currently checked out inside the workspace.
    pub head: String,
    /// True if a command is currently executing in this workspace (`RUNNING.lock` present) —
    /// SPEC.md §7's lock protocol, reused here rather than a second cleanup mechanism.
    pub locked: bool,
}

pub struct WorkspaceManager {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    exec: Arc<dyn Executor>,
}

impl WorkspaceManager {
    pub fn new(
        git: Arc<GitRepo>,
        worktrees: Arc<WorktreeManager>,
        exec: Arc<dyn Executor>,
    ) -> Self {
        WorkspaceManager {
            git,
            worktrees,
            exec,
        }
    }

    /// Creates an isolated task workspace from `base_ref`, resolved and pinned to a concrete
    /// commit before anything is created — a workspace is never tied to a floating ref. Fails
    /// if `id` is empty, contains anything outside `[A-Za-z0-9_-]`, or a workspace with that id
    /// already exists.
    pub fn create(&self, id: &str, base_ref: &str) -> Result<WorkspaceInfo> {
        validate_id(id)?;
        let path = self.workspace_path(id);
        if path.exists() {
            return Err(Error::AlreadyExists(id.to_string()));
        }

        let resolved = self.git.resolve_commit(base_ref)?;
        let handle = self.worktrees.create_experiment_worktree(id, &resolved)?;
        ensure_within_state_root(self.state_root(), &handle.path)?;

        if let Ok(sink) = self.audit_sink(id) {
            let _ = sink.record(&AuditEvent::WorktreeCreated {
                path: handle.path.clone(),
                base_commit: resolved.clone(),
                at: chrono::Utc::now(),
            });
        }

        Ok(WorkspaceInfo {
            id: id.to_string(),
            path: handle.path,
            head: resolved,
            locked: false,
        })
    }

    /// Lists every existing workspace. The filesystem under the state root's `worktrees/` is
    /// the source of truth — no separate manifest to fall out of sync with it. Entries that
    /// can't be inspected (e.g. a directory that isn't actually a valid worktree) are silently
    /// skipped rather than failing the whole listing.
    pub fn list(&self) -> Result<Vec<WorkspaceInfo>> {
        let root = self.state_root().join("worktrees");
        if !root.is_dir() {
            return Ok(Vec::new());
        }
        let mut infos = Vec::new();
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if let Ok(info) = self.show(&id) {
                infos.push(info);
            }
        }
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(infos)
    }

    pub fn show(&self, id: &str) -> Result<WorkspaceInfo> {
        validate_id(id)?;
        let path = self.workspace_path(id);
        if !path.is_dir() {
            return Err(Error::NotFound(id.to_string()));
        }
        let head = self.git.head_commit(&path)?;
        Ok(WorkspaceInfo {
            id: id.to_string(),
            path,
            head,
            locked: self.worktrees.is_locked(id),
        })
    }

    /// Runs `command` (an argument array — never a shell string) inside the workspace, subject
    /// to `policy` — timeout, output cap, env-var allowlist, and (SPEC.md §16's permission-policy
    /// layer) command-program allow/denylist and cwd-root confinement — through the same
    /// `Executor` everything else uses. A real `JsonlAuditSink` records every policy check and
    /// spawn/exit under this workspace's own audit log. The workspace is marked "running" for
    /// the duration and always unmarked afterward — even if the command fails, is refused by
    /// policy, or the spawn itself errors.
    pub fn exec(
        &self,
        id: &str,
        command: ProcessSpec,
        policy: &PermissionPolicy,
    ) -> Result<ProcessOutcome> {
        validate_id(id)?;
        if command.program.is_empty() {
            return Err(Error::EmptyCommand);
        }
        let path = self.workspace_path(id);
        if !path.is_dir() {
            return Err(Error::NotFound(id.to_string()));
        }
        ensure_within_state_root(self.state_root(), &path)?;

        let budget = ExecutionBudget {
            timeout_secs: policy.max_wall_time_secs,
            max_output_bytes: policy.max_output_bytes,
        };
        let command_policy = policy.command_policy();

        let audit = self.audit_sink(id)?;
        self.worktrees.mark_running(id)?;
        // Captured, not `?`-propagated immediately: `clear_running` must run whether the
        // command succeeded, failed, was refused by policy, or the spawn itself errored, so the
        // lock never outlives this call regardless of outcome.
        let outcome = self.exec.spawn(
            &command,
            &path,
            &policy.env_passthrough,
            &budget,
            &command_policy,
            &audit,
        );
        let _ = self.worktrees.clear_running(id);
        Ok(outcome?)
    }

    /// Removes a single workspace. Refuses if the workspace is currently locked unless `force`
    /// is set. Idempotent: removing an already-gone workspace succeeds without error.
    pub fn remove(&self, id: &str, force: bool) -> Result<()> {
        validate_id(id)?;
        if self.worktrees.is_locked(id) && !force {
            return Err(Error::Locked(id.to_string()));
        }

        let path = self.workspace_path(id);
        ensure_within_state_root(self.state_root(), &path)?;
        let handle = WorktreeHandle {
            path: path.clone(),
            kind: WorktreeKind::Experiment,
        };
        self.worktrees.remove(&handle)?;
        if force {
            let _ = self.worktrees.clear_running(id);
        }

        if let Ok(sink) = self.audit_sink(id) {
            let _ = sink.record(&AuditEvent::WorktreeRemoved {
                path,
                at: chrono::Utc::now(),
            });
        }
        Ok(())
    }

    /// Removes every workspace that is not currently locked; with `force`, removes locked ones
    /// too. Returns the ids actually removed, in the same order `list()` reports them.
    pub fn clean(&self, force: bool) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        for info in self.list()? {
            if info.locked && !force {
                continue;
            }
            self.remove(&info.id, force)?;
            removed.push(info.id);
        }
        Ok(removed)
    }

    /// Path to a workspace's audit log — exposed so callers (e.g. `agentforge workspace show`)
    /// can point a user at it without this module needing to render it themselves.
    pub fn audit_log_path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self
            .state_root()
            .join("experiments")
            .join(id)
            .join("audit.jsonl"))
    }

    fn workspace_path(&self, id: &str) -> PathBuf {
        self.state_root().join("worktrees").join(id)
    }

    fn state_root(&self) -> &Path {
        self.worktrees.state_root()
    }

    fn audit_sink(&self, id: &str) -> Result<JsonlAuditSink> {
        let path = self.audit_log_path(id)?;
        Ok(JsonlAuditSink::open(path)?)
    }
}

/// Rejects anything that isn't a plain, path-separator-free, traversal-free token — the
/// structural guarantee behind "never remove paths outside AgentForge-owned workspace roots":
/// every path this module touches is built by joining a *validated* id onto a known-safe root,
/// never accepted as a raw path from the caller. The `[A-Za-z0-9_-]`-only check itself lives in
/// `domain::ids`, shared with `fault`/`mutant`'s own id validation.
fn validate_id(id: &str) -> Result<()> {
    crate::domain::ids::validate_id(id).map_err(|e| match e {
        crate::domain::ids::IdError::Empty => Error::EmptyId,
        crate::domain::ids::IdError::Invalid(id) => Error::InvalidId(id),
    })
}

/// Defense in depth on top of `validate_id`: confirms a constructed path actually sits inside
/// the state root before anything is allowed to touch it. Every path reaching this check was
/// already built by joining a validated id onto `state_root`, so this can never legitimately
/// fail — it exists to catch a *future* bug (e.g. a call site that forgets `validate_id`), not
/// because it's expected to trigger today.
fn ensure_within_state_root(state_root: &Path, path: &Path) -> Result<()> {
    if !path.starts_with(state_root) {
        return Err(Error::PathOutsideRoots(path.to_path_buf()));
    }
    Ok(())
}
