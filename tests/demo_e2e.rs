//! The AgentForge full feature showcase, wired in as a real `cargo test` integration test — see
//! `tests/support/demo_scenario.rs` for the full, narrated walkthrough (also runnable directly
//! via `cargo run --example demo`). Zero paid API: the only "agent" involved is
//! `src/bin/mock_claude.rs`, a deterministic stand-in substituted through the `claude-code`
//! adapter's own documented `AGENTFORGE_CLAUDE_EXECUTABLE` override — `adapter::resolve` itself
//! is untouched and still only ever resolves the real `"claude-code"` adapter.
//!
//! New to AgentForge? See `tests/quickstart_e2e.rs` / `cargo run --example quickstart` instead —
//! this is the advanced, comprehensive walkthrough.
//!
//! Run with `cargo test --test demo_e2e -- --nocapture` to see the full narration.

#[path = "support/demo_scenario.rs"]
mod demo_scenario;

use std::path::Path;

#[test]
fn full_local_demo_runs_end_to_end_with_zero_paid_api() {
    let outcome = demo_scenario::run(
        Path::new(env!("CARGO_BIN_EXE_agentforge")),
        Path::new(env!("CARGO_BIN_EXE_mock_claude")),
        true,
    );

    // `demo_scenario::run` already asserts thoroughly at every step (ranking order, gating,
    // recaptured baselines, restore/discard round trips, etc.) — these are a final top-level
    // sanity check that every headline artifact the demo promises actually came back populated.
    assert!(!outcome.run_experiment_id.is_empty());
    assert!(!outcome.race_id.is_empty());
    assert_eq!(outcome.race_leaderboard.len(), 3);
    assert!(!outcome.bisect_id.is_empty());
    assert!(!outcome.bisect_culprit.is_empty());
}
