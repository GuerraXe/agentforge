//! CLI surface for `evaluator`, `task`, and `experiment mutation create`/`show`/`replay` —
//! argument parsing plus true end-to-end invocations of the compiled `agentforge` binary,
//! mirroring tests/cli_workspace.rs's structure.

mod common;

use std::path::Path;
use std::process::Command;

use agentforge::cli::{
    Cli, Command as CliCommand, EvaluatorAction, ExperimentAction, MutationAction, TaskAction,
};
use clap::Parser;

// ---- argument parsing ----------------------------------------------------------------------

#[test]
fn parses_mutation_create_with_all_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "experiment",
        "mutation",
        "create",
        "--repo",
        "/some/repo",
        "--task-id",
        "t1",
        "--spec",
        "spec.toml",
        "--base",
        "HEAD",
        "--evaluator",
        "eval-1",
        "--force",
    ])
    .expect("should parse");

    match cli.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Mutation {
                    action: MutationAction::Create(args),
                },
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.task_id, "t1");
            assert_eq!(args.spec, std::path::PathBuf::from("spec.toml"));
            assert_eq!(args.base, "HEAD");
            assert_eq!(args.evaluator, "eval-1");
            assert!(args.force);
        }
        other => panic!("expected Experiment(Mutation(Create)), got {other:?}"),
    }
}

#[test]
fn parses_mutation_show_and_replay() {
    let show = Cli::try_parse_from([
        "agentforge",
        "experiment",
        "mutation",
        "show",
        "--repo",
        ".",
        "t1",
    ])
    .expect("should parse");
    match show.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Mutation {
                    action: MutationAction::Show(args),
                },
        } => assert_eq!(args.task_id, "t1"),
        other => panic!("expected Experiment(Mutation(Show)), got {other:?}"),
    }

    let replay = Cli::try_parse_from(["agentforge", "experiment", "mutation", "replay", "t1"])
        .expect("should parse");
    match replay.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Mutation {
                    action: MutationAction::Replay(args),
                },
        } => assert_eq!(args.task_id, "t1"),
        other => panic!("expected Experiment(Mutation(Replay)), got {other:?}"),
    }
}

#[test]
fn parses_evaluator_add_list_show() {
    let add = Cli::try_parse_from(["agentforge", "evaluator", "add", "spec.toml", "--force"])
        .expect("should parse");
    match add.command {
        CliCommand::Evaluator {
            action: EvaluatorAction::Add(args),
        } => {
            assert_eq!(args.file, std::path::PathBuf::from("spec.toml"));
            assert!(args.force);
        }
        other => panic!("expected Evaluator(Add), got {other:?}"),
    }

    let show =
        Cli::try_parse_from(["agentforge", "evaluator", "show", "eval-1"]).expect("should parse");
    match show.command {
        CliCommand::Evaluator {
            action: EvaluatorAction::Show(args),
        } => assert_eq!(args.id, "eval-1"),
        other => panic!("expected Evaluator(Show), got {other:?}"),
    }
}

#[test]
fn parses_task_add_list_show() {
    let add =
        Cli::try_parse_from(["agentforge", "task", "add", "task.toml"]).expect("should parse");
    match add.command {
        CliCommand::Task {
            action: TaskAction::Add(args),
        } => assert_eq!(args.file, std::path::PathBuf::from("task.toml")),
        other => panic!("expected Task(Add), got {other:?}"),
    }

    let list = Cli::try_parse_from(["agentforge", "task", "list"]).expect("should parse");
    assert!(matches!(
        list.command,
        CliCommand::Task {
            action: TaskAction::List(_)
        }
    ));
}

// ---- end-to-end (real binary) --------------------------------------------------------------

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

/// A `test_cmd` that fails (nonzero exit) iff `src/flag.txt` contains the literal text `false` —
/// a stand-in "test suite" that catches exactly the fault `BooleanFlip` injects.
fn detect_false_cmd() -> (String, Vec<String>) {
    if cfg!(windows) {
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                "findstr /C:false src\\flag.txt >nul && exit /b 1 || exit /b 0".to_string(),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                "grep -q false src/flag.txt && exit 1 || exit 0".to_string(),
            ],
        )
    }
}

fn write_detecting_evaluator(repo: &Path, id: &str) -> std::path::PathBuf {
    let (program, args) = detect_false_cmd();
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

/// A cross-platform trivially-succeeding command — mirrors `common`'s private `true_cmd`.
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

fn write_noop_evaluator(repo: &Path, id: &str) -> std::path::PathBuf {
    let (program, args) = always_succeeds_cmd();
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

fn write_boolean_flip_spec(repo: &Path, seed: u64) -> std::path::PathBuf {
    let toml = format!(
        r#"
operator = "BooleanFlip"
target_glob = "**/*.txt"
seed = {seed}
operator_version = 1
"#
    );
    let path = repo.join("mutation-spec.toml");
    std::fs::write(&path, toml).expect("write mutation spec toml");
    path
}

#[test]
fn e2e_mutation_create_creates_a_task_when_the_fault_is_detected() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");

    let eval_path = write_detecting_evaluator(repo.path(), "eval-detect");
    write_evaluator_via_cli(&repo_str, &eval_path);
    let spec_path = write_boolean_flip_spec(repo.path(), 0);

    let create = agentforge_cmd()
        .args([
            "experiment",
            "mutation",
            "create",
            "--repo",
            &repo_str,
            "--task-id",
            "t1",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
            "--evaluator",
            "eval-detect",
        ])
        .output()
        .expect("run agentforge experiment mutation create");

    assert!(
        create.status.success(),
        "mutation create should succeed when the fault is detected: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(String::from_utf8_lossy(&create.stdout).contains("created task t1"));

    let show = agentforge_cmd()
        .args(["experiment", "mutation", "show", "--repo", &repo_str, "t1"])
        .output()
        .expect("run agentforge experiment mutation show");
    assert!(show.status.success());
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(show_out.contains("BooleanFlip"));
    assert!(show_out.contains("src/flag.txt"));

    let replay = agentforge_cmd()
        .args([
            "experiment",
            "mutation",
            "replay",
            "--repo",
            &repo_str,
            "t1",
        ])
        .output()
        .expect("run agentforge experiment mutation replay");
    assert!(
        replay.status.success(),
        "replay should reproduce the identical mutant: stdout={} stderr={}",
        String::from_utf8_lossy(&replay.stdout),
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(String::from_utf8_lossy(&replay.stdout).contains("replay OK"));
}

#[test]
fn e2e_mutation_create_rejects_when_the_fault_is_undetectable() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");

    let eval_path = write_noop_evaluator(repo.path(), "eval-toothless");
    write_evaluator_via_cli(&repo_str, &eval_path);
    let spec_path = write_boolean_flip_spec(repo.path(), 0);

    let create = agentforge_cmd()
        .args([
            "experiment",
            "mutation",
            "create",
            "--repo",
            &repo_str,
            "--task-id",
            "t2",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
            "--evaluator",
            "eval-toothless",
        ])
        .output()
        .expect("run agentforge experiment mutation create");

    assert_eq!(
        create.status.code(),
        Some(2),
        "an undetectable mutation must exit 2, not succeed or panic: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );
    assert!(String::from_utf8_lossy(&create.stderr).contains("rejected"));

    let show = agentforge_cmd()
        .args(["task", "show", "--repo", &repo_str, "t2"])
        .output()
        .expect("run agentforge task show");
    assert!(
        !show.status.success(),
        "no task must be created for a rejected mutation"
    );
}

#[test]
fn e2e_evaluator_add_list_and_show_roundtrip() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let eval_path = write_noop_evaluator(repo.path(), "eval-rt");
    write_evaluator_via_cli(&repo_str, &eval_path);

    let list = agentforge_cmd()
        .args(["evaluator", "list", "--repo", &repo_str])
        .output()
        .expect("run agentforge evaluator list");
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("eval-rt"));

    let show = agentforge_cmd()
        .args(["evaluator", "show", "--repo", &repo_str, "eval-rt"])
        .output()
        .expect("run agentforge evaluator show");
    assert!(show.status.success());
    assert!(String::from_utf8_lossy(&show.stdout).contains("eval-rt"));
}

#[test]
fn e2e_task_add_resolves_base_ref_and_captures_baseline() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let eval_path = write_noop_evaluator(repo.path(), "eval-task");
    write_evaluator_via_cli(&repo_str, &eval_path);

    let task_toml = r#"
id = "task-manual"
name = "manual task"
prompt = "fix the thing"
repo_path = "."
base_ref = "HEAD"
evaluator = "eval-task"
agent_timeout_secs = 30
created_at = "2026-01-01T00:00:00Z"

[baseline]
build_succeeded = false
exit_code = 1
timed_out = false
wall_time_secs = 0.0
"#;
    let task_path = repo.path().join("task.toml");
    std::fs::write(&task_path, task_toml).expect("write task toml");

    let add = agentforge_cmd()
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
        add.status.success(),
        "task add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );

    let show = agentforge_cmd()
        .args(["task", "show", "--repo", &repo_str, "task-manual"])
        .output()
        .expect("run agentforge task show");
    assert!(show.status.success());
    let show_out = String::from_utf8_lossy(&show.stdout);
    // base_ref must have been resolved from "HEAD" to a real 40-hex SHA, and baseline must have
    // been recaptured for real (build_succeeded=true, since the noop evaluator always passes) —
    // not left as the placeholder `false` the input file supplied.
    assert!(
        !show_out.contains("base_ref:  HEAD"),
        "base_ref must be resolved, not left as \"HEAD\": {show_out}"
    );
    assert!(
        show_out.contains("build_succeeded=true"),
        "baseline must be recaptured by task add, not left as the file's placeholder: {show_out}"
    );

    let list = agentforge_cmd()
        .args(["task", "list", "--repo", &repo_str])
        .output()
        .expect("run agentforge task list");
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("task-manual"));
}

/// `agentforge evaluator add` end-to-end, used as setup for the `mutation create` tests above.
fn write_evaluator_via_cli(repo_str: &str, eval_path: &std::path::Path) {
    let add = agentforge_cmd()
        .args([
            "evaluator",
            "add",
            "--repo",
            repo_str,
            &eval_path.to_string_lossy(),
        ])
        .output()
        .expect("run agentforge evaluator add");
    assert!(
        add.status.success(),
        "evaluator add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );
}
