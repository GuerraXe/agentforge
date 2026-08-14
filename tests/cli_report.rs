//! CLI surface for `report score`/`report show` — argument parsing plus true end-to-end
//! invocations of the compiled `agentforge` binary, mirroring tests/cli_mutant.rs's structure.
//! SPEC.md §6, §13, §14.
//!
//! The end-to-end tests populate `.agentforge/`/the state root directly via `Store` — exactly
//! the persisted shape `run` would have produced — then invoke the real binary to read it back,
//! so these tests exercise `report` in isolation from `run`'s own real-adapter requirement.

mod common;

use std::path::Path;
use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand, ReportAction};
use agentforge::domain::{DiffStats, EvaluatorVerdict, ExperimentStatus};
use agentforge::git::worktree::WorktreeManager;
use agentforge::store::Store;
use clap::Parser;

// ---- argument parsing ----------------------------------------------------------------------

#[test]
fn parses_score_with_all_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "report",
        "score",
        "--repo",
        "/some/repo",
        "--weights",
        "alt.toml",
        "--verbose",
        "--json",
        "exp-1",
    ])
    .expect("should parse");

    match cli.command {
        CliCommand::Report {
            action: ReportAction::Score(args),
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.experiment_id, "exp-1");
            assert_eq!(args.weights, Some(std::path::PathBuf::from("alt.toml")));
            assert!(args.verbose);
            assert!(args.json);
        }
        other => panic!("expected Report(Score), got {other:?}"),
    }
}

#[test]
fn parses_score_with_defaults() {
    let cli =
        Cli::try_parse_from(["agentforge", "report", "score", "exp-1"]).expect("should parse");
    match cli.command {
        CliCommand::Report {
            action: ReportAction::Score(args),
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("."));
            assert_eq!(args.experiment_id, "exp-1");
            assert_eq!(args.weights, None);
            assert!(!args.verbose);
            assert!(!args.json);
        }
        other => panic!("expected Report(Score), got {other:?}"),
    }
}

#[test]
fn parses_show_with_all_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "report",
        "show",
        "--repo",
        "/some/repo",
        "--verbose",
        "--json",
        "race-1",
    ])
    .expect("should parse");

    match cli.command {
        CliCommand::Report {
            action: ReportAction::Show(args),
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.id, "race-1");
            assert!(args.verbose);
            assert!(args.json);
        }
        other => panic!("expected Report(Show), got {other:?}"),
    }
}

// ---- end-to-end (real binary) --------------------------------------------------------------

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

fn write_evaluator_toml(repo: &Path, id: &str) -> std::path::PathBuf {
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
    repo: &Path,
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

/// Seeds an evaluated, scored experiment directly through `Store` — standing in for a `run` that
/// isn't implemented yet, using exactly the persisted layout `run` would have written.
fn seed_experiment(repo: &Path, task_id: &str, evaluator_id: &str, experiment_id: &str) {
    let state_root = WorktreeManager::resolve_state_root(repo).expect("resolve_state_root");
    let store = Store::open(repo.join(".agentforge"), state_root);

    let verdict = EvaluatorVerdict {
        build_succeeded: true,
        tests_total: Some(10),
        tests_passed: Some(10),
        exit_code: 0,
        timed_out: false,
        wall_time_secs: 5.0,
    };
    let baseline = EvaluatorVerdict {
        wall_time_secs: 1.0,
        ..verdict.clone()
    };
    let diff = DiffStats {
        files_changed: 1,
        lines_added: 10,
        lines_removed: 2,
    };
    let metrics = agentforge::domain::RawMetrics {
        verdict: verdict.clone(),
        diff,
        agent_timed_out: false,
    };
    let evaluator_spec = agentforge::domain::EvaluatorSpec {
        id: evaluator_id.to_string(),
        setup_cmds: vec![],
        test_cmd: agentforge::domain::Cmd {
            program: "true".to_string(),
            args: vec![],
            cwd_relative: ".".into(),
        },
        timeout_secs: 30,
        budget_secs: 60,
        size_budget_lines: 200,
        metric_extractors: vec![],
    };
    let weights = agentforge::scoring::default_weights();
    let score = agentforge::scoring::score(&metrics, &baseline, &evaluator_spec, &weights);

    let mut record = common::experiment_record(
        experiment_id,
        task_id,
        common::fake_agent_config("claude-code"),
    );
    record.status = ExperimentStatus::Completed;
    record.raw_metrics = Some(metrics);
    record.score = Some(score);
    store.save_experiment(&record).expect("save_experiment");
}

#[test]
fn e2e_show_prints_a_human_readable_report_for_an_experiment() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let eval_path = write_evaluator_toml(repo.path(), "eval-1");
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

    let base_ref = common::head_sha(repo.path());
    let task_path = write_task_toml(repo.path(), "task-1", "eval-1", &base_ref);
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

    seed_experiment(repo.path(), "task-1", "eval-1", "exp-1");

    let show = agentforge_cmd()
        .args(["report", "show", "--repo", &repo_str, "exp-1"])
        .output()
        .expect("run agentforge show");
    assert!(
        show.status.success(),
        "show failed: stdout={} stderr={}",
        String::from_utf8_lossy(&show.stdout),
        String::from_utf8_lossy(&show.stderr)
    );
    let stdout = String::from_utf8_lossy(&show.stdout);
    assert!(stdout.contains("exp-1"), "{stdout}");
    assert!(stdout.contains("10/10"), "{stdout}");
    assert!(stdout.contains("Score"), "{stdout}");
}

#[test]
fn e2e_show_json_emits_parseable_json_with_raw_metrics_and_score() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let eval_path = write_evaluator_toml(repo.path(), "eval-1");
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
    let base_ref = common::head_sha(repo.path());
    let task_path = write_task_toml(repo.path(), "task-1", "eval-1", &base_ref);
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

    seed_experiment(repo.path(), "task-1", "eval-1", "exp-1");

    let show = agentforge_cmd()
        .args(["report", "show", "--repo", &repo_str, "--json", "exp-1"])
        .output()
        .expect("run agentforge show --json");
    assert!(show.status.success());
    let stdout = String::from_utf8_lossy(&show.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected valid JSON, got error {e}: {stdout}"));
    assert_eq!(value["id"].as_str(), Some("exp-1"));
    assert!(value["raw_metrics"]["verdict"]["tests_passed"]
        .as_u64()
        .is_some());
    assert!(value["score"]["total"].as_u64().is_some());
    assert!(value["failed_checks"].is_array());
}

#[test]
fn e2e_score_recomputes_from_persisted_raw_metrics() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let eval_path = write_evaluator_toml(repo.path(), "eval-1");
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
    let base_ref = common::head_sha(repo.path());
    let task_path = write_task_toml(repo.path(), "task-1", "eval-1", &base_ref);
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

    seed_experiment(repo.path(), "task-1", "eval-1", "exp-1");

    let score = agentforge_cmd()
        .args(["report", "score", "--repo", &repo_str, "exp-1"])
        .output()
        .expect("run agentforge score");
    assert!(
        score.status.success(),
        "score failed: stdout={} stderr={}",
        String::from_utf8_lossy(&score.stdout),
        String::from_utf8_lossy(&score.stderr)
    );
    let stdout = String::from_utf8_lossy(&score.stdout);
    assert!(stdout.contains("Score"), "{stdout}");
    assert!(stdout.contains("10/10"), "{stdout}");
}

#[test]
fn e2e_show_reports_exit_2_for_an_unknown_id() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let show = agentforge_cmd()
        .args(["report", "show", "--repo", &repo_str, "does-not-exist"])
        .output()
        .expect("run agentforge show");
    assert!(!show.status.success());
    assert_eq!(show.status.code(), Some(2));
}
