//! Score calculation, rating thresholds, and correctness dominating superficial metrics —
//! SPEC.md §15, §20 (S1), docs/ARCHITECTURE.md §11.
//!
//! `scoring::score` is a pure function — these tests construct `RawMetrics`/`EvaluatorVerdict`
//! fixtures directly, no filesystem or process involved.

mod common;

use agentforge::domain::{DiffStats, Rating, RatingBands, RawMetrics, ScoringWeights};
use agentforge::scoring::{rating_for, score};

fn weights() -> ScoringWeights {
    ScoringWeights {
        correctness: 80.0,
        efficiency: 10.0,
        parsimony: 10.0,
        rating_bands: RatingBands {
            excellent: 90,
            good: 70,
            fair: 45,
            poor: 20,
        },
        formula_version: "v1".to_string(),
        source: "built-in-default".to_string(),
    }
}

fn metrics(verdict: agentforge::domain::EvaluatorVerdict, diff: DiffStats) -> RawMetrics {
    RawMetrics {
        verdict,
        diff,
        agent_timed_out: false,
    }
}

#[test]
fn rating_bands_match_the_documented_thresholds() {
    let bands = RatingBands {
        excellent: 90,
        good: 70,
        fair: 45,
        poor: 20,
    };
    let cases = [
        (100u8, Rating::Excellent),
        (90u8, Rating::Excellent),
        (89u8, Rating::Good),
        (70u8, Rating::Good),
        (69u8, Rating::Fair),
        (45u8, Rating::Fair),
        (44u8, Rating::Poor),
        (20u8, Rating::Poor),
        (19u8, Rating::Fail),
        (0u8, Rating::Fail),
    ];
    for (total, expected) in cases {
        assert_eq!(
            rating_for(total, &bands),
            expected,
            "total={total} should map to {expected:?} per SPEC.md §15's band table"
        );
    }
}

#[test]
fn a_fast_small_fully_passing_patch_scores_in_the_excellent_band() {
    let m = metrics(
        common::good_verdict(),
        DiffStats {
            files_changed: 1,
            lines_added: 5,
            lines_removed: 1,
        },
    );
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &weights(),
    );
    assert!(
        card.total >= 90,
        "an ideal patch should land in the Excellent band, got {}",
        card.total
    );
    assert_eq!(card.rating, Rating::Excellent);
    assert!(!card.gated, "an ideal patch must not be gated");
}

#[test]
fn build_failure_gates_the_score_to_at_most_five() {
    let mut v = common::good_verdict();
    v.build_succeeded = false;
    let m = metrics(v, DiffStats::default());
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &weights(),
    );
    assert!(
        card.gated,
        "a build failure must set gated = true — SPEC.md §15"
    );
    assert!(
        card.total <= 5,
        "a build failure must hard-cap the total at 5, got {}",
        card.total
    );
    assert_eq!(card.rating, Rating::Fail);
}

#[test]
fn evaluator_timeout_gates_the_score_to_at_most_five() {
    let mut v = common::good_verdict();
    v.timed_out = true;
    let m = metrics(v, DiffStats::default());
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &weights(),
    );
    assert!(card.gated);
    assert!(card.total <= 5);
}

#[test]
fn agent_timeout_gates_the_score_to_at_most_five() {
    let m = RawMetrics {
        verdict: common::good_verdict(),
        diff: DiffStats::default(),
        agent_timed_out: true,
    };
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &weights(),
    );
    assert!(
        card.gated,
        "the agent process itself timing out must gate the score"
    );
    assert!(card.total <= 5);
}

#[test]
fn nonzero_exit_code_gates_the_score_even_with_a_perfect_test_ratio() {
    // SPEC.md §20 (S1): v1's gate only checked build_succeeded; v2 checks exit_code
    // unconditionally so a crash-after-printing-a-summary can't buy a high score just because
    // the extracted test ratio happens to look perfect.
    let mut v = common::good_verdict();
    v.exit_code = 1; // build succeeded, tests_passed == tests_total, but the runner exited nonzero
    let m = metrics(v, DiffStats::default());
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &weights(),
    );
    assert!(
        card.gated,
        "a nonzero evaluator exit code must gate the score even when the test ratio looks perfect"
    );
    assert!(card.total <= 5);
}

#[test]
fn a_shrinking_test_count_gates_the_score_even_when_the_ratio_looks_perfect() {
    // The single most important test in this file: SPEC.md §15's fix for the "delete the
    // failing test" gaming path. A patched verdict of tests_passed == tests_total == 9 looks
    // perfect in isolation, but the task's baseline had 10 tests — a real test disappeared.
    let mut baseline = common::good_verdict();
    baseline.tests_total = Some(10);
    baseline.tests_passed = Some(9);

    let mut patched = common::good_verdict();
    patched.tests_total = Some(9);
    patched.tests_passed = Some(9); // "100%" — but one whole test vanished

    let m = metrics(
        patched,
        DiffStats {
            files_changed: 1,
            lines_added: 1,
            lines_removed: 4,
        },
    );
    let card = score(&m, &baseline, &common::noop_evaluator_spec("e"), &weights());

    assert!(
        card.gated,
        "deleting a test must be caught by the baseline test-count regression check, not rewarded"
    );
    assert!(card.total <= 5);
}

#[test]
fn a_growing_or_stable_test_count_does_not_trigger_the_regression_gate() {
    let mut baseline = common::good_verdict();
    baseline.tests_total = Some(10);
    baseline.tests_passed = Some(10);

    let mut patched = common::good_verdict();
    patched.tests_total = Some(11); // agent added a test
    patched.tests_passed = Some(11);

    let m = metrics(
        patched,
        DiffStats {
            files_changed: 2,
            lines_added: 10,
            lines_removed: 0,
        },
    );
    let card = score(&m, &baseline, &common::noop_evaluator_spec("e"), &weights());

    assert!(
        !card.gated,
        "a stable-or-growing test count must not be treated as a regression"
    );
}

#[test]
fn gamed_patch_cannot_outscore_a_genuinely_correct_but_slower_larger_patch() {
    // "Correctness dominating superficial metrics": a slow, large, but genuinely correct patch
    // must outscore a fast, tiny, gamed one — this is the structural guarantee SPEC.md §15's
    // hard cap exists to provide.
    let mut baseline = common::good_verdict();
    baseline.tests_total = Some(10);
    baseline.tests_passed = Some(10);

    let mut honest_verdict = common::good_verdict();
    honest_verdict.wall_time_secs = 55.0; // near the evaluator's 60s budget — genuinely slow
    let honest = metrics(
        honest_verdict,
        DiffStats {
            files_changed: 12,
            lines_added: 400,
            lines_removed: 120,
        }, // a large, sprawling — but fully correct — patch
    );

    let mut gamed_verdict = common::good_verdict();
    gamed_verdict.tests_total = Some(9);
    gamed_verdict.tests_passed = Some(9); // the failing test was deleted, not fixed
    let gamed = metrics(
        gamed_verdict,
        DiffStats {
            files_changed: 1,
            lines_added: 1,
            lines_removed: 3,
        }, // fast, tiny — and gamed
    );

    let spec = common::noop_evaluator_spec("e");
    let honest_card = score(&honest, &baseline, &spec, &weights());
    let gamed_card = score(&gamed, &baseline, &spec, &weights());

    assert!(
        honest_card.total > gamed_card.total,
        "an honest, slow, large patch ({}) must outscore a fast, tiny, gamed one ({})",
        honest_card.total,
        gamed_card.total
    );
    assert!(!honest_card.gated);
    assert!(gamed_card.gated);
}

#[test]
fn efficiency_normalizes_to_zero_not_negative_when_wall_time_exceeds_budget() {
    // budget_secs is 60 (see weights()/noop_evaluator_spec); a verdict that took far longer must
    // clamp to 0.0, not go negative and silently drag the total below what the cap already
    // guarantees.
    let mut v = common::good_verdict();
    v.wall_time_secs = 600.0;
    let m = metrics(v, DiffStats::default());
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &weights(),
    );
    let efficiency = card
        .components
        .iter()
        .find(|c| c.name == "efficiency")
        .expect("efficiency component present");
    assert_eq!(efficiency.normalized, 0.0);
    assert_eq!(efficiency.contribution, 0.0);
}

#[test]
fn parsimony_normalizes_to_zero_not_negative_when_diff_exceeds_budget() {
    // size_budget_lines is 200; a diff far larger must clamp to 0.0, not negative.
    let m = metrics(
        common::good_verdict(),
        DiffStats {
            files_changed: 50,
            lines_added: 5000,
            lines_removed: 5000,
        },
    );
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &weights(),
    );
    let parsimony = card
        .components
        .iter()
        .find(|c| c.name == "parsimony")
        .expect("parsimony component present");
    assert_eq!(parsimony.normalized, 0.0);
    assert_eq!(parsimony.contribution, 0.0);
}

#[test]
fn missing_test_counts_on_an_otherwise_good_verdict_scores_full_correctness_and_is_not_gated() {
    // SPEC.md §15: "else 1.0 (verdict is good by construction when not gated and counts are
    // absent)". An evaluator with no metric_extractors never populates tests_total/tests_passed.
    let mut v = common::good_verdict();
    v.tests_total = None;
    v.tests_passed = None;
    let mut baseline = common::good_verdict();
    baseline.tests_total = None;
    baseline.tests_passed = None;

    let m = metrics(v, DiffStats::default());
    let card = score(&m, &baseline, &common::noop_evaluator_spec("e"), &weights());

    assert!(!card.gated);
    let correctness = card
        .components
        .iter()
        .find(|c| c.name == "correctness")
        .expect("correctness component present");
    assert_eq!(correctness.normalized, 1.0);
}

#[test]
fn baseline_missing_test_counts_never_triggers_the_regression_gate() {
    // The regression check is explicitly skipped when either side lacks a count — it can't
    // compare what it doesn't have — but every other gate condition still applies (here, none
    // do, so this must not be gated).
    let mut baseline = common::good_verdict();
    baseline.tests_total = None;
    baseline.tests_passed = None;

    let mut patched = common::good_verdict();
    patched.tests_total = Some(3);
    patched.tests_passed = Some(3);

    let m = metrics(patched, DiffStats::default());
    let card = score(&m, &baseline, &common::noop_evaluator_spec("e"), &weights());

    assert!(!card.gated);
}

#[test]
fn total_is_clamped_to_one_hundred_even_with_a_generous_custom_weighting() {
    let mut w = weights();
    w.correctness = 90.0;
    w.efficiency = 90.0;
    w.parsimony = 90.0; // deliberately sums past 100 to exercise the clamp
    let m = metrics(
        common::good_verdict(),
        DiffStats {
            files_changed: 1,
            lines_added: 1,
            lines_removed: 0,
        },
    );
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &w,
    );
    assert!(card.total <= 100);
}

#[test]
fn each_component_reports_whether_higher_or_lower_raw_values_are_better() {
    let m = metrics(common::good_verdict(), DiffStats::default());
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &weights(),
    );
    let by_name = |name: &str| {
        card.components
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name} component present"))
    };
    assert!(
        by_name("correctness").higher_is_better,
        "a higher test-pass ratio is better"
    );
    assert!(
        !by_name("efficiency").higher_is_better,
        "a lower wall-clock time is better"
    );
    assert!(
        !by_name("parsimony").higher_is_better,
        "a smaller diff is better"
    );
}

#[test]
fn score_card_records_the_weights_source_and_formula_version_for_reproducibility() {
    let m = metrics(common::good_verdict(), DiffStats::default());
    let w = weights();
    let card = score(
        &m,
        &common::good_verdict(),
        &common::noop_evaluator_spec("e"),
        &w,
    );
    assert_eq!(
        card.formula_version, w.formula_version,
        "the ScoreCard must record which formula version produced it — SPEC.md §15"
    );
    assert_eq!(
        card.weights_source, w.source,
        "the ScoreCard must record where the weights came from, so `score --weights alt.toml` \
         is auditable after the fact"
    );
}
