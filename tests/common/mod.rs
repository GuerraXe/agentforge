//! Shared fixtures for the integration test suite. Every test that touches a filesystem or a
//! git repository uses a fresh temporary directory (`tempfile::tempdir`) — nothing in this
//! suite ever runs against the real AgentForge project repo.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use std::sync::Mutex;

use agentforge::adapter::fake::FakeAdapter;
use agentforge::bisect::BisectRunner;
use agentforge::domain::{
    AgentConfig, AuditEvent, BisectRecord, BisectStep, Cmd, CommandPolicy, DiffStats,
    EvaluatorSpec, EvaluatorVerdict, ExperimentRecord, ExperimentStatus, FaultKind, FaultRef,
    FaultSpec, FaultTarget, MutantRef, MutantSpec, MutantTarget, MutationOperator,
    PermissionPolicy, ProcessSpec, RaceParticipant, RaceRecord, RawMetrics, TaskSpec,
};
use agentforge::evaluator::Evaluator;
use agentforge::exec::{Executor, SystemExecutor};
use agentforge::experiment::ExperimentRunner;
use agentforge::git::worktree::WorktreeManager;
use agentforge::git::GitRepo;
use agentforge::race::RaceRunner;
use agentforge::store::Store;

/// Owns the `TempDir` so it isn't dropped (and deleted) while a test still needs the path.
pub struct TempRepo {
    pub dir: tempfile::TempDir,
}

impl TempRepo {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Retries a bounded number of times, on Windows only, when git reports "Permission denied"
/// writing a loose object/index/lock file — a transient handle-sharing race with another process
/// (commonly real-time antivirus scanning a just-created file), not an AgentForge bug. See the
/// identical rationale on `git::GitRepo`'s own `run_git_env`, which this mirrors so the full
/// suite's heavy parallel git activity doesn't intermittently fail fixture setup. Any other
/// failure (or a durable permission problem that outlasts the retry budget) is still returned
/// as-is for the caller's own `assert!` to report.
pub fn run_git(cwd: &Path, args: &[&str]) -> std::process::Output {
    const MAX_ATTEMPTS: u32 = if cfg!(windows) { 8 } else { 1 };
    let mut attempt = 0;
    loop {
        attempt += 1;
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("failed to run git — is git on PATH?");
        let transient_windows_lock =
            cfg!(windows) && String::from_utf8_lossy(&out.stderr).contains("Permission denied");
        if out.status.success() || !transient_windows_lock || attempt >= MAX_ATTEMPTS {
            return out;
        }
        std::thread::sleep(std::time::Duration::from_millis(20 * u64::from(attempt)));
    }
}

/// A fresh temporary git repository with one commit, so every test starts from known,
/// disposable state.
pub fn init_temp_repo() -> TempRepo {
    let dir = tempfile::tempdir().expect("create temp dir");
    let out = run_git(dir.path(), &["init", "-q"]);
    assert!(out.status.success(), "git init failed: {out:?}");
    run_git(
        dir.path(),
        &["config", "user.email", "agentforge-test@example.com"],
    );
    run_git(dir.path(), &["config", "user.name", "AgentForge Test"]);
    // Pin line-ending behavior for this fixture repo regardless of the host machine's global
    // git config — a `core.autocrlf=true` host would otherwise smudge LF to CRLF on every
    // `git checkout` (worktree creation, `fault::FaultInjector::restore`, etc.), making
    // on-disk content differ from what a test wrote/committed for reasons unrelated to
    // AgentForge's own logic.
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);
    std::fs::write(dir.path().join("README.md"), "test fixture repo\n").unwrap();
    run_git(dir.path(), &["add", "."]);
    let out = run_git(dir.path(), &["commit", "-q", "-m", "initial"]);
    assert!(out.status.success(), "git commit failed: {out:?}");
    TempRepo { dir }
}

pub fn head_sha(repo: &Path) -> String {
    let out = run_git(repo, &["rev-parse", "HEAD"]);
    assert!(out.status.success(), "rev-parse HEAD failed: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Writes `relative_path` with `contents`, commits it, and returns the new HEAD sha.
pub fn commit_file(repo: &Path, relative_path: &str, contents: &str, message: &str) -> String {
    let full = repo.join(relative_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, contents).unwrap();
    run_git(repo, &["add", relative_path]);
    let out = run_git(repo, &["commit", "-q", "-m", message]);
    assert!(out.status.success(), "commit failed: {out:?}");
    head_sha(repo)
}

/// A unique, disposable directory for use as an external state root — never inside a repo
/// fixture, mirroring SPEC.md's requirement that the real state root lives outside the target
/// repo (§20, U1).
pub fn temp_state_root() -> PathBuf {
    let unique = format!(
        "agentforge-test-state-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

pub fn executor() -> Arc<dyn Executor> {
    Arc::new(SystemExecutor::new())
}

pub fn git_repo(path: &Path) -> Arc<GitRepo> {
    Arc::new(GitRepo::open(path.to_path_buf(), executor()).expect("open git repo"))
}

pub fn worktree_manager(git: Arc<GitRepo>) -> Arc<WorktreeManager> {
    Arc::new(WorktreeManager::new(temp_state_root(), git))
}

/// Returns the `WorkspaceManager` under test alongside the same `WorktreeManager` it was built
/// with, so tests can reach in and simulate lock state directly (`mark_running`) without the
/// public `WorkspaceManager` API needing a test-only backdoor of its own.
pub fn workspace_manager(
    repo_path: &Path,
) -> (
    agentforge::workspace::WorkspaceManager,
    Arc<WorktreeManager>,
) {
    let git = git_repo(repo_path);
    let wt = worktree_manager(git.clone());
    let mgr = agentforge::workspace::WorkspaceManager::new(git, wt.clone(), executor());
    (mgr, wt)
}

pub fn evaluator() -> Arc<Evaluator> {
    Arc::new(Evaluator::new(executor()))
}

pub fn mutant_tester(repo_path: &Path) -> agentforge::mutant::MutantTester {
    let git = git_repo(repo_path);
    let wt = worktree_manager(git.clone());
    let ev = evaluator();
    agentforge::mutant::MutantTester::new(git, wt, ev)
}

pub fn store(repo_path: &Path) -> Arc<Store> {
    store_at(repo_path, temp_state_root())
}

/// Like `store`, but at a caller-chosen `state_root` rather than a fresh random one — used to
/// build a `Store` that agrees with some other component (e.g. a `WorktreeManager`) about where
/// external state lives.
pub fn store_at(repo_path: &Path, state_root: PathBuf) -> Arc<Store> {
    let repo_root = repo_path.join(".agentforge");
    Arc::new(Store::open(repo_root, state_root))
}

/// Builds an `ExperimentRunner` and returns it alongside the *same* `Store` it was wired with —
/// unlike a bare `store(repo_path)` call, which would resolve to an independent, unrelated
/// external state root (each call to `store`/`worktree_manager` picks a fresh random one via
/// `temp_state_root`). Tests that need to read back what `run`/`race` persisted (e.g. through
/// `Reporter`) must go through this shared store, not construct their own.
pub fn experiment_runner_with_store(repo_path: &Path) -> (Arc<ExperimentRunner>, Arc<Store>) {
    let git = git_repo(repo_path);
    let state_root = temp_state_root();
    let wt = Arc::new(WorktreeManager::new(state_root.clone(), git.clone()));
    let ev = evaluator();
    let st = store_at(repo_path, state_root);
    (
        Arc::new(ExperimentRunner::new(git, wt, executor(), ev, st.clone())),
        st,
    )
}

pub fn experiment_runner(repo_path: &Path) -> Arc<ExperimentRunner> {
    experiment_runner_with_store(repo_path).0
}

/// Builds a `RaceRunner` and returns it alongside the same `Store` its underlying
/// `ExperimentRunner` persists through — see `experiment_runner_with_store`'s doc for why this
/// matters.
pub fn race_runner_with_store(repo_path: &Path) -> (RaceRunner, Arc<Store>) {
    let (runner, st) = experiment_runner_with_store(repo_path);
    (RaceRunner::new(runner, st.clone()), st)
}

pub fn race_runner(repo_path: &Path) -> RaceRunner {
    race_runner_with_store(repo_path).0
}

/// Builds a `BisectRunner` and returns it alongside the same `Store` it persists through — see
/// `experiment_runner_with_store`'s doc for why this matters (tests that read back a persisted
/// `BisectRecord` must go through this shared store, not construct their own).
pub fn bisect_runner_with_store(repo_path: &Path) -> (BisectRunner, Arc<Store>) {
    let git = git_repo(repo_path);
    let state_root = temp_state_root();
    let wt = Arc::new(WorktreeManager::new(state_root.clone(), git.clone()));
    let ev = evaluator();
    let st = store_at(repo_path, state_root);
    (BisectRunner::new(git, wt, ev, st.clone()), st)
}

pub fn bisect_runner(repo_path: &Path) -> BisectRunner {
    bisect_runner_with_store(repo_path).0
}

/// A cross-platform trivially-succeeding command.
fn true_cmd() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/C".to_string(), "exit 0".to_string()],
        )
    } else {
        ("true".to_string(), vec![])
    }
}

/// A cross-platform trivially-failing command.
fn false_cmd() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/C".to_string(), "exit 1".to_string()],
        )
    } else {
        ("false".to_string(), vec![])
    }
}

/// An evaluator whose `test_cmd` always succeeds and reports no test counts.
pub fn noop_evaluator_spec(id: &str) -> EvaluatorSpec {
    let (program, args) = true_cmd();
    EvaluatorSpec {
        id: id.to_string(),
        setup_cmds: vec![],
        test_cmd: Cmd {
            program,
            args,
            cwd_relative: ".".into(),
        },
        timeout_secs: 30,
        budget_secs: 60,
        size_budget_lines: 200,
        metric_extractors: vec![],
    }
}

/// An evaluator whose `test_cmd` always fails — for build/verdict-failure fixtures.
pub fn failing_evaluator_spec(id: &str) -> EvaluatorSpec {
    let (program, args) = false_cmd();
    EvaluatorSpec {
        test_cmd: Cmd {
            program,
            args,
            cwd_relative: ".".into(),
        },
        ..noop_evaluator_spec(id)
    }
}

pub fn valid_permission_policy(name: &str) -> PermissionPolicy {
    PermissionPolicy {
        name: name.to_string(),
        extra_readonly_paths: vec![],
        deny_network: true,
        env_passthrough: vec![],
        max_wall_time_secs: 30,
        max_output_bytes: 1_000_000,
        allowed_programs: vec![],
        denied_programs: vec![],
        allowed_roots: vec![],
        max_memory_bytes: None,
    }
}

/// No program/cwd restriction configured — the baseline used by tests that only care about
/// budget/env behavior and shouldn't incidentally trip the new command-policy checks.
pub fn unrestricted_command_policy() -> CommandPolicy {
    CommandPolicy::unrestricted()
}

/// Captures every `AuditEvent` recorded during a call, for tests that need to assert on
/// `PermissionCheck` events the way `NullAuditSink`/`JsonlAuditSink` can't (the former discards
/// everything, the latter needs a file round-trip).
#[derive(Default)]
pub struct RecordingAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

impl RecordingAuditSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<AuditEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl agentforge::audit::AuditSink for RecordingAuditSink {
    fn record(&self, event: &AuditEvent) -> agentforge::audit::Result<()> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
        Ok(())
    }
}

pub fn good_verdict() -> EvaluatorVerdict {
    EvaluatorVerdict {
        build_succeeded: true,
        tests_total: Some(10),
        tests_passed: Some(10),
        exit_code: 0,
        timed_out: false,
        wall_time_secs: 1.0,
    }
}

pub fn task_spec(
    id: &str,
    base_ref: &str,
    evaluator_id: &str,
    baseline: EvaluatorVerdict,
) -> TaskSpec {
    TaskSpec {
        id: id.to_string(),
        name: id.to_string(),
        prompt: "fix the thing".to_string(),
        repo_path: PathBuf::from("."),
        base_ref: base_ref.to_string(),
        mutation: None,
        evaluator: evaluator_id.to_string(),
        agent_timeout_secs: 30,
        baseline,
        created_at: chrono::Utc::now(),
    }
}

pub fn fault_ref(id: &str, base_commit: &str, file: &str) -> FaultRef {
    FaultRef {
        id: id.to_string(),
        spec: FaultSpec {
            kind: FaultKind::MissingFile,
            target_glob: "**/*".to_string(),
            seed: 0,
            fault_version: 1,
        },
        base_commit: base_commit.to_string(),
        selected_target: FaultTarget {
            file: file.to_string(),
            line: None,
        },
        description: format!("deleted tracked file {file}"),
        worktree_path: PathBuf::from("/tmp/nonexistent-fault-workspace"),
        diff_stats: DiffStats::default(),
        applied_at: chrono::Utc::now(),
    }
}

pub fn mutant_ref(id: &str, base_commit: &str, file: &str) -> MutantRef {
    MutantRef {
        id: id.to_string(),
        spec: MutantSpec {
            operator: MutationOperator::BooleanFlip,
            target_glob: "**/*".to_string(),
            seed: 0,
            operator_version: 1,
        },
        base_commit: base_commit.to_string(),
        selected_target: MutantTarget {
            file: file.to_string(),
            line: 1,
            column: 1,
        },
        description: format!("{file}:1:1: applied BooleanFlip"),
        worktree_path: PathBuf::from("/tmp/nonexistent-mutant-workspace"),
        diff_stats: DiffStats::default(),
        applied_at: chrono::Utc::now(),
        evaluation: None,
    }
}

pub fn fake_agent_config(name: &str) -> AgentConfig {
    AgentConfig {
        adapter: name.to_string(),
        model: None,
        extra_args: vec![],
        policy: "default".to_string(),
    }
}

pub fn raw_metrics(verdict: EvaluatorVerdict, diff: DiffStats) -> RawMetrics {
    RawMetrics {
        verdict,
        diff,
        agent_timed_out: false,
    }
}

/// A minimal, otherwise-`Running`, unscored `ExperimentRecord` — tests set `status`/
/// `raw_metrics`/`score`/`race_id`/`race_index` directly on the returned struct (every field is
/// `pub`) rather than threading them all through builder parameters.
pub fn experiment_record(id: &str, task_id: &str, agent_config: AgentConfig) -> ExperimentRecord {
    let worktree_path = PathBuf::from(format!("/tmp/nonexistent-worktree-{id}"));
    ExperimentRecord {
        id: id.to_string(),
        task_id: task_id.to_string(),
        agent_config,
        policy_snapshot: valid_permission_policy("default"),
        base_ref: "deadbeef".to_string(),
        mutation_ref: None,
        race_id: None,
        race_index: None,
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: ExperimentStatus::Running,
        patch_path: worktree_path.join("patch.diff"),
        raw_metrics: None,
        score: None,
        audit_log_path: worktree_path.join("audit.jsonl"),
        worktree_path,
    }
}

pub fn race_record(id: &str, task_id: &str, participants: &[(&str, u32)]) -> RaceRecord {
    RaceRecord {
        id: id.to_string(),
        task_id: task_id.to_string(),
        max_parallel: participants.len() as u32,
        participants: participants
            .iter()
            .map(|(experiment_id, race_index)| {
                (
                    experiment_id.to_string(),
                    RaceParticipant {
                        race_index: *race_index,
                    },
                )
            })
            .collect(),
    }
}

pub fn bisect_record(id: &str, task_id: &str, evaluator_id: &str) -> BisectRecord {
    BisectRecord {
        id: id.to_string(),
        task_id: task_id.to_string(),
        evaluator_id: evaluator_id.to_string(),
        range: ("good-sha".to_string(), "bad-sha".to_string()),
        worktree_path: PathBuf::from("/tmp/nonexistent-bisect-worktree"),
        steps: vec![],
        culprit: None,
    }
}

pub fn bisect_step(commit: &str, verdict: EvaluatorVerdict, is_good: bool) -> BisectStep {
    BisectStep {
        commit: commit.to_string(),
        verdict,
        is_good,
    }
}

/// A `FakeAdapter` whose scripted command deterministically writes `marker` into a file in its
/// cwd — used to check that the same script produces the same observable result across runs.
pub fn scripted_fake_adapter(marker: &str) -> Arc<dyn agentforge::adapter::AgentAdapter> {
    let (program, args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                format!("echo {marker} > agent-output.txt"),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                format!("echo {marker} > agent-output.txt"),
            ],
        )
    };
    Arc::new(FakeAdapter {
        scripted_command: ProcessSpec {
            program,
            args,
            extra_env: vec![],
        },
    })
}

/// A `FakeAdapter` that sleeps far longer than any test's configured `agent_timeout_secs` — for
/// exercising `ExperimentStatus::TimedOut`. Mirrors `tests/exec_boundaries.rs`'s own
/// `sleep_command` fixture.
pub fn sleeping_fake_adapter(seconds: u32) -> Arc<dyn agentforge::adapter::AgentAdapter> {
    let (program, args) = if cfg!(windows) {
        (
            "powershell".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                format!("Start-Sleep -Seconds {seconds}"),
            ],
        )
    } else {
        ("sleep".to_string(), vec![seconds.to_string()])
    };
    Arc::new(FakeAdapter {
        scripted_command: ProcessSpec {
            program,
            args,
            extra_env: vec![],
        },
    })
}

/// A `FakeAdapter` that overwrites a tracked file's content in its cwd — unlike
/// `scripted_fake_adapter`'s untracked marker file, this shows up in `git diff` (untracked files
/// never do). Used to give different race participants genuinely different, evaluator-visible
/// patches.
pub fn file_writing_fake_adapter(
    relative_path: &str,
    contents: &str,
) -> Arc<dyn agentforge::adapter::AgentAdapter> {
    let (program, args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                format!("echo {contents}> {relative_path}"),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                format!("echo {contents} > {relative_path}"),
            ],
        )
    };
    Arc::new(FakeAdapter {
        scripted_command: ProcessSpec {
            program,
            args,
            extra_env: vec![],
        },
    })
}

/// A `FakeAdapter` whose scripted command points at a program that doesn't exist — for
/// exercising an `ExperimentRunner::run` participant that fails internally (spawn refused before
/// any process runs), independent of any evaluator/agent judgment.
pub fn broken_fake_adapter() -> Arc<dyn agentforge::adapter::AgentAdapter> {
    Arc::new(FakeAdapter {
        scripted_command: ProcessSpec {
            program: "definitely-not-a-real-binary-agentforge-race-test-xyz".to_string(),
            args: vec![],
            extra_env: vec![],
        },
    })
}

/// An adapter whose `command_for` panics instead of returning — for the adversarial-review
/// regression proving `race::RaceRunner::run_race` isolates a panicking participant (via
/// `std::panic::catch_unwind`) rather than losing every other participant's already-collected
/// result when one thread unwinds. `ExperimentRunner::run` itself never panics in production
/// (SPEC.md §12 F3), so this fixture simulates the only realistic way a participant thread could
/// still panic: a bug on a path that runs adapter/repo-controlled code.
pub struct PanickingAdapter;

impl agentforge::adapter::AgentAdapter for PanickingAdapter {
    fn name(&self) -> &str {
        "panicking"
    }

    fn capabilities(&self) -> agentforge::adapter::AdapterCapabilities {
        agentforge::adapter::AdapterCapabilities {
            can_confine_filesystem: agentforge::domain::EnforcementLevel::Enforced,
            can_restrict_network: agentforge::domain::EnforcementLevel::Enforced,
        }
    }

    fn command_for(
        &self,
        _prompt: &str,
        _model: Option<&str>,
        _extra_args: &[String],
    ) -> ProcessSpec {
        panic!("PanickingAdapter: deliberate panic for the race-panic-isolation regression test");
    }
}

pub fn panicking_fake_adapter() -> Arc<dyn agentforge::adapter::AgentAdapter> {
    Arc::new(PanickingAdapter)
}
