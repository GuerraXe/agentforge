//! The AgentForge local end-to-end demo — shared by `examples/demo.rs` (a narrated, runnable
//! walkthrough: `cargo run --example demo`) and `tests/demo_e2e.rs` (the same walkthrough as a
//! real `cargo test` integration test). One implementation, included into both compilation units
//! via `#[path]`, so the demo can never drift from what the test actually verifies.
//!
//! Everything here drives the *compiled* `agentforge` binary through its real, documented CLI
//! surface (`std::process::Command`, exactly like `tests/cli_*.rs` already do) — no library-level
//! shortcuts, no `FakeAdapter`. The one substitution is the `claude-code` adapter's own
//! documented `AGENTFORGE_CLAUDE_EXECUTABLE` override point (see `src/bin/mock_claude.rs`),
//! which lets `run`/`race` exercise a real (if scripted) agent process with zero paid API and
//! zero change to `adapter::resolve`'s "only `claude-code` is resolvable by name" boundary.
//!
//! Demonstrates every piece SPEC.md's product contract describes, end to end, against one small
//! fixture repo (a "billing" service with a known discount-rate bug): `init`, isolated
//! `workspace`s, `evaluator`/`task`/`policy` registration, standalone repository fault injection
//! (`experiment fault`), standalone source mutation testing (`experiment mutant`), a single `run`,
//! a three-candidate `race` with real scoring/ranking, a standalone `verify`, semantic `bisect`
//! over a scripted regression, human-readable and `--json` `report` output, and `clean`.

#[path = "narrate.rs"]
mod narrate;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct DemoOutcome {
    pub run_experiment_id: String,
    pub race_id: String,
    pub race_leaderboard: Vec<serde_json::Value>,
    pub bisect_id: String,
    /// Already checked against the fixture's known-scripted-flip commit inside
    /// `bisect_walkthrough` itself (`assert_eq!(culprit, expected_culprit)`) — carried here only
    /// so callers can display it, not as a second correctness check.
    pub bisect_culprit: String,
}

pub struct Demo {
    agentforge_bin: PathBuf,
    mock_claude_bin: PathBuf,
    repo: PathBuf,
    repo_str: String,
    narrate: bool,
}

pub fn run(agentforge_bin: &Path, mock_claude_bin: &Path, narrate: bool) -> DemoOutcome {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let repo = tmp.path().to_path_buf();
    let demo = Demo {
        agentforge_bin: agentforge_bin.to_path_buf(),
        mock_claude_bin: mock_claude_bin.to_path_buf(),
        repo_str: repo.to_string_lossy().to_string(),
        repo,
        narrate,
    };

    demo.section("1. Fixture repository — a billing service with a discount-rate bug");
    let initial_sha = demo.build_fixture_repo();

    demo.section("2. agentforge init");
    demo.af_ok(&["init", "--repo", &demo.repo_str]);

    demo.section("3. Register the evaluator");
    demo.add_billing_evaluator();

    demo.section("4. Register a named policy");
    demo.add_demo_policy();

    demo.section("5. Register the task");
    demo.add_billing_task(&initial_sha);

    demo.section("6. Isolated candidate workspace (low-level primitive)");
    demo.workspace_walkthrough();

    demo.section("7. Standalone repository fault injection (experiment metadata)");
    demo.fault_walkthrough();

    demo.section("8. Standalone source mutation testing (experiment metadata)");
    demo.mutant_walkthrough();

    demo.section("9. Standalone evaluator run (verify) against the still-buggy HEAD");
    demo.verify_walkthrough();

    demo.section("10. A single candidate run");
    let run_experiment_id = demo.run_walkthrough();

    demo.section("11. A three-candidate race — evaluator execution, scoring, and ranking");
    let (race_id, race_leaderboard) = demo.race_walkthrough();

    demo.section("12. Semantic bisect over a scripted regression");
    let (bisect_id, bisect_culprit) = demo.bisect_walkthrough();

    demo.section("13. Cleanup");
    demo.cleanup_walkthrough();

    DemoOutcome {
        run_experiment_id,
        race_id,
        race_leaderboard,
        bisect_id,
        bisect_culprit,
    }
}

impl Demo {
    fn section(&self, title: &str) {
        narrate::print_section(self.narrate, title);
    }

    fn af(&self, args: &[&str]) -> Output {
        self.af_env(args, &[])
    }

    fn af_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        if self.narrate {
            println!("$ agentforge {}", args.join(" "));
        }
        let mut cmd = Command::new(&self.agentforge_bin);
        cmd.args(args);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn agentforge");
        if self.narrate {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if !stdout.trim().is_empty() {
                println!("{}", stdout.trim_end());
            }
        }
        out
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

    fn commit_file(&self, rel: &str, contents: &str, message: &str) -> String {
        self.write(rel, contents);
        self.run_git(&["add", rel]);
        self.run_git(&["commit", "-q", "-m", message]);
        self.git_output(&["rev-parse", "HEAD"])
    }

    // ---- 1. fixture repo ------------------------------------------------------------------

    fn build_fixture_repo(&self) -> String {
        self.run_git(&["init", "-q"]);
        self.run_git(&["config", "user.email", "agentforge-demo@example.com"]);
        self.run_git(&["config", "user.name", "AgentForge Demo"]);
        self.run_git(&["config", "core.autocrlf", "false"]);

        self.write(
            "README.md",
            "A tiny billing service with a discount-rate bug — AgentForge demo fixture.\n",
        );
        // The bug: a 50% discount is applied to every order instead of the intended 10%, and an
        // audit-logging flag was left disabled. `billing-tests` (registered below) checks both.
        self.write(
            "billing/discount.txt",
            "DISCOUNT_RATE=0.50\nLOGGING=disabled\n",
        );
        // A second, unrelated tracked file used only by the standalone mutation-testing demo
        // (step 8) — a plain boolean flag, the shape `MutationOperator::BooleanFlip` targets.
        self.write("billing/flags.txt", "ENABLED=true\n");
        self.write("evaluate.cmd", EVALUATE_CMD);
        self.write("evaluate.sh", EVALUATE_SH);

        self.run_git(&["add", "."]);
        self.run_git(&["commit", "-q", "-m", "initial: buggy billing config"]);
        self.git_output(&["rev-parse", "HEAD"])
    }

    // ---- 3. evaluator -----------------------------------------------------------------------

    fn add_billing_evaluator(&self) {
        let (program, args) = if cfg!(windows) {
            ("cmd", vec!["/C", "evaluate.cmd"])
        } else {
            ("sh", vec!["evaluate.sh"])
        };
        let toml = format!(
            r#"
id = "billing-tests"
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
        let path = self.write_tmp("billing-tests.toml", &toml);
        self.af_ok(&[
            "evaluator",
            "add",
            "--repo",
            &self.repo_str,
            &path.to_string_lossy(),
        ]);
    }

    /// A second evaluator, used only to detect the standalone `BooleanFlip` mutant applied to
    /// `billing/flags.txt` in step 8 — mirrors `tests/cli_mutant.rs`'s `detect_false_cmd`.
    fn add_flags_detector_evaluator(&self) {
        let toml = if cfg!(windows) {
            r#"
id = "flags-detector"
setup_cmds = []
timeout_secs = 30
budget_secs = 30
size_budget_lines = 50
metric_extractors = []

[test_cmd]
program = "cmd"
args = ["/C", "findstr /C:false billing\\flags.txt >nul && exit /b 1 || exit /b 0"]
cwd_relative = "."
"#
            .to_string()
        } else {
            r#"
id = "flags-detector"
setup_cmds = []
timeout_secs = 30
budget_secs = 30
size_budget_lines = 50
metric_extractors = []

[test_cmd]
program = "sh"
args = ["-c", "grep -q false billing/flags.txt && exit 1 || exit 0"]
cwd_relative = "."
"#
            .to_string()
        };
        let path = self.write_tmp("flags-detector.toml", &toml);
        self.af_ok(&[
            "evaluator",
            "add",
            "--repo",
            &self.repo_str,
            &path.to_string_lossy(),
        ]);
    }

    /// A third evaluator, used only by the bisect demo (step 12) — mirrors
    /// `tests/bisect.rs`'s `status_flag_evaluator_spec`.
    fn add_bisect_evaluator(&self) {
        let toml = if cfg!(windows) {
            r#"
id = "bisect-status"
setup_cmds = []
timeout_secs = 30
budget_secs = 30
size_budget_lines = 50
metric_extractors = []

[test_cmd]
program = "cmd"
args = ["/C", "findstr /C:GOOD status.flag >nul && exit /b 0 || exit /b 1"]
cwd_relative = "."
"#
            .to_string()
        } else {
            r#"
id = "bisect-status"
setup_cmds = []
timeout_secs = 30
budget_secs = 30
size_budget_lines = 50
metric_extractors = []

[test_cmd]
program = "sh"
args = ["-c", "grep -q GOOD status.flag && exit 0 || exit 1"]
cwd_relative = "."
"#
            .to_string()
        };
        let path = self.write_tmp("bisect-status.toml", &toml);
        self.af_ok(&[
            "evaluator",
            "add",
            "--repo",
            &self.repo_str,
            &path.to_string_lossy(),
        ]);
    }

    // ---- 4. policy --------------------------------------------------------------------------

    fn add_demo_policy(&self) {
        let toml = r#"
name = "demo-policy"
extra_readonly_paths = []
deny_network = true
env_passthrough = []
max_wall_time_secs = 120
max_output_bytes = 5000000
allowed_programs = []
denied_programs = []
allowed_roots = []
"#;
        let path = self.write_tmp("demo-policy.toml", toml);
        let out = self.af_ok(&[
            "policy",
            "add",
            "--repo",
            &self.repo_str,
            &path.to_string_lossy(),
        ]);
        assert!(out.contains("demo-policy"));
        self.af_ok(&["policy", "list", "--repo", &self.repo_str]);
        let show = self.af_ok(&["policy", "show", "--repo", &self.repo_str, "demo-policy"]);
        assert!(show.contains("Enforced"), "policy show: {show}");
        let validate = self.af_ok(&[
            "policy",
            "validate",
            "--repo",
            &self.repo_str,
            "demo-policy",
        ]);
        assert!(validate.contains("valid"), "policy validate: {validate}");
    }

    // ---- 5. task ----------------------------------------------------------------------------

    fn add_billing_task(&self, base_sha: &str) {
        let toml = format!(
            r#"
id = "fix-discount-bug"
name = "fix-discount-bug"
prompt = "Fix billing/discount.txt: DISCOUNT_RATE must be 0.10 and LOGGING must be enabled."
repo_path = "."
base_ref = "{base_sha}"
evaluator = "billing-tests"
agent_timeout_secs = 30
created_at = "2026-01-01T00:00:00Z"

[baseline]
build_succeeded = true
exit_code = 1
timed_out = false
wall_time_secs = 0.0
"#
        );
        let path = self.write_tmp("fix-discount-bug.toml", &toml);
        self.af_ok(&[
            "task",
            "add",
            "--repo",
            &self.repo_str,
            &path.to_string_lossy(),
        ]);
        let show = self.af_ok(&["task", "show", "--repo", &self.repo_str, "fix-discount-bug"]);
        // `task add` recaptures the baseline for real against `base_ref` — the still-buggy
        // seed commit — rather than trusting the file's placeholder values.
        assert!(
            show.contains("tests=Some(0)/Some(2)"),
            "expected the recaptured baseline to reflect the still-buggy seed commit: {show}"
        );
    }

    // ---- 6. workspace -------------------------------------------------------------------------

    fn workspace_walkthrough(&self) {
        let create = self.af_ok(&[
            "workspace",
            "create",
            "--repo",
            &self.repo_str,
            "--id",
            "explore",
            "--base",
            "HEAD",
        ]);
        assert!(create.contains("explore"));
        self.af_ok(&["workspace", "show", "--repo", &self.repo_str, "explore"]);
        self.af_ok(&["workspace", "list", "--repo", &self.repo_str]);

        let cat_args: &[&str] = if cfg!(windows) {
            &["cmd", "/C", "type", "billing\\discount.txt"]
        } else {
            &["cat", "billing/discount.txt"]
        };
        let mut exec_args: Vec<&str> = vec![
            "workspace",
            "exec",
            "--repo",
            &self.repo_str,
            "explore",
            "--",
        ];
        exec_args.extend_from_slice(cat_args);
        let out = self.af(&exec_args);
        assert!(
            out.status.success(),
            "workspace exec failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // `workspace exec` writes captured-output *paths* to stderr, not the content itself, so
        // narrate what the isolated candidate workspace actually contains for real.
        if self.narrate {
            println!(
                "  (workspace exec ran in an isolated worktree — its own copy of the buggy file)"
            );
        }
        // Leave "explore" alive — step 13 demonstrates `clean --all-worktrees` sweeping it.
    }

    // ---- 7. fault -----------------------------------------------------------------------------

    fn fault_walkthrough(&self) {
        let spec = r#"
kind = "BrokenConfigValue"
target_glob = "billing/discount.txt"
seed = 0
fault_version = 1
"#;
        let spec_path = self.write_tmp("fault-spec.toml", spec);
        let inject = self.af_ok(&[
            "experiment",
            "fault",
            "inject",
            "--repo",
            &self.repo_str,
            "--id",
            "billing-fault",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
        ]);
        assert!(inject.contains("injected fault billing-fault"), "{inject}");

        let show = self.af_ok(&[
            "experiment",
            "fault",
            "show",
            "--repo",
            &self.repo_str,
            "billing-fault",
        ]);
        assert!(show.contains("BrokenConfigValue"), "{show}");
        assert!(show.contains("billing/discount.txt"), "{show}");

        // The source repository itself must never be touched by inject — only the fault's own
        // isolated worktree is.
        assert_eq!(
            std::fs::read_to_string(self.repo.join("billing/discount.txt")).unwrap(),
            "DISCOUNT_RATE=0.50\nLOGGING=disabled\n"
        );

        self.af_ok(&[
            "experiment",
            "fault",
            "restore",
            "--repo",
            &self.repo_str,
            "billing-fault",
        ]);
        self.af_ok(&[
            "experiment",
            "fault",
            "discard",
            "--repo",
            &self.repo_str,
            "billing-fault",
        ]);
    }

    // ---- 8. mutant ----------------------------------------------------------------------------

    fn mutant_walkthrough(&self) {
        self.add_flags_detector_evaluator();

        let spec = r#"
operator = "BooleanFlip"
target_glob = "billing/flags.txt"
seed = 0
operator_version = 1
"#;
        let spec_path = self.write_tmp("mutant-spec.toml", spec);
        let apply = self.af_ok(&[
            "experiment",
            "mutant",
            "apply",
            "--repo",
            &self.repo_str,
            "--id",
            "billing-mutant",
            "--spec",
            &spec_path.to_string_lossy(),
            "--base",
            "HEAD",
        ]);
        assert!(apply.contains("applied mutant billing-mutant"), "{apply}");

        let show_before = self.af_ok(&[
            "experiment",
            "mutant",
            "show",
            "--repo",
            &self.repo_str,
            "billing-mutant",
        ]);
        assert!(show_before.contains("BooleanFlip"), "{show_before}");
        assert!(show_before.contains("evaluation:    none"), "{show_before}");

        let evaluate = self.af_ok(&[
            "experiment",
            "mutant",
            "evaluate",
            "--repo",
            &self.repo_str,
            "--evaluator",
            "flags-detector",
            "billing-mutant",
        ]);
        assert!(
            evaluate.contains("KILLED"),
            "the flags-detector evaluator must catch the boolean flip: {evaluate}"
        );

        self.af_ok(&[
            "experiment",
            "mutant",
            "restore",
            "--repo",
            &self.repo_str,
            "billing-mutant",
        ]);
        self.af_ok(&[
            "experiment",
            "mutant",
            "discard",
            "--repo",
            &self.repo_str,
            "billing-mutant",
        ]);
    }

    // ---- 9. verify ------------------------------------------------------------------------

    fn verify_walkthrough(&self) {
        let out = self.af(&[
            "verify",
            "--repo",
            &self.repo_str,
            "--evaluator",
            "billing-tests",
            "--ref",
            "HEAD",
        ]);
        // HEAD is still the unfixed seed commit — `verify` must report a BAD verdict (exit 3),
        // not an internal error.
        assert_eq!(
            out.status.code(),
            Some(3),
            "verify against the still-buggy HEAD must report a bad verdict: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("Verdict         BAD"), "{stdout}");
        if self.narrate {
            println!("{}", stdout.trim_end());
        }
    }

    // ---- 10. run ------------------------------------------------------------------------------

    fn run_walkthrough(&self) -> String {
        let stdout = self.af_ok_with_claude(&[
            "run",
            "--repo",
            &self.repo_str,
            "--task",
            "fix-discount-bug",
            "--agent",
            "claude-code:goodfix",
            "--policy",
            "demo-policy",
            "--json",
        ]);
        let value: serde_json::Value =
            serde_json::from_str(&stdout).expect("run --json prints valid JSON");
        let id = value["id"]
            .as_str()
            .expect("experiment json has an id")
            .to_string();
        assert_eq!(value["status"], "Completed");
        assert_eq!(
            value["score"]["gated"], false,
            "a full fix must not trip the correctness gate: {value:#}"
        );

        self.af_ok(&["report", "show", "--repo", &self.repo_str, &id]);
        self.af_ok(&["report", "show", "--repo", &self.repo_str, "--verbose", &id]);
        self.af_ok(&["report", "log", "--repo", &self.repo_str, &id]);
        id
    }

    // ---- 11. race -----------------------------------------------------------------------------

    fn race_walkthrough(&self) -> (String, Vec<serde_json::Value>) {
        let stdout = self.af_ok_with_claude(&[
            "race",
            "--repo",
            &self.repo_str,
            "--task",
            "fix-discount-bug",
            "--agents",
            "claude-code:goodfix,claude-code:partialfix,claude-code:nofix",
            "--repeat",
            "1",
            "--max-parallel",
            "3",
            "--json",
        ]);
        let value: serde_json::Value =
            serde_json::from_str(&stdout).expect("race --json prints valid JSON");
        let race_id = value["id"]
            .as_str()
            .expect("race json has an id")
            .to_string();
        let leaderboard = value["leaderboard"]
            .as_array()
            .expect("leaderboard array")
            .clone();
        assert_eq!(leaderboard.len(), 3, "leaderboard: {leaderboard:#?}");

        let models: Vec<&str> = leaderboard
            .iter()
            .map(|entry| entry["agent_config"]["model"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            models,
            vec!["goodfix", "partialfix", "nofix"],
            "ranking must be goodfix > partialfix > nofix by score.total desc: {leaderboard:#?}"
        );
        assert_eq!(leaderboard[0]["score"]["gated"], false, "{leaderboard:#?}");
        assert_eq!(
            leaderboard[2]["score"]["gated"], true,
            "the candidate that never fixed the discount rate must trip the gate: {leaderboard:#?}"
        );

        // Human-readable comparison table, for the reader following along.
        let text = self.af_ok(&["report", "show", "--repo", &self.repo_str, &race_id]);
        assert!(
            text.contains("Candidate") && text.contains("Score"),
            "{text}"
        );
        self.af_ok(&[
            "report",
            "show",
            "--repo",
            &self.repo_str,
            "--verbose",
            &race_id,
        ]);

        let winner_id = leaderboard[0]["id"].as_str().unwrap().to_string();
        self.af_ok(&[
            "report",
            "score",
            "--repo",
            &self.repo_str,
            "--verbose",
            &winner_id,
        ]);

        (race_id, leaderboard)
    }

    // ---- 12. bisect ---------------------------------------------------------------------------

    fn bisect_walkthrough(&self) -> (String, String) {
        let good = self.git_output(&["rev-parse", "HEAD"]);
        // Mirrors tests/bisect.rs's exact fixture shape: GOOD/GOOD/GOOD then BAD/BAD/BAD/BAD, so
        // the flip (and thus the culprit) is commits[3] — a textbook binary search visits it in
        // exactly 3 steps.
        let flags = [
            ("GOOD\n# c1\n", "c1"),
            ("GOOD\n# c2\n", "c2"),
            ("GOOD\n# c3\n", "c3"),
            ("BAD\n# c4 (regression)\n", "c4 introduces a regression"),
            ("BAD\n# c5\n", "c5"),
            ("BAD\n# c6\n", "c6"),
            ("BAD\n# c7\n", "c7"),
        ];
        let mut shas = Vec::new();
        for (content, message) in flags {
            shas.push(self.commit_file("status.flag", content, message));
        }
        let bad = shas.last().unwrap().clone();
        let expected_culprit = shas[3].clone();

        self.add_bisect_evaluator();
        let task_toml = format!(
            r#"
id = "bisect-regression-hunt"
name = "bisect-regression-hunt"
prompt = "n/a — bisect only runs the evaluator, never an agent"
repo_path = "."
base_ref = "{good}"
evaluator = "bisect-status"
agent_timeout_secs = 30
created_at = "2026-01-01T00:00:00Z"

[baseline]
build_succeeded = true
exit_code = 0
timed_out = false
wall_time_secs = 0.0
"#
        );
        let task_path = self.write_tmp("bisect-regression-hunt.toml", &task_toml);
        self.af_ok(&[
            "task",
            "add",
            "--repo",
            &self.repo_str,
            &task_path.to_string_lossy(),
        ]);

        let range = format!("{good}..{bad}");
        let text = self.af_ok(&[
            "bisect",
            "--repo",
            &self.repo_str,
            "--task",
            "bisect-regression-hunt",
            "--range",
            &range,
        ]);
        assert!(
            text.contains(&expected_culprit),
            "expected culprit {expected_culprit} in bisect output: {text}"
        );

        let json = self.af_ok(&[
            "bisect",
            "--repo",
            &self.repo_str,
            "--task",
            "bisect-regression-hunt",
            "--range",
            &range,
            "--json",
        ]);
        let value: serde_json::Value =
            serde_json::from_str(&json).expect("bisect --json prints valid JSON");
        let bisect_id = value["id"].as_str().unwrap().to_string();
        let culprit = value["culprit"].as_str().unwrap().to_string();
        assert_eq!(culprit, expected_culprit);

        self.af_ok(&["report", "show", "--repo", &self.repo_str, &bisect_id]);

        (bisect_id, culprit)
    }

    // ---- 13. cleanup ----------------------------------------------------------------------

    fn cleanup_walkthrough(&self) {
        let before = self.af_ok(&["workspace", "list", "--repo", &self.repo_str]);
        assert!(before.contains("explore"), "{before}");

        // `clean --all-worktrees` sweeps `run`/`race`/`bisect`'s own `ExperimentRecord`-tracked
        // worktrees (already removed automatically once each finalized, so this is a no-op here
        // — it's still worth demonstrating). It's a distinct lifecycle from a plain `workspace`,
        // which has its own `remove`/`clean` — SPEC.md §7 keeps the two worktree flavors separate.
        self.af_ok(&[
            "clean",
            "--repo",
            &self.repo_str,
            "--all-worktrees",
            "--force",
        ]);

        self.af_ok(&["workspace", "clean", "--repo", &self.repo_str, "--force"]);

        let after = self.af_ok(&["workspace", "list", "--repo", &self.repo_str]);
        assert!(
            !after.contains("explore"),
            "workspace clean --force must have swept the leftover workspace: {after}"
        );
    }
}

const EVALUATE_CMD: &str = r#"@echo off
setlocal enabledelayedexpansion
set /a passed=0
findstr /C:"DISCOUNT_RATE=0.10" billing\discount.txt >nul
if !errorlevel! EQU 0 set /a passed+=1
findstr /C:"LOGGING=enabled" billing\discount.txt >nul
if !errorlevel! EQU 0 set /a passed+=1
echo tests_passed=!passed!
echo tests_total=2
findstr /C:"DISCOUNT_RATE=0.10" billing\discount.txt >nul
exit /b !errorlevel!
"#;

const EVALUATE_SH: &str = r#"#!/bin/sh
passed=0
grep -q "DISCOUNT_RATE=0.10" billing/discount.txt && passed=$((passed+1))
grep -q "LOGGING=enabled" billing/discount.txt && passed=$((passed+1))
echo "tests_passed=$passed"
echo "tests_total=2"
grep -q "DISCOUNT_RATE=0.10" billing/discount.txt
exit $?
"#;
