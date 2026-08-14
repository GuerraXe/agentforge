//! `Store`'s task/evaluator persistence — SPEC.md §5, §20 (C6).

mod common;

/// Adversarial-review regression (docs/ADVERSARIAL_REVIEW.md): `workspace::validate_id`,
/// `fault::validate_id`, and `mutant`'s reuse of it already reject exactly these ids (see
/// `tests/workspace.rs`, `tests/fault_reproducibility.rs`, `tests/mutant_reproducibility.rs`),
/// but nothing enforced the same rule for `task`/`evaluator`/`policy` ids or for
/// `experiment`/`race`/`bisect` ids read back via a bare CLI argument (`report show <id>`,
/// `clean --experiment <id>`) — every one of those built a filesystem path directly from a
/// caller-supplied id/name with no validation at all before this fix. Shared across every
/// `Store` collection this test module covers, mirroring the id sets those other test files use.
const MALICIOUS_IDS: [&str; 6] = ["../../evil", "..", "a/../../b", "a/b", "a\\b", "."];

#[test]
fn save_task_rejects_a_path_traversal_id() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    for malicious in MALICIOUS_IDS {
        let task = common::task_spec(malicious, "deadbeef", "eval-a", common::good_verdict());
        let result = store.save_task(&task, true);
        assert!(
            matches!(result, Err(agentforge::store::Error::InvalidId(_)))
                || matches!(result, Err(agentforge::store::Error::EmptyId)),
            "task id {malicious:?} must be rejected as invalid, got {result:?}"
        );
        assert!(
            !repo
                .path()
                .parent()
                .expect("repo has a parent")
                .join("evil")
                .exists(),
            "a rejected task id must never cause anything to be created outside the repo"
        );
    }
}

#[test]
fn load_task_rejects_a_path_traversal_id() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    for malicious in MALICIOUS_IDS {
        let result = store.load_task(malicious);
        assert!(
            matches!(result, Err(agentforge::store::Error::InvalidId(_)))
                || matches!(result, Err(agentforge::store::Error::EmptyId)),
            "task id {malicious:?} must be rejected as invalid on read too, got {result:?}"
        );
    }
}

#[test]
fn save_evaluator_rejects_a_path_traversal_id() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    for malicious in MALICIOUS_IDS {
        let spec = common::noop_evaluator_spec(malicious);
        let result = store.save_evaluator(&spec, true);
        assert!(
            matches!(result, Err(agentforge::store::Error::InvalidId(_)))
                || matches!(result, Err(agentforge::store::Error::EmptyId)),
            "evaluator id {malicious:?} must be rejected as invalid, got {result:?}"
        );
    }
}

#[test]
fn save_policy_rejects_a_path_traversal_name() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    for malicious in MALICIOUS_IDS {
        let policy = common::valid_permission_policy(malicious);
        let result = store.save_policy(&policy, true);
        assert!(
            matches!(result, Err(agentforge::store::Error::InvalidId(_)))
                || matches!(result, Err(agentforge::store::Error::EmptyId)),
            "policy name {malicious:?} must be rejected as invalid, got {result:?}"
        );
    }
}

#[test]
fn load_experiment_rejects_a_path_traversal_id() {
    // Reachable directly from the CLI (`report show <id>`, `clean --experiment <id>`) with no
    // upstream validation of any kind — `Store` is the only choke point every such id passes
    // through.
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    for malicious in MALICIOUS_IDS {
        let result = store.load_experiment(malicious);
        assert!(
            matches!(result, Err(agentforge::store::Error::InvalidId(_)))
                || matches!(result, Err(agentforge::store::Error::EmptyId)),
            "experiment id {malicious:?} must be rejected as invalid, got {result:?}"
        );
    }
}

#[test]
fn save_then_load_task_round_trips() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let task = common::task_spec("task-a", "deadbeef", "eval-a", common::good_verdict());

    store.save_task(&task, false).expect("save_task");
    let loaded = store.load_task("task-a").expect("load_task");

    assert_eq!(loaded.id, task.id);
    assert_eq!(loaded.base_ref, task.base_ref);
    assert_eq!(loaded.evaluator, task.evaluator);
}

#[test]
fn save_task_without_force_refuses_to_overwrite_an_existing_task() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let task = common::task_spec("task-a", "deadbeef", "eval-a", common::good_verdict());

    store.save_task(&task, false).expect("first save");
    let result = store.save_task(&task, false);

    assert!(
        matches!(result, Err(agentforge::store::Error::AlreadyExists(_))),
        "a second save_task without --force must be a collision error, not a silent overwrite: {result:?}"
    );
}

#[test]
fn save_task_with_force_overwrites_an_existing_task() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mut task = common::task_spec("task-a", "deadbeef", "eval-a", common::good_verdict());
    store.save_task(&task, false).expect("first save");

    task.name = "renamed".to_string();
    store.save_task(&task, true).expect("forced overwrite");

    let loaded = store.load_task("task-a").expect("load_task");
    assert_eq!(loaded.name, "renamed");
}

#[test]
fn load_task_reports_not_found_for_an_unknown_id() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    let result = store.load_task("does-not-exist");

    assert!(
        matches!(result, Err(agentforge::store::Error::NotFound(_))),
        "loading a never-saved task id must be a clean NotFound, not an IO error: {result:?}"
    );
}

#[test]
fn list_tasks_returns_sorted_saved_ids() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    store
        .save_task(
            &common::task_spec("task-b", "deadbeef", "eval-a", common::good_verdict()),
            false,
        )
        .unwrap();
    store
        .save_task(
            &common::task_spec("task-a", "deadbeef", "eval-a", common::good_verdict()),
            false,
        )
        .unwrap();

    let ids = store.list_tasks().expect("list_tasks");

    assert_eq!(ids, vec!["task-a".to_string(), "task-b".to_string()]);
}

#[test]
fn save_then_load_evaluator_round_trips() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let spec = common::noop_evaluator_spec("eval-a");

    store.save_evaluator(&spec, false).expect("save_evaluator");
    let loaded = store.load_evaluator("eval-a").expect("load_evaluator");

    assert_eq!(loaded.id, spec.id);
    assert_eq!(loaded.timeout_secs, spec.timeout_secs);
}

#[test]
fn list_evaluators_returns_sorted_saved_ids() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("eval-z"), false)
        .unwrap();
    store
        .save_evaluator(&common::noop_evaluator_spec("eval-a"), false)
        .unwrap();

    let ids = store.list_evaluators().expect("list_evaluators");

    assert_eq!(ids, vec!["eval-a".to_string(), "eval-z".to_string()]);
}
