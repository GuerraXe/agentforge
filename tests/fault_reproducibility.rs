//! Repository fault injection reproducibility, restoration, cleanup, and path-safety — SPEC.md
//! §10 Amendment, docs/ARCHITECTURE.md §9a.
//!
//! `FaultInjector` must be a pure function of `(kind, target_glob, seed, fault_version,
//! base_commit)` for its content decisions (`selected_target`, `description`, `diff_stats`) —
//! which `id` a fault is injected under only changes its `worktree_path`, never what fault was
//! chosen or how it was applied. Every fault must also be reversible (`restore`) or disposable
//! (`discard`) without ever touching the source repository itself.

mod common;

use agentforge::domain::{FaultKind, FaultSpec};
use agentforge::fault::{Error as FaultError, FaultInjector};

fn injector(repo_path: &std::path::Path) -> FaultInjector {
    let git = common::git_repo(repo_path);
    let wt = common::worktree_manager(git.clone());
    FaultInjector::new(git, wt)
}

fn spec(kind: FaultKind, glob: &str, seed: u64) -> FaultSpec {
    FaultSpec {
        kind,
        target_glob: glob.to_string(),
        seed,
        fault_version: 1,
    }
}

// ---- determinism ----------------------------------------------------------------------------

#[test]
fn find_candidates_is_deterministic_for_a_fixed_seed_and_base() {
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "config.toml",
        "port = 8080\ntimeout = 30\n",
        "add config",
    );
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::BrokenConfigValue, "**/*.toml", 0);

    let a = inj.find_candidates(&head, &s).expect("candidates run 1");
    let b = inj.find_candidates(&head, &s).expect("candidates run 2");

    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(x.file, y.file);
        assert_eq!(x.line, y.line);
    }
}

#[test]
fn inject_selects_the_same_target_and_description_across_different_ids() {
    // The core reproducibility guarantee: same (kind, target_glob, seed, fault_version,
    // base_commit) must select the same candidate and produce the same description, regardless
    // of which id it's injected under.
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "config.toml",
        "port = 8080\ntimeout = 30\n",
        "add config",
    );
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::BrokenConfigValue, "**/*.toml", 0);

    let first = inj.inject(&head, &s, "fault-repro-a").expect("inject 1");
    let second = inj.inject(&head, &s, "fault-repro-b").expect("inject 2");

    assert_eq!(first.selected_target, second.selected_target);
    assert_eq!(first.description, second.description);
    assert_eq!(first.diff_stats, second.diff_stats);
    assert_ne!(
        first.worktree_path, second.worktree_path,
        "distinct ids must still get distinct workspaces"
    );
}

#[test]
fn inject_fails_loudly_when_no_candidates_match() {
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "notes.txt",
        "nothing to see here\n",
        "add notes",
    );
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::DependencyCorruption, "**/*.txt", 0);

    let result = inj.inject(&head, &s, "fault-no-candidates");
    assert!(
        matches!(result, Err(FaultError::NoCandidates { .. })),
        "zero matching candidates must fail loudly, never silently succeed: {result:?}"
    );
}

// ---- per-kind behavior + restoration ---------------------------------------------------------

#[test]
fn missing_file_deletes_the_file_and_restore_recreates_it() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/data.txt", "hello\n", "add data");
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::MissingFile, "**/*.txt", 0);

    let fault_ref = inj.inject(&head, &s, "fault-missing").expect("inject");
    assert_eq!(fault_ref.selected_target.file, "src/data.txt");
    let target = fault_ref.worktree_path.join("src").join("data.txt");
    assert!(!target.exists(), "MissingFile must delete the file");

    inj.restore(&fault_ref).expect("restore");
    assert!(target.exists(), "restore must recreate the deleted file");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello\n");

    // The source repository itself must never have been touched.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("src/data.txt")).unwrap(),
        "hello\n"
    );
}

#[test]
fn broken_config_value_corrupts_the_line_and_restore_reverts_it() {
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "config.toml",
        "port = 8080\ntimeout = 30\n",
        "add config",
    );
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::BrokenConfigValue, "**/*.toml", 0);

    let fault_ref = inj
        .inject(&head, &s, "fault-broken-config")
        .expect("inject");
    let target = fault_ref.worktree_path.join("config.toml");
    let corrupted = std::fs::read_to_string(&target).unwrap();
    assert!(
        corrupted.contains("__AGENTFORGE_BROKEN__"),
        "the selected line must be corrupted: {corrupted:?}"
    );
    assert_ne!(corrupted, "port = 8080\ntimeout = 30\n");

    inj.restore(&fault_ref).expect("restore");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "port = 8080\ntimeout = 30\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("config.toml")).unwrap(),
        "port = 8080\ntimeout = 30\n"
    );
}

#[test]
fn stale_artifact_overwrites_contents_with_a_fixed_marker_and_restore_reverts_it() {
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "build/manifest.json",
        "{\"generated\": true}\n",
        "add manifest",
    );
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::StaleArtifact, "**/*.json", 0);

    let fault_ref = inj.inject(&head, &s, "fault-stale").expect("inject");
    let target = fault_ref.worktree_path.join("build").join("manifest.json");
    let contents = std::fs::read_to_string(&target).unwrap();
    assert_eq!(contents, "AGENTFORGE_STALE_MARKER\n");

    inj.restore(&fault_ref).expect("restore");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "{\"generated\": true}\n"
    );
}

#[test]
fn stale_artifact_marker_is_timestamp_independent() {
    // Injecting the same spec twice must produce byte-identical content — never derived from
    // the wall clock.
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "build/manifest.json",
        "{\"generated\": true}\n",
        "add manifest",
    );
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::StaleArtifact, "**/*.json", 0);

    let first = inj.inject(&head, &s, "fault-stale-a").expect("inject 1");
    std::thread::sleep(std::time::Duration::from_millis(20));
    let second = inj.inject(&head, &s, "fault-stale-b").expect("inject 2");

    let first_contents =
        std::fs::read_to_string(first.worktree_path.join("build").join("manifest.json")).unwrap();
    let second_contents =
        std::fs::read_to_string(second.worktree_path.join("build").join("manifest.json")).unwrap();
    assert_eq!(first_contents, second_contents);
}

#[test]
fn dependency_corruption_replaces_the_version_and_restore_reverts_it() {
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "Cargo.toml",
        "[dependencies]\nserde = \"1.2.3\"\n",
        "add manifest",
    );
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::DependencyCorruption, "**/*.toml", 0);

    let fault_ref = inj.inject(&head, &s, "fault-dependency").expect("inject");
    let target = fault_ref.worktree_path.join("Cargo.toml");
    let corrupted = std::fs::read_to_string(&target).unwrap();
    assert!(
        corrupted.contains("0.0.0-agentforge-corrupted"),
        "the version pin must be corrupted: {corrupted:?}"
    );
    assert!(!corrupted.contains("1.2.3"));

    inj.restore(&fault_ref).expect("restore");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "[dependencies]\nserde = \"1.2.3\"\n"
    );
}

// ---- cleanup ------------------------------------------------------------------------------

#[test]
fn discard_removes_the_entire_fault_workspace() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/data.txt", "hello\n", "add data");
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::MissingFile, "**/*.txt", 0);

    let fault_ref = inj.inject(&head, &s, "fault-discard").expect("inject");
    assert!(fault_ref.worktree_path.is_dir());

    inj.discard(&fault_ref).expect("discard");
    assert!(
        !fault_ref.worktree_path.exists(),
        "discard must remove the entire fault workspace"
    );
}

// ---- path safety ----------------------------------------------------------------------------

#[test]
fn inject_rejects_a_path_traversal_or_invalid_id() {
    // Mirrors workspace::WorkspaceManager's `create_rejects_a_path_traversal_id` — the fault id
    // becomes a filesystem directory name (and a Store key), so it must never be usable to
    // escape the fault-worktrees root.
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/data.txt", "hello\n", "add data");
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::MissingFile, "**/*.txt", 0);

    for malicious in ["../../evil", "..", "a/../../b", "a/b", "a\\b", "."] {
        let result = inj.inject(&head, &s, malicious);
        assert!(
            matches!(result, Err(FaultError::InvalidId(_))),
            "id {malicious:?} must be rejected as invalid, got {result:?}"
        );
    }
}

#[test]
fn inject_rejects_an_empty_id() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/data.txt", "hello\n", "add data");
    let head = common::head_sha(repo.path());
    let inj = injector(repo.path());
    let s = spec(FaultKind::MissingFile, "**/*.txt", 0);

    let result = inj.inject(&head, &s, "");
    assert!(matches!(result, Err(FaultError::EmptyId)));
}

/// Adversarial-review regression (docs/ADVERSARIAL_REVIEW.md): `find_candidates`/
/// `is_safe_relative_path` only check the string shape of a tracked path (no `..`/absolute
/// components) — they say nothing about what a *symlink* checked out at that path actually
/// resolves to on disk. A hostile repository can track a symlink whose target lives outside the
/// fault worktree entirely; before this fix, `StaleArtifact`/`BrokenConfigValue`/
/// `DependencyCorruption` would follow it via a plain `std::fs::write`, letting the repository
/// under test escape its isolated worktree the moment `fault inject` touched that candidate.
/// Unix-only: creating a symlink needs no special privilege here, unlike Windows (see the
/// `#[cfg(windows)]` counterpart below, which is privilege-dependent and skips gracefully).
#[cfg(unix)]
#[test]
fn inject_refuses_to_follow_a_tracked_symlink_outside_the_worktree() {
    let repo = common::init_temp_repo();
    let outside = tempfile::tempdir().expect("dir outside the repo entirely");
    let sensitive = outside.path().join("sensitive.txt");
    std::fs::write(&sensitive, "do-not-touch").expect("seed the symlink target");

    std::os::unix::fs::symlink(&sensitive, repo.path().join("config.toml"))
        .expect("create a tracked symlink pointing outside the repo");
    common::run_git(repo.path(), &["add", "config.toml"]);
    common::run_git(repo.path(), &["commit", "-m", "add malicious symlink"]);
    let head = common::head_sha(repo.path());

    let inj = injector(repo.path());
    let s = spec(FaultKind::StaleArtifact, "**/*.toml", 0);

    let result = inj.inject(&head, &s, "sym-fault");
    assert!(
        matches!(result, Err(FaultError::TargetIsSymlink(_))),
        "a fault write must refuse to follow a tracked symlink out of the worktree, got {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&sensitive).expect("read the symlink target"),
        "do-not-touch",
        "the file the symlink points at, outside the worktree, must be untouched"
    );
}

/// Windows counterpart of the Unix test above. Creating a symlink on Windows needs
/// `SeCreateSymbolicLinkPrivilege` (granted by Developer Mode or an elevated process) — absent in
/// some sandboxes/CI runners, so this skips rather than failing when that privilege isn't
/// available, instead of asserting a false negative. Where the privilege *is* available, it
/// verifies exactly the same guarantee as the Unix test.
#[cfg(windows)]
#[test]
fn inject_refuses_to_follow_a_tracked_symlink_outside_the_worktree() {
    let repo = common::init_temp_repo();
    let outside = tempfile::tempdir().expect("dir outside the repo entirely");
    let sensitive = outside.path().join("sensitive.txt");
    std::fs::write(&sensitive, "do-not-touch").expect("seed the symlink target");

    if let Err(e) = std::os::windows::fs::symlink_file(&sensitive, repo.path().join("config.toml"))
    {
        eprintln!(
            "skipping inject_refuses_to_follow_a_tracked_symlink_outside_the_worktree: \
             cannot create a symlink in this environment ({e}) — needs Developer Mode or \
             elevation"
        );
        return;
    }
    common::run_git(repo.path(), &["add", "config.toml"]);
    common::run_git(repo.path(), &["commit", "-m", "add malicious symlink"]);
    let head = common::head_sha(repo.path());

    let inj = injector(repo.path());
    let s = spec(FaultKind::StaleArtifact, "**/*.toml", 0);

    let result = inj.inject(&head, &s, "sym-fault");
    assert!(
        matches!(result, Err(FaultError::TargetIsSymlink(_))),
        "a fault write must refuse to follow a tracked symlink out of the worktree, got {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&sensitive).expect("read the symlink target"),
        "do-not-touch",
        "the file the symlink points at, outside the worktree, must be untouched"
    );
}
