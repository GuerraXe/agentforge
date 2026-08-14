//! Worktree lifecycle behavior, including cleanup — SPEC.md §7, docs/ARCHITECTURE.md §6.

mod common;

use agentforge::git::worktree::WorktreeKind;

#[test]
fn experiment_worktree_is_created_on_disk() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let git = common::git_repo(repo.path());
    let mgr = common::worktree_manager(git);

    let handle = mgr
        .create_experiment_worktree("exp-1", &head)
        .expect("create experiment worktree");

    assert!(
        handle.path.exists(),
        "worktree directory must exist on disk after creation"
    );
    assert_eq!(handle.kind, WorktreeKind::Experiment);
}

#[test]
fn two_experiment_worktrees_for_the_same_task_never_share_a_path() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let git = common::git_repo(repo.path());
    let mgr = common::worktree_manager(git);

    let a = mgr
        .create_experiment_worktree("exp-a", &head)
        .expect("create a");
    let b = mgr
        .create_experiment_worktree("exp-b", &head)
        .expect("create b");

    assert_ne!(
        a.path, b.path,
        "concurrent experiments must never collide on worktree path"
    );
}

#[test]
fn experiment_and_bisect_worktrees_are_distinct_kinds_and_paths() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let git = common::git_repo(repo.path());
    let mgr = common::worktree_manager(git);

    let exp = mgr
        .create_experiment_worktree("exp-1", &head)
        .expect("experiment worktree");
    let bis = mgr
        .create_bisect_worktree("bis-1", &head)
        .expect("bisect worktree");

    assert_eq!(exp.kind, WorktreeKind::Experiment);
    assert_eq!(bis.kind, WorktreeKind::Bisect);
    assert_ne!(exp.path, bis.path);
}

#[test]
fn evaluation_worktree_has_its_own_kind() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let git = common::git_repo(repo.path());
    let mgr = common::worktree_manager(git);

    let handle = mgr
        .create_evaluation_worktree(&head)
        .expect("create evaluation worktree");
    assert_eq!(handle.kind, WorktreeKind::Evaluation);
}

#[test]
fn removed_worktree_no_longer_exists_on_disk() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let git = common::git_repo(repo.path());
    let mgr = common::worktree_manager(git);

    let handle = mgr
        .create_experiment_worktree("exp-1", &head)
        .expect("create");
    mgr.remove(&handle).expect("remove");

    assert!(
        !handle.path.exists(),
        "cleanup behavior: the worktree directory must be gone after remove()"
    );
}

#[test]
fn creating_and_removing_a_worktree_never_touches_the_primary_checkout() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let git = common::git_repo(repo.path());
    let mgr = common::worktree_manager(git.clone());

    let before = git.status_porcelain(repo.path()).expect("status before");
    let handle = mgr
        .create_experiment_worktree("exp-1", &head)
        .expect("create");
    let mid = git.status_porcelain(repo.path()).expect("status mid");
    mgr.remove(&handle).expect("remove");
    let after = git.status_porcelain(repo.path()).expect("status after");

    assert_eq!(
        before, mid,
        "creating an experiment worktree must not change the caller's primary checkout — SPEC.md §7"
    );
    assert_eq!(
        mid, after,
        "removing an experiment worktree must not change the caller's primary checkout"
    );
}

#[test]
fn mark_running_locks_and_clear_running_unlocks() {
    // The RUNNING.lock protocol — SPEC.md §7, §20 (F1/M1): this is what lets `clean`
    // distinguish an experiment that's genuinely in progress from one that was orphaned by a
    // crash.
    let repo = common::init_temp_repo();
    let git = common::git_repo(repo.path());
    let mgr = common::worktree_manager(git);

    assert!(
        !mgr.is_locked("exp-lock-test"),
        "an experiment must not be considered locked before mark_running is ever called"
    );

    mgr.mark_running("exp-lock-test").expect("mark_running");
    assert!(
        mgr.is_locked("exp-lock-test"),
        "is_locked must be true immediately after mark_running"
    );

    mgr.clear_running("exp-lock-test").expect("clear_running");
    assert!(
        !mgr.is_locked("exp-lock-test"),
        "cleanup behavior: is_locked must be false after clear_running"
    );
}

#[test]
fn locks_for_different_experiments_are_independent() {
    let repo = common::init_temp_repo();
    let git = common::git_repo(repo.path());
    let mgr = common::worktree_manager(git);

    mgr.mark_running("exp-a").expect("mark a running");
    assert!(mgr.is_locked("exp-a"));
    assert!(
        !mgr.is_locked("exp-b"),
        "locking one experiment must not affect another experiment's lock state"
    );
}
