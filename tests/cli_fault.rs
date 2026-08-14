//! CLI surface for `experiment fault inject`/`show`/`restore`/`discard` — argument parsing plus
//! true end-to-end invocations of the compiled `agentforge` binary, mirroring
//! tests/cli_mutant.rs's structure. SPEC.md §10 Amendment.

mod common;

use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand, ExperimentAction, FaultAction};
use clap::Parser;

// ---- argument parsing ----------------------------------------------------------------------

#[test]
fn parses_fault_inject_with_all_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "experiment",
        "fault",
        "inject",
        "--repo",
        "/some/repo",
        "--id",
        "f1",
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
                ExperimentAction::Fault {
                    action: FaultAction::Inject(args),
                },
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.id, "f1");
            assert_eq!(args.spec, std::path::PathBuf::from("spec.toml"));
            assert_eq!(args.base, "HEAD");
            assert!(args.force);
        }
        other => panic!("expected Experiment(Fault(Inject)), got {other:?}"),
    }
}

#[test]
fn parses_fault_show_restore_discard() {
    let show = Cli::try_parse_from([
        "agentforge",
        "experiment",
        "fault",
        "show",
        "--repo",
        ".",
        "f1",
    ])
    .expect("should parse");
    match show.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Fault {
                    action: FaultAction::Show(args),
                },
        } => assert_eq!(args.id, "f1"),
        other => panic!("expected Experiment(Fault(Show)), got {other:?}"),
    }

    let restore = Cli::try_parse_from(["agentforge", "experiment", "fault", "restore", "f1"])
        .expect("should parse");
    match restore.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Fault {
                    action: FaultAction::Restore(args),
                },
        } => assert_eq!(args.id, "f1"),
        other => panic!("expected Experiment(Fault(Restore)), got {other:?}"),
    }

    let discard = Cli::try_parse_from(["agentforge", "experiment", "fault", "discard", "f1"])
        .expect("should parse");
    match discard.command {
        CliCommand::Experiment {
            action:
                ExperimentAction::Fault {
                    action: FaultAction::Discard(args),
                },
        } => assert_eq!(args.id, "f1"),
        other => panic!("expected Experiment(Fault(Discard)), got {other:?}"),
    }
}

// ---- end-to-end (real binary) --------------------------------------------------------------

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

fn write_missing_file_spec(repo: &std::path::Path) -> std::path::PathBuf {
    let toml = r#"
kind = "MissingFile"
target_glob = "**/*.txt"
seed = 0
fault_version = 1
"#;
    let path = repo.join("fault-spec.toml");
    std::fs::write(&path, toml).expect("write fault spec toml");
    path
}

#[test]
fn e2e_fault_inject_show_restore_discard_round_trip() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    common::commit_file(repo.path(), "src/data.txt", "hello\n", "add data");
    let spec_path = write_missing_file_spec(repo.path());

    let inject = agentforge_cmd()
        .args([
            "experiment",
            "fault",
            "inject",
            "--repo",
            &repo_str,
            "--id",
            "f1",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
        ])
        .output()
        .expect("run agentforge experiment fault inject");
    assert!(
        inject.status.success(),
        "fault inject failed: stdout={} stderr={}",
        String::from_utf8_lossy(&inject.stdout),
        String::from_utf8_lossy(&inject.stderr)
    );
    assert!(String::from_utf8_lossy(&inject.stdout).contains("injected fault f1"));

    let show = agentforge_cmd()
        .args(["experiment", "fault", "show", "--repo", &repo_str, "f1"])
        .output()
        .expect("run agentforge experiment fault show");
    assert!(show.status.success());
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(show_out.contains("MissingFile"));
    assert!(show_out.contains("src/data.txt"));

    // The source repository itself must never have been touched by inject.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("src/data.txt")).unwrap(),
        "hello\n"
    );

    let restore = agentforge_cmd()
        .args(["experiment", "fault", "restore", "--repo", &repo_str, "f1"])
        .output()
        .expect("run agentforge experiment fault restore");
    assert!(
        restore.status.success(),
        "fault restore failed: stdout={} stderr={}",
        String::from_utf8_lossy(&restore.stdout),
        String::from_utf8_lossy(&restore.stderr)
    );

    let discard = agentforge_cmd()
        .args(["experiment", "fault", "discard", "--repo", &repo_str, "f1"])
        .output()
        .expect("run agentforge experiment fault discard");
    assert!(
        discard.status.success(),
        "fault discard failed: stdout={} stderr={}",
        String::from_utf8_lossy(&discard.stdout),
        String::from_utf8_lossy(&discard.stderr)
    );
}

#[test]
fn e2e_fault_inject_rejects_invalid_id_with_nonzero_exit() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    common::commit_file(repo.path(), "src/data.txt", "hello\n", "add data");
    let spec_path = write_missing_file_spec(repo.path());

    let inject = agentforge_cmd()
        .args([
            "experiment",
            "fault",
            "inject",
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
        .expect("run agentforge experiment fault inject");

    assert!(!inject.status.success(), "an invalid id must not exit 0");
    assert_eq!(
        inject.status.code(),
        Some(2),
        "an invalid id is a usage error (exit 2): stdout={} stderr={}",
        String::from_utf8_lossy(&inject.stdout),
        String::from_utf8_lossy(&inject.stderr)
    );
    assert!(
        !repo.path().parent().unwrap().join("escape").exists(),
        "a rejected id must never cause anything to be created outside the repo"
    );
}

#[test]
fn e2e_fault_inject_fails_loudly_with_no_candidates() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    common::commit_file(
        repo.path(),
        "README.md",
        "no txt files here\n",
        "add readme",
    );
    let spec_path = write_missing_file_spec(repo.path());

    let inject = agentforge_cmd()
        .args([
            "experiment",
            "fault",
            "inject",
            "--repo",
            &repo_str,
            "--id",
            "f-empty",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
        ])
        .output()
        .expect("run agentforge experiment fault inject");

    assert_eq!(
        inject.status.code(),
        Some(2),
        "zero candidates must exit 2, not succeed or panic: stdout={} stderr={}",
        String::from_utf8_lossy(&inject.stdout),
        String::from_utf8_lossy(&inject.stderr)
    );
}
