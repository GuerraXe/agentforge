//! Safe command construction/execution boundaries — SPEC.md §8, §16, docs/ARCHITECTURE.md §5.
//!
//! `SystemExecutor` is the only thing in the codebase allowed to call `std::process::Command`.
//! These tests check the guarantees SPEC.md §16 marks **Enforced**: forced cwd, exact env
//! allowlisting, timeout enforcement, output truncation, and — new in this pass — the
//! permission-policy layer's command-program allow/denylist and filesystem-root confinement,
//! plus the auditability of every one of those decisions.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use agentforge::audit::NullAuditSink;
use agentforge::domain::{AuditEvent, CommandPolicy, ExecutionBudget, ProcessSpec};
use agentforge::exec::{Error as ExecError, Executor, SystemExecutor};

fn cwd_echo_command() -> ProcessSpec {
    if cfg!(windows) {
        ProcessSpec {
            program: "cmd".into(),
            args: vec!["/C".into(), "cd".into()],
            extra_env: vec![],
        }
    } else {
        ProcessSpec {
            program: "sh".into(),
            args: vec!["-c".into(), "pwd".into()],
            extra_env: vec![],
        }
    }
}

fn env_dump_command() -> ProcessSpec {
    if cfg!(windows) {
        ProcessSpec {
            program: "cmd".into(),
            args: vec!["/C".into(), "set".into()],
            extra_env: vec![],
        }
    } else {
        ProcessSpec {
            program: "sh".into(),
            args: vec!["-c".into(), "env".into()],
            extra_env: vec![],
        }
    }
}

fn sleep_command(seconds: u32) -> ProcessSpec {
    if cfg!(windows) {
        ProcessSpec {
            program: "powershell".into(),
            args: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                format!("Start-Sleep -Seconds {seconds}"),
            ],
            extra_env: vec![],
        }
    } else {
        ProcessSpec {
            program: "sleep".into(),
            args: vec![seconds.to_string()],
            extra_env: vec![],
        }
    }
}

fn big_output_command(bytes: usize) -> ProcessSpec {
    if cfg!(windows) {
        ProcessSpec {
            program: "powershell".into(),
            args: vec![
                "-NoProfile".into(),
                "-Command".into(),
                format!("Write-Output ('X' * {bytes})"),
            ],
            extra_env: vec![],
        }
    } else {
        ProcessSpec {
            program: "sh".into(),
            args: vec![
                "-c".into(),
                format!("head -c {bytes} /dev/zero | tr '\\0' 'X'"),
            ],
            extra_env: vec![],
        }
    }
}

fn generous_budget(timeout_secs: u64) -> ExecutionBudget {
    ExecutionBudget {
        timeout_secs,
        max_output_bytes: 10_000_000,
    }
}

#[test]
fn spawned_process_cwd_matches_the_supplied_path_exactly() {
    // SPEC.md §20 (U2): cwd is set by the Executor, always — never adapter-suppliable.
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let outcome = executor
        .spawn(
            &cwd_echo_command(),
            dir.path(),
            &[],
            &generous_budget(5),
            &CommandPolicy::unrestricted(),
            &NullAuditSink,
        )
        .expect("spawn should succeed");
    let output = std::fs::read_to_string(&outcome.stdout_path).expect("read captured stdout");
    let reported = Path::new(output.trim());
    assert_eq!(
        reported.canonicalize().ok(),
        dir.path().canonicalize().ok(),
        "the Executor must force cwd to exactly the supplied worktree path"
    );
}

#[test]
fn env_passthrough_allowlist_is_exact() {
    // SPEC.md §16: the spawned process's environment is built from exactly the allowlisted
    // host vars, never the full host environment.
    let dir = tempfile::tempdir().expect("temp dir");
    std::env::set_var("AGENTFORGE_TEST_ALLOWED_VAR", "1");
    std::env::set_var("AGENTFORGE_TEST_BLOCKED_VAR", "1");

    let executor = SystemExecutor::new();
    let outcome = executor
        .spawn(
            &env_dump_command(),
            dir.path(),
            &["AGENTFORGE_TEST_ALLOWED_VAR".to_string()],
            &generous_budget(5),
            &CommandPolicy::unrestricted(),
            &NullAuditSink,
        )
        .expect("spawn should succeed");

    let output = std::fs::read_to_string(&outcome.stdout_path).expect("read captured stdout");
    assert!(
        output.contains("AGENTFORGE_TEST_ALLOWED_VAR"),
        "an allowlisted host var must be present in the child's environment"
    );
    assert!(
        !output.contains("AGENTFORGE_TEST_BLOCKED_VAR"),
        "a non-allowlisted host var must never reach the child's environment"
    );
}

#[test]
fn process_exceeding_timeout_is_reported_as_timed_out() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let budget = ExecutionBudget {
        timeout_secs: 2,
        max_output_bytes: 1_000_000,
    };

    let outcome = executor
        .spawn(
            &sleep_command(30),
            dir.path(),
            &[],
            &budget,
            &CommandPolicy::unrestricted(),
            &NullAuditSink,
        )
        .expect("spawn should succeed even though the child is killed");

    assert!(
        outcome.timed_out,
        "a process outliving its budget must report timed_out = true"
    );
}

#[test]
fn timeout_kill_happens_within_a_generous_bounded_margin() {
    // SPEC.md §18: a fixed, generous margin (budget 2s, bound 5s) — chosen specifically to
    // avoid flakiness under loaded CI, per SPEC.md §20 (T3). This asserts "killed within a
    // bound," not "killed instantly."
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let budget = ExecutionBudget {
        timeout_secs: 2,
        max_output_bytes: 1_000_000,
    };

    let start = Instant::now();
    let _ = executor.spawn(
        &sleep_command(30),
        dir.path(),
        &[],
        &budget,
        &CommandPolicy::unrestricted(),
        &NullAuditSink,
    );
    let elapsed = start.elapsed();

    assert!(
        elapsed <= Duration::from_secs(5),
        "a 2s timeout budget must be enforced within a 5s bound; took {elapsed:?}"
    );
}

/// Adversarial-review regression (docs/ADVERSARIAL_REVIEW.md): SPEC.md §8 claims the timeout
/// kill reaches "the child, and its process group / job object, where the platform provides
/// one" — before the `exec::tree` module existed, `wait_with_timeout` only ever called
/// `Child::kill()`, which never touched a grandchild the direct child spawned and detached, so
/// that claim wasn't actually true on any platform. This proves the Windows Job Object path
/// really does reach a detached grandchild, not just the direct child.
#[cfg(windows)]
#[test]
fn timeout_kill_also_terminates_a_detached_grandchild_process_on_windows() {
    let dir = tempfile::tempdir().expect("temp dir");
    let marker = dir.path().join("grandchild_pid.txt");
    let executor = SystemExecutor::new();
    // Unlike this file's other `timeout_secs: 2` tests, the direct child here must itself spawn
    // and wait on a *nested* PowerShell process before it can write the marker file — on a
    // loaded/cold CI runner, PowerShell startup latency alone (let alone a second, nested
    // startup) can exceed 2s, which previously killed the direct child before it ever wrote the
    // marker (observed in CI: "the direct child must have written the grandchild's pid before
    // being killed"). A generous 15s budget keeps the same assertion while giving real headroom.
    let budget = ExecutionBudget {
        timeout_secs: 15,
        max_output_bytes: 1_000_000,
    };

    // The direct child starts a detached grandchild (itself a long sleep), records its pid to a
    // marker file, then sleeps long enough to hit `budget.timeout_secs` itself.
    let script = format!(
        "$p = Start-Process -FilePath 'powershell' -ArgumentList \
         '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 120' \
         -WindowStyle Hidden -PassThru; \
         Set-Content -Path '{}' -Value $p.Id; Start-Sleep -Seconds 120",
        marker.display()
    );
    let spec = ProcessSpec {
        program: "powershell".into(),
        args: vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            script,
        ],
        extra_env: vec![],
    };

    let outcome = executor
        .spawn(
            &spec,
            dir.path(),
            &[],
            &budget,
            &CommandPolicy::unrestricted(),
            &NullAuditSink,
        )
        .expect("spawn should succeed even though the child is killed");
    assert!(
        outcome.timed_out,
        "the direct child must be reported as timed out"
    );

    let mut grandchild_pid = None;
    for _ in 0..150 {
        if let Ok(raw) = std::fs::read_to_string(&marker) {
            if let Ok(pid) = raw.trim().parse::<u32>() {
                grandchild_pid = Some(pid);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let grandchild_pid = grandchild_pid
        .expect("the direct child must have written the grandchild's pid before being killed");

    // Job Object teardown is asynchronous relative to `TerminateJobObject`/`CloseHandle`
    // returning, so poll rather than asserting the grandchild is gone instantly.
    let mut still_alive = true;
    for _ in 0..50 {
        let check = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "(Get-Process -Id {grandchild_pid} -ErrorAction SilentlyContinue) -eq $null"
                ),
            ])
            .output()
            .expect("probe grandchild liveness");
        if String::from_utf8_lossy(&check.stdout).trim() == "True" {
            still_alive = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !still_alive,
        "a detached grandchild (pid {grandchild_pid}) spawned by the timed-out process must \
         also be killed via the Job Object, not just the direct child"
    );
}

/// Unix counterpart of the Windows Job Object test above — proves the process-group kill
/// (`command.process_group(0)` + `kill(-pgid, SIGKILL)`) reaches a detached grandchild too.
#[cfg(unix)]
#[test]
fn timeout_kill_also_terminates_a_detached_grandchild_process_on_unix() {
    let dir = tempfile::tempdir().expect("temp dir");
    let marker = dir.path().join("grandchild_pid.txt");
    let executor = SystemExecutor::new();
    let budget = ExecutionBudget {
        timeout_secs: 2,
        max_output_bytes: 1_000_000,
    };

    // The direct child backgrounds a detached grandchild (a long sleep), records its pid, then
    // sleeps long enough itself to hit `budget.timeout_secs`.
    let script = format!("sleep 120 & echo $! > '{}' ; sleep 120", marker.display());
    let spec = ProcessSpec {
        program: "sh".into(),
        args: vec!["-c".into(), script],
        extra_env: vec![],
    };

    let outcome = executor
        .spawn(
            &spec,
            dir.path(),
            &[],
            &budget,
            &CommandPolicy::unrestricted(),
            &NullAuditSink,
        )
        .expect("spawn should succeed even though the child is killed");
    assert!(
        outcome.timed_out,
        "the direct child must be reported as timed out"
    );

    let mut grandchild_pid = None;
    for _ in 0..20 {
        if let Ok(raw) = std::fs::read_to_string(&marker) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                grandchild_pid = Some(pid);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let grandchild_pid = grandchild_pid
        .expect("the direct child must have written the grandchild's pid before being killed");

    let mut still_alive = true;
    for _ in 0..50 {
        let check = std::process::Command::new("kill")
            .args(["-0", &grandchild_pid.to_string()])
            .status()
            .expect("probe grandchild liveness");
        if !check.success() {
            still_alive = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !still_alive,
        "a detached grandchild (pid {grandchild_pid}) spawned by the timed-out process must \
         also be killed via the process group, not just the direct child"
    );
}

#[test]
fn captured_output_is_truncated_at_max_output_bytes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let budget = ExecutionBudget {
        timeout_secs: 30,
        max_output_bytes: 1_000,
    };

    let outcome = executor
        .spawn(
            &big_output_command(10_000),
            dir.path(),
            &[],
            &budget,
            &CommandPolicy::unrestricted(),
            &NullAuditSink,
        )
        .expect("spawn should succeed");

    let captured_len = std::fs::metadata(&outcome.stdout_path)
        .expect("stdout file must exist")
        .len();

    assert!(
        captured_len <= 1_000 + 128, // + generous allowance for a truncation marker
        "captured stdout ({captured_len} bytes) must be truncated near max_output_bytes (1000)"
    );
}

#[test]
fn a_well_behaved_short_command_is_not_reported_as_timed_out() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let outcome = executor
        .spawn(
            &cwd_echo_command(),
            dir.path(),
            &[],
            &generous_budget(30),
            &CommandPolicy::unrestricted(),
            &NullAuditSink,
        )
        .expect("spawn should succeed");
    assert!(
        !outcome.timed_out,
        "a command that exits well within budget must not be marked timed_out"
    );
    assert_eq!(outcome.exit_code, Some(0));
}

// ---- permission-policy layer: command program allow/denylist ------------------------------

#[test]
fn spawn_denies_a_program_on_the_denylist() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let spec = cwd_echo_command();
    let policy = CommandPolicy {
        denied_programs: vec![spec.program.clone()],
        ..CommandPolicy::unrestricted()
    };

    let result = executor.spawn(
        &spec,
        dir.path(),
        &[],
        &generous_budget(5),
        &policy,
        &NullAuditSink,
    );

    assert!(
        matches!(result, Err(ExecError::PolicyDenied(_))),
        "a program on policy.denied_programs must be refused, not spawned: {result:?}"
    );
}

#[test]
fn spawn_denies_a_program_absent_from_a_nonempty_allowlist() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let spec = cwd_echo_command();
    let policy = CommandPolicy {
        allowed_programs: vec!["definitely-not-this-program".to_string()],
        ..CommandPolicy::unrestricted()
    };

    let result = executor.spawn(
        &spec,
        dir.path(),
        &[],
        &generous_budget(5),
        &policy,
        &NullAuditSink,
    );

    assert!(
        matches!(result, Err(ExecError::PolicyDenied(_))),
        "a program absent from a non-empty allowlist must be refused: {result:?}"
    );
}

#[test]
fn spawn_allows_a_program_explicitly_in_the_allowlist() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let spec = cwd_echo_command();
    let policy = CommandPolicy {
        allowed_programs: vec![spec.program.clone()],
        ..CommandPolicy::unrestricted()
    };

    let result = executor.spawn(
        &spec,
        dir.path(),
        &[],
        &generous_budget(5),
        &policy,
        &NullAuditSink,
    );

    assert!(
        result.is_ok(),
        "a program present in the allowlist must be permitted: {result:?}"
    );
}

#[test]
fn a_denied_program_is_refused_even_if_also_allowlisted() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let spec = cwd_echo_command();
    let policy = CommandPolicy {
        allowed_programs: vec![spec.program.clone()],
        denied_programs: vec![spec.program.clone()],
        ..CommandPolicy::unrestricted()
    };

    let result = executor.spawn(
        &spec,
        dir.path(),
        &[],
        &generous_budget(5),
        &policy,
        &NullAuditSink,
    );

    assert!(
        matches!(result, Err(ExecError::PolicyDenied(_))),
        "denied_programs must win over allowed_programs for a program listed in both: {result:?}"
    );
}

// ---- permission-policy layer: filesystem-root confinement ----------------------------------

#[test]
fn spawn_denies_a_cwd_outside_every_allowed_root() {
    let dir = tempfile::tempdir().expect("temp dir");
    let other_root = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let policy = CommandPolicy {
        allowed_roots: vec![other_root.path().to_path_buf()],
        ..CommandPolicy::unrestricted()
    };

    let result = executor.spawn(
        &cwd_echo_command(),
        dir.path(),
        &[],
        &generous_budget(5),
        &policy,
        &NullAuditSink,
    );

    assert!(
        matches!(result, Err(ExecError::PolicyDenied(_))),
        "a cwd outside every configured allowed root must be refused: {result:?}"
    );
}

#[test]
fn spawn_allows_a_cwd_inside_a_configured_allowed_root() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let policy = CommandPolicy {
        allowed_roots: vec![dir.path().to_path_buf()],
        ..CommandPolicy::unrestricted()
    };

    let result = executor.spawn(
        &cwd_echo_command(),
        dir.path(),
        &[],
        &generous_budget(5),
        &policy,
        &NullAuditSink,
    );

    assert!(
        result.is_ok(),
        "a cwd inside a configured allowed root must be permitted: {result:?}"
    );
}

// ---- auditability: every policy decision must be recorded ---------------------------------

#[test]
fn denied_spawn_emits_a_permission_check_audit_event_and_no_process_events() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let spec = cwd_echo_command();
    let policy = CommandPolicy {
        denied_programs: vec![spec.program.clone()],
        ..CommandPolicy::unrestricted()
    };
    let sink = common::RecordingAuditSink::new();

    let result = executor.spawn(&spec, dir.path(), &[], &generous_budget(5), &policy, &sink);
    assert!(result.is_err(), "the denied program must not be spawned");

    let events = sink.events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AuditEvent::PermissionCheck { allowed: false, .. })),
        "a denied spawn must record a PermissionCheck audit event with allowed = false: \
         {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AuditEvent::ProcessSpawn { .. } | AuditEvent::ProcessExit { .. }
        )),
        "a process that was never spawned must not produce ProcessSpawn/ProcessExit events: \
         {events:?}"
    );
}

#[test]
fn successful_spawn_emits_an_allowed_permission_check_for_every_enforceable_dimension() {
    let dir = tempfile::tempdir().expect("temp dir");
    let executor = SystemExecutor::new();
    let sink = common::RecordingAuditSink::new();

    executor
        .spawn(
            &cwd_echo_command(),
            dir.path(),
            &[],
            &generous_budget(5),
            &CommandPolicy::unrestricted(),
            &sink,
        )
        .expect("spawn should succeed");

    let events = sink.events();
    let allowed_actions: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AuditEvent::PermissionCheck {
                action,
                allowed: true,
                ..
            } => Some(action.as_str()),
            _ => None,
        })
        .collect();

    for expected in ["spawn_program", "spawn_cwd", "env_passthrough"] {
        assert!(
            allowed_actions.contains(&expected),
            "expected an allowed PermissionCheck for {expected:?}, got {allowed_actions:?}"
        );
    }
}
