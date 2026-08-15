//! Narrated, runnable walkthrough of AgentForge's core workflow against one tiny fixture repo —
//! `cargo run --example quickstart`. The fastest way to see what AgentForge actually does: one
//! obvious bug, one agent attempt, one verdict, in under a minute. Zero paid API — see
//! `src/bin/mock_claude.rs`.
//!
//! Pass `--compare` to also run the optional "compare two agents in parallel" step:
//! `cargo run --example quickstart -- --compare`.
//!
//! For the full command surface (fault injection, mutation testing, races, semantic bisect,
//! policies, workspaces, cleanup), see the full feature showcase instead: `cargo run --example
//! demo`.
//!
//! See `tests/support/quickstart_scenario.rs` for the shared, asserted implementation — the same
//! walkthrough is also wired in as `cargo test --test quickstart_e2e`.

#[path = "../tests/support/quickstart_scenario.rs"]
mod quickstart_scenario;

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let include_race = std::env::args().any(|a| a == "--compare");
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

    let outcome = quickstart_scenario::run(&agentforge_bin, &mock_claude_bin, true, include_race);

    println!("\nQuickstart complete.");
    println!("  run:  {}", outcome.run_experiment_id);
    if let Some(race_id) = &outcome.race_id {
        println!("  race: {race_id}");
    }
    println!("\nNext: try it on a real repository — see docs/USAGE.md.");
    if outcome.race_id.is_none() {
        println!(
            "Or see two agents compared side by side: cargo run --example quickstart -- --compare"
        );
    }
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// The `target/<profile>/` directory this example's own binary was built into — robust to
/// debug/release and to whatever profile invoked `cargo run --example quickstart`, since it's
/// derived from this process's own real location rather than guessed.
fn target_profile_dir() -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop(); // this example's own binary file name
    if path.ends_with("examples") {
        path.pop();
    }
    path
}
