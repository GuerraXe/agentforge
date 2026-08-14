//! The CLI surface for workspace management — argument parsing (no process spawn) plus true
//! end-to-end invocations of the compiled `agentforge` binary via `CARGO_BIN_EXE_agentforge`
//! (set automatically by Cargo for integration tests — no extra dependency needed).

mod common;

use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand, WorkspaceAction};
use clap::Parser;

// ---- argument parsing ----------------------------------------------------------------------

#[test]
fn parses_workspace_create_with_id_base_and_repo() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "workspace",
        "create",
        "--repo",
        "/some/repo",
        "--id",
        "my-task",
        "--base",
        "main",
    ])
    .expect("should parse");

    match cli.command {
        CliCommand::Workspace {
            action: WorkspaceAction::Create(args),
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("/some/repo"));
            assert_eq!(args.id, "my-task");
            assert_eq!(args.base, "main");
        }
        other => panic!("expected Workspace(Create), got {other:?}"),
    }
}

#[test]
fn parses_workspace_exec_trailing_command_array_verbatim() {
    // Everything after `--` must be captured as a plain argument array, unmodified — the
    // structural guarantee behind "avoid shell-string command construction."
    let cli = Cli::try_parse_from([
        "agentforge",
        "workspace",
        "exec",
        "--repo",
        ".",
        "my-task",
        "--",
        "cargo",
        "test",
        "--",
        "--nocapture",
    ])
    .expect("should parse");

    match cli.command {
        CliCommand::Workspace {
            action: WorkspaceAction::Exec(args),
        } => {
            assert_eq!(args.id, "my-task");
            assert_eq!(
                args.command,
                vec!["cargo", "test", "--", "--nocapture"],
                "trailing tokens after the first `--` must be preserved exactly, including a \
                 second `--`"
            );
        }
        other => panic!("expected Workspace(Exec), got {other:?}"),
    }
}

#[test]
fn parses_workspace_exec_command_policy_flags() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "workspace",
        "exec",
        "--repo",
        ".",
        "--allow-program",
        "cargo,cmd",
        "--deny-program",
        "rm",
        "--allowed-root",
        "/some/root",
        "my-task",
        "--",
        "cargo",
        "test",
    ])
    .expect("should parse");

    match cli.command {
        CliCommand::Workspace {
            action: WorkspaceAction::Exec(args),
        } => {
            assert_eq!(args.allowed_programs, vec!["cargo", "cmd"]);
            assert_eq!(args.denied_programs, vec!["rm"]);
            assert_eq!(
                args.allowed_roots,
                vec![std::path::PathBuf::from("/some/root")]
            );
        }
        other => panic!("expected Workspace(Exec), got {other:?}"),
    }
}

#[test]
fn parses_workspace_exec_with_no_command_policy_flags_as_unrestricted() {
    let cli = Cli::try_parse_from([
        "agentforge",
        "workspace",
        "exec",
        "--repo",
        ".",
        "my-task",
        "--",
        "cargo",
        "test",
    ])
    .expect("should parse");

    match cli.command {
        CliCommand::Workspace {
            action: WorkspaceAction::Exec(args),
        } => {
            assert!(args.allowed_programs.is_empty());
            assert!(args.denied_programs.is_empty());
            assert!(args.allowed_roots.is_empty());
        }
        other => panic!("expected Workspace(Exec), got {other:?}"),
    }
}

#[test]
fn parses_workspace_remove_force_flag() {
    let cli = Cli::try_parse_from(["agentforge", "workspace", "remove", "my-task", "--force"])
        .expect("should parse");

    match cli.command {
        CliCommand::Workspace {
            action: WorkspaceAction::Remove(args),
        } => {
            assert_eq!(args.id, "my-task");
            assert!(args.force);
        }
        other => panic!("expected Workspace(Remove), got {other:?}"),
    }
}

#[test]
fn parses_workspace_list_default_repo_is_current_dir() {
    let cli = Cli::try_parse_from(["agentforge", "workspace", "list"]).expect("should parse");

    match cli.command {
        CliCommand::Workspace {
            action: WorkspaceAction::List(args),
        } => {
            assert_eq!(args.repo, std::path::PathBuf::from("."));
        }
        other => panic!("expected Workspace(List), got {other:?}"),
    }
}

#[test]
fn parses_workspace_clean_force_flag() {
    let cli =
        Cli::try_parse_from(["agentforge", "workspace", "clean", "--force"]).expect("should parse");

    match cli.command {
        CliCommand::Workspace {
            action: WorkspaceAction::Clean(args),
        } => {
            assert!(args.force);
        }
        other => panic!("expected Workspace(Clean), got {other:?}"),
    }
}

// ---- end-to-end (real binary) --------------------------------------------------------------

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

fn cross_platform_exit_command(code: i32) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".to_string(), "/C".to_string(), format!("exit {code}")]
    } else {
        vec!["sh".to_string(), "-c".to_string(), format!("exit {code}")]
    }
}

#[test]
fn e2e_workspace_lifecycle_roundtrip() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let create = agentforge_cmd()
        .args([
            "workspace",
            "create",
            "--repo",
            &repo_str,
            "--id",
            "ws1",
            "--base",
            "HEAD",
        ])
        .output()
        .expect("run agentforge workspace create");
    assert!(
        create.status.success(),
        "create failed: stdout={} stderr={}",
        String::from_utf8_lossy(&create.stdout),
        String::from_utf8_lossy(&create.stderr)
    );

    let list = agentforge_cmd()
        .args(["workspace", "list", "--repo", &repo_str])
        .output()
        .expect("run agentforge workspace list");
    assert!(list.status.success());
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("ws1"),
        "list output must mention the created workspace id"
    );

    let show = agentforge_cmd()
        .args(["workspace", "show", "--repo", &repo_str, "ws1"])
        .output()
        .expect("run agentforge workspace show");
    assert!(show.status.success());

    let remove = agentforge_cmd()
        .args(["workspace", "remove", "--repo", &repo_str, "ws1"])
        .output()
        .expect("run agentforge workspace remove");
    assert!(remove.status.success());

    let list_after = agentforge_cmd()
        .args(["workspace", "list", "--repo", &repo_str])
        .output()
        .expect("run agentforge workspace list again");
    assert!(list_after.status.success());
    assert!(
        !String::from_utf8_lossy(&list_after.stdout).contains("ws1"),
        "list output must not mention a removed workspace"
    );
}

#[test]
fn e2e_workspace_exec_exit_code_passthrough() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    agentforge_cmd()
        .args([
            "workspace",
            "create",
            "--repo",
            &repo_str,
            "--id",
            "ws1",
            "--base",
            "HEAD",
        ])
        .output()
        .expect("create");

    let mut args = vec![
        "workspace".to_string(),
        "exec".to_string(),
        "--repo".to_string(),
        repo_str,
        "ws1".to_string(),
        "--".to_string(),
    ];
    args.extend(cross_platform_exit_command(42));

    let exec = agentforge_cmd()
        .args(&args)
        .output()
        .expect("run agentforge workspace exec");

    assert_eq!(
        exec.status.code(),
        Some(42),
        "the CLI's own exit code must mirror the child command's exit code — stderr={}",
        String::from_utf8_lossy(&exec.stderr)
    );
}

#[test]
fn e2e_workspace_exec_fails_closed_when_program_is_denied_by_policy() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    agentforge_cmd()
        .args([
            "workspace",
            "create",
            "--repo",
            &repo_str,
            "--id",
            "ws1",
            "--base",
            "HEAD",
        ])
        .output()
        .expect("create");

    let command = cross_platform_exit_command(0);
    let mut args = vec![
        "workspace".to_string(),
        "exec".to_string(),
        "--repo".to_string(),
        repo_str,
        "--deny-program".to_string(),
        command[0].clone(),
        "ws1".to_string(),
        "--".to_string(),
    ];
    args.extend(command);

    let exec = agentforge_cmd()
        .args(&args)
        .output()
        .expect("run agentforge workspace exec");

    assert!(
        !exec.status.success(),
        "a program denied by policy must not be spawned, so the command must not succeed"
    );
    assert_eq!(
        exec.status.code(),
        Some(2),
        "a policy denial is a usage/validation-shaped failure (exit 2), not an internal error \
         (exit 1) or a timeout (124) — stderr={}",
        String::from_utf8_lossy(&exec.stderr)
    );
}

#[test]
fn e2e_workspace_create_rejects_invalid_id_with_nonzero_exit() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let create = agentforge_cmd()
        .args([
            "workspace",
            "create",
            "--repo",
            &repo_str,
            "--id",
            "../escape",
            "--base",
            "HEAD",
        ])
        .output()
        .expect("run agentforge workspace create");

    assert!(!create.status.success(), "an invalid id must not exit 0");
    assert!(
        !repo.path().parent().unwrap().join("escape").exists(),
        "a rejected id must never cause anything to be created outside the repo"
    );
}

#[test]
fn e2e_unknown_subcommand_reports_a_clean_usage_error_not_a_panic() {
    // Every variant in `Command` now has real dispatch — no command is left in the old
    // catch-all "not implemented yet" bucket. What's still worth guarding here is that a
    // genuinely unknown top-level subcommand (e.g. a stale `score`/`show`/`eval`/`mutate` from
    // before this pass's `report`/`experiment`/`verify` regrouping) fails as a clean clap usage
    // error, never a Rust panic/backtrace.
    let dir = tempfile::tempdir().expect("temp dir");
    let output = agentforge_cmd()
        .args(["score", "some-experiment-id"])
        .current_dir(dir.path())
        .output()
        .expect("run agentforge score");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "an unknown subcommand must fail cleanly, not panic: {stderr}"
    );
}
