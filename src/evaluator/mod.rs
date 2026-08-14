//! The one shared judgment — SPEC.md §11, docs/ARCHITECTURE.md §8.
//!
//! `run`, `race`'s participants, `bisect`'s binary search, `mutate`'s sanity gate, `task add`'s
//! baseline capture, and `eval` all call `Evaluator::evaluate`. There is no second
//! implementation.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;

use crate::audit::AuditSink;
use crate::domain::{Cmd, CommandPolicy, EvaluatorSpec, EvaluatorVerdict, ExecutionBudget};
use crate::exec::Executor;

#[derive(Debug, Error)]
pub enum Error {
    #[error("execution error: {0}")]
    Exec(#[from] crate::exec::Error),
    #[error("audit error: {0}")]
    Audit(#[from] crate::audit::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Generous, fixed cap on captured stdout/stderr for evaluator-spawned commands — `EvaluatorSpec`
/// has no output-size field of its own (that's a `PermissionPolicy` concern for agent-controlled
/// spawns; evaluator commands are AgentForge/task-author-controlled, same trust tier as `git`'s
/// own internal calls).
const EVALUATOR_MAX_OUTPUT_BYTES: u64 = 100_000_000;

pub struct Evaluator {
    exec: Arc<dyn Executor>,
}

impl Evaluator {
    pub fn new(exec: Arc<dyn Executor>) -> Self {
        Evaluator { exec }
    }

    /// Takes an already-checked-out worktree path, not a commit — checking out the right commit
    /// is `git`/`WorktreeManager`'s job, which is what lets `bisect` reuse this directly without
    /// going through `experiment` at all.
    ///
    /// 1. Runs `setup_cmds` in order; the first failure short-circuits the rest
    ///    (`build_succeeded = false`, `exit_code` = that command's) — SPEC.md §20 (F4).
    /// 2. Runs `test_cmd`, subject to `spec.timeout_secs`.
    /// 3. Applies `metric_extractors` against the combined output to populate
    ///    `tests_total`/`tests_passed`.
    pub fn evaluate(
        &self,
        worktree_path: &Path,
        spec: &EvaluatorSpec,
        audit: &dyn AuditSink,
    ) -> Result<EvaluatorVerdict> {
        let start = Instant::now();

        for setup_cmd in &spec.setup_cmds {
            let outcome = self.run_cmd(setup_cmd, worktree_path, spec.timeout_secs, audit)?;
            if outcome.timed_out || outcome.exit_code != Some(0) {
                let verdict = EvaluatorVerdict {
                    build_succeeded: false,
                    tests_total: None,
                    tests_passed: None,
                    exit_code: exit_code_or_timeout(&outcome),
                    timed_out: outcome.timed_out,
                    wall_time_secs: start.elapsed().as_secs_f64(),
                };
                remove_captured_output(&outcome);
                return Ok(verdict);
            }
            remove_captured_output(&outcome);
        }

        let outcome = self.run_cmd(&spec.test_cmd, worktree_path, spec.timeout_secs, audit)?;
        let combined = read_combined_output(&outcome);
        let (tests_passed, tests_total) = extract_metrics(&combined, &spec.metric_extractors);
        remove_captured_output(&outcome);

        Ok(EvaluatorVerdict {
            build_succeeded: true,
            tests_total,
            tests_passed,
            exit_code: exit_code_or_timeout(&outcome),
            timed_out: outcome.timed_out,
            wall_time_secs: start.elapsed().as_secs_f64(),
        })
    }

    /// `env_passthrough` is intentionally empty (the fail-closed-empty convention) and
    /// `command_policy` is intentionally unrestricted — evaluator commands are task/evaluator-
    /// author-controlled, not agent-controlled, so the `PermissionPolicy` layer (built for
    /// mediating an *agent's* spawns) doesn't apply here; SystemExecutor's fixed OS-required
    /// passthrough vars (PATH etc.) still let `setup_cmds`/`test_cmd` locate real programs.
    fn run_cmd(
        &self,
        cmd: &Cmd,
        worktree_path: &Path,
        timeout_secs: u64,
        audit: &dyn AuditSink,
    ) -> Result<crate::domain::ProcessOutcome> {
        let spec = crate::domain::ProcessSpec {
            program: cmd.program.clone(),
            args: cmd.args.clone(),
            extra_env: vec![],
        };
        let cwd = worktree_path.join(&cmd.cwd_relative);
        let budget = ExecutionBudget {
            timeout_secs,
            max_output_bytes: EVALUATOR_MAX_OUTPUT_BYTES,
        };
        Ok(self.exec.spawn(
            &spec,
            &cwd,
            &[],
            &budget,
            &CommandPolicy::unrestricted(),
            audit,
        )?)
    }
}

fn exit_code_or_timeout(outcome: &crate::domain::ProcessOutcome) -> i32 {
    if outcome.timed_out {
        return -1;
    }
    outcome.exit_code.unwrap_or(-1)
}

fn read_combined_output(outcome: &crate::domain::ProcessOutcome) -> String {
    let stdout = std::fs::read_to_string(&outcome.stdout_path).unwrap_or_default();
    let stderr = std::fs::read_to_string(&outcome.stderr_path).unwrap_or_default();
    format!("{stdout}\n{stderr}")
}

/// `setup_cmds`/`test_cmd` output is consumed (or deliberately never read, on the short-circuit
/// path) entirely within `evaluate` — unlike `workspace exec`'s captured output, nothing else
/// ever reads these paths again, so each evaluator-spawned command would otherwise leak two files
/// into the OS temp dir permanently.
fn remove_captured_output(outcome: &crate::domain::ProcessOutcome) {
    crate::exec::remove_file_best_effort(&outcome.stdout_path);
    crate::exec::remove_file_best_effort(&outcome.stderr_path);
}

/// Private: raw process output never crosses the module boundary as a `String` — only the
/// structured `EvaluatorVerdict` does. This is the concrete mechanism behind "reporting consumes
/// structured results, not terminal output" (SPEC.md §1, docs/ARCHITECTURE.md §1).
///
/// Each extractor's `name` selects which `EvaluatorVerdict` field it populates — `"tests_passed"`
/// and `"tests_total"` are the two fields with a defined mapping; any other name matches nothing,
/// same as an extractor whose pattern never matches (extractors add granularity, they aren't
/// required — SPEC.md §11).
fn extract_metrics(
    combined_output: &str,
    extractors: &[crate::domain::MetricExtractor],
) -> (Option<u32>, Option<u32>) {
    let mut tests_passed = None;
    let mut tests_total = None;
    for extractor in extractors {
        let Ok(re) = regex::Regex::new(&extractor.pattern) else {
            continue;
        };
        let Some(caps) = re.captures(combined_output) else {
            continue;
        };
        let Some(value) = caps
            .get(extractor.capture_group as usize)
            .and_then(|m| m.as_str().parse::<u32>().ok())
        else {
            continue;
        };
        match extractor.name.as_str() {
            "tests_passed" => tests_passed = Some(value),
            "tests_total" => tests_total = Some(value),
            _ => {}
        }
    }
    (tests_passed, tests_total)
}
