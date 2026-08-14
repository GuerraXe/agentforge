//! `Store`'s mutant persistence — mirrors `tests/store_fault.rs`'s coverage, SPEC.md §10
//! Amendment (standalone mutation testing pass).

mod common;

#[test]
fn save_then_load_mutant_round_trips() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mutant = common::mutant_ref("mutant-a", "deadbeef", "src/flag.txt");

    store.save_mutant(&mutant, false).expect("save_mutant");
    let loaded = store.load_mutant("mutant-a").expect("load_mutant");

    assert_eq!(loaded.id, mutant.id);
    assert_eq!(loaded.base_commit, mutant.base_commit);
    assert_eq!(loaded.selected_target.file, mutant.selected_target.file);
    assert_eq!(loaded.description, mutant.description);
    assert!(loaded.evaluation.is_none());
}

#[test]
fn save_mutant_without_force_refuses_to_overwrite_an_existing_mutant() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mutant = common::mutant_ref("mutant-a", "deadbeef", "src/flag.txt");

    store.save_mutant(&mutant, false).expect("first save");
    let result = store.save_mutant(&mutant, false);

    assert!(
        matches!(result, Err(agentforge::store::Error::AlreadyExists(_))),
        "a second save_mutant without --force must be a collision error, not a silent overwrite: {result:?}"
    );
}

#[test]
fn save_mutant_with_force_overwrites_an_existing_mutant() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mut mutant = common::mutant_ref("mutant-a", "deadbeef", "src/flag.txt");
    store.save_mutant(&mutant, false).expect("first save");

    mutant.description = "renamed".to_string();
    store.save_mutant(&mutant, true).expect("forced overwrite");

    let loaded = store.load_mutant("mutant-a").expect("load_mutant");
    assert_eq!(loaded.description, "renamed");
}

#[test]
fn save_mutant_with_force_persists_a_recorded_evaluation() {
    // The deferred-evaluation contract: an `evaluate()` outcome is recorded onto the persisted
    // record via a re-save, not lost after the call that produced it returns.
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mut mutant = common::mutant_ref("mutant-a", "deadbeef", "src/flag.txt");
    store.save_mutant(&mutant, false).expect("first save");

    mutant.evaluation = Some(agentforge::domain::MutantEvaluation {
        verdict: common::good_verdict(),
        evaluated_at: chrono::Utc::now(),
    });
    store
        .save_mutant(&mutant, true)
        .expect("save with evaluation");

    let loaded = store.load_mutant("mutant-a").expect("load_mutant");
    assert!(loaded.evaluation.is_some());
    assert!(loaded.evaluation.unwrap().verdict.is_good());
}

#[test]
fn load_mutant_reports_not_found_for_an_unknown_id() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    let result = store.load_mutant("does-not-exist");

    assert!(
        matches!(result, Err(agentforge::store::Error::NotFound(_))),
        "loading a never-saved mutant id must be a clean NotFound, not an IO error: {result:?}"
    );
}

#[test]
fn list_mutants_returns_sorted_saved_ids() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    store
        .save_mutant(&common::mutant_ref("mutant-b", "deadbeef", "a.txt"), false)
        .unwrap();
    store
        .save_mutant(&common::mutant_ref("mutant-a", "deadbeef", "a.txt"), false)
        .unwrap();

    let ids = store.list_mutants().expect("list_mutants");

    assert_eq!(ids, vec!["mutant-a".to_string(), "mutant-b".to_string()]);
}
