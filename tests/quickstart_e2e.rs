//! The AgentForge beginner quickstart, wired in as a real `cargo test` integration test — see
//! `tests/support/quickstart_scenario.rs` for the full, narrated walkthrough (also runnable
//! directly via `cargo run --example quickstart`). Zero paid API: the only "agent" involved is
//! `src/bin/mock_claude.rs`, a deterministic stand-in substituted through the `claude-code`
//! adapter's own documented `AGENTFORGE_CLAUDE_EXECUTABLE` override.
//!
//! Run with `cargo test --test quickstart_e2e -- --nocapture` to see the full narration.

#[path = "support/quickstart_scenario.rs"]
mod quickstart_scenario;

use std::path::Path;

#[test]
fn quickstart_runs_end_to_end_single_agent_with_zero_paid_api() {
    let outcome = quickstart_scenario::run(
        Path::new(env!("CARGO_BIN_EXE_agentforge")),
        Path::new(env!("CARGO_BIN_EXE_mock_claude")),
        true,
        false,
    );

    assert!(!outcome.run_experiment_id.is_empty());
    assert!(outcome.race_id.is_none());
    assert_stages_in_order(&outcome.transcript);
    assert_glossary_terms_explained(&outcome.transcript);
    assert_no_raw_ansi_when_not_a_tty(&outcome.transcript);
}

#[test]
fn quickstart_optional_parallel_comparison_is_deterministic() {
    // `quickstart_scenario::bonus_compare_in_parallel` already asserts the ranking/gating
    // contrast (goodfix not gated, nofix gated) internally — this is a top-level sanity check
    // that the optional step actually ran and produced a race.
    let outcome = quickstart_scenario::run(
        Path::new(env!("CARGO_BIN_EXE_agentforge")),
        Path::new(env!("CARGO_BIN_EXE_mock_claude")),
        true,
        true,
    );

    assert!(!outcome.run_experiment_id.is_empty());
    assert!(outcome.race_id.is_some());
}

fn assert_stages_in_order(transcript: &[String]) {
    let idx = |needle: &str| {
        transcript
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| {
                panic!("missing stage marker {needle:?} in transcript: {transcript:#?}")
            })
    };
    let stages = ["[1/6]", "[2/6]", "[3/6]", "[4/6]", "[5/6]", "[6/6]"];
    let positions: Vec<usize> = stages.iter().map(|s| idx(s)).collect();
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "stages out of order: {positions:?}"
    );
}

fn assert_glossary_terms_explained(transcript: &[String]) {
    for (term, keyword) in [
        ("Task", "prompt"),
        ("Evaluator", "pass or fail"),
        ("Adapter", "coding-agent CLI"),
        ("Worktree", "isolated"),
        ("Experiment", "Score"),
        ("Policy", "built-in default"),
        ("Gated result", "capped low"),
    ] {
        assert!(
            transcript
                .iter()
                .any(|l| l.contains(term) && l.contains(keyword)),
            "missing explanation of {term:?} (expected keyword {keyword:?}) in transcript: {transcript:#?}"
        );
    }
}

fn assert_no_raw_ansi_when_not_a_tty(transcript: &[String]) {
    // `cargo test`'s stdout is captured (not a real tty), so color must be off end to end —
    // this is what "output remains useful when stdout is not a TTY" means in practice.
    assert!(
        transcript.iter().all(|l| !l.contains('\u{1b}')),
        "raw ANSI escape leaked into non-tty output: {transcript:#?}"
    );
}
