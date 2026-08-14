//! Runnable, narrated walkthrough of the entire AgentForge product against one small fixture
//! repo — `cargo run --example demo`. Builds `agentforge` and its demo-only `mock_claude`
//! stand-in agent first, then drives the real, compiled `agentforge` binary through its
//! documented CLI surface end to end: `init`, isolated workspaces, evaluator/task/policy
//! registration, standalone fault injection and mutation testing, a single `run`, a
//! three-candidate `race` with real scoring/ranking, a standalone `verify`, semantic `bisect`,
//! human-readable and `--json` reporting, and `clean`. Zero paid API — see
//! `src/bin/mock_claude.rs` for how the one agent-shaped step is satisfied locally.
//!
//! See `tests/support/demo_scenario.rs` for the shared, asserted implementation — the exact same
//! walkthrough is also wired in as `cargo test --test demo_e2e`.

#[path = "../tests/support/demo_scenario.rs"]
mod demo_scenario;

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let target_dir = target_profile_dir();
    let profile = target_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    println!("Building agentforge + mock_claude ({profile} profile)...");
    let mut build = Command::new("cargo");
    build.args(["build", "--bin", "agentforge", "--bin", "mock_claude"]);
    if profile == "release" {
        build.arg("--release");
    }
    let status = build.status().expect("run `cargo build`");
    assert!(status.success(), "cargo build failed");

    let agentforge_bin = target_dir.join(exe_name("agentforge"));
    let mock_claude_bin = target_dir.join(exe_name("mock_claude"));

    let outcome = demo_scenario::run(&agentforge_bin, &mock_claude_bin, true);

    println!(
        "\nDemo complete — every AgentForge command above ran for real, locally, with zero paid API."
    );
    println!("  run:    {}", outcome.run_experiment_id);
    println!(
        "  race:   {} ({} candidates ranked)",
        outcome.race_id,
        outcome.race_leaderboard.len()
    );
    println!(
        "  bisect: {} (culprit {})",
        outcome.bisect_id, outcome.bisect_culprit
    );
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// The `target/<profile>/` directory this example's own binary was built into — robust to
/// debug/release and to whatever profile invoked `cargo run --example demo`, since it's derived
/// from this process's own real location rather than guessed.
fn target_profile_dir() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop(); // this example's own binary file name
    if path.ends_with("examples") {
        path.pop();
    }
    path
}
