//! Adapter contract tests — SPEC.md §9, docs/ARCHITECTURE.md §7. Written before
//! `ClaudeCodeAdapter::command_for`/`adapter::resolve` had real bodies, and still the source of
//! truth for what must hold of any `AgentAdapter`: `command_for` only returns a value (never
//! blocks, never spawns, never touches an audit sink); the prompt is passed as one discrete
//! argv element, never shell-interpolated; executable/model/options are all configurable rather
//! than hard-coded; and a missing executable fails cleanly through the existing `Executor`
//! rather than panicking. `FakeAdapter` is the deterministic, no-paid-API adapter these same
//! contract shapes are checked against too.

mod common;

use agentforge::adapter::claude_code::{ClaudeCodeAdapter, ClaudeCodeConfig};
use agentforge::adapter::fake::FakeAdapter;
use agentforge::adapter::{self, AgentAdapter};
use agentforge::domain::{
    AuditEvent, CommandPolicy, EnforcementLevel, ExecutionBudget, ProcessSpec,
};
use agentforge::exec::{Error as ExecError, Executor, SystemExecutor};

fn adapter_with_executable(executable: &str) -> ClaudeCodeAdapter {
    ClaudeCodeAdapter::new(ClaudeCodeConfig {
        executable: executable.to_string(),
        permission_mode: None,
        extra_default_args: vec![],
    })
}

// ---- command_for: shape, model mapping, no shell interpolation ----------------------------

#[test]
fn command_for_builds_a_non_interactive_invocation_with_the_prompt_as_a_discrete_arg() {
    let adapter = adapter_with_executable("claude");
    let spec = adapter.command_for("fix the bug", None, &[]);
    assert_eq!(spec.program, "claude");
    assert!(spec.args.contains(&"-p".to_string()));
    assert!(
        spec.args.iter().any(|a| a == "fix the bug"),
        "the prompt must appear as its own args element: {:?}",
        spec.args
    );
}

#[test]
fn command_for_maps_model_to_a_model_flag_only_when_given() {
    let adapter = adapter_with_executable("claude");
    let with_model = adapter.command_for("prompt", Some("claude-opus-5"), &[]);
    let idx = with_model
        .args
        .iter()
        .position(|a| a == "--model")
        .expect("--model flag must be present when a model is given");
    assert_eq!(with_model.args[idx + 1], "claude-opus-5");

    let without_model = adapter.command_for("prompt", None, &[]);
    assert!(
        !without_model.args.contains(&"--model".to_string()),
        "no --model flag must be added when model is None: {:?}",
        without_model.args
    );
}

#[test]
fn command_for_never_shell_interpolates_the_prompt() {
    // A prompt containing shell metacharacters must survive as one literal argv element — never
    // concatenated into a string handed to `sh -c`/`cmd /C`.
    let adapter = adapter_with_executable("claude");
    let dangerous = "hello; rm -rf / && echo $(whoami) `id`";
    let spec = adapter.command_for(dangerous, None, &[]);

    assert!(
        spec.args.iter().any(|a| a == dangerous),
        "the dangerous prompt must appear byte-for-byte as a single args element: {:?}",
        spec.args
    );
    assert_ne!(spec.program, "sh", "the adapter must never invoke a shell");
    assert_ne!(spec.program, "cmd", "the adapter must never invoke a shell");
    assert!(
        !spec.args.iter().any(|a| a == "-c" || a == "/C"),
        "no shell -c/-C flag may appear — the prompt is a literal argv element, not a shell \
         command line: {:?}",
        spec.args
    );
}

// ---- configurability: executable/model/options, never hard-coded --------------------------

#[test]
fn executable_and_options_are_configurable_not_hard_coded() {
    let custom = ClaudeCodeAdapter::new(ClaudeCodeConfig {
        executable: "my-claude-fork".to_string(),
        permission_mode: Some("bypassPermissions".to_string()),
        extra_default_args: vec!["--verbose".to_string()],
    });
    let spec = custom.command_for("prompt", Some("some-model"), &["--extra".to_string()]);

    assert_eq!(spec.program, "my-claude-fork");
    assert!(spec.args.contains(&"--permission-mode".to_string()));
    assert!(spec.args.contains(&"bypassPermissions".to_string()));
    assert!(spec.args.contains(&"--verbose".to_string()));
    assert!(spec.args.contains(&"--extra".to_string()));
}

#[test]
fn per_call_extra_args_are_appended_after_configured_default_args() {
    let adapter = ClaudeCodeAdapter::new(ClaudeCodeConfig {
        executable: "claude".to_string(),
        permission_mode: None,
        extra_default_args: vec!["--configured-default".to_string()],
    });
    let spec = adapter.command_for("prompt", None, &["--per-call".to_string()]);
    let default_idx = spec
        .args
        .iter()
        .position(|a| a == "--configured-default")
        .expect("configured default arg must be present");
    let call_idx = spec
        .args
        .iter()
        .position(|a| a == "--per-call")
        .expect("per-call extra arg must be present");
    assert!(
        default_idx < call_idx,
        "configured defaults must precede per-call extras"
    );
}

#[test]
fn command_for_is_deterministic_for_identical_inputs() {
    let adapter = adapter_with_executable("claude");
    let a = adapter.command_for("same prompt", Some("m"), &["--x".to_string()]);
    let b = adapter.command_for("same prompt", Some("m"), &["--x".to_string()]);
    assert_eq!(a.program, b.program);
    assert_eq!(a.args, b.args);
}

#[test]
fn default_executable_is_overridable_via_environment_without_recompiling() {
    std::env::set_var(
        "AGENTFORGE_CLAUDE_EXECUTABLE",
        "claude-from-env-override-test",
    );
    let adapter = ClaudeCodeAdapter::default();
    assert_eq!(
        adapter.command_for("p", None, &[]).program,
        "claude-from-env-override-test"
    );
    std::env::remove_var("AGENTFORGE_CLAUDE_EXECUTABLE");
}

#[test]
fn claude_code_capabilities_are_requested_only_not_enforced() {
    let adapter = adapter_with_executable("claude");
    let caps = adapter.capabilities();
    assert_eq!(caps.can_confine_filesystem, EnforcementLevel::RequestedOnly);
    assert_eq!(caps.can_restrict_network, EnforcementLevel::RequestedOnly);
}

// ---- resolve() --------------------------------------------------------------------------

#[test]
fn resolve_returns_the_claude_code_adapter_by_name() {
    let found = adapter::resolve("claude-code").expect("claude-code must resolve");
    assert_eq!(found.name(), "claude-code");
}

#[test]
fn resolve_rejects_an_unknown_adapter_name() {
    let result = adapter::resolve("not-a-real-adapter");
    assert!(matches!(result, Err(adapter::Error::UnknownAdapter(_))));
}

// ---- FakeAdapter: deterministic, no paid API needed ----------------------------------------

#[test]
fn fake_adapter_ignores_prompt_model_and_extra_args_and_returns_the_scripted_command_verbatim() {
    let scripted = ProcessSpec {
        program: "true".to_string(),
        args: vec!["fixed".to_string()],
        extra_env: vec![],
    };
    let adapter = FakeAdapter {
        scripted_command: scripted.clone(),
    };
    let spec = adapter.command_for("anything", Some("any-model"), &["ignored".to_string()]);
    assert_eq!(spec.program, scripted.program);
    assert_eq!(spec.args, scripted.args);
}

#[test]
fn fake_adapter_capabilities_are_fully_enforced() {
    let adapter = FakeAdapter {
        scripted_command: ProcessSpec {
            program: "true".into(),
            args: vec![],
            extra_env: vec![],
        },
    };
    let caps = adapter.capabilities();
    assert_eq!(caps.can_confine_filesystem, EnforcementLevel::Enforced);
    assert_eq!(caps.can_restrict_network, EnforcementLevel::Enforced);
}

// ---- missing executable / composition through the real Executor ---------------------------

#[test]
fn a_missing_claude_executable_fails_cleanly_through_the_executor_not_a_panic() {
    let adapter = adapter_with_executable("definitely-not-a-real-claude-binary-xyz");
    let spec = adapter.command_for("prompt", None, &[]);
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();

    let result = executor.spawn(
        &spec,
        dir.path(),
        &[],
        &ExecutionBudget {
            timeout_secs: 5,
            max_output_bytes: 10_000,
        },
        &CommandPolicy::unrestricted(),
        &agentforge::audit::NullAuditSink,
    );

    assert!(
        matches!(result, Err(ExecError::SpawnFailed(_))),
        "a missing executable must surface as a clean SpawnFailed error, not a panic: {result:?}"
    );
}

#[test]
fn the_adapter_built_processspec_composes_with_the_executor_capturing_exit_status_output_and_audit_timestamps(
) {
    // Point the adapter at a real, always-present binary (`cargo` — never the real `claude` CLI,
    // so this needs no paid API) to prove the composed pipeline captures everything required:
    // exit status, bounded stdout/stderr, and audit events whose timestamps bound the run's
    // duration. The stand-in binary doesn't need to understand `claude`'s own flags for this.
    let adapter = adapter_with_executable("cargo");
    let spec = adapter.command_for("prompt", None, &[]);
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let sink = common::RecordingAuditSink::new();

    let outcome = executor
        .spawn(
            &spec,
            dir.path(),
            &[],
            &ExecutionBudget {
                timeout_secs: 10,
                max_output_bytes: 1_000_000,
            },
            &CommandPolicy::unrestricted(),
            &sink,
        )
        .expect("spawning a real, present binary must succeed even if it rejects the args");

    assert!(outcome.exit_code.is_some(), "exit status must be captured");
    assert!(outcome.stdout_path.exists());
    assert!(outcome.stderr_path.exists());

    let events = sink.events();
    let spawn_at = events.iter().find_map(|e| match e {
        AuditEvent::ProcessSpawn { at, .. } => Some(*at),
        _ => None,
    });
    let exit_at = events.iter().find_map(|e| match e {
        AuditEvent::ProcessExit { at, .. } => Some(*at),
        _ => None,
    });
    let (spawn_at, exit_at) = (
        spawn_at.expect("a ProcessSpawn audit event must be recorded"),
        exit_at.expect("a ProcessExit audit event must be recorded"),
    );
    assert!(
        exit_at >= spawn_at,
        "ProcessSpawn/ProcessExit timestamps must bound the run's duration"
    );
}
