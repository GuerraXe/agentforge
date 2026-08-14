//! CLI surface for `race` — argument parsing, the error paths reachable without a real agent
//! adapter, and (via `src/bin/mock_claude.rs` substituted through `AGENTFORGE_CLAUDE_EXECUTABLE`,
//! the same mechanism `tests/cli_run.rs` and `tests/support/demo_scenario.rs` use) the real
//! `report_race_result` exit-code branch reachable through the compiled binary. Ranking/tie-break
//! correctness is covered at the library level by `tests/race.rs` via `FakeAdapter`. SPEC.md §6,
//! §12.

mod common;

use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand};
use clap::Parser;

#[test]
fn parses_race_with_all_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "race",
        "--repo",
        "/some/repo",
        "--task",
        "t1",
        "--agents",
        "claude-code:opus,claude-code:sonnet",
        "--repeat",
        "2",
        "--max-parallel",
        "3",
        "--json",
    ])
    .expect("should parse");
    match cli.command {
        CliCommand::Race(args) => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.task, "t1");
            assert_eq!(
                args.agents,
                vec![
                    "claude-code:opus".to_string(),
                    "claude-code:sonnet".to_string()
                ]
            );
            assert_eq!(args.repeat, 2);
            assert_eq!(args.max_parallel, Some(3));
            assert!(args.json);
        }
        other => panic!("expected Race, got {other:?}"),
    }
}

#[test]
fn parses_race_with_defaults() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "race",
        "--task",
        "t1",
        "--agents",
        "claude-code",
    ])
    .expect("should parse");
    match cli.command {
        CliCommand::Race(args) => {
            assert_eq!(args.repeat, 1);
            assert_eq!(args.max_parallel, None);
        }
        other => panic!("expected Race, got {other:?}"),
    }
}

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

#[test]
fn e2e_race_reports_exit_2_for_an_unknown_task() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let race = agentforge_cmd()
        .args([
            "race",
            "--repo",
            &repo_str,
            "--task",
            "does-not-exist",
            "--agents",
            "claude-code",
        ])
        .output()
        .expect("run agentforge race");
    assert_eq!(race.status.code(), Some(2));
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
fn e2e_race_reports_exit_2_for_an_unknown_adapter_in_the_agent_list() {
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

    let race = agentforge_cmd()
        .args([
            "race",
            "--repo",
            &repo_str,
            "--task",
            "task-1",
            "--agents",
            "claude-code,not-a-real-adapter",
        ])
        .output()
        .expect("run agentforge race");
    assert_eq!(
        race.status.code(),
        Some(2),
        "an unknown adapter anywhere in --agents is a usage error, checked before any \
         participant runs: stdout={} stderr={}",
        String::from_utf8_lossy(&race.stdout),
        String::from_utf8_lossy(&race.stderr)
    );
}

/// SPEC.md §17/§18 (row 20): "the race process exits 0 if at least one participant completed."
/// `report_race_result` (cli/mod.rs) can't be unit-tested directly (private, and `ExitCode` is
/// opaque in stable Rust — see `tests/cli_run.rs`'s equivalent note for `run`), so this proves it
/// through the real compiled binary. `race` has no `--policy` flag (always uses its own internal
/// `race::default_policy()`, RaceArgs has no such field), so unlike `run` there's no CLI-level way
/// to force every participant to end something other than `Completed` — the exit-1 branch stays
/// unverified through the compiled binary for that structural reason, noted in `docs/VERIFICATION.md`
/// rather than silently left untested.
#[test]
fn e2e_race_exit_0_when_at_least_one_participant_completes() {
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

    let mock = std::path::PathBuf::from(env!("CARGO_BIN_EXE_mock_claude"));
    let race = agentforge_cmd()
        .args([
            "race",
            "--repo",
            &repo_str,
            "--task",
            "task-1",
            "--agents",
            "claude-code:goodfix,claude-code:nofix",
        ])
        .env("AGENTFORGE_CLAUDE_EXECUTABLE", &mock)
        .output()
        .expect("run agentforge race");
    assert_eq!(
        race.status.code(),
        Some(0),
        "a race with at least one Completed participant must exit 0: stdout={} stderr={}",
        String::from_utf8_lossy(&race.stdout),
        String::from_utf8_lossy(&race.stderr)
    );
}
