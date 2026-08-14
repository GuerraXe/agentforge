//! Isolated task worktrees and controlled repository execution — the CLI-facing
//! `workspace::WorkspaceManager` layered on the already-tested `WorktreeManager`/`Executor`/
//! `GitRepo` foundation.
//!
//! Safety properties under test: never remove paths outside AgentForge-owned workspace roots,
//! validate git refs and ids before touching anything, cleanup is idempotent, and workspace
//! lifecycle actions are recorded in a structured (JSONL) audit log.

mod common;

use agentforge::domain::{PermissionPolicy, ProcessSpec};
use agentforge::exec::Error as ExecError;
use agentforge::workspace::Error as WorkspaceError;

fn exit_with_code(code: i32) -> ProcessSpec {
    if cfg!(windows) {
        ProcessSpec {
            program: "cmd".into(),
            args: vec!["/C".into(), format!("exit {code}")],
            extra_env: vec![],
        }
    } else {
        ProcessSpec {
            program: "sh".into(),
            args: vec!["-c".into(), format!("exit {code}")],
            extra_env: vec![],
        }
    }
}

fn unrestricted_policy() -> PermissionPolicy {
    common::valid_permission_policy("test")
}

#[test]
fn create_produces_a_workspace_directory_at_the_expected_path() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());

    let info = mgr.create("ws-1", &head).expect("create");

    assert!(info.path.is_dir(), "workspace directory must exist on disk");
    assert_eq!(info.id, "ws-1");
    assert!(
        !info.locked,
        "a freshly created workspace must not be locked"
    );
}

#[test]
fn create_pins_head_to_the_resolved_base_commit() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());

    let info = mgr
        .create("ws-1", "HEAD")
        .expect("create from a floating ref");

    assert_eq!(
        info.head, head,
        "the workspace must be pinned to the resolved commit, not the floating ref string"
    );
}

#[test]
fn create_rejects_an_empty_id() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());

    let result = mgr.create("", &head);
    assert!(matches!(result, Err(WorkspaceError::EmptyId)));
}

#[test]
fn create_rejects_a_path_traversal_id() {
    // The core safety property: an id must never be usable to escape the workspace root.
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());

    for malicious in ["../../evil", "..", "a/../../b", "a/b", "a\\b", "."] {
        let result = mgr.create(malicious, &head);
        assert!(
            matches!(result, Err(WorkspaceError::InvalidId(_))),
            "id {malicious:?} must be rejected as invalid, got {result:?}"
        );
    }
}

#[test]
fn create_rejects_an_unresolvable_base_ref() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());

    let result = mgr.create("ws-1", "this-ref-does-not-exist");
    assert!(
        result.is_err(),
        "an unresolvable base ref must not silently create a workspace"
    );
}

#[test]
fn create_rejects_a_duplicate_id() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());

    mgr.create("ws-1", &head).expect("first create");
    let result = mgr.create("ws-1", &head);
    assert!(
        matches!(result, Err(WorkspaceError::AlreadyExists(_))),
        "creating the same id twice must fail rather than silently reusing/overwriting it"
    );
}

#[test]
fn list_is_empty_before_any_workspace_exists() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());

    let workspaces = mgr.list().expect("list");
    assert!(workspaces.is_empty());
}

#[test]
fn list_includes_created_workspaces() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());

    mgr.create("ws-a", &head).expect("create a");
    mgr.create("ws-b", &head).expect("create b");

    let mut ids: Vec<String> = mgr
        .list()
        .expect("list")
        .into_iter()
        .map(|w| w.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["ws-a".to_string(), "ws-b".to_string()]);
}

#[test]
fn show_reports_head_and_unlocked_state_for_a_fresh_workspace() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());

    mgr.create("ws-1", &head).expect("create");
    let info = mgr.show("ws-1").expect("show");

    assert_eq!(info.head, head);
    assert!(!info.locked);
}

#[test]
fn show_errors_for_an_unknown_id() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());

    let result = mgr.show("does-not-exist");
    assert!(matches!(result, Err(WorkspaceError::NotFound(_))));
}

#[test]
fn exec_runs_a_controlled_command_and_reports_its_exit_code() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    mgr.create("ws-1", &head).expect("create");

    let outcome = mgr
        .exec("ws-1", exit_with_code(7), &unrestricted_policy())
        .expect("exec should run without an execution error");

    assert_eq!(outcome.exit_code, Some(7));
    assert!(!outcome.timed_out);
}

#[test]
fn exec_writes_audit_events_for_the_command() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    mgr.create("ws-1", &head).expect("create");

    mgr.exec("ws-1", exit_with_code(0), &unrestricted_policy())
        .expect("exec");

    let log_path = mgr.audit_log_path("ws-1").expect("audit log path");
    let contents =
        std::fs::read_to_string(&log_path).expect("audit log must exist and be readable");
    assert!(
        contents.contains("ProcessSpawn"),
        "audit log must record the command's spawn: {contents}"
    );
    assert!(
        contents.contains("ProcessExit"),
        "audit log must record the command's exit: {contents}"
    );
    for line in contents.lines() {
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "every audit log line must be valid JSON: {line:?}"
        );
    }
}

#[test]
fn exec_clears_the_lock_even_when_the_command_fails() {
    let repo = common::init_temp_repo();
    let (mgr, wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    mgr.create("ws-1", &head).expect("create");

    let _ = mgr.exec("ws-1", exit_with_code(1), &unrestricted_policy());

    assert!(
        !wt.is_locked("ws-1"),
        "the RUNNING.lock must be cleared after exec finishes, even on a nonzero exit"
    );
}

#[test]
fn exec_rejects_an_unknown_workspace() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());

    let result = mgr.exec("does-not-exist", exit_with_code(0), &unrestricted_policy());
    assert!(matches!(result, Err(WorkspaceError::NotFound(_))));
}

#[test]
fn exec_fails_closed_when_the_command_program_is_denied_by_policy() {
    let repo = common::init_temp_repo();
    let (mgr, wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    mgr.create("ws-1", &head).expect("create");

    let command = exit_with_code(0);
    let mut policy = unrestricted_policy();
    policy.denied_programs = vec![command.program.clone()];

    let result = mgr.exec("ws-1", command, &policy);

    assert!(
        matches!(
            result,
            Err(WorkspaceError::Exec(ExecError::PolicyDenied(_)))
        ),
        "a denied program must fail closed through workspace::exec too: {result:?}"
    );
    assert!(
        !wt.is_locked("ws-1"),
        "the RUNNING.lock must still be cleared when the spawn itself is refused by policy"
    );
}

#[test]
fn remove_deletes_the_workspace_directory() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    let info = mgr.create("ws-1", &head).expect("create");

    mgr.remove("ws-1", false).expect("remove");

    assert!(
        !info.path.exists(),
        "workspace directory must be gone after remove"
    );
}

#[test]
fn remove_is_idempotent_when_called_twice() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    mgr.create("ws-1", &head).expect("create");

    mgr.remove("ws-1", false).expect("first remove");
    let second = mgr.remove("ws-1", false);
    assert!(
        second.is_ok(),
        "a second remove of an already-gone workspace must succeed, not error"
    );
}

#[test]
fn remove_refuses_when_locked_without_force() {
    let repo = common::init_temp_repo();
    let (mgr, wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    let info = mgr.create("ws-1", &head).expect("create");
    wt.mark_running("ws-1")
        .expect("simulate an in-progress command");

    let result = mgr.remove("ws-1", false);

    assert!(matches!(result, Err(WorkspaceError::Locked(_))));
    assert!(
        info.path.exists(),
        "a refused remove must not delete anything"
    );
}

#[test]
fn remove_succeeds_when_locked_with_force() {
    let repo = common::init_temp_repo();
    let (mgr, wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    let info = mgr.create("ws-1", &head).expect("create");
    wt.mark_running("ws-1")
        .expect("simulate an in-progress command");

    mgr.remove("ws-1", true).expect("force remove");

    assert!(!info.path.exists());
    assert!(
        !wt.is_locked("ws-1"),
        "force-remove must also clear the lock it overrode"
    );
}

#[test]
fn remove_rejects_a_path_traversal_id() {
    let repo = common::init_temp_repo();
    let (mgr, _wt) = common::workspace_manager(repo.path());

    let result = mgr.remove("../../evil", true);
    assert!(
        matches!(result, Err(WorkspaceError::InvalidId(_))),
        "remove must independently validate its id, not just create — got {result:?}"
    );
}

#[test]
fn clean_removes_only_unlocked_workspaces_by_default() {
    let repo = common::init_temp_repo();
    let (mgr, wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    let idle = mgr.create("idle", &head).expect("create idle");
    let busy = mgr.create("busy", &head).expect("create busy");
    wt.mark_running("busy").expect("simulate busy");

    let removed = mgr.clean(false).expect("clean");

    assert_eq!(removed, vec!["idle".to_string()]);
    assert!(!idle.path.exists());
    assert!(
        busy.path.exists(),
        "a locked workspace must survive a non-forced clean"
    );
}

#[test]
fn clean_with_force_removes_locked_workspaces_too() {
    let repo = common::init_temp_repo();
    let (mgr, wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());
    let busy = mgr.create("busy", &head).expect("create busy");
    wt.mark_running("busy").expect("simulate busy");

    let removed = mgr.clean(true).expect("forced clean");

    assert_eq!(removed, vec!["busy".to_string()]);
    assert!(!busy.path.exists());
}

#[test]
fn creating_and_removing_a_workspace_never_touches_the_primary_checkout() {
    let repo = common::init_temp_repo();
    let git = common::git_repo(repo.path());
    let (mgr, _wt) = common::workspace_manager(repo.path());
    let head = common::head_sha(repo.path());

    let before = git.status_porcelain(repo.path()).expect("status before");
    mgr.create("ws-1", &head).expect("create");
    let mid = git.status_porcelain(repo.path()).expect("status mid");
    mgr.remove("ws-1", false).expect("remove");
    let after = git.status_porcelain(repo.path()).expect("status after");

    assert_eq!(
        before, mid,
        "creating a workspace must not change the primary checkout"
    );
    assert_eq!(
        mid, after,
        "removing a workspace must not change the primary checkout"
    );
}
