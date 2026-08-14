//! CLI surface for `policy add`/`list`/`show`/`validate` — argument parsing plus true end-to-end
//! invocations of the compiled `agentforge` binary. SPEC.md §6, §17.

mod common;

use std::process::Command;

use agentforge::cli::{Cli, Command as CliCommand, PolicyAction};
use clap::Parser;

#[test]
fn parses_policy_add_list_show_validate() {
    let add = Cli::try_parse_from(["agentforge", "policy", "add", "policy.toml", "--force"])
        .expect("should parse");
    match add.command {
        CliCommand::Policy {
            action: PolicyAction::Add(args),
        } => {
            assert_eq!(args.file, std::path::PathBuf::from("policy.toml"));
            assert!(args.force);
        }
        other => panic!("expected Policy(Add), got {other:?}"),
    }

    let list = Cli::try_parse_from(["agentforge", "policy", "list"]).expect("should parse");
    assert!(matches!(
        list.command,
        CliCommand::Policy {
            action: PolicyAction::List(_)
        }
    ));

    let show =
        Cli::try_parse_from(["agentforge", "policy", "show", "default"]).expect("should parse");
    match show.command {
        CliCommand::Policy {
            action: PolicyAction::Show(args),
        } => assert_eq!(args.id, "default"),
        other => panic!("expected Policy(Show), got {other:?}"),
    }

    let validate =
        Cli::try_parse_from(["agentforge", "policy", "validate", "default"]).expect("should parse");
    match validate.command {
        CliCommand::Policy {
            action: PolicyAction::Validate(args),
        } => assert_eq!(args.id, "default"),
        other => panic!("expected Policy(Validate), got {other:?}"),
    }
}

fn agentforge_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_agentforge"))
}

fn write_policy_toml(
    repo: &std::path::Path,
    name: &str,
    max_wall_time_secs: u64,
) -> std::path::PathBuf {
    let toml = format!(
        r#"
name = "{name}"
extra_readonly_paths = []
deny_network = true
env_passthrough = []
max_wall_time_secs = {max_wall_time_secs}
max_output_bytes = 1000000
allowed_programs = []
denied_programs = []
allowed_roots = []
"#
    );
    let path = repo.join(format!("{name}.toml"));
    std::fs::write(&path, toml).expect("write policy toml");
    path
}

#[test]
fn e2e_policy_add_list_show_validate_round_trip() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let policy_path = write_policy_toml(repo.path(), "ci-policy", 3600);

    let add = agentforge_cmd()
        .args([
            "policy",
            "add",
            "--repo",
            &repo_str,
            &policy_path.to_string_lossy(),
        ])
        .output()
        .expect("run agentforge policy add");
    assert!(
        add.status.success(),
        "policy add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(String::from_utf8_lossy(&add.stdout).contains("ci-policy"));

    let list = agentforge_cmd()
        .args(["policy", "list", "--repo", &repo_str])
        .output()
        .expect("run agentforge policy list");
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("ci-policy"));

    let show = agentforge_cmd()
        .args(["policy", "show", "--repo", &repo_str, "ci-policy"])
        .output()
        .expect("run agentforge policy show");
    assert!(show.status.success());
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(show_out.contains("Enforced"));
    assert!(show_out.contains("max_wall_time_secs"));

    let validate = agentforge_cmd()
        .args(["policy", "validate", "--repo", &repo_str, "ci-policy"])
        .output()
        .expect("run agentforge policy validate");
    assert!(validate.status.success());
    assert!(String::from_utf8_lossy(&validate.stdout).contains("valid"));
}

/// SPEC.md §17/§18 (row 26, M2 resolution): every `policy show` field must be tagged exactly
/// per `PermissionPolicy::enforcement_report` (src/domain/policy.rs) — not just "the word
/// `Enforced` appears somewhere in the output" (the weaker check above). One line per field, in
/// `enforcement_report`'s own order, each checked against its exact tag.
#[test]
fn e2e_policy_show_tags_every_field_exactly_per_enforcement_report() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let policy_path = write_policy_toml(repo.path(), "golden-policy", 3600);
    agentforge_cmd()
        .args([
            "policy",
            "add",
            "--repo",
            &repo_str,
            &policy_path.to_string_lossy(),
        ])
        .output()
        .expect("run agentforge policy add");

    let show = agentforge_cmd()
        .args(["policy", "show", "--repo", &repo_str, "golden-policy"])
        .output()
        .expect("run agentforge policy show");
    assert!(show.status.success());
    let show_out = String::from_utf8_lossy(&show.stdout).into_owned();

    let expected: &[(&str, &str)] = &[
        ("env_passthrough", "Enforced"),
        ("max_wall_time_secs", "Enforced"),
        ("max_output_bytes", "Enforced"),
        ("allowed_programs", "Enforced"),
        ("denied_programs", "Enforced"),
        ("allowed_roots", "Enforced"),
        ("deny_network", "RequestedOnly"),
        ("extra_readonly_paths", "RequestedOnly"),
        ("max_memory_bytes", "Unsupported"),
    ];
    for (field, tag) in expected {
        let line = show_out
            .lines()
            .find(|l| l.trim_start().starts_with(field))
            .unwrap_or_else(|| panic!("no output line for field {field:?}: {show_out}"));
        assert!(
            line.split_whitespace().nth(1) == Some(*tag),
            "field {field:?} must be tagged exactly {tag:?}, got line {line:?}"
        );
    }
}

#[test]
fn e2e_policy_add_rejects_a_zero_wall_time_budget() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();
    let policy_path = write_policy_toml(repo.path(), "broken-policy", 0);

    let add = agentforge_cmd()
        .args([
            "policy",
            "add",
            "--repo",
            &repo_str,
            &policy_path.to_string_lossy(),
        ])
        .output()
        .expect("run agentforge policy add");
    assert_eq!(
        add.status.code(),
        Some(2),
        "an invalid policy must be rejected at add time: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );
}

#[test]
fn e2e_policy_show_reports_exit_2_for_an_unknown_name() {
    let repo = common::init_temp_repo();
    let repo_str = repo.path().to_string_lossy().to_string();

    let show = agentforge_cmd()
        .args(["policy", "show", "--repo", &repo_str, "does-not-exist"])
        .output()
        .expect("run agentforge policy show");
    assert_eq!(show.status.code(), Some(2));
}
