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
