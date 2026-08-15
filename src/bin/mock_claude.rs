//! Deterministic mock "coding agent" — demo/test-only, never a real product feature.
//!
//! Stands in for the real `claude` executable via `AGENTFORGE_CLAUDE_EXECUTABLE`
//! (`src/adapter/claude_code.rs`'s own documented override point), so `examples/demo.rs` and
//! `tests/demo_e2e.rs` can exercise the real `agentforge run`/`race` commands end to end with
//! zero paid API and zero changes to `adapter::resolve` — which still only ever resolves the
//! real `"claude-code"` adapter (SPEC.md §9/§18: `FakeAdapter` stays "constructed directly by
//! tests, not resolved by name"; this binary doesn't touch that boundary at all, it just
//! impersonates the *executable* the already-real adapter shells out to, exactly the way a CI
//! pipeline might swap in a stub `claude` for a dry run).
//!
//! Accepts (and mostly ignores) the real `ClaudeCodeAdapter::command_for`-built argv shape
//! (`-p <prompt> --output-format json --model <variant>`). Based solely on `--model`, it
//! deterministically edits a tracked fixture file in its current working directory (always the
//! isolated experiment worktree — never adapter-suppliable, per SPEC.md §8) to one of a few fixed
//! "fix quality" outcomes, then exits 0. Never touches anything outside its cwd.
//!
//! Which fixture file to edit is detected from the worktree's own contents, not an env var — a
//! fresh `PermissionPolicy` defaults `env_passthrough` to empty (fail-closed), so an env var set
//! on the `agentforge` process itself never reaches this spawned child; only files actually
//! present in the isolated worktree are visible here. `examples/demo.rs`'s fixture tracks
//! `billing/discount.txt`; `examples/quickstart.rs`'s tracks `store/tax.txt` — whichever exists
//! selects the behavior below.
//!
//! `AGENTFORGE_MOCK_CLAUDE_SLEEP_SECS`, if set, sleeps that many seconds before doing anything
//! else — the one hook that lets a test exercise the real `TimedOut` path (SPEC.md §17/§18 row
//! 25) through the actual compiled binary and `Executor`, not a library-level substitute.

use std::env;
use std::time::Duration;

fn main() {
    if let Ok(secs) = env::var("AGENTFORGE_MOCK_CLAUDE_SLEEP_SECS") {
        let secs: u64 = secs
            .parse()
            .expect("AGENTFORGE_MOCK_CLAUDE_SLEEP_SECS must be a u64");
        std::thread::sleep(Duration::from_secs(secs));
    }

    let args: Vec<String> = env::args().collect();
    let mut model = String::new();
    let mut prompt = String::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--model" if i + 1 < args.len() => {
                model = args[i + 1].clone();
                i += 2;
            }
            "-p" if i + 1 < args.len() => {
                prompt = args[i + 1].clone();
                i += 2;
            }
            _ => i += 1,
        }
    }

    // "nofix" and any unrecognized/absent variant deliberately leave the bug in place — a
    // candidate that made no real change is exactly as valid a demo outcome as a good one.
    let (target_path, new_contents): (&str, Option<&str>) =
        if std::path::Path::new("store/tax.txt").exists() {
            (
                "store/tax.txt",
                match model.as_str() {
                    "goodfix" => Some("TAX_RATE=0.08\n"),
                    _ => None,
                },
            )
        } else {
            (
                "billing/discount.txt",
                match model.as_str() {
                    "goodfix" => Some("DISCOUNT_RATE=0.10\nLOGGING=enabled\n"),
                    "partialfix" => Some("DISCOUNT_RATE=0.10\nLOGGING=disabled\n"),
                    _ => None,
                },
            )
        };

    if let Some(contents) = new_contents {
        std::fs::write(target_path, contents)
            .unwrap_or_else(|e| panic!("mock_claude: write {target_path}: {e}"));
    }

    println!(
        "{{\"mock_claude\":true,\"variant\":{:?},\"prompt\":{:?},\"applied_fix\":{}}}",
        model,
        prompt,
        new_contents.is_some()
    );
}
