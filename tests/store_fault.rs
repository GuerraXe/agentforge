//! `Store`'s fault persistence — mirrors `tests/store.rs`'s task/evaluator coverage, SPEC.md §10
//! Amendment.

mod common;

#[test]
fn save_then_load_fault_round_trips() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let fault = common::fault_ref("fault-a", "deadbeef", "src/data.txt");

    store.save_fault(&fault, false).expect("save_fault");
    let loaded = store.load_fault("fault-a").expect("load_fault");

    assert_eq!(loaded.id, fault.id);
    assert_eq!(loaded.base_commit, fault.base_commit);
    assert_eq!(loaded.selected_target.file, fault.selected_target.file);
    assert_eq!(loaded.description, fault.description);
}

#[test]
fn save_fault_without_force_refuses_to_overwrite_an_existing_fault() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let fault = common::fault_ref("fault-a", "deadbeef", "src/data.txt");

    store.save_fault(&fault, false).expect("first save");
    let result = store.save_fault(&fault, false);

    assert!(
        matches!(result, Err(agentforge::store::Error::AlreadyExists(_))),
        "a second save_fault without --force must be a collision error, not a silent overwrite: {result:?}"
    );
}

#[test]
fn save_fault_with_force_overwrites_an_existing_fault() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mut fault = common::fault_ref("fault-a", "deadbeef", "src/data.txt");
    store.save_fault(&fault, false).expect("first save");

    fault.description = "renamed".to_string();
    store.save_fault(&fault, true).expect("forced overwrite");

    let loaded = store.load_fault("fault-a").expect("load_fault");
    assert_eq!(loaded.description, "renamed");
}

#[test]
fn load_fault_reports_not_found_for_an_unknown_id() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    let result = store.load_fault("does-not-exist");

    assert!(
        matches!(result, Err(agentforge::store::Error::NotFound(_))),
        "loading a never-saved fault id must be a clean NotFound, not an IO error: {result:?}"
    );
}

#[test]
fn list_faults_returns_sorted_saved_ids() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    store
        .save_fault(&common::fault_ref("fault-b", "deadbeef", "a.txt"), false)
        .unwrap();
    store
        .save_fault(&common::fault_ref("fault-a", "deadbeef", "a.txt"), false)
        .unwrap();

    let ids = store.list_faults().expect("list_faults");

    assert_eq!(ids, vec!["fault-a".to_string(), "fault-b".to_string()]);
}
