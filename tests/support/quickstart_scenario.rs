//! The AgentForge beginner quickstart — shared by `examples/quickstart.rs` (narrated, runnable:
//! `cargo run --example quickstart`) and `tests/quickstart_e2e.rs` (the same walkthrough as a
//! real `cargo test` integration test). Same sharing trick as `demo_scenario.rs`: one
//! implementation, included into both compilation units via `#[path]`, so the walkthrough can
//! never drift from what's tested.
//!
//! Everything here drives the *compiled* `agentforge` binary through its real, documented CLI
//! surface (`std::process::Command`), exactly like `demo_scenario.rs` — no library-level
//! shortcuts, no `FakeAdapter`. Zero paid API: the agent step uses the `claude-code` adapter's
//! own documented `AGENTFORGE_CLAUDE_EXECUTABLE` override point, pointed at the deterministic
//! `src/bin/mock_claude.rs` stand-in (see `AGENTFORGE_MOCK_CLAUDE_FIXTURE=tax`).
//!
//! Scope, deliberately minimal: one tiny repo, one obvious bug, one task, one evaluator, one
//! agent attempt, one patch, one verdict — nothing about fault injection, mutation testing,
//! bisect, policy registration, or manual worktree cleanup belongs here. Those live in the full
//! feature showcase (`cargo run --example demo`). The only thing beyond the core six stages is
//! an explicitly optional "compare two agents in parallel" bonus stage, gated behind
//! `include_race`.

#[path = "narrate.rs"]
mod narrate;

use narrate::Narrator;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct QuickstartOutcome {
    pub run_experiment_id: String,
    /// `Some(..)` only when `include_race` was true.
    pub race_id: Option<String>,
    /// Every narrated line, in order — lets `tests/quickstart_e2e.rs` assert stage order and
    /// glossary content without brittle full-output snapshotting. Unused by
    /// `examples/quickstart.rs`'s own `main` (narration already went to stdout live), so this
    /// compilation unit sees it as dead code even though the test compilation unit reads it.
    #[allow(dead_code)]
    pub transcript: Vec<String>,
}

struct Quickstart {
    agentforge_bin: PathBuf,
    mock_claude_bin: PathBuf,
    repo: PathBuf,
    repo_str: String,
    n: Narrator,
}

pub fn run(
    agentforge_bin: &Path,
    mock_claude_bin: &Path,
    narrate: bool,
    include_race: bool,
) -> QuickstartOutcome {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let repo = tmp.path().to_path_buf();
    let qs = Quickstart {
        agentforge_bin: agentforge_bin.to_path_buf(),
        mock_claude_bin: mock_claude_bin.to_path_buf(),
        repo_str: repo.to_string_lossy().to_string(),
        repo,
        n: Narrator::new(narrate),
    };

    let base_sha = qs.stage1_fixture();
    qs.stage2_register(&base_sha);
    let experiment_id = qs.stage3_run_agent();
    qs.stage4_show_patch(&experiment_id);
    qs.stage5_verdict_and_score(&experiment_id);
    qs.stage6_where_and_next(&experiment_id);

    let race_id = if include_race {
        Some(qs.bonus_compare_in_parallel())
    } else {
        None
    };

    QuickstartOutcome {
        run_experiment_id: experiment_id,
        race_id,
        transcript: qs.n.transcript(),
    }
}

impl Quickstart {
    // ---- process/fixture helpers, mirroring demo_scenario.rs's ------------------------------
    // (no built-in narration here — every user-facing line goes through `self.n` explicitly, so
    // output stays curated rather than a raw dump of every subprocess invocation.)

    fn af(&self, args: &[&str]) -> Output {
        self.af_env(args, &[])
    }

    fn af_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(&self.agentforge_bin);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().expect("spawn agentforge")
    }

    fn af_ok(&self, args: &[&str]) -> String {
        let out = self.af(args);
        assert!(
            out.status.success(),
            "agentforge {:?} failed (exit {:?}): stdout={} stderr={}",
            args,
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn af_ok_with_claude(&self, args: &[&str]) -> String {
        let mock = self.mock_claude_bin.to_string_lossy().into_owned();
        let out = self.af_env(args, &[("AGENTFORGE_CLAUDE_EXECUTABLE", &mock)]);
        assert!(
            out.status.success(),
            "agentforge {:?} failed (exit {:?}): stdout={} stderr={}",
            args,
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn write(&self, rel: &str, contents: &str) {
        let full = self.repo.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&full, contents).expect("write fixture file");
    }

    fn write_tmp(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.repo.join(name);
        std::fs::write(&path, contents).expect("write spec file");
        path
    }

    fn run_git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: stderr={}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_output(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {:?} failed", args);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    // ---- [1/6] the fixture -------------------------------------------------------------------

    fn stage1_fixture(&self) -> String {
        self.n
            .step(1, 6, "The fixture: one tiny repo, one obvious bug");
        self.n.explain(
            "AgentForge always runs against a real git repository. Here's a tiny one - a \
             checkout process for a store - with a single planted bug.",
        );
        self.run_git(&["init", "-q"]);
        self.run_git(&["config", "user.email", "agentforge-quickstart@example.com"]);
        self.run_git(&["config", "user.name", "AgentForge Quickstart"]);
        self.run_git(&["config", "core.autocrlf", "false"]);
        self.write("store/tax.txt", "TAX_RATE=0.99\n");
        self.write("evaluate.sh", TAX_EVALUATE_SH);
        self.write("evaluate.cmd", TAX_EVALUATE_CMD);
        self.run_git(&["add", "."]);
        self.run_git(&["commit", "-q", "-m", "initial: checkout charges 99% tax"]);
        let sha = self.git_output(&["rev-parse", "HEAD"]);

        self.n.result("store/tax.txt currently contains:");
        self.n.result("  TAX_RATE=0.99");
        self.n.explain(
            "Intended behavior: checkout should charge 8% tax, not 99%. That's the bug this \
             quickstart fixes.",
        );
        sha
    }

    // ---- [2/6] register task + evaluator --------------------------------------------------

    fn stage2_register(&self, base_sha: &str) {
        self.n.step(2, 6, "Registering the task and evaluator");
        self.n
            .command(&format!("agentforge init --repo {}", self.repo_str));
        self.n.explain(
            "`agentforge init` sets up a small .agentforge folder inside this repo, plus a \
             separate results folder outside it (more on that in step 6).",
        );
        self.af_ok(&["init", "--repo", &self.repo_str]);

        self.n.term(
            "Evaluator",
            "the deterministic test that decides pass or fail - never the agent's own opinion \
             of its work.",
        );
        self.add_tax_evaluator();

        self.n.term(
            "Task",
            "a registered unit of work: a prompt describing what to fix, which repo/commit to \
             start from, and which Evaluator judges the result.",
        );
        self.n.term(
            "Policy",
            "controls what a spawned process is allowed to do (timeouts, allowed programs, \
             output limits) - this quickstart skips registering one and uses a safe built-in \
             default.",
        );
        self.add_tax_task(base_sha);
    }

    fn add_tax_evaluator(&self) {
        let (program, args) = if cfg!(windows) {
            ("cmd", vec!["/C", "evaluate.cmd"])
        } else {
            ("sh", vec!["evaluate.sh"])
        };
        let toml = format!(
            r#"
id = "tax-check"
setup_cmds = []
timeout_secs = 30
budget_secs = 30
size_budget_lines = 50

[[metric_extractors]]
name = "tests_passed"
pattern = 'tests_passed=(\d+)'
capture_group = 1

[[metric_extractors]]
name = "tests_total"
pattern = 'tests_total=(\d+)'
capture_group = 1

[test_cmd]
program = "{program}"
args = {args:?}
cwd_relative = "."
"#
        );
        let path = self.write_tmp("tax-check.toml", &toml);
        self.af_ok(&[
            "evaluator",
            "add",
            "--repo",
            &self.repo_str,
            &path.to_string_lossy(),
        ]);
    }

    fn add_tax_task(&self, base_sha: &str) {
        let toml = format!(
            r#"
id = "fix-tax-bug"
name = "fix-tax-bug"
prompt = "Fix store/tax.txt: TAX_RATE must be 0.08 (8%), not 0.99 (99%)."
repo_path = "."
base_ref = "{base_sha}"
evaluator = "tax-check"
agent_timeout_secs = 30
created_at = "2026-01-01T00:00:00Z"

[baseline]
build_succeeded = true
exit_code = 1
timed_out = false
wall_time_secs = 0.0
"#
        );
        let path = self.write_tmp("fix-tax-bug.toml", &toml);
        self.af_ok(&[
            "task",
            "add",
            "--repo",
            &self.repo_str,
            &path.to_string_lossy(),
        ]);
        let show = self.af_ok(&["task", "show", "--repo", &self.repo_str, "fix-tax-bug"]);
        // `task add` recaptures the baseline for real against `base_ref` - the still-buggy seed
        // commit - rather than trusting the file's placeholder values.
        assert!(
            show.contains("tests=Some(0)/Some(1)"),
            "expected the recaptured baseline to reflect the still-buggy seed commit: {show}"
        );
    }

    // ---- [3/6] run one agent attempt --------------------------------------------------------

    fn stage3_run_agent(&self) -> String {
        self.n.step(3, 6, "Running one coding agent attempt");
        self.n.term(
            "Adapter",
            "how AgentForge talks to a specific coding-agent CLI - today, only Claude Code is \
             implemented; this quickstart's agent is a small deterministic stand-in, so no paid \
             API is called.",
        );
        self.n.term(
            "Worktree",
            "a disposable, isolated copy of your repository (a real git worktree) where the \
             agent's changes happen - your actual working directory is never touched.",
        );
        self.n.term(
            "Experiment",
            "one agent attempt against a task: a fresh Worktree, a captured patch, an Evaluator \
             verdict, and a Score.",
        );
        self.n
            .command("agentforge run --task fix-tax-bug --agent claude-code:goodfix --json");
        let stdout = self.af_ok_with_claude(&[
            "run",
            "--repo",
            &self.repo_str,
            "--task",
            "fix-tax-bug",
            "--agent",
            "claude-code:goodfix",
            "--json",
        ]);
        let value: serde_json::Value =
            serde_json::from_str(&stdout).expect("run --json is valid JSON");
        assert_eq!(value["status"], "Completed");
        assert_eq!(
            value["score"]["gated"], false,
            "a full fix must not trip the correctness gate: {value:#}"
        );
        let id = value["id"]
            .as_str()
            .expect("experiment json has an id")
            .to_string();
        self.n.result(&format!("Experiment {id} - Completed"));
        id
    }

    // ---- [4/6] show the patch ----------------------------------------------------------------

    fn stage4_show_patch(&self, experiment_id: &str) {
        self.n.step(4, 6, "The patch - before and after");
        self.n.command(&format!(
            "agentforge report show --repo {} --json {experiment_id}",
            self.repo_str
        ));
        let show = self.af_ok(&[
            "report",
            "show",
            "--repo",
            &self.repo_str,
            "--json",
            experiment_id,
        ]);
        let value: serde_json::Value =
            serde_json::from_str(&show).expect("report show --json is valid JSON");
        let patch_path = value["patch_path"].as_str().expect("patch_path");
        let patch = std::fs::read_to_string(patch_path).expect("read patch.diff");
        assert!(patch.contains("-TAX_RATE=0.99"), "{patch}");
        assert!(patch.contains("+TAX_RATE=0.08"), "{patch}");
        self.n.explain("Before:  TAX_RATE=0.99");
        self.n.explain("After:   TAX_RATE=0.08");
        self.n
            .result(&format!("(full unified diff saved at {patch_path})"));
    }

    // ---- [5/6] verdict + score ---------------------------------------------------------------

    fn stage5_verdict_and_score(&self, experiment_id: &str) {
        self.n.step(5, 6, "The verdict and the score");
        self.n.term(
            "Gated result",
            "means the correctness check failed - the patch is scored, but capped low, no \
             matter how efficient or small it looked.",
        );
        self.n.command(&format!(
            "agentforge report show --repo {} {experiment_id}",
            self.repo_str
        ));
        let text = self.af_ok(&["report", "show", "--repo", &self.repo_str, experiment_id]);
        assert!(text.contains("Gated"), "{text}");
        self.n.explain(text.trim_end());
        self.n.explain(
            "The score blends three things: mostly correctness (did the tests pass - 80% of \
             the score), plus a little for efficiency and how small/focused the patch was (10% \
             each). A gated result never scores above 5, regardless of the other two.",
        );
    }

    // ---- [6/6] where it lives + next step ------------------------------------------------

    fn stage6_where_and_next(&self, experiment_id: &str) {
        self.n.step(6, 6, "Where this is recorded, and what's next");
        let show = self.af_ok(&[
            "report",
            "show",
            "--repo",
            &self.repo_str,
            "--json",
            experiment_id,
        ]);
        let value: serde_json::Value =
            serde_json::from_str(&show).expect("report show --json is valid JSON");
        let audit_path = value["audit_log_path"].as_str().expect("audit_log_path");
        self.n.explain(
            "Every experiment's patch and audit trail live outside your repository, in a \
             separate results folder AgentForge manages:",
        );
        self.n.result(audit_path);
        self.n.command(&format!(
            "agentforge report log --repo {} {experiment_id}",
            self.repo_str
        ));
        self.af_ok(&["report", "log", "--repo", &self.repo_str, experiment_id]);

        self.n.recap(&[
            "Registered one task and one evaluator against a tiny fixture repo.",
            "Ran one agent attempt in an isolated worktree - your repo was never touched.",
            "Reviewed the patch it produced, and its verdict/score.",
            "Every artifact is saved outside your repo for later inspection.",
        ]);
        self.n.explain("Next step - try it on a real repository:");
        self.n
            .result("see docs/USAGE.md, starting at \"First five minutes\".");
        self.n
            .explain("Optional - see two agents compared side by side:");
        self.n.result("cargo run --example quickstart -- --compare");
    }

    // ---- bonus: compare two agents in parallel (explicitly optional, not part of the 6 core
    //      stages) --------------------------------------------------------------------------

    fn bonus_compare_in_parallel(&self) -> String {
        self.n.section("Optional: compare two agents in parallel");
        self.n.explain(
            "This uses the same `race` command a real comparison would - two candidates here, \
             both this quickstart's deterministic stand-in (one fixes the bug, one doesn't), so \
             you can see how ranking works with zero cost.",
        );
        self.n.command(
            "agentforge race --task fix-tax-bug --agents claude-code:goodfix,claude-code:nofix \
             --max-parallel 2 --json",
        );
        let stdout = self.af_ok_with_claude(&[
            "race",
            "--repo",
            &self.repo_str,
            "--task",
            "fix-tax-bug",
            "--agents",
            "claude-code:goodfix,claude-code:nofix",
            "--max-parallel",
            "2",
            "--json",
        ]);
        let value: serde_json::Value =
            serde_json::from_str(&stdout).expect("race --json is valid JSON");
        let leaderboard = value["leaderboard"]
            .as_array()
            .expect("leaderboard array")
            .clone();
        let models: Vec<&str> = leaderboard
            .iter()
            .map(|entry| entry["agent_config"]["model"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            models,
            vec!["goodfix", "nofix"],
            "ranking must be goodfix > nofix by score.total desc: {leaderboard:#?}"
        );
        assert_eq!(leaderboard[0]["score"]["gated"], false, "{leaderboard:#?}");
        assert_eq!(
            leaderboard[1]["score"]["gated"], true,
            "the candidate that never fixed the tax rate must trip the gate: {leaderboard:#?}"
        );
        let race_id = value["id"].as_str().unwrap().to_string();
        self.n.result(
            "#0 claude-code:goodfix - not gated (fixed it)    #1 claude-code:nofix - gated (didn't fix it)",
        );
        self.n.explain(
            "Only Claude Code is a real, implemented adapter today - the AgentAdapter trait \
             exists for more (src/adapter/mod.rs's resolve function is the extension point), \
             none else are wired up yet.",
        );
        self.n.explain(
            "Against a real agent, every extra candidate or --repeat is a separate API call - \
             --max-parallel bounds how many run at once, not how many total calls happen.",
        );
        race_id
    }
}

const TAX_EVALUATE_SH: &str = r#"#!/bin/sh
passed=0
grep -q "TAX_RATE=0.08" store/tax.txt && passed=1
echo "tests_passed=$passed"
echo "tests_total=1"
grep -q "TAX_RATE=0.08" store/tax.txt
exit $?
"#;

const TAX_EVALUATE_CMD: &str = r#"@echo off
setlocal enabledelayedexpansion
set /a passed=0
findstr /C:"TAX_RATE=0.08" store\tax.txt >nul
if !errorlevel! EQU 0 set /a passed+=1
echo tests_passed=!passed!
echo tests_total=1
findstr /C:"TAX_RATE=0.08" store\tax.txt >nul
exit /b !errorlevel!
"#;
