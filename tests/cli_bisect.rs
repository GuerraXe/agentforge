//! CLI surface for `bisect` — argument parsing plus true end-to-end invocations of the compiled
//! `agentforge` binary. Unlike `run`/`race`, `bisect` never spawns an agent adapter, so a full
//! success path is exercisable through the real binary — mirrors tests/bisect.rs's fixture shape
//! (a tracked `status.flag` file that flips from `GOOD` to `BAD` partway through a commit range).
//! SPEC.md §13.

mod common;

use std::path::Path;
use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand};
use clap::Parser;

#[test]
fn parses_bisect_with_all_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "bisect",
        "--repo",
        "/some/repo",
        "--task",
        "t1",
        "--range",
        "good..bad",
        "--json",
    ])
    .expect("should parse");

    match cli.command {
        CliCommand::Bisect(args) => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.task, "t1");
            assert_eq!(args.range, "good..bad");
            assert!(args.json);
        }
        other => panic!("expected Bisect, got {other:?}"),
    }
}

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

fn status_flag_test_cmd() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                "findstr /C:GOOD status.flag >nul && exit /b 0 || exit /b 1".to_string(),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                "grep -q GOOD status.flag && exit 0 || exit 1".to_string(),
            ],
        )
    }
}

fn write_status_flag_evaluator(repo: &Path, id: &str) -> std::path::PathBuf {
    let (program, args) = status_flag_test_cmd();
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

/// Sets up a repo with a tracked `status.flag` that is `GOOD` for the seed commit plus two more,
/// then `BAD` for two further commits, adds a matching evaluator and task via the real CLI, and
/// returns `(repo, repo_str, good_sha, bad_sha, culprit_sha)`.
fn bisect_fixture(
    evaluator_id: &str,
    task_id: &str,
) -> (common::TempRepo, String, String, String, String) {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    std::fs::write(repo.path().join("status.flag"), "GOOD\n").expect("write status.flag");
    common::run_git(repo.path(), &["add", "."]);
    let out = common::run_git(repo.path(), &["commit", "-q", "-m", "seed status"]);
    assert!(out.status.success());
    let good = common::head_sha(repo.path());

    // Each commit's content must be byte-distinct from the previous one (a unique marker line),
    // even when the meaningful GOOD/BAD value repeats — otherwise `git commit` no-ops on an
    // unchanged tree.
    common::commit_file(repo.path(), "status.flag", "GOOD\n# c1\n", "c1");
    let culprit = common::commit_file(repo.path(), "status.flag", "BAD\n# c2\n", "c2 (flip)");
    let bad = common::commit_file(repo.path(), "status.flag", "BAD\n# c3\n", "c3");

    let eval_path = write_status_flag_evaluator(repo.path(), evaluator_id);
    let add_eval = agentforge_cmd()
        .args([
            "evaluator",
            "add",
            "--repo",
            &repo_str,
            &eval_path.to_string_lossy(),
        ])
        .output()
        .expect("run agentforge evaluator add");
    assert!(add_eval.status.success());

    let task_toml = format!(
        r#"
id = "{task_id}"
name = "{task_id}"
prompt = "n/a"
repo_path = "."
base_ref = "{good}"
evaluator = "{evaluator_id}"
agent_timeout_secs = 30
created_at = "2026-01-01T00:00:00Z"

[baseline]
build_succeeded = true
exit_code = 0
timed_out = false
wall_time_secs = 0.0
"#
    );
    let task_path = repo.path().join("task.toml");
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
        .expect("run agentforge task add");
    assert!(
        add_task.status.success(),
        "task add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add_task.stdout),
        String::from_utf8_lossy(&add_task.stderr)
    );

    (repo, repo_str, good, bad, culprit)
}

#[test]
fn e2e_bisect_finds_the_exact_culprit_commit() {
    let (_repo, repo_str, good, bad, culprit) = bisect_fixture("eval-bisect", "task-bisect");
    let range = format!("{good}..{bad}");

    let bisect = agentforge_cmd()
        .args([
            "bisect",
            "--repo",
            &repo_str,
            "--task",
            "task-bisect",
            "--range",
            &range,
        ])
        .output()
        .expect("run agentforge bisect");
    assert!(
        bisect.status.success(),
        "bisect failed: stdout={} stderr={}",
        String::from_utf8_lossy(&bisect.stdout),
        String::from_utf8_lossy(&bisect.stderr)
    );
    let stdout = String::from_utf8_lossy(&bisect.stdout);
    assert!(
        stdout.contains(&culprit),
        "expected culprit {culprit} in output: {stdout}"
    );
}

#[test]
fn e2e_bisect_json_reports_the_culprit_field() {
    let (_repo, repo_str, good, bad, culprit) =
        bisect_fixture("eval-bisect-json", "task-bisect-json");
    let range = format!("{good}..{bad}");

    let bisect = agentforge_cmd()
        .args([
            "bisect",
            "--repo",
            &repo_str,
            "--task",
            "task-bisect-json",
            "--range",
            &range,
            "--json",
        ])
        .output()
        .expect("run agentforge bisect --json");
    assert!(bisect.status.success());
    let stdout = String::from_utf8_lossy(&bisect.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected valid JSON, got error {e}: {stdout}"));
    assert_eq!(value["culprit"].as_str(), Some(culprit.as_str()));
}

#[test]
fn e2e_bisect_exits_3_when_the_whole_range_shares_one_verdict() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    std::fs::write(repo.path().join("status.flag"), "GOOD\n").expect("write status.flag");
    common::run_git(repo.path(), &["add", "."]);
    common::run_git(repo.path(), &["commit", "-q", "-m", "seed status"]);
    let good = common::head_sha(repo.path());
    let bad = common::commit_file(
        repo.path(),
        "status.flag",
        "GOOD\n# still good\n",
        "still good",
    );

    let eval_path = write_status_flag_evaluator(repo.path(), "eval-inconclusive");
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
    let task_toml = format!(
        r#"
id = "task-inconclusive"
name = "task-inconclusive"
prompt = "n/a"
repo_path = "."
base_ref = "{good}"
evaluator = "eval-inconclusive"
agent_timeout_secs = 30
created_at = "2026-01-01T00:00:00Z"

[baseline]
build_succeeded = true
exit_code = 0
timed_out = false
wall_time_secs = 0.0
"#
    );
    let task_path = repo.path().join("task.toml");
    std::fs::write(&task_path, task_toml).expect("write task toml");
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

    let range = format!("{good}..{bad}");
    let bisect = agentforge_cmd()
        .args([
            "bisect",
            "--repo",
            &repo_str,
            "--task",
            "task-inconclusive",
            "--range",
            &range,
        ])
        .output()
        .expect("run agentforge bisect");
    assert_eq!(
        bisect.status.code(),
        Some(3),
        "an inconclusive range must exit 3: stdout={} stderr={}",
        String::from_utf8_lossy(&bisect.stdout),
        String::from_utf8_lossy(&bisect.stderr)
    );
}

#[test]
fn e2e_bisect_rejects_a_malformed_range_with_a_usage_error() {
    let (_repo, repo_str, _good, _bad, _culprit) = bisect_fixture("eval-badrange", "task-badrange");

    let bisect = agentforge_cmd()
        .args([
            "bisect",
            "--repo",
            &repo_str,
            "--task",
            "task-badrange",
            "--range",
            "not-a-range",
        ])
        .output()
        .expect("run agentforge bisect");
    assert_eq!(bisect.status.code(), Some(2));
}

#[test]
fn e2e_bisect_reports_exit_2_for_an_unknown_task() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let bisect = agentforge_cmd()
        .args([
            "bisect",
            "--repo",
            &repo_str,
            "--task",
            "does-not-exist",
            "--range",
            "HEAD..HEAD",
        ])
        .output()
        .expect("run agentforge bisect");
    assert_eq!(bisect.status.code(), Some(2));
}
