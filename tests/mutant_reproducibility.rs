//! Standalone source mutation testing (`mutant::MutantTester`) reproducibility, restoration,
//! deferred evaluation, and cleanup — SPEC.md §10 Amendment (standalone mutation testing pass).
//!
//! `MutantTester` must be a pure function of `(operator, target_glob, seed, operator_version,
//! base_commit)` for its content decisions (`selected_target`, `description`, `diff_stats`) — the
//! same reproducibility guarantee `fault::FaultInjector` and `mutation::MutationEngine` both
//! carry — and `evaluate` must be a separate, later, non-gating step against the still-alive
//! workspace `apply` materialized.

mod common;

use agentforge::domain::{MutantSpec, MutationOperator};
use agentforge::mutant::Error as MutantError;

fn spec(operator: MutationOperator, glob: &str, seed: u64) -> MutantSpec {
    MutantSpec {
        operator,
        target_glob: glob.to_string(),
        seed,
        operator_version: 1,
    }
}

// ---- determinism ----------------------------------------------------------------------------

#[test]
fn find_candidates_is_deterministic_for_a_fixed_seed_and_base() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let tester = common::mutant_tester(repo.path());
    let s = spec(MutationOperator::BooleanFlip, "**/*.txt", 0);

    let a = tester.find_candidates(&head, &s).expect("candidates run 1");
    let b = tester.find_candidates(&head, &s).expect("candidates run 2");

    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(x.file, y.file);
        assert_eq!(x.line, y.line);
        assert_eq!(x.column, y.column);
    }
}

#[test]
fn apply_selects_the_same_target_and_description_across_different_ids() {
    // The core reproducibility guarantee: same (operator, target_glob, seed, operator_version,
    // base_commit) must select the same candidate and produce the same description, regardless
    // of which id it's applied under.
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let tester = common::mutant_tester(repo.path());
    let s = spec(MutationOperator::BooleanFlip, "**/*.txt", 0);

    let first = tester.apply(&head, &s, "mutant-repro-a").expect("apply 1");
    let second = tester.apply(&head, &s, "mutant-repro-b").expect("apply 2");

    assert_eq!(first.selected_target, second.selected_target);
    assert_eq!(first.description, second.description);
    assert_eq!(first.diff_stats, second.diff_stats);
    assert_ne!(
        first.worktree_path, second.worktree_path,
        "distinct ids must still get distinct workspaces"
    );
}

#[test]
fn apply_fails_loudly_when_no_candidates_match() {
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "src/nums.txt",
        "let x = 1;\n",
        "no booleans here",
    );
    let head = common::head_sha(repo.path());
    let tester = common::mutant_tester(repo.path());
    let s = spec(MutationOperator::BooleanFlip, "**/*.txt", 0);

    let result = tester.apply(&head, &s, "mutant-no-candidates");
    assert!(
        matches!(result, Err(MutantError::NoCandidates { .. })),
        "zero matching candidates must fail loudly, never silently succeed: {result:?}"
    );
}

// ---- apply behavior + restore -----------------------------------------------------------------

#[test]
fn apply_mutates_the_file_on_disk_and_restore_reverts_it() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let tester = common::mutant_tester(repo.path());
    let s = spec(MutationOperator::BooleanFlip, "**/*.txt", 0);

    let mutant_ref = tester.apply(&head, &s, "mutant-apply").expect("apply");
    assert_eq!(mutant_ref.selected_target.file, "src/flag.txt");
    let target = mutant_ref.worktree_path.join("src").join("flag.txt");
    let mutated = std::fs::read_to_string(&target).unwrap();
    assert_eq!(mutated, "let ok = false;\n");

    // The source repository itself must never have been touched.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("src/flag.txt")).unwrap(),
        "let ok = true;\n"
    );

    tester.restore(&mutant_ref).expect("restore");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "let ok = true;\n"
    );
}

// ---- deferred, non-gating evaluation -----------------------------------------------------------

#[test]
fn apply_never_evaluates_and_evaluate_is_a_separate_later_step() {
    // Unlike `mutation::MutationEngine::apply` + `sanity_check`, `apply` alone must never run the
    // evaluator or decide anything — `evaluation` starts out unset, and only `evaluate` (called
    // independently, whenever a caller wants) populates a verdict.
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let tester = common::mutant_tester(repo.path());
    let s = spec(MutationOperator::BooleanFlip, "**/*.txt", 0);

    let mutant_ref = tester.apply(&head, &s, "mutant-deferred").expect("apply");
    assert!(
        mutant_ref.evaluation.is_none(),
        "apply must never itself evaluate or gate — the record starts unevaluated"
    );

    let toothless_evaluator = common::noop_evaluator_spec("toothless");
    let verdict = tester
        .evaluate(
            &mutant_ref,
            &toothless_evaluator,
            &agentforge::audit::NullAuditSink,
        )
        .expect("evaluate should run without an execution error");
    assert!(
        verdict.is_good(),
        "a toothless evaluator cannot detect the mutation, so evaluate must honestly report a \
         good verdict rather than pretending the mutant was caught"
    );

    // Calling `apply` again must not have happened — the same workspace is still there and
    // `evaluate` ran against it directly, with no worktree created or removed around the call.
    assert!(mutant_ref.worktree_path.is_dir());
}

// ---- cleanup ------------------------------------------------------------------------------

#[test]
fn discard_removes_the_entire_mutant_workspace() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let tester = common::mutant_tester(repo.path());
    let s = spec(MutationOperator::BooleanFlip, "**/*.txt", 0);

    let mutant_ref = tester.apply(&head, &s, "mutant-discard").expect("apply");
    assert!(mutant_ref.worktree_path.is_dir());

    tester.discard(&mutant_ref).expect("discard");
    assert!(
        !mutant_ref.worktree_path.exists(),
        "discard must remove the entire mutant workspace"
    );
}

// ---- path safety ----------------------------------------------------------------------------

#[test]
fn apply_rejects_a_path_traversal_or_invalid_id() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let tester = common::mutant_tester(repo.path());
    let s = spec(MutationOperator::BooleanFlip, "**/*.txt", 0);

    for malicious in ["../../evil", "..", "a/../../b", "a/b", "a\\b", "."] {
        let result = tester.apply(&head, &s, malicious);
        assert!(
            matches!(result, Err(MutantError::InvalidId(_))),
            "id {malicious:?} must be rejected as invalid, got {result:?}"
        );
    }
}

#[test]
fn apply_rejects_an_empty_id() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let tester = common::mutant_tester(repo.path());
    let s = spec(MutationOperator::BooleanFlip, "**/*.txt", 0);

    let result = tester.apply(&head, &s, "");
    assert!(matches!(result, Err(MutantError::EmptyId)));
}
