//! Evaluator pass/fail behavior — SPEC.md §11, docs/ARCHITECTURE.md §8.
//!
//! `Evaluator::evaluate` is the one shared judgment `run`, `race`, `bisect`, `mutate`'s sanity
//! gate, and `task add`'s baseline capture all call — these tests exercise it directly.

mod common;

use agentforge::audit::NullAuditSink;
use agentforge::domain::Cmd;

#[test]
fn is_good_is_true_only_when_build_succeeded_and_not_timed_out_and_exit_zero() {
    // `EvaluatorVerdict::is_good()` is real, already-implemented logic — this test should pass
    // today, independent of everything else still being `todo!()`.
    let mut v = common::good_verdict();
    assert!(v.is_good(), "a fully passing verdict must be good");

    v = common::good_verdict();
    v.build_succeeded = false;
    assert!(!v.is_good(), "must not be good when the build failed");

    v = common::good_verdict();
    v.timed_out = true;
    assert!(
        !v.is_good(),
        "must not be good when the evaluator timed out"
    );

    v = common::good_verdict();
    v.exit_code = 1;
    assert!(
        !v.is_good(),
        "must not be good when exit_code is nonzero, even though build_succeeded is true — \
         SPEC.md §20 (S1)"
    );
}

#[test]
fn evaluate_reports_a_good_verdict_for_a_trivially_passing_test_cmd() {
    let repo = common::init_temp_repo();
    let ev = common::evaluator();
    let spec = common::noop_evaluator_spec("noop");

    let verdict = ev
        .evaluate(repo.path(), &spec, &NullAuditSink)
        .expect("evaluate should run without an execution error");

    assert!(
        verdict.is_good(),
        "a trivially-succeeding test_cmd must yield a good verdict"
    );
}

#[test]
fn evaluate_reports_a_bad_verdict_for_a_failing_test_cmd() {
    let repo = common::init_temp_repo();
    let ev = common::evaluator();
    let spec = common::failing_evaluator_spec("fails");

    let verdict = ev
        .evaluate(repo.path(), &spec, &NullAuditSink)
        .expect("evaluate should run without an execution error");

    assert!(
        !verdict.is_good(),
        "a failing test_cmd must not yield a good verdict"
    );
    assert_ne!(verdict.exit_code, 0);
}

#[test]
fn evaluate_short_circuits_on_the_first_failing_setup_cmd() {
    // SPEC.md §20 (F4): the first failing setup command short-circuits the rest — remaining
    // setup_cmds and test_cmd never run.
    let repo = common::init_temp_repo();
    let ev = common::evaluator();
    let marker = repo.path().join("test-cmd-ran.marker");

    let (fail_program, fail_args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/C".to_string(), "exit 7".to_string()],
        )
    } else {
        (
            "sh".to_string(),
            vec!["-c".to_string(), "exit 7".to_string()],
        )
    };
    let (touch_program, touch_args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                format!("echo ran > \"{}\"", marker.display()),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                format!("echo ran > '{}'", marker.display()),
            ],
        )
    };

    let mut spec = common::noop_evaluator_spec("short-circuit");
    spec.setup_cmds = vec![
        Cmd {
            program: fail_program,
            args: fail_args,
            cwd_relative: ".".into(),
        },
        // A second setup_cmd that would also mark the file if reached — proves it's the
        // *first* failure that stops everything, not just "some" failure being ignored.
        Cmd {
            program: touch_program.clone(),
            args: touch_args.clone(),
            cwd_relative: ".".into(),
        },
    ];
    spec.test_cmd = Cmd {
        program: touch_program,
        args: touch_args,
        cwd_relative: ".".into(),
    };

    let verdict = ev
        .evaluate(repo.path(), &spec, &NullAuditSink)
        .expect("evaluate should run without an execution error");

    assert!(
        !verdict.build_succeeded,
        "a failing setup_cmd must mark build_succeeded = false"
    );
    assert!(
        !marker.exists(),
        "neither the remaining setup_cmds nor test_cmd may run once a setup_cmd has failed"
    );
}

#[test]
fn evaluate_is_deterministic_for_an_unchanged_commit() {
    let repo = common::init_temp_repo();
    let ev = common::evaluator();
    let spec = common::noop_evaluator_spec("determinism");

    let a = ev
        .evaluate(repo.path(), &spec, &NullAuditSink)
        .expect("first evaluate");
    let b = ev
        .evaluate(repo.path(), &spec, &NullAuditSink)
        .expect("second evaluate");

    assert_eq!(a.build_succeeded, b.build_succeeded);
    assert_eq!(a.tests_total, b.tests_total);
    assert_eq!(a.tests_passed, b.tests_passed);
    assert_eq!(a.exit_code, b.exit_code);
    // wall_time_secs is explicitly exempt from the determinism contract — SPEC.md §11.
}

#[test]
fn evaluate_extracts_test_counts_via_metric_extractors() {
    let repo = common::init_temp_repo();
    let ev = common::evaluator();

    let (program, args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec!["/C".to_string(), "echo 8 of 10 tests passed".to_string()],
        )
    } else {
        (
            "sh".to_string(),
            vec!["-c".to_string(), "echo '8 of 10 tests passed'".to_string()],
        )
    };

    let mut spec = common::noop_evaluator_spec("extractors");
    spec.test_cmd = Cmd {
        program,
        args,
        cwd_relative: ".".into(),
    };
    spec.metric_extractors = vec![
        agentforge::domain::MetricExtractor {
            name: "tests_passed".to_string(),
            pattern: r"(\d+) of (\d+) tests passed".to_string(),
            capture_group: 1,
        },
        agentforge::domain::MetricExtractor {
            name: "tests_total".to_string(),
            pattern: r"(\d+) of (\d+) tests passed".to_string(),
            capture_group: 2,
        },
    ];

    let verdict = ev
        .evaluate(repo.path(), &spec, &NullAuditSink)
        .expect("evaluate should run without an execution error");

    assert_eq!(verdict.tests_passed, Some(8));
    assert_eq!(verdict.tests_total, Some(10));
}
