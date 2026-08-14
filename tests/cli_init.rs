//! CLI surface for `init` — argument parsing plus true end-to-end invocations of the compiled
//! `agentforge` binary. SPEC.md §5, §6.

mod common;

use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand};
use clap::Parser;

#[test]
fn parses_init_with_flags() {
    let cli = Cli::try_parse_from(["agentforge", "init", "--repo", "/some/repo", "--json"])
        .expect("should parse");
    match cli.command {
        CliCommand::Init(args) => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert!(args.json);
        }
        other => panic!("expected Init, got {other:?}"),
    }
}

#[test]
fn parses_init_with_defaults() {
    let cli = Cli::try_parse_from(["agentforge", "init"]).expect("should parse");
    match cli.command {
        CliCommand::Init(args) => {
            assert_eq!(args.repo, std::path::PathBuf::from("."));
            assert!(!args.json);
        }
        other => panic!("expected Init, got {other:?}"),
    }
}

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

#[test]
fn e2e_init_scaffolds_the_tracked_layout_and_prints_the_state_root() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let init = agentforge_cmd()
        .args(["init", "--repo", &repo_str])
        .output()
        .expect("run agentforge init");
    assert!(
        init.status.success(),
        "init failed: stdout={} stderr={}",
        String::from_utf8_lossy(&init.stdout),
        String::from_utf8_lossy(&init.stderr)
    );
    let stdout = String::from_utf8_lossy(&init.stdout);
    assert!(stdout.contains("state_root"), "{stdout}");

    let agentforge_dir = repo.path().join(".agentforge");
    assert!(agentforge_dir.join("config.toml").is_file());
    assert!(agentforge_dir.join("scoring.toml").is_file());
    assert!(agentforge_dir.join("tasks").is_dir());
    assert!(agentforge_dir.join("evaluators").is_dir());
    assert!(agentforge_dir.join("policies").is_dir());

    let config = std::fs::read_to_string(agentforge_dir.join("config.toml")).unwrap();
    assert!(config.contains("state_root"));
}

#[test]
fn e2e_init_json_emits_parseable_json_with_the_resolved_state_root() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let init = agentforge_cmd()
        .args(["init", "--repo", &repo_str, "--json"])
        .output()
        .expect("run agentforge init --json");
    assert!(init.status.success());
    let stdout = String::from_utf8_lossy(&init.stdout);
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected valid JSON, got error {e}: {stdout}"));
    assert!(value["state_root"].as_str().is_some());
    assert!(value["agentforge_dir"].as_str().is_some());
}

#[test]
fn e2e_init_refuses_to_run_twice() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let first = agentforge_cmd()
        .args(["init", "--repo", &repo_str])
        .output()
        .expect("run agentforge init");
    assert!(first.status.success());

    let second = agentforge_cmd()
        .args(["init", "--repo", &repo_str])
        .output()
        .expect("run agentforge init (second time)");
    assert!(
        !second.status.success(),
        "a second init must not silently succeed"
    );
    assert_eq!(second.status.code(), Some(2));
}

#[test]
fn e2e_init_fails_outside_a_git_repository() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let dir_str = dir.path().to_string_lossy().to_string();

    let init = agentforge_cmd()
        .args(["init", "--repo", &dir_str])
        .output()
        .expect("run agentforge init");
    assert!(!init.status.success());
    assert_eq!(init.status.code(), Some(2));
}
