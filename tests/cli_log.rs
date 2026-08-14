//! CLI surface for `report log` — argument parsing plus a true end-to-end invocation of the
//! compiled `agentforge` binary against a real `audit.jsonl`. SPEC.md §6.

mod common;

use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand, ReportAction};
use agentforge::domain::AuditEvent;
use agentforge::git::worktree::WorktreeManager;
use agentforge::store::Store;
use clap::Parser;

#[test]
fn parses_log_with_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "report",
        "log",
        "--repo",
        "/some/repo",
        "--follow",
        "exp-1",
    ])
    .expect("should parse");
    match cli.command {
        CliCommand::Report {
            action: ReportAction::Log(args),
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.experiment_id, "exp-1");
            assert!(args.follow);
        }
        other => panic!("expected Report(Log), got {other:?}"),
    }
}

#[test]
fn parses_log_without_follow() {
    let cli = Cli::try_parse_from(["agentforge", "report", "log", "exp-1"]).expect("should parse");
    match cli.command {
        CliCommand::Report {
            action: ReportAction::Log(args),
        } => assert!(!args.follow),
        other => panic!("expected Report(Log), got {other:?}"),
    }
}

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

#[test]
fn e2e_log_pretty_prints_audit_events_in_order_without_follow() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let state_root = WorktreeManager::resolve_state_root(repo.path()).expect("resolve_state_root");
    let store = Store::open(repo.path().join(".agentforge"), state_root);
    let audit_path = repo.path().join("audit.jsonl");
    let mut record = common::experiment_record(
        "exp-log",
        "task-x",
        common::fake_agent_config("claude-code"),
    );
    record.audit_log_path = audit_path.clone();
    store.save_experiment(&record).expect("save_experiment");

    let events = [
        AuditEvent::ProcessSpawn {
            command: "echo".to_string(),
            args: vec!["hi".to_string()],
            cwd: repo.path().to_path_buf(),
            at: chrono::Utc::now(),
        },
        AuditEvent::ProcessExit {
            exit_code: Some(0),
            timed_out: false,
            at: chrono::Utc::now(),
        },
    ];
    let mut content = String::new();
    for event in &events {
        content.push_str(&serde_json::to_string(event).expect("event serializes"));
        content.push('\n');
    }
    std::fs::write(&audit_path, content).expect("write audit.jsonl");

    let log = agentforge_cmd()
        .args(["report", "log", "--repo", &repo_str, "exp-log"])
        .output()
        .expect("run agentforge report log");
    assert!(
        log.status.success(),
        "report log failed: stdout={} stderr={}",
        String::from_utf8_lossy(&log.stdout),
        String::from_utf8_lossy(&log.stderr)
    );
    let stdout = String::from_utf8_lossy(&log.stdout);
    let spawn_pos = stdout.find("spawn").expect("spawn event printed");
    let exit_pos = stdout.find("exit").expect("exit event printed");
    assert!(
        spawn_pos < exit_pos,
        "events must print in the order they were recorded: {stdout}"
    );
}

#[test]
fn e2e_log_reports_exit_2_for_an_unknown_experiment() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let log = agentforge_cmd()
        .args(["report", "log", "--repo", &repo_str, "does-not-exist"])
        .output()
        .expect("run agentforge report log");
    assert_eq!(log.status.code(), Some(2));
}
