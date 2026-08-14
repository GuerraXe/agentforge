//! CLI surface for `verify` — argument parsing plus true end-to-end invocations of the compiled
//! `agentforge` binary. Like `bisect`, `verify` never spawns an agent adapter, so both the
//! `--ref` and `--experiment` shapes are exercisable through the real binary. SPEC.md §11.

mod common;

use std::path::Path;
use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand};
use agentforge::git::worktree::WorktreeManager;
use agentforge::store::Store;
use clap::Parser;

#[test]
fn parses_verify_ref_form() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "verify",
        "--repo",
        "/some/repo",
        "--evaluator",
        "eval-1",
        "--ref",
        "HEAD",
        "--json",
    ])
    .expect("should parse");
    match cli.command {
        CliCommand::Verify(args) => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.evaluator, Some("eval-1".to_string()));
            assert_eq!(args.r#ref, Some("HEAD".to_string()));
            assert!(args.experiment.is_none());
            assert!(args.json);
        }
        other => panic!("expected Verify, got {other:?}"),
    }
}

#[test]
fn parses_verify_experiment_form() {
    let cli = Cli::try_parse_from(["agentforge", "verify", "--experiment", "exp-1"])
        .expect("should parse");
    match cli.command {
        CliCommand::Verify(args) => {
            assert_eq!(args.experiment, Some("exp-1".to_string()));
            assert!(args.r#ref.is_none());
        }
        other => panic!("expected Verify, got {other:?}"),
    }
}

#[test]
fn rejects_verify_with_neither_ref_nor_experiment() {
    let result = Cli::try_parse_from(["agentforge", "verify", "--evaluator", "eval-1"]);
    assert!(result.is_err(), "verify must require --ref or --experiment");
}

#[test]
fn rejects_verify_with_both_ref_and_experiment() {
    let result = Cli::try_parse_from([
        "agentforge",
        "verify",
        "--ref",
        "HEAD",
        "--experiment",
        "exp-1",
    ]);
    assert!(
        result.is_err(),
        "verify's --ref and --experiment are mutually exclusive"
    );
}

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

fn always_succeeds_cmd() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/C".to_string(), "exit 0".to_string()],
        )
    } else {
        ("true".to_string(), vec![])
    }
}

fn always_fails_cmd() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/C".to_string(), "exit 1".to_string()],
        )
    } else {
        ("false".to_string(), vec![])
    }
}

fn write_evaluator_toml(
    repo: &Path,
    id: &str,
    program: &str,
    args: &[String],
) -> std::path::PathBuf {
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

#[test]
fn e2e_verify_ref_succeeds_with_a_good_verdict() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let (program, args) = always_succeeds_cmd();
    let eval_path = write_evaluator_toml(repo.path(), "eval-good", &program, &args);
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
    let verify = agentforge_cmd()
        .args([
            "verify",
            "--repo",
            &repo_str,
            "--evaluator",
            "eval-good",
            "--ref",
            &head,
        ])
        .output()
        .expect("run agentforge verify");
    assert!(
        verify.status.success(),
        "verify failed: stdout={} stderr={}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("GOOD"));
}

#[test]
fn e2e_verify_ref_exits_3_with_a_bad_verdict() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let (program, args) = always_fails_cmd();
    let eval_path = write_evaluator_toml(repo.path(), "eval-bad", &program, &args);
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
    let verify = agentforge_cmd()
        .args([
            "verify",
            "--repo",
            &repo_str,
            "--evaluator",
            "eval-bad",
            "--ref",
            &head,
        ])
        .output()
        .expect("run agentforge verify");
    assert_eq!(
        verify.status.code(),
        Some(3),
        "a bad verdict must exit 3: stdout={} stderr={}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("BAD"));
}

#[test]
fn e2e_verify_json_emits_a_parseable_verdict() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let (program, args) = always_succeeds_cmd();
    let eval_path = write_evaluator_toml(repo.path(), "eval-json", &program, &args);
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
    let verify = agentforge_cmd()
        .args([
            "verify",
            "--repo",
            &repo_str,
            "--evaluator",
            "eval-json",
            "--ref",
            &head,
            "--json",
        ])
        .output()
        .expect("run agentforge verify --json");
    assert!(verify.status.success());
    let stdout = String::from_utf8_lossy(&verify.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected valid JSON, got error {e}: {stdout}"));
    assert_eq!(value["build_succeeded"].as_bool(), Some(true));
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

#[test]
fn e2e_verify_experiment_reevaluates_the_stored_patch() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let (program, args) = always_succeeds_cmd();
    let eval_path = write_evaluator_toml(repo.path(), "eval-exp", &program, &args);
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
    let task_path = write_task_toml(repo.path(), "task-verify", "eval-exp", &head);
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

    // Seed an experiment record directly through Store — standing in for `run`, which needs a
    // real adapter this suite can't invoke — with a real (empty) patch file, exactly the shape
    // `verify --experiment` reads.
    let state_root = WorktreeManager::resolve_state_root(repo.path()).expect("resolve_state_root");
    let store = Store::open(repo.path().join(".agentforge"), state_root);
    let patch_path = repo.path().join("empty.patch");
    std::fs::write(&patch_path, "").expect("write empty patch");
    let mut record = common::experiment_record(
        "exp-verify",
        "task-verify",
        common::fake_agent_config("claude-code"),
    );
    record.base_ref = head;
    record.patch_path = patch_path;
    store.save_experiment(&record).expect("save_experiment");

    let verify = agentforge_cmd()
        .args(["verify", "--repo", &repo_str, "--experiment", "exp-verify"])
        .output()
        .expect("run agentforge verify --experiment");
    assert!(
        verify.status.success(),
        "verify --experiment failed: stdout={} stderr={}",
        String::from_utf8_lossy(&verify.stdout),
        String::from_utf8_lossy(&verify.stderr)
    );
    assert!(String::from_utf8_lossy(&verify.stdout).contains("GOOD"));
}

#[test]
fn e2e_verify_reports_exit_2_for_an_unknown_evaluator() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let head = common::head_sha(repo.path());

    let verify = agentforge_cmd()
        .args([
            "verify",
            "--repo",
            &repo_str,
            "--evaluator",
            "does-not-exist",
            "--ref",
            &head,
        ])
        .output()
        .expect("run agentforge verify");
    assert_eq!(verify.status.code(), Some(2));
}
