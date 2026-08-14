//! CLI surface for `run` — argument parsing plus the error paths reachable without a real agent
//! adapter. `run`'s success path spawns whatever `--agent` resolves to
//! (`adapter::resolve` only knows `"claude-code"`, the real Claude Code CLI), so this suite
//! can't exercise a full successful run end-to-end — that's covered at the library level by
//! `tests/experiment_run.rs` via `FakeAdapter`. What *is* exercisable through the real binary
//! without spawning any agent: argument parsing, an unknown task, an unknown adapter name, and
//! an unknown named policy, all of which `cli::run_cmd` must reject before ever resolving an
//! adapter or spawning a worktree. SPEC.md §6, §8.

mod common;

use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand};
use clap::Parser;

#[test]
fn parses_run_with_all_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "run",
        "--repo",
        "/some/repo",
        "--task",
        "t1",
        "--agent",
        "claude-code:opus",
        "--policy",
        "ci",
        "--keep-worktree-on-fail",
        "--json",
    ])
    .expect("should parse");
    match cli.command {
        CliCommand::Run(args) => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.task, "t1");
            assert_eq!(args.agent, "claude-code:opus");
            assert_eq!(args.policy, Some("ci".to_string()));
            assert!(args.keep_worktree_on_fail);
            assert!(args.json);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn parses_run_with_defaults() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "run",
        "--task",
        "t1",
        "--agent",
        "claude-code",
    ])
    .expect("should parse");
    match cli.command {
        CliCommand::Run(args) => {
            assert_eq!(args.repo, std::path::PathBuf::from("."));
            assert_eq!(args.policy, None);
            assert!(!args.keep_worktree_on_fail);
            assert!(!args.json);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

#[test]
fn e2e_run_reports_exit_2_for_an_unknown_task() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let run = agentforge_cmd()
        .args([
            "run",
            "--repo",
            &repo_str,
            "--task",
            "does-not-exist",
            "--agent",
            "claude-code",
        ])
        .output()
        .expect("run agentforge run");
    assert_eq!(run.status.code(), Some(2));
}

fn write_noop_evaluator_toml(repo: &std::path::Path, id: &str) -> std::path::PathBuf {
    let (program, args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/C".to_string(), "exit 0".to_string()],
        )
    } else {
        ("true".to_string(), vec![])
    };
    let toml = format!(
        r#"
id = "{id}"
setup_cmds = []
timeout_secs = 30
budget_secs = 60
size_budget_lines = 200
metric_extractors = []

[test_cmd]
program = "{program}"
args = {args:?}
cwd_relative = "."
"#
    );
    let path = repo.join(format!("{id}.toml"));
    std::fs::write(&path, toml).expect("write evaluator toml");
    path
}

fn write_task_toml(
    repo: &std::path::Path,
    id: &str,
    evaluator_id: &str,
    base_ref: &str,
) -> std::path::PathBuf {
    let toml = format!(
        r#"
id = "{id}"
name = "{id}"
prompt = "fix the thing"
repo_path = "."
base_ref = "{base_ref}"
evaluator = "{evaluator_id}"
agent_timeout_secs = 30
created_at = "2026-01-01T00:00:00Z"

[baseline]
build_succeeded = true
tests_total = 10
tests_passed = 10
exit_code = 0
timed_out = false
wall_time_secs = 1.0
"#
    );
    let path = repo.join(format!("{id}.toml"));
    std::fs::write(&path, toml).expect("write task toml");
    path
}

#[test]
fn e2e_run_reports_exit_2_for_an_unknown_adapter() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let eval_path = write_noop_evaluator_toml(repo.path(), "eval-1");
    agentforge_cmd()
        .args([
            "evaluator",
            "add",
            "--repo",
            &repo_str,
            &eval_path.to_string_lossy(),
        ])
        .output()
        .expect("evaluator add");
    let head = common::head_sha(repo.path());
    let task_path = write_task_toml(repo.path(), "task-1", "eval-1", &head);
    let add_task = agentforge_cmd()
        .args([
            "task",
            "add",
            "--repo",
            &repo_str,
            &task_path.to_string_lossy(),
        ])
        .output()
        .expect("task add");
    assert!(add_task.status.success());

    let run = agentforge_cmd()
        .args([
            "run",
            "--repo",
            &repo_str,
            "--task",
            "task-1",
            "--agent",
            "not-a-real-adapter",
        ])
        .output()
        .expect("run agentforge run");
    assert_eq!(
        run.status.code(),
        Some(2),
        "an unknown adapter is a usage error: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

// ---- run exit-code precedence (SPEC.md §6/§17, Test Plan §18 row 25) --------------------------
//
// `experiment_exit_code` (cli/mod.rs) can't be unit-tested directly — it returns an opaque
// `std::process::ExitCode`, which stable Rust deliberately exposes no accessor for — so the only
// way to prove the mapping is a real subprocess whose OS exit code is inspected, exactly like the
// exit-2 cases above already do. Reuses the same `AGENTFORGE_CLAUDE_EXECUTABLE` /
// `src/bin/mock_claude.rs` substitution `tests/support/demo_scenario.rs` established: a real
// `agentforge run` against a real (if scripted) agent process, zero paid API.

fn mock_claude_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_mock_claude"))
}

/// A tiny fixture distinct from `tests/support/demo_scenario.rs`'s billing scenario (kept
/// self-contained here rather than sharing it, since only `billing/discount.txt` plus one
/// evaluator/task/policy is needed to observe an exit code, not the full demo walkthrough).
/// Returns `(repo, base_sha)`; `agent_timeout_secs` is threaded through so the TimedOut case can
/// use a short budget without affecting the others.
fn build_exit_code_fixture(agent_timeout_secs: u64) -> (common::TempRepo, String) {
    let repo = common::init_temp_repo();
    std::fs::create_dir_all(repo.path().join("billing")).expect("create billing dir");
    common::commit_file(
        repo.path(),
        "billing/discount.txt",
        "DISCOUNT_RATE=0.50\n",
        "seed",
    );
    let base_sha = common::head_sha(repo.path());
    let repo_str = repo.path().to_string_lossy().to_string();

    let (program, args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                "findstr /C:DISCOUNT_RATE=0.10 billing\\discount.txt >nul".to_string(),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                "grep -q DISCOUNT_RATE=0.10 billing/discount.txt".to_string(),
            ],
        )
    };
    let evaluator_toml = format!(
        r#"
id = "discount-check"
setup_cmds = []
timeout_secs = 30
budget_secs = 30
size_budget_lines = 50
metric_extractors = []

[test_cmd]
program = "{program}"
args = {args:?}
cwd_relative = "."
"#
    );
    let evaluator_path = repo.path().join("discount-check.toml");
    std::fs::write(&evaluator_path, evaluator_toml).expect("write evaluator toml");
    let add_evaluator = agentforge_cmd()
        .args([
            "evaluator",
            "add",
            "--repo",
            &repo_str,
            &evaluator_path.to_string_lossy(),
        ])
        .output()
        .expect("evaluator add");
    assert!(add_evaluator.status.success());

    let task_toml = format!(
        r#"
id = "fix-discount"
name = "fix-discount"
prompt = "fix the discount rate"
repo_path = "."
base_ref = "{base_sha}"
evaluator = "discount-check"
agent_timeout_secs = {agent_timeout_secs}
created_at = "2026-01-01T00:00:00Z"

[baseline]
build_succeeded = true
tests_total = 0
tests_passed = 0
exit_code = 1
timed_out = false
wall_time_secs = 1.0
"#
    );
    let task_path = repo.path().join("fix-discount.toml");
    std::fs::write(&task_path, task_toml).expect("write task toml");
    let add_task = agentforge_cmd()
        .args([
            "task",
            "add",
            "--repo",
            &repo_str,
            &task_path.to_string_lossy(),
        ])
        .output()
        .expect("task add");
    assert!(
        add_task.status.success(),
        "task add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add_task.stdout),
        String::from_utf8_lossy(&add_task.stderr)
    );

    (repo, base_sha)
}

#[test]
fn e2e_run_exit_0_for_a_completed_run_with_a_good_verdict() {
    let (repo, _base_sha) = build_exit_code_fixture(30);
    let repo_str = repo.path().to_string_lossy().to_string();

    let run = agentforge_cmd()
        .args([
            "run",
            "--repo",
            &repo_str,
            "--task",
            "fix-discount",
            "--agent",
            "claude-code:goodfix",
        ])
        .env("AGENTFORGE_CLAUDE_EXECUTABLE", mock_claude_bin())
        .output()
        .expect("run agentforge run");
    assert_eq!(
        run.status.code(),
        Some(0),
        "a Completed run with a good (ungated) verdict must exit 0: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn e2e_run_exit_3_for_a_completed_run_with_a_bad_verdict() {
    let (repo, _base_sha) = build_exit_code_fixture(30);
    let repo_str = repo.path().to_string_lossy().to_string();

    let run = agentforge_cmd()
        .args([
            "run",
            "--repo",
            &repo_str,
            "--task",
            "fix-discount",
            "--agent",
            "claude-code:nofix",
        ])
        .env("AGENTFORGE_CLAUDE_EXECUTABLE", mock_claude_bin())
        .output()
        .expect("run agentforge run");
    assert_eq!(
        run.status.code(),
        Some(3),
        "a Completed run with a gated (bad) verdict must exit 3: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn e2e_run_exit_1_for_a_run_that_ends_failed() {
    let (repo, _base_sha) = build_exit_code_fixture(30);
    let repo_str = repo.path().to_string_lossy().to_string();
    let mock = mock_claude_bin().to_string_lossy().into_owned();

    // A policy that denies the very program `run` would otherwise spawn — proven at the library
    // level by `tests/experiment_run.rs`'s `a_spawn_refused_by_policy_ends_failed_not_a_panic_or_err`
    // to end the experiment `Failed`, not `Err`; this closes the loop by checking the real CLI's
    // resulting OS exit code for that `Failed` status.
    let policy_toml = format!(
        r#"
name = "deny-agent"
extra_readonly_paths = []
deny_network = false
env_passthrough = []
max_wall_time_secs = 120
max_output_bytes = 5000000
allowed_programs = []
denied_programs = ['{mock}']
allowed_roots = []
"#
    );
    let policy_path = repo.path().join("deny-agent.toml");
    std::fs::write(&policy_path, policy_toml).expect("write policy toml");
    let add_policy = agentforge_cmd()
        .args([
            "policy",
            "add",
            "--repo",
            &repo_str,
            &policy_path.to_string_lossy(),
        ])
        .output()
        .expect("policy add");
    assert!(
        add_policy.status.success(),
        "policy add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add_policy.stdout),
        String::from_utf8_lossy(&add_policy.stderr)
    );

    let run = agentforge_cmd()
        .args([
            "run",
            "--repo",
            &repo_str,
            "--task",
            "fix-discount",
            "--agent",
            "claude-code:goodfix",
            "--policy",
            "deny-agent",
        ])
        .env("AGENTFORGE_CLAUDE_EXECUTABLE", &mock)
        .output()
        .expect("run agentforge run");
    assert_eq!(
        run.status.code(),
        Some(1),
        "a run whose experiment ends Failed must exit 1: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn e2e_run_exit_124_for_a_run_that_times_out() {
    let (repo, _base_sha) = build_exit_code_fixture(2);
    let repo_str = repo.path().to_string_lossy().to_string();

    // `run` without `--policy` falls back to `PermissionPolicy::generous_default`, whose
    // `env_passthrough` is empty (SPEC.md §16) — without a named policy explicitly allowlisting
    // it, the sleep hook below would never reach the spawned mock_claude process (unlike
    // `AGENTFORGE_CLAUDE_EXECUTABLE`, read in-process by `ClaudeCodeAdapter` before spawning,
    // never passed through to the child at all).
    let policy_toml = r#"
name = "allow-sleep-hook"
extra_readonly_paths = []
deny_network = false
env_passthrough = ["AGENTFORGE_MOCK_CLAUDE_SLEEP_SECS"]
max_wall_time_secs = 120
max_output_bytes = 5000000
allowed_programs = []
denied_programs = []
allowed_roots = []
"#;
    let policy_path = repo.path().join("allow-sleep-hook.toml");
    std::fs::write(&policy_path, policy_toml).expect("write policy toml");
    let add_policy = agentforge_cmd()
        .args([
            "policy",
            "add",
            "--repo",
            &repo_str,
            &policy_path.to_string_lossy(),
        ])
        .output()
        .expect("policy add");
    assert!(add_policy.status.success());

    let start = std::time::Instant::now();
    let run = agentforge_cmd()
        .args([
            "run",
            "--repo",
            &repo_str,
            "--task",
            "fix-discount",
            "--agent",
            "claude-code:goodfix",
            "--policy",
            "allow-sleep-hook",
        ])
        .env("AGENTFORGE_CLAUDE_EXECUTABLE", mock_claude_bin())
        .env("AGENTFORGE_MOCK_CLAUDE_SLEEP_SECS", "30")
        .output()
        .expect("run agentforge run");
    let elapsed = start.elapsed();

    assert_eq!(
        run.status.code(),
        Some(124),
        "a run whose agent process exceeds its timeout must exit 124: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    // The bound here is deliberately wide, not a tight timing assertion: under this suite's own
    // heavy parallel subprocess load (every sibling test in this file spawns real git/process
    // children concurrently), a run observed to genuinely finish in ~3s standalone took upward of
    // 65s under full-suite contention on this dev machine — the same class of transient Windows
    // scheduling/antivirus contention already documented in docs/TEST_STATUS.md's "root-cause
    // debugging pass" — without the kill itself ever failing to fire (`exit 124` above already
    // proves that). What this bound actually guards against is the timeout never being enforced
    // at all: `mock_claude`'s own unforced runtime is a fixed 30s
    // (`AGENTFORGE_MOCK_CLAUDE_SLEEP_SECS`), so a real "no kill happened" regression would still
    // be caught by this bound with wide margin, and would in any case already fail the exit-code
    // assertion above (a run that hits its natural 30s completion exits via the Completed path,
    // not 124).
    assert!(
        elapsed <= std::time::Duration::from_secs(120),
        "a 2s agent_timeout_secs budget must be enforced within a generous bound; took {elapsed:?}"
    );
}

#[test]
fn e2e_run_reports_exit_2_for_an_unknown_named_policy() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let eval_path = write_noop_evaluator_toml(repo.path(), "eval-1");
    agentforge_cmd()
        .args([
            "evaluator",
            "add",
            "--repo",
            &repo_str,
            &eval_path.to_string_lossy(),
        ])
        .output()
        .expect("evaluator add");
    let head = common::head_sha(repo.path());
    let task_path = write_task_toml(repo.path(), "task-1", "eval-1", &head);
    agentforge_cmd()
        .args([
            "task",
            "add",
            "--repo",
            &repo_str,
            &task_path.to_string_lossy(),
        ])
        .output()
        .expect("task add");

    let run = agentforge_cmd()
        .args([
            "run",
            "--repo",
            &repo_str,
            "--task",
            "task-1",
            "--agent",
            "claude-code",
            "--policy",
            "does-not-exist",
        ])
        .output()
        .expect("run agentforge run");
    assert_eq!(
        run.status.code(),
        Some(2),
        "an unknown named policy is a usage error: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}
