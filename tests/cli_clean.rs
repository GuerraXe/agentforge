//! CLI surface for `clean` — argument parsing plus true end-to-end invocations of the compiled
//! `agentforge` binary. `run` isn't available to this suite (needs a real agent adapter), so
//! these tests seed `ExperimentRecord`s directly through `Store`/`WorktreeManager` — exactly the
//! persisted shape `run` would have produced — then invoke the real binary against them.
//! SPEC.md §6, §7.

mod common;

use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand};
use agentforge::domain::ExperimentStatus;
use agentforge::git::worktree::WorktreeManager;
use agentforge::store::Store;
use clap::Parser;

#[test]
fn parses_clean_with_all_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "clean",
        "--repo",
        "/some/repo",
        "--experiment",
        "exp-1",
        "--all-worktrees",
        "--older-than",
        "24h",
        "--force",
    ])
    .expect("should parse");
    match cli.command {
        CliCommand::Clean(args) => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.experiment, Some("exp-1".to_string()));
            assert!(args.all_worktrees);
            assert_eq!(args.older_than, Some("24h".to_string()));
            assert!(args.force);
        }
        other => panic!("expected Clean, got {other:?}"),
    }
}

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

fn seed_running_experiment(repo: &std::path::Path, id: &str, lock: bool) {
    let git = common::git_repo(repo);
    let state_root = WorktreeManager::resolve_state_root(repo).expect("resolve_state_root");
    let wt = WorktreeManager::new(state_root.clone(), git);
    if lock {
        wt.mark_running(id).expect("mark_running");
    }
    let store = Store::open(repo.join(".agentforge"), state_root);
    let mut record =
        common::experiment_record(id, "task-x", common::fake_agent_config("claude-code"));
    record.status = ExperimentStatus::Running;
    store.save_experiment(&record).expect("save_experiment");
}

fn experiment_status_via_show(repo_str: &str, id: &str) -> String {
    let show = agentforge_cmd()
        .args(["report", "show", "--repo", repo_str, "--json", id])
        .output()
        .expect("run agentforge report show --json");
    assert!(show.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&show.stdout).expect("valid JSON from report show");
    value["status"].as_str().expect("status field").to_string()
}

#[test]
fn e2e_clean_reconciles_an_orphaned_running_experiment_to_failed() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    seed_running_experiment(repo.path(), "exp-orphan", false);

    let clean = agentforge_cmd()
        .args(["clean", "--repo", &repo_str])
        .output()
        .expect("run agentforge clean");
    assert!(
        clean.status.success(),
        "clean failed: stdout={} stderr={}",
        String::from_utf8_lossy(&clean.stdout),
        String::from_utf8_lossy(&clean.stderr)
    );
    assert!(String::from_utf8_lossy(&clean.stdout).contains("reconciled"));

    assert_eq!(
        experiment_status_via_show(&repo_str, "exp-orphan"),
        "Failed"
    );
}

#[test]
fn e2e_clean_does_not_reconcile_a_genuinely_locked_experiment() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    seed_running_experiment(repo.path(), "exp-active", true);

    agentforge_cmd()
        .args(["clean", "--repo", &repo_str])
        .output()
        .expect("run agentforge clean");

    assert_eq!(
        experiment_status_via_show(&repo_str, "exp-active"),
        "Running"
    );
}

#[test]
fn e2e_clean_experiment_removes_its_worktree() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    seed_running_experiment(repo.path(), "exp-remove", false);

    let clean = agentforge_cmd()
        .args(["clean", "--repo", &repo_str, "--experiment", "exp-remove"])
        .output()
        .expect("run agentforge clean --experiment");
    assert!(clean.status.success());
    assert!(String::from_utf8_lossy(&clean.stdout).contains("removed worktree for exp-remove"));
}

#[test]
fn e2e_clean_refuses_a_locked_worktree_without_force() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    seed_running_experiment(repo.path(), "exp-locked", true);

    let clean = agentforge_cmd()
        .args(["clean", "--repo", &repo_str, "--experiment", "exp-locked"])
        .output()
        .expect("run agentforge clean --experiment");
    assert!(clean.status.success(), "clean itself doesn't fail loudly");
    assert!(
        String::from_utf8_lossy(&clean.stderr).contains("skipping exp-locked"),
        "stderr={}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert!(!String::from_utf8_lossy(&clean.stdout).contains("removed worktree for exp-locked"));

    let forced = agentforge_cmd()
        .args([
            "clean",
            "--repo",
            &repo_str,
            "--experiment",
            "exp-locked",
            "--force",
        ])
        .output()
        .expect("run agentforge clean --experiment --force");
    assert!(forced.status.success());
    assert!(String::from_utf8_lossy(&forced.stdout).contains("removed worktree for exp-locked"));
}

#[test]
fn e2e_clean_older_than_skips_a_recent_experiment() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    seed_running_experiment(repo.path(), "exp-recent", false);

    let clean = agentforge_cmd()
        .args([
            "clean",
            "--repo",
            &repo_str,
            "--all-worktrees",
            "--older-than",
            "999d",
        ])
        .output()
        .expect("run agentforge clean --older-than");
    assert!(clean.status.success());
    assert!(
        !String::from_utf8_lossy(&clean.stdout).contains("removed worktree for exp-recent"),
        "a just-created experiment must not look 999 days old"
    );
}

#[test]
fn e2e_clean_rejects_a_malformed_duration() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let clean = agentforge_cmd()
        .args([
            "clean",
            "--repo",
            &repo_str,
            "--all-worktrees",
            "--older-than",
            "not-a-duration",
        ])
        .output()
        .expect("run agentforge clean --older-than");
    assert_eq!(clean.status.code(), Some(2));
}
