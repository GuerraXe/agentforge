//! Pure function, no infrastructure — SPEC.md §15, docs/ARCHITECTURE.md §11.
//!
//! No struct, no trait, no dependencies beyond `domain`: recomputable from persisted data with
//! zero I/O and zero process spawns.

use crate::domain::Rating;
use crate::domain::{
    EvaluatorSpec, EvaluatorVerdict, RatingBands, RawMetrics, ScoreCard, ScoreComponent,
    ScoringWeights,
};

/// Total is hard-capped here when `gated` is true — SPEC.md §15, §20 (S1).
const GATED_TOTAL_CAP: u8 = 5;

/// The scoring formula's own version identifier — distinct from SPEC.md's document version.
/// Stamped onto `default_weights()` and compared against a `--weights` file's own
/// `formula_version` by the `score` command (SPEC.md §14: "prints a warning, never silent").
pub const FORMULA_VERSION: &str = "v1";

/// The SPEC.md §14 built-in weights/bands, used whenever no `scoring.toml`/`--weights` override
/// is given. `source` is always `"built-in-default"` — never a path, since nothing was read.
pub fn default_weights() -> ScoringWeights {
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
        formula_version: FORMULA_VERSION.to_string(),
        source: "built-in-default".to_string(),
    }
}

/// Every correctness-gate condition currently true, in SPEC.md §15's bulleted order, as a
/// human-readable reason — the transparent backing for `is_gated` (`!failed_checks(..).is_empty()`)
/// and for `report`'s "failed checks" section, so gating and its explanation can never drift
/// apart into two separately-maintained conditions.
pub fn failed_checks(metrics: &RawMetrics, baseline: &EvaluatorVerdict) -> Vec<String> {
    let verdict = &metrics.verdict;
    let mut reasons = Vec::new();
    if !verdict.build_succeeded {
        reasons.push("build did not succeed".to_string());
    }
    if verdict.timed_out {
        reasons.push("evaluator's test command timed out".to_string());
    }
    if metrics.agent_timed_out {
        reasons.push("agent process was killed for exceeding its timeout".to_string());
    }
    if verdict.exit_code != 0 {
        reasons.push(format!(
            "test command exited non-zero (exit code {})",
            verdict.exit_code
        ));
    }
    if let (Some(baseline_total), Some(verdict_total)) = (baseline.tests_total, verdict.tests_total)
    {
        if verdict_total < baseline_total {
            reasons.push(format!(
                "test count regressed vs baseline ({verdict_total} < {baseline_total})"
            ));
        }
    }
    reasons
}

/// True if any correctness-gate condition holds — SPEC.md §15's bulleted list, checked
/// unconditionally (not only as a fallback when test counts are absent, per §20 S1).
fn is_gated(metrics: &RawMetrics, baseline: &EvaluatorVerdict) -> bool {
    !failed_checks(metrics, baseline).is_empty()
}

/// The correctness ratio the verdict would earn if it were not gated — `tests_passed /
/// tests_total` when the evaluator reports counts, else `1.0` (a good verdict by construction
/// when there's nothing to gate on and no counts to compare). Kept separate from gating so the
/// raw measurement stays visible even on a gated `ScoreCard` (SPEC.md §15's transparency goal).
fn correctness_ratio(verdict: &EvaluatorVerdict) -> f64 {
    match (verdict.tests_passed, verdict.tests_total) {
        (Some(passed), Some(total)) if total > 0 => (passed as f64 / total as f64).clamp(0.0, 1.0),
        _ => 1.0,
    }
}

fn diff_lines_changed(diff: &crate::domain::DiffStats) -> u32 {
    diff.lines_added + diff.lines_removed
}

/// Compute a `ScoreCard` from persisted `RawMetrics` — SPEC.md §15. Correctness dominates by
/// default weighting (80/10/10) and by the gate below, which hard-caps `total` at
/// `GATED_TOTAL_CAP` regardless of how favorable efficiency/parsimony look, so a fast, tiny,
/// gamed patch can never outrank a slow, large, genuinely correct one.
pub fn score(
    metrics: &RawMetrics,
    baseline: &EvaluatorVerdict,
    evaluator_spec: &EvaluatorSpec,
    weights: &ScoringWeights,
) -> ScoreCard {
    let gated = is_gated(metrics, baseline);

    let correctness_raw = correctness_ratio(&metrics.verdict);
    let correctness_normalized = if gated { 0.0 } else { correctness_raw };

    let efficiency_raw = metrics.verdict.wall_time_secs;
    let efficiency_normalized =
        (1.0 - efficiency_raw / evaluator_spec.budget_secs as f64).clamp(0.0, 1.0);

    let parsimony_raw = diff_lines_changed(&metrics.diff) as f64;
    let parsimony_normalized =
        (1.0 - parsimony_raw / evaluator_spec.size_budget_lines as f64).clamp(0.0, 1.0);

    let components = vec![
        component(
            "correctness",
            correctness_raw,
            correctness_normalized,
            weights.correctness,
            true,
        ),
        component(
            "efficiency",
            efficiency_raw,
            efficiency_normalized,
            weights.efficiency,
            false,
        ),
        component(
            "parsimony",
            parsimony_raw,
            parsimony_normalized,
            weights.parsimony,
            false,
        ),
    ];

    let raw_total: f64 = components.iter().map(|c| c.contribution).sum();
    let mut total = raw_total.round().clamp(0.0, 100.0) as u8;
    if gated {
        total = total.min(GATED_TOTAL_CAP);
    }

    ScoreCard {
        total,
        rating: rating_for(total, &weights.rating_bands),
        gated,
        components,
        weights_source: weights.source.clone(),
        formula_version: weights.formula_version.clone(),
    }
}

fn component(
    name: &str,
    raw: f64,
    normalized: f64,
    weight: f64,
    higher_is_better: bool,
) -> ScoreComponent {
    ScoreComponent {
        name: name.to_string(),
        raw,
        normalized,
        weight,
        contribution: weight * normalized,
        higher_is_better,
    }
}

/// Maps a total (0-100) to a `Rating` per `bands` — SPEC.md §15's band table. Exposed
/// separately from `score()` so rating-threshold behavior is testable in isolation from the
/// rest of the scoring formula.
pub fn rating_for(total: u8, bands: &RatingBands) -> Rating {
    if total >= bands.excellent {
        Rating::Excellent
    } else if total >= bands.good {
        Rating::Good
    } else if total >= bands.fair {
        Rating::Fair
    } else if total >= bands.poor {
        Rating::Poor
    } else {
        Rating::Fail
    }
}
