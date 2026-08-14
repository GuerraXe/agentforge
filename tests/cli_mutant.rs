//! CLI surface for `experiment mutant apply`/`show`/`evaluate`/`restore`/`discard` — argument
//! parsing plus true end-to-end invocations of the compiled `agentforge` binary, mirroring
//! tests/cli_fault.rs's structure. SPEC.md §10 Amendment (standalone mutation testing pass).

mod common;

use std::path::Path;
use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand, ExperimentAction, MutantAction};
use clap::Parser;

// ---- argument parsing ----------------------------------------------------------------------

#[test]
fn parses_mutant_apply_with_all_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "experiment",
        "mutant",
        "apply",
        "--repo",
        "/some/repo",
        "--id",
        "m1",
        "--spec",
        "spec.toml",
        "--base",
        "HEAD",
        "--force",
    ])
    .expect("should parse");

    match cli.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Mutant {
                    action: MutantAction::Apply(args),
                },
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.id, "m1");
            assert_eq!(args.spec, std::path::PathBuf::from("spec.toml"));
            assert_eq!(args.base, "HEAD");
            assert!(args.force);
        }
        other => panic!("expected Experiment(Mutant(Apply)), got {other:?}"),
    }
}

#[test]
fn parses_mutant_show_evaluate_restore_discard() {
    let show = Cli::try_parse_from([
        "agentforge",
        "experiment",
        "mutant",
        "show",
        "--repo",
        ".",
        "m1",
    ])
    .expect("should parse");
    match show.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Mutant {
                    action: MutantAction::Show(args),
                },
        } => assert_eq!(args.id, "m1"),
        other => panic!("expected Experiment(Mutant(Show)), got {other:?}"),
    }

    let evaluate = Cli::try_parse_from([
        "agentforge",
        "experiment",
        "mutant",
        "evaluate",
        "--repo",
        ".",
        "--evaluator",
        "eval-1",
        "m1",
    ])
    .expect("should parse");
    match evaluate.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Mutant {
                    action: MutantAction::Evaluate(args),
                },
        } => {
            assert_eq!(args.id, "m1");
            assert_eq!(args.evaluator, "eval-1");
        }
        other => panic!("expected Experiment(Mutant(Evaluate)), got {other:?}"),
    }

    let restore = Cli::try_parse_from(["agentforge", "experiment", "mutant", "restore", "m1"])
        .expect("should parse");
    match restore.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Mutant {
                    action: MutantAction::Restore(args),
                },
        } => assert_eq!(args.id, "m1"),
        other => panic!("expected Experiment(Mutant(Restore)), got {other:?}"),
    }

    let discard = Cli::try_parse_from(["agentforge", "experiment", "mutant", "discard", "m1"])
        .expect("should parse");
    match discard.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Mutant {
                    action: MutantAction::Discard(args),
                },
        } => assert_eq!(args.id, "m1"),
        other => panic!("expected Experiment(Mutant(Discard)), got {other:?}"),
    }
}

// ---- end-to-end (real binary) --------------------------------------------------------------

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
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
    let path = repo.join("mutant-spec.toml");
    std::fs::write(&path, toml).expect("write mutant spec toml");
    path
}

/// A `test_cmd` that fails (nonzero exit) iff `src/flag.txt` contains the literal text `false` —
/// a stand-in "test suite" that catches exactly the fault `BooleanFlip` injects. Mirrors
/// tests/cli_mutation.rs's `detect_false_cmd`.
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

fn write_evaluator_via_cli(repo_str: &str, eval_path: &Path) {
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

#[test]
fn e2e_mutant_apply_show_evaluate_restore_discard_round_trip() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let spec_path = write_boolean_flip_spec(repo.path(), 0);

    let (program, args) = detect_false_cmd();
    let eval_path = write_evaluator_toml(repo.path(), "eval-detect", &program, &args);
    write_evaluator_via_cli(&repo_str, &eval_path);

    let apply = agentforge_cmd()
        .args([
            "experiment",
            "mutant",
            "apply",
            "--repo",
            &repo_str,
            "--id",
            "m1",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
        ])
        .output()
        .expect("run agentforge experiment mutant apply");
    assert!(
        apply.status.success(),
        "mutant apply failed: stdout={} stderr={}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(String::from_utf8_lossy(&apply.stdout).contains("applied mutant m1"));

    // The source repository itself must never have been touched by apply.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("src/flag.txt")).unwrap(),
        "let ok = true;\n"
    );

    let show = agentforge_cmd()
        .args(["experiment", "mutant", "show", "--repo", &repo_str, "m1"])
        .output()
        .expect("run agentforge experiment mutant show");
    assert!(show.status.success());
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(show_out.contains("BooleanFlip"));
    assert!(show_out.contains("src/flag.txt"));
    assert!(
        show_out.contains("evaluation:    none"),
        "an un-evaluated mutant must clearly report no evaluation yet: {show_out}"
    );

    let evaluate = agentforge_cmd()
        .args([
            "experiment",
            "mutant",
            "evaluate",
            "--repo",
            &repo_str,
            "--evaluator",
            "eval-detect",
            "m1",
        ])
        .output()
        .expect("run agentforge experiment mutant evaluate");
    assert!(
        evaluate.status.success(),
        "mutant evaluate failed: stdout={} stderr={}",
        String::from_utf8_lossy(&evaluate.stdout),
        String::from_utf8_lossy(&evaluate.stderr)
    );
    assert!(String::from_utf8_lossy(&evaluate.stdout).contains("KILLED"));

    let show_after = agentforge_cmd()
        .args(["experiment", "mutant", "show", "--repo", &repo_str, "m1"])
        .output()
        .expect("run agentforge experiment mutant show");
    assert!(show_after.status.success());
    let show_after_out = String::from_utf8_lossy(&show_after.stdout);
    assert!(
        show_after_out.contains("build_succeeded=true"),
        "the recorded evaluation must survive the round trip through Store: {show_after_out}"
    );

    let restore = agentforge_cmd()
        .args(["experiment", "mutant", "restore", "--repo", &repo_str, "m1"])
        .output()
        .expect("run agentforge experiment mutant restore");
    assert!(
        restore.status.success(),
        "mutant restore failed: stdout={} stderr={}",
        String::from_utf8_lossy(&restore.stdout),
        String::from_utf8_lossy(&restore.stderr)
    );

    let discard = agentforge_cmd()
        .args(["experiment", "mutant", "discard", "--repo", &repo_str, "m1"])
        .output()
        .expect("run agentforge experiment mutant discard");
    assert!(
        discard.status.success(),
        "mutant discard failed: stdout={} stderr={}",
        String::from_utf8_lossy(&discard.stdout),
        String::from_utf8_lossy(&discard.stderr)
    );
}

#[test]
fn e2e_mutant_evaluate_reports_survived_for_an_undetectable_mutant() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let spec_path = write_boolean_flip_spec(repo.path(), 0);

    let (program, args) = always_succeeds_cmd();
    let eval_path = write_evaluator_toml(repo.path(), "eval-toothless", &program, &args);
    write_evaluator_via_cli(&repo_str, &eval_path);

    agentforge_cmd()
        .args([
            "experiment",
            "mutant",
            "apply",
            "--repo",
            &repo_str,
            "--id",
            "m2",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
        ])
        .output()
        .expect("run agentforge experiment mutant apply");

    let evaluate = agentforge_cmd()
        .args([
            "experiment",
            "mutant",
            "evaluate",
            "--repo",
            &repo_str,
            "--evaluator",
            "eval-toothless",
            "m2",
        ])
        .output()
        .expect("run agentforge experiment mutant evaluate");
    assert!(
        evaluate.status.success(),
        "evaluate itself must succeed even though the mutant survives — SURVIVED is not an \
         execution error: stdout={} stderr={}",
        String::from_utf8_lossy(&evaluate.stdout),
        String::from_utf8_lossy(&evaluate.stderr)
    );
    assert!(String::from_utf8_lossy(&evaluate.stdout).contains("SURVIVED"));
}

#[test]
fn e2e_mutant_apply_rejects_invalid_id_with_nonzero_exit() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let spec_path = write_boolean_flip_spec(repo.path(), 0);

    let apply = agentforge_cmd()
        .args([
            "experiment",
            "mutant",
            "apply",
            "--repo",
            &repo_str,
            "--id",
            "../escape",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
        ])
        .output()
        .expect("run agentforge experiment mutant apply");

    assert!(!apply.status.success(), "an invalid id must not exit 0");
    assert_eq!(
        apply.status.code(),
        Some(2),
        "an invalid id is a usage error (exit 2): stdout={} stderr={}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );
}

#[test]
fn e2e_mutant_apply_fails_loudly_with_no_candidates() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    common::commit_file(
        repo.path(),
        "src/nums.txt",
        "let x = 1;\n",
        "no booleans here",
    );
    let spec_path = write_boolean_flip_spec(repo.path(), 0);

    let apply = agentforge_cmd()
        .args([
            "experiment",
            "mutant",
            "apply",
            "--repo",
            &repo_str,
            "--id",
            "m-empty",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
        ])
        .output()
        .expect("run agentforge experiment mutant apply");

    assert_eq!(
        apply.status.code(),
        Some(2),
        "zero candidates must exit 2, not succeed or panic: stdout={} stderr={}",
        String::from_utf8_lossy(&apply.stdout),
        String::from_utf8_lossy(&apply.stderr)
    );
}
