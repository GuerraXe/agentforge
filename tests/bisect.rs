//! `bisect::BisectRunner::run_bisect` — in-process binary search over `evaluate()` calls,
//! SPEC.md §13, docs/ARCHITECTURE.md §10.
//!
//! Every fixture repo has a known, scripted behavioral flip at a specific commit, so the exact
//! culprit and exact step count are checkable, not just "a plausible-looking answer" — mirrors
//! SPEC.md §18's test-table entry 21 ("8-commit fixture with one scripted flip → exact culprit
//! commit, exact expected step count").

mod common;

use agentforge::bisect::Error as BisectError;
use agentforge::domain::{Cmd, EvaluatorSpec};

/// `test_cmd`-based criterion: reads a tracked `status.flag` file and passes iff it contains
/// `GOOD` — the behavioral flip is a plain tracked-file content change across commits, exactly
/// the "test" style of criterion SPEC.md §13/§11 describes.
fn status_flag_evaluator_spec(id: &str) -> EvaluatorSpec {
    let (program, args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                "findstr /C:GOOD status.flag >nul && exit /b 0 || exit /b 1".to_string(),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                "grep -q GOOD status.flag && exit 0 || exit 1".to_string(),
            ],
        )
    };
    EvaluatorSpec {
        id: id.to_string(),
        setup_cmds: vec![],
        test_cmd: Cmd {
            program,
            args,
            cwd_relative: ".".into(),
        },
        timeout_secs: 30,
        budget_secs: 60,
        size_budget_lines: 200,
        metric_extractors: vec![],
    }
}

/// `setup_cmds`-based criterion: fails (before `test_cmd` even runs, `build_succeeded = false`)
/// unless a tracked `build.ok` file is present — the "build" style of criterion SPEC.md §11
/// (F4, first setup failure short-circuits) describes, distinct from a `test_cmd` failure.
fn build_marker_evaluator_spec(id: &str) -> EvaluatorSpec {
    let (setup_program, setup_args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                "if exist build.ok (exit /b 0) else (exit /b 1)".to_string(),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec!["-c".to_string(), "test -f build.ok".to_string()],
        )
    };
    let (test_program, test_args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/C".to_string(), "exit 0".to_string()],
        )
    } else {
        ("true".to_string(), vec![])
    };
    EvaluatorSpec {
        id: id.to_string(),
        setup_cmds: vec![Cmd {
            program: setup_program,
            args: setup_args,
            cwd_relative: ".".into(),
        }],
        test_cmd: Cmd {
            program: test_program,
            args: test_args,
            cwd_relative: ".".into(),
        },
        timeout_secs: 30,
        budget_secs: 60,
        size_budget_lines: 200,
        metric_extractors: vec![],
    }
}

/// Builds the 8-commit fixture SPEC.md §18's test-table entry 21 describes: `C0` (the `init_temp_repo`
/// seed commit, used as `good`) followed by `C1..C7`, `status.flag` = `GOOD` through `C3` and
/// `BAD` from `C4` onward. Returns `(good_sha, bad_sha, [C1..C7 shas])`.
fn eight_commit_status_flag_fixture(repo: &std::path::Path) -> (String, String, Vec<String>) {
    let good = common::head_sha(repo);
    let commits: [(&str, &str); 7] = [
        ("GOOD\n", "c1"),
        ("GOOD\n", "c2"),
        ("GOOD\n", "c3"),
        ("BAD\n", "c4"),
        ("BAD\n", "c5"),
        ("BAD\n", "c6"),
        ("BAD\n", "c7"),
    ];
    let mut shas = Vec::new();
    for (label, message) in commits {
        // Each commit's content must actually differ from the last (`commit_file` no-ops on an
        // unchanged tracked file), so the flag word is paired with the unique commit message.
        let content = format!("{label}# {message}\n");
        shas.push(common::commit_file(repo, "status.flag", &content, message));
    }
    let bad = shas.last().unwrap().clone();
    (good, bad, shas)
}

#[test]
fn finds_exact_culprit_via_binary_search_with_expected_step_count() {
    let repo = common::init_temp_repo();
    let (good, bad, commits) = eight_commit_status_flag_fixture(repo.path());
    let (runner, store) = common::bisect_runner_with_store(repo.path());
    store
        .save_evaluator(&status_flag_evaluator_spec("status"), false)
        .expect("save_evaluator");
    let task = common::task_spec("bisect-task", &good, "status", common::good_verdict());

    let before_status = common::run_git(repo.path(), &["status", "--porcelain"]);
    let record = runner.run_bisect(&task, &good, &bad).expect("run_bisect");
    let after_status = common::run_git(repo.path(), &["status", "--porcelain"]);

    // C4 (commits[3]) is the first commit whose status.flag flips to BAD — a textbook binary
    // search over the 7-candidate range [C1..C7] visits exactly 3 midpoints (C4, C2, C3) to
    // find it, in that order.
    assert_eq!(record.culprit.as_deref(), Some(commits[3].as_str()));
    assert_eq!(record.steps.len(), 3, "steps: {:?}", record.steps);
    let visited: Vec<&str> = record.steps.iter().map(|s| s.commit.as_str()).collect();
    assert_eq!(
        visited,
        vec![
            commits[3].as_str(),
            commits[1].as_str(),
            commits[2].as_str()
        ]
    );
    assert_eq!(
        record.steps.iter().map(|s| s.is_good).collect::<Vec<_>>(),
        vec![false, true, true]
    );

    assert_eq!(record.task_id, "bisect-task");
    assert_eq!(record.evaluator_id, "status");
    assert_eq!(record.range, (good.clone(), bad.clone()));

    assert!(
        !record.worktree_path.exists(),
        "the dedicated bisect worktree must be removed once the session finishes"
    );
    assert_eq!(
        before_status.stdout, after_status.stdout,
        "the primary checkout's working tree/index must never be touched by bisect — \
         SPEC.md §7/§17, mirroring experiment's own guarantee"
    );
}

#[test]
fn inconclusive_range_returns_no_culprit_not_an_error() {
    let repo = common::init_temp_repo();
    let good = common::head_sha(repo.path());
    // Every commit in range stays GOOD — the endpoints agree, so there is no flip to find. Each
    // commit's content still differs (a per-commit marker line), since `commit_file` no-ops on
    // an unchanged tracked file.
    let mut last = good.clone();
    for i in 0..7 {
        let content = format!("GOOD\n# c{i}\n");
        last = common::commit_file(repo.path(), "status.flag", &content, &format!("c{i}"));
    }
    let bad = last;
    let (runner, store) = common::bisect_runner_with_store(repo.path());
    store
        .save_evaluator(&status_flag_evaluator_spec("status"), false)
        .expect("save_evaluator");
    let task = common::task_spec(
        "bisect-inconclusive",
        &good,
        "status",
        common::good_verdict(),
    );

    let record = runner
        .run_bisect(&task, &good, &bad)
        .expect("an inconclusive range is Ok(record), not Err — a judgment, not a failure");

    assert!(
        record.culprit.is_none(),
        "no candidate ever flipped bad, so there is no culprit"
    );
    assert!(
        !record.steps.is_empty(),
        "the search still records the steps it actually tested, even without finding a culprit"
    );
    assert!(
        record.steps.iter().all(|s| s.is_good),
        "every tested candidate must be recorded as good in this fixture"
    );
    assert!(
        !record.worktree_path.exists(),
        "the worktree is still cleaned up on an inconclusive result"
    );
}

#[test]
fn a_non_ancestor_range_is_rejected_before_any_worktree_is_created() {
    let repo = common::init_temp_repo();
    let base = common::head_sha(repo.path());
    common::run_git(repo.path(), &["checkout", "-b", "branch-a", &base]);
    let branch_a_tip = common::commit_file(repo.path(), "a.txt", "A\n", "a");
    common::run_git(repo.path(), &["checkout", "-b", "branch-b", &base]);
    let branch_b_tip = common::commit_file(repo.path(), "b.txt", "B\n", "b");

    let (runner, store) = common::bisect_runner_with_store(repo.path());
    store
        .save_evaluator(&status_flag_evaluator_spec("status"), false)
        .expect("save_evaluator");
    let task = common::task_spec(
        "bisect-nonlinear",
        &branch_a_tip,
        "status",
        common::good_verdict(),
    );

    let err = runner
        .run_bisect(&task, &branch_a_tip, &branch_b_tip)
        .expect_err("diverged branches must not be treated as a linear range");
    match err {
        BisectError::NotLinear { good, bad } => {
            assert_eq!(good, branch_a_tip);
            assert_eq!(bad, branch_b_tip);
        }
        other => panic!("expected NotLinear, got {other:?}"),
    }
}

#[test]
fn persisted_bisect_record_round_trips_through_store() {
    let repo = common::init_temp_repo();
    let (good, bad, commits) = eight_commit_status_flag_fixture(repo.path());
    let (runner, store) = common::bisect_runner_with_store(repo.path());
    store
        .save_evaluator(&status_flag_evaluator_spec("status"), false)
        .expect("save_evaluator");
    let task = common::task_spec("bisect-persist", &good, "status", common::good_verdict());

    let record = runner.run_bisect(&task, &good, &bad).expect("run_bisect");
    let loaded = store.load_bisect(&record.id).expect("load_bisect");

    assert_eq!(loaded.culprit, record.culprit);
    assert_eq!(loaded.culprit.as_deref(), Some(commits[3].as_str()));
    assert_eq!(loaded.steps.len(), record.steps.len());
    for (a, b) in loaded.steps.iter().zip(record.steps.iter()) {
        assert_eq!(a.commit, b.commit);
        assert_eq!(a.is_good, b.is_good);
    }
    assert_eq!(loaded.range, record.range);
    assert_eq!(loaded.task_id, record.task_id);
    assert_eq!(loaded.evaluator_id, record.evaluator_id);
}

#[test]
fn a_build_failure_criterion_expressed_via_setup_cmds_is_treated_as_bad() {
    let repo = common::init_temp_repo();
    let good = common::head_sha(repo.path());
    // C1: still builds. C2, C3: build.ok removed — build breaks starting at C2.
    common::commit_file(repo.path(), "build.ok", "1\n", "c1-still-builds");
    let c2 = {
        std::fs::remove_file(repo.path().join("build.ok")).unwrap();
        common::run_git(repo.path(), &["add", "-A"]);
        let out = common::run_git(repo.path(), &["commit", "-q", "-m", "c2-build-breaks"]);
        assert!(out.status.success(), "commit failed: {out:?}");
        common::head_sha(repo.path())
    };
    let c3 = common::commit_file(repo.path(), "unrelated.txt", "x\n", "c3-still-broken");
    let bad = c3;

    let (runner, store) = common::bisect_runner_with_store(repo.path());
    store
        .save_evaluator(&build_marker_evaluator_spec("build"), false)
        .expect("save_evaluator");
    let task = common::task_spec("bisect-build", &good, "build", common::good_verdict());

    let record = runner.run_bisect(&task, &good, &bad).expect("run_bisect");

    assert_eq!(
        record.culprit.as_deref(),
        Some(c2.as_str()),
        "the first commit whose build fails (setup_cmds, not test_cmd) must be the culprit"
    );
    for step in &record.steps {
        if step.commit == c2 {
            assert!(!step.verdict.build_succeeded);
        }
    }
}
