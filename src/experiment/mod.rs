//! The one `run` primitive — SPEC.md §8, docs/ARCHITECTURE.md §10.
//!
//! Everything else that produces an `ExperimentRecord` (namely `race`) calls
//! `ExperimentRunner::run`. Bisect deliberately does not — a bisect step is not an experiment.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use thiserror::Error;

use crate::adapter::AgentAdapter;
use crate::audit::JsonlAuditSink;
use crate::domain::{
    AgentConfig, ExecutionBudget, ExperimentRecord, ExperimentStatus, PermissionPolicy, RawMetrics,
    ScoreCard, TaskSpec,
};
use crate::evaluator::Evaluator;
use crate::exec::Executor;
use crate::git::worktree::WorktreeManager;
use crate::git::GitRepo;
use crate::store::Store;

#[derive(Debug, Error)]
pub enum Error {
    #[error("git error: {0}")]
    Git(#[from] crate::git::Error),
    #[error("execution error: {0}")]
    Exec(#[from] crate::exec::Error),
    #[error("evaluator error: {0}")]
    Evaluator(#[from] crate::evaluator::Error),
    #[error("store error: {0}")]
    Store(#[from] crate::store::Error),
    #[error("audit error: {0}")]
    Audit(#[from] crate::audit::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A participant thread panicked instead of returning a `Result` — should never happen under
    /// normal operation (`run_inner` itself converts every internal failure into `Failed`, never
    /// panics), but `race::RaceRunner::run_race` still guards against it: without this, one
    /// participant panicking (e.g. from a bug introduced later on a path that processes
    /// repo/agent-controlled data) would unwind past `std::thread::scope`, losing every already-
    /// collected result for the whole race rather than just failing that one participant.
    #[error("participant thread panicked: {0}")]
    Panicked(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct ExperimentRunner {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    exec: Arc<dyn Executor>,
    evaluator: Arc<Evaluator>,
    store: Arc<Store>,
}

impl ExperimentRunner {
    pub fn new(
        git: Arc<GitRepo>,
        worktrees: Arc<WorktreeManager>,
        exec: Arc<dyn Executor>,
        evaluator: Arc<Evaluator>,
        store: Arc<Store>,
    ) -> Self {
        ExperimentRunner {
            git,
            worktrees,
            exec,
            evaluator,
            store,
        }
    }

    /// Create worktree → adapter.command_for → Executor.spawn → capture patch via `git diff` →
    /// evaluate() → score → write record → remove worktree — SPEC.md §8.
    ///
    /// Every error surfaced once the `Running` record exists (spawn refused/failed, `git diff`,
    /// `evaluate()`, a missing evaluator) collapses to `status = Failed` on the finalized record
    /// rather than an `Err` — SPEC.md §12 (F3): "an AgentForge-internal error, not a judgment
    /// about a patch" is something callers (notably `race`) need to see as a normal, inspectable
    /// result, not a `Result::Err` that would otherwise force them to invent a placeholder
    /// record. Only failures *before* a record exists at all (worktree creation, the first
    /// persisted save) propagate as `Err`, since there is nothing yet to mark `Failed`.
    pub fn run(
        &self,
        task: &TaskSpec,
        agent: &dyn AgentAdapter,
        agent_config: &AgentConfig,
        policy: &PermissionPolicy,
    ) -> Result<ExperimentRecord> {
        self.run_inner(task, agent, agent_config, policy, false)
    }

    /// Identical to `run`, except the worktree is preserved (not removed) when the finalized
    /// record's status is `Failed` or `TimedOut` — the `run`-CLI-level `--keep-worktree-on-fail`
    /// flag (SPEC.md §6/§7), layered on top of the primitive rather than built into `run` itself
    /// since `race` never wants this behavior. A `Completed` status (good or bad verdict) still
    /// always removes the worktree — SPEC.md §7's "unless the experiment did not complete" only
    /// covers `Failed`/`TimedOut`.
    pub fn run_keep_worktree_on_fail(
        &self,
        task: &TaskSpec,
        agent: &dyn AgentAdapter,
        agent_config: &AgentConfig,
        policy: &PermissionPolicy,
    ) -> Result<ExperimentRecord> {
        self.run_inner(task, agent, agent_config, policy, true)
    }

    fn run_inner(
        &self,
        task: &TaskSpec,
        agent: &dyn AgentAdapter,
        agent_config: &AgentConfig,
        policy: &PermissionPolicy,
        keep_worktree_on_fail: bool,
    ) -> Result<ExperimentRecord> {
        let experiment_id = crate::domain::ids::new_id(Utc::now());
        let handle = self
            .worktrees
            .create_experiment_worktree(&experiment_id, &task.base_ref)?;

        let dir = self.experiment_dir(&experiment_id);
        let mut record = ExperimentRecord {
            id: experiment_id.clone(),
            task_id: task.id.clone(),
            agent_config: agent_config.clone(),
            policy_snapshot: policy.clone(),
            base_ref: task.base_ref.clone(),
            mutation_ref: task.mutation.clone(),
            race_id: None,
            race_index: None,
            worktree_path: handle.path.clone(),
            started_at: Utc::now(),
            ended_at: None,
            status: ExperimentStatus::Running,
            patch_path: dir.join("patch.diff"),
            raw_metrics: None,
            score: None,
            audit_log_path: dir.join("audit.jsonl"),
        };

        self.store.save_experiment(&record)?;
        self.worktrees.mark_running(&experiment_id)?;

        match self.execute(&record, &handle.path, agent, agent_config, policy, task) {
            Ok((status, raw_metrics, score)) => {
                record.status = status;
                record.raw_metrics = Some(raw_metrics);
                record.score = Some(score);
            }
            Err(_) => {
                record.status = ExperimentStatus::Failed;
            }
        }
        record.ended_at = Some(Utc::now());

        // The lock is cleared before the final save rewrites `status` to a terminal value —
        // SPEC.md §7's run-lock protocol — so `clean`'s reconciliation pass never observes a
        // window where `status == Running` and the lock is also present-but-stale.
        let _ = self.worktrees.clear_running(&experiment_id);
        self.store.save_experiment(&record)?;
        // Removed unconditionally once finalized — SPEC.md §7 ("removed after the experiment
        // finalizes unless preserved on failure") — unless the caller asked to keep it via
        // `run_keep_worktree_on_fail` and the run actually landed on `Failed`/`TimedOut`.
        let preserve = keep_worktree_on_fail
            && matches!(
                record.status,
                ExperimentStatus::Failed | ExperimentStatus::TimedOut
            );
        if !preserve {
            let _ = self.worktrees.remove(&handle);
        }

        Ok(record)
    }

    fn experiment_dir(&self, id: &str) -> PathBuf {
        self.worktrees.state_root().join("experiments").join(id)
    }

    /// Spawns the agent, captures its patch via `git diff` — adapter-independent, always run
    /// after the agent process exits or is killed, never trusting the adapter to self-report —
    /// then evaluates and scores it. Returns the terminal, non-`Running` status this run reached.
    fn execute(
        &self,
        record: &ExperimentRecord,
        worktree_path: &Path,
        agent: &dyn AgentAdapter,
        agent_config: &AgentConfig,
        policy: &PermissionPolicy,
        task: &TaskSpec,
    ) -> Result<(ExperimentStatus, RawMetrics, ScoreCard)> {
        let audit = JsonlAuditSink::open(record.audit_log_path.clone())?;

        let spec = agent.command_for(
            &task.prompt,
            agent_config.model.as_deref(),
            &agent_config.extra_args,
        );
        let budget = ExecutionBudget {
            timeout_secs: task.agent_timeout_secs,
            max_output_bytes: policy.max_output_bytes,
        };
        let outcome = self.exec.spawn(
            &spec,
            worktree_path,
            &policy.env_passthrough,
            &budget,
            &policy.command_policy(),
            &audit,
        )?;
        // The agent's raw stdout/stderr never crosses this module boundary (SPEC.md §1 —
        // reporting consumes the structured audit log/patch/verdict, not terminal output), so
        // nothing ever reads these paths again; leaving them on disk would leak two files per
        // experiment into the OS temp dir permanently.
        crate::exec::remove_file_best_effort(&outcome.stdout_path);
        crate::exec::remove_file_best_effort(&outcome.stderr_path);

        let diff = self.git.diff(worktree_path, &record.base_ref)?;
        std::fs::write(&record.patch_path, &diff)?;
        let diff_stats = self.git.diff_stats(worktree_path, &record.base_ref)?;

        let evaluator_spec = self.store.load_evaluator(&task.evaluator)?;
        let verdict = self
            .evaluator
            .evaluate(worktree_path, &evaluator_spec, &audit)?;

        let raw_metrics = RawMetrics {
            verdict,
            diff: diff_stats,
            agent_timed_out: outcome.timed_out,
        };
        let weights = self.store.load_scoring_weights()?;
        let score = crate::scoring::score(&raw_metrics, &task.baseline, &evaluator_spec, &weights);

        let status = if outcome.timed_out {
            ExperimentStatus::TimedOut
        } else {
            ExperimentStatus::Completed
        };

        Ok((status, raw_metrics, score))
    }
}
