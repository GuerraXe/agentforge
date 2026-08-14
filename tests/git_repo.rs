//! Repository / path validation — SPEC.md §6, docs/ARCHITECTURE.md §6.
//!
//! `GitRepo` is the one safe Git abstraction; these tests exercise it against real temporary
//! repositories, never the AgentForge project's own repo.

mod common;

use agentforge::git::worktree::WorktreeManager;
use agentforge::git::GitRepo;

#[test]
fn open_succeeds_on_a_real_git_repo() {
    let repo = common::init_temp_repo();
    let result = GitRepo::open(repo.path().to_path_buf(), common::executor());
    assert!(
        result.is_ok(),
        "GitRepo::open should succeed against a real git repo: {:?}",
        result.err()
    );
}

#[test]
fn open_rejects_a_directory_that_is_not_a_git_repo() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let result = GitRepo::open(dir.path().to_path_buf(), common::executor());
    assert!(
        result.is_err(),
        "GitRepo::open must reject a directory that isn't a git repo, not silently succeed"
    );
}

#[test]
fn resolve_commit_returns_a_40_character_hex_sha_for_head() {
    let repo = common::init_temp_repo();
    let git = GitRepo::open(repo.path().to_path_buf(), common::executor()).expect("open");
    let sha = git.resolve_commit("HEAD").expect("resolve HEAD");
    assert_eq!(
        sha.len(),
        40,
        "resolved commit-ish must be a 40-hex-char SHA, got: {sha}"
    );
    assert!(
        sha.chars().all(|c| c.is_ascii_hexdigit()),
        "resolved SHA must be pure hex, got: {sha}"
    );
}

#[test]
fn resolve_commit_rejects_an_unknown_ref() {
    let repo = common::init_temp_repo();
    let git = GitRepo::open(repo.path().to_path_buf(), common::executor()).expect("open");
    let result = git.resolve_commit("this-ref-does-not-exist-anywhere");
    assert!(
        result.is_err(),
        "resolving a nonexistent commit-ish must fail loudly, never silently return something"
    );
}

#[test]
fn resolve_commit_is_deterministic_for_a_fixed_ref() {
    // Guards SPEC.md §20 (R1): base_ref must be pinnable to a stable SHA, not re-resolved
    // differently across calls.
    let repo = common::init_temp_repo();
    let git = GitRepo::open(repo.path().to_path_buf(), common::executor()).expect("open");
    let a = git.resolve_commit("HEAD").expect("resolve once");
    let b = git.resolve_commit("HEAD").expect("resolve again");
    assert_eq!(a, b, "resolving the same ref twice must yield the same SHA");
}

#[test]
fn resolving_a_commit_after_a_new_commit_changes_head_but_not_the_old_sha() {
    let repo = common::init_temp_repo();
    let git = GitRepo::open(repo.path().to_path_buf(), common::executor()).expect("open");
    let first_sha = git.resolve_commit("HEAD").expect("resolve initial HEAD");

    common::commit_file(repo.path(), "second.txt", "more\n", "second commit");

    let second_sha = git.resolve_commit("HEAD").expect("resolve new HEAD");
    assert_ne!(first_sha, second_sha, "HEAD must move to the new commit");
    let reresolved_first = git
        .resolve_commit(&first_sha)
        .expect("resolve the pinned first SHA");
    assert_eq!(
        first_sha, reresolved_first,
        "a SHA, once resolved and pinned, must always resolve back to itself regardless of \
         where HEAD has since moved — this is the whole point of pinning base_ref"
    );
}

#[test]
fn diff_stats_report_zero_change_against_an_unmodified_worktree() {
    let repo = common::init_temp_repo();
    let git = GitRepo::open(repo.path().to_path_buf(), common::executor()).expect("open");
    let head = common::head_sha(repo.path());
    let stats = git
        .diff_stats(repo.path(), &head)
        .expect("diff_stats against an unmodified tree should succeed");
    assert_eq!(stats.files_changed, 0);
    assert_eq!(stats.lines_added, 0);
    assert_eq!(stats.lines_removed, 0);
}

#[test]
fn state_root_is_never_nested_inside_the_target_repo() {
    // SPEC.md §20 (U1): the worst part of the original isolation gap was worktrees living
    // inside the very repo they were supposed to be isolated from.
    let repo = common::init_temp_repo();
    let state_root = WorktreeManager::resolve_state_root(repo.path())
        .expect("resolve_state_root should succeed");
    assert!(
        !state_root.starts_with(repo.path()),
        "state root {state_root:?} must not be nested inside the repo {:?}",
        repo.path()
    );
}

#[test]
fn state_root_resolution_is_deterministic_for_the_same_repo() {
    let repo = common::init_temp_repo();
    let a = WorktreeManager::resolve_state_root(repo.path()).expect("resolve once");
    let b = WorktreeManager::resolve_state_root(repo.path()).expect("resolve again");
    assert_eq!(
        a, b,
        "the same repo must always resolve to the same state root"
    );
}
