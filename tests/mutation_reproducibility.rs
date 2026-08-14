//! Fault/mutation selection reproducibility — SPEC.md §10, §20 (R4/R5), docs/ARCHITECTURE.md §9.
//!
//! `MutationEngine` must be a pure function of `(operator, target_glob, seed, operator_version,
//! base_ref)` — the same inputs must always select the same candidate and produce the same
//! mutant commit, regardless of how many times it's called.

mod common;

use agentforge::domain::{MutationOperator, MutationSpec};
use agentforge::mutation::{Error as MutationError, MutationEngine};

fn engine(repo_path: &std::path::Path) -> MutationEngine {
    let git = common::git_repo(repo_path);
    let wt = common::worktree_manager(git.clone());
    let ev = common::evaluator();
    MutationEngine::new(git, wt, ev)
}

fn boolean_flip_spec(seed: u64) -> MutationSpec {
    MutationSpec {
        operator: MutationOperator::BooleanFlip,
        target_glob: "**/*.txt".to_string(),
        seed,
        operator_version: 1,
    }
}

#[test]
fn find_candidates_is_deterministic_for_a_fixed_seed_and_base() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let eng = engine(repo.path());

    let a = eng
        .find_candidates(&head, &boolean_flip_spec(1))
        .expect("candidates run 1");
    let b = eng
        .find_candidates(&head, &boolean_flip_spec(1))
        .expect("candidates run 2");

    assert_eq!(
        a.len(),
        b.len(),
        "identical (operator, target_glob, base) must discover the same candidate count both times"
    );
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(
            x.file, y.file,
            "candidate file order must be identical across runs"
        );
        assert_eq!(x.line, y.line);
        assert_eq!(x.column, y.column);
    }
}

#[test]
fn apply_produces_an_identical_mutant_commit_across_repeated_runs() {
    // The core reproducibility guarantee: same (operator, target_glob, seed, operator_version,
    // base_ref) must produce a byte-identical mutant, not just a similar one.
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let eng = engine(repo.path());

    let first = eng
        .apply(&head, &boolean_flip_spec(1), "task-repro-a")
        .expect("apply run 1");
    let second = eng
        .apply(&head, &boolean_flip_spec(1), "task-repro-b")
        .expect("apply run 2");

    assert_eq!(
        first.mutant_commit, second.mutant_commit,
        "identical mutation inputs must produce an identical mutant commit SHA"
    );
}

#[test]
fn the_same_seed_reapplied_later_still_selects_the_same_candidate() {
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "src/flags.txt",
        "let a = true;\nlet b = true;\nlet c = true;\n",
        "add flags",
    );
    let head = common::head_sha(repo.path());
    let eng = engine(repo.path());

    let first = eng
        .apply(&head, &boolean_flip_spec(0), "task-seed-first")
        .expect("apply seed 0, first time");
    let second = eng
        .apply(&head, &boolean_flip_spec(0), "task-seed-second")
        .expect("apply seed 0, second time");

    assert_eq!(
        first.mutant_commit, second.mutant_commit,
        "re-running with the same seed later must select the same candidate, not drift"
    );
}

#[test]
fn apply_fails_loudly_when_no_candidates_match() {
    // SPEC.md §10: zero candidates found is an error, never a silent no-op.
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "src/nums.txt",
        "let x = 1;\n",
        "no booleans here",
    );
    let head = common::head_sha(repo.path());
    let eng = engine(repo.path());

    let result = eng.apply(&head, &boolean_flip_spec(0), "task-no-candidates");
    assert!(
        matches!(result, Err(MutationError::NoCandidates { .. })),
        "applying a spec with zero matching candidates must fail with NoCandidates, not silently succeed: {result:?}"
    );
}

#[test]
fn apply_records_the_selected_target_and_structured_diff_stats() {
    // The reproducible record must say *what* was mutated (SelectedTarget) and *what changed*
    // (DiffStats), not just the resulting commit SHA.
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let eng = engine(repo.path());

    let mutation_ref = eng
        .apply(&head, &boolean_flip_spec(0), "task-target")
        .expect("apply");

    assert_eq!(mutation_ref.selected_target.file, "src/flag.txt");
    assert_eq!(mutation_ref.selected_target.line, 1);
    assert_eq!(mutation_ref.base_commit, head);
    assert_eq!(
        mutation_ref.diff_stats.files_changed, 1,
        "exactly one file was mutated"
    );
    assert_eq!(mutation_ref.diff_stats.lines_added, 1);
    assert_eq!(mutation_ref.diff_stats.lines_removed, 1);
}

#[test]
fn apply_points_a_ref_at_the_mutant_commit_and_discard_removes_it() {
    // Restore/cleanup information: `mutant_ref` names the exact git ref, and `discard` is the
    // entire cleanup surface — no worktree or HEAD was ever touched by `apply`.
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let eng = engine(repo.path());

    let mutation_ref = eng
        .apply(&head, &boolean_flip_spec(0), "task-cleanup")
        .expect("apply");
    assert_eq!(
        mutation_ref.mutant_ref,
        "refs/agentforge/mutants/task-cleanup"
    );

    let resolved = common::run_git(
        repo.path(),
        &["rev-parse", "--verify", "--quiet", &mutation_ref.mutant_ref],
    );
    assert!(
        resolved.status.success(),
        "mutant_ref must resolve to a real commit before discard"
    );

    eng.discard(&mutation_ref).expect("discard");

    let resolved_after = common::run_git(
        repo.path(),
        &["rev-parse", "--verify", "--quiet", &mutation_ref.mutant_ref],
    );
    assert!(
        !resolved_after.status.success(),
        "mutant_ref must no longer resolve after discard"
    );
}

#[test]
fn apply_ignores_boolean_literals_inside_string_and_comment_lines() {
    // SPEC.md §10's best-effort string/comment skip: a real (non-literal) `true` must still be
    // found even when a comment line and a string literal containing the word `true` sit right
    // next to it.
    let repo = common::init_temp_repo();
    common::commit_file(
        repo.path(),
        "src/flag.txt",
        "// true\nlet label = \"true\";\nlet ok = true;\n",
        "add flag with noise",
    );
    let head = common::head_sha(repo.path());
    let eng = engine(repo.path());

    let candidates = eng
        .find_candidates(&head, &boolean_flip_spec(0))
        .expect("find_candidates");

    assert_eq!(
        candidates.len(),
        1,
        "the comment line and the string-literal line must not produce candidates: {candidates:?}"
    );
    assert_eq!(candidates[0].line, 3);
}

#[test]
fn sanity_check_reports_a_good_verdict_when_the_mutant_is_undetectable() {
    // `sanity_check` reports the evaluator's honest verdict on a mutant — it doesn't itself
    // decide accept/reject (that's `mutate`'s job, SPEC.md §10). A "toothless" evaluator with
    // no real assertions cannot detect any mutation, so the verdict must come back good; the
    // caller is responsible for treating "good verdict on a mutant" as "reject this mutation."
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "src/flag.txt", "let ok = true;\n", "add flag");
    let head = common::head_sha(repo.path());
    let eng = engine(repo.path());

    let mutation_ref = eng
        .apply(&head, &boolean_flip_spec(0), "task-sanity")
        .expect("apply");
    let toothless_evaluator = common::noop_evaluator_spec("toothless");

    let verdict = eng
        .sanity_check(&mutation_ref, &toothless_evaluator)
        .expect("sanity_check should run without an execution error");
    assert!(
        verdict.is_good(),
        "a toothless evaluator cannot detect the mutation, so sanity_check must honestly \
         report a good verdict rather than pretending the mutant was caught"
    );
}
