//! Wiring, not logic — SPEC.md §6, docs/ARCHITECTURE.md §14.
//!
//! Every subcommand maps 1:1 to a row in SPEC.md §6's command table. `cli` is the only module
//! allowed to depend on everything else; it constructs the shared `Arc<dyn Executor>`,
//! `Arc<GitRepo>`, `Arc<WorktreeManager>`, `Arc<Store>`, etc. once at startup and passes them
//! down — no other module constructs its own dependencies.
//!
//! Top-level surface (SPEC.md §6): `init`, `workspace`, `evaluator`, `task`, `experiment`
//! (`fault`/`mutation`/`mutant` — repository-state test-fixture mechanisms, sibling to one
//! another, none of which produce an `ExperimentRecord`), `run`, `race`, `bisect`, `verify`
//! (standalone evaluator run), `report` (`show`/`score`/`log`), `policy`, `clean`. Every command
//! that reads repo-tracked or state-root state takes a consistent `--repo` (default `.`); every
//! command that can produce a structured result takes a consistent `--json`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};

use crate::domain::{
    EvaluatorSpec, EvaluatorVerdict, ExperimentStatus, FaultSpec, MutantEvaluation, MutantSpec,
    MutationSpec, PermissionPolicy, ProcessSpec, ScoringWeights, TaskSpec,
};
use crate::evaluator::Evaluator;
use crate::exec::{Executor, SystemExecutor};
use crate::experiment::ExperimentRunner;
use crate::fault::{self, FaultInjector};
use crate::git::worktree::{WorktreeHandle, WorktreeKind, WorktreeManager};
use crate::git::GitRepo;
use crate::mutant::{self, MutantTester};
use crate::mutation::{self, MutationEngine};
use crate::race::RaceRunner;
use crate::report::{self, Reporter};
use crate::scoring;
use crate::store::{self, Store};
use crate::workspace::{self, WorkspaceManager};

/// `experiment mutation create` (formerly `mutate`) has no agent-timeout flag of its own
/// (SPEC.md §6) — the created task's `agent_timeout_secs` is a fixed, documented default rather
/// than a synthesized guess.
const DEFAULT_MUTATION_AGENT_TIMEOUT_SECS: u64 = 1800;

#[derive(Debug, Parser)]
#[command(
    name = "agentforge",
    about = "Safely run, test, compare, and evaluate coding agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Bootstrap `.agentforge/` and the external state root — SPEC.md §5, §6.
    Init(InitArgs),
    /// Create, inspect, run commands in, and remove isolated task worktrees directly — a
    /// lower-level primitive than `run`, independent of any task/evaluator/agent config.
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
    Evaluator {
        #[command(subcommand)]
        action: EvaluatorAction,
    },
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
    /// Mechanisms that prepare a repository-state test fixture (a deterministic fault,
    /// sanity-gated mutation, or standalone mutant) — a sibling to `run`/`race`/`bisect`, not a
    /// variant of them: none of these three produce an `ExperimentRecord` on their own.
    Experiment {
        #[command(subcommand)]
        action: ExperimentAction,
    },
    /// The one `run` primitive — SPEC.md §8.
    Run(RunArgs),
    /// N agent/config combinations against the same task and evaluator — SPEC.md §12.
    Race(RaceArgs),
    /// In-process binary search using the task's evaluator as the oracle — SPEC.md §13.
    Bisect(BisectArgs),
    /// Standalone evaluator run, against an arbitrary ref (optionally with a patch applied) or a
    /// stored experiment's already-captured patch — SPEC.md §11.
    Verify(VerifyArgs),
    /// Human-or-JSON reporting: `show` a result, `score` recomputes it, `log` prints its audit
    /// trail — SPEC.md §6, §13, §15.
    Report {
        #[command(subcommand)]
        action: ReportAction,
    },
    Policy {
        #[command(subcommand)]
        action: PolicyAction,
    },
    Clean(CleanArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceAction {
    /// Create a new workspace from a base git ref.
    Create(WorkspaceCreateArgs),
    /// List every workspace under this repo's state root.
    List(WorkspaceRepoArgs),
    /// Inspect a single workspace: path, current HEAD, lock state.
    Show(WorkspaceIdArgs),
    /// Run a controlled command inside a workspace. Everything after `--` is passed as an
    /// argument array directly to the child process — never interpreted by a shell.
    Exec(WorkspaceExecArgs),
    /// Remove a single workspace. Idempotent; refuses if the workspace is locked unless
    /// `--force` is passed.
    Remove(WorkspaceRemoveArgs),
    /// Remove every unlocked workspace (or, with `--force`, every workspace).
    Clean(WorkspaceCleanArgs),
}

#[derive(Debug, Args)]
pub struct WorkspaceRepoArgs {
    /// Path to the target git repository.
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
}

#[derive(Debug, Args)]
pub struct WorkspaceIdArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub id: String,
}

#[derive(Debug, Args)]
pub struct WorkspaceCreateArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Workspace id — ASCII letters, digits, `-`, `_` only.
    #[arg(long)]
    pub id: String,
    /// Any git commit-ish; resolved and pinned to a concrete commit at creation time.
    #[arg(long)]
    pub base: String,
}

#[derive(Debug, Args)]
pub struct WorkspaceExecArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub id: String,
    #[arg(long, default_value_t = 300)]
    pub timeout_secs: u64,
    #[arg(long, default_value_t = 10_000_000)]
    pub max_output_bytes: u64,
    /// Comma-separated host env vars to expose to the command — nothing else from the host
    /// environment is passed through (SPEC.md §16).
    #[arg(long, value_delimiter = ',')]
    pub env_passthrough: Vec<String>,
    /// Comma-separated program allowlist; empty means unrestricted. Checked by the Executor
    /// itself before spawning, before this command ever touches the OS (SPEC.md §16).
    #[arg(long = "allow-program", value_delimiter = ',')]
    pub allowed_programs: Vec<String>,
    /// Comma-separated program denylist, checked first — a program on both lists is refused.
    #[arg(long = "deny-program", value_delimiter = ',')]
    pub denied_programs: Vec<String>,
    /// Comma-separated directories the command's cwd must resolve inside; empty means
    /// unrestricted. Defense-in-depth on top of this workspace's own state-root confinement.
    #[arg(long = "allowed-root", value_delimiter = ',')]
    pub allowed_roots: Vec<PathBuf>,
    /// The command and its arguments, e.g. `-- cargo test`.
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Args)]
pub struct WorkspaceRemoveArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub id: String,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct WorkspaceCleanArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum EvaluatorAction {
    Add(AddFileArgs),
    List(RepoArgs),
    Show(RepoIdArgs),
}

#[derive(Debug, Subcommand)]
pub enum TaskAction {
    Add(AddFileArgs),
    List(RepoArgs),
    Show(RepoIdArgs),
}

/// Shared by `evaluator add`/`task add`/`policy add`: validate a TOML-serialized spec file and
/// copy it into the repo-tracked collection, subject to the same `--force` collision rule
/// (SPEC.md §20, C6). All three commands had identical shapes before this pass — one struct
/// instead of three near-duplicates.
#[derive(Debug, Args)]
pub struct AddFileArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub file: PathBuf,
    #[arg(long)]
    pub force: bool,
}

/// Shared by any `<noun> list` subcommand that only needs a repo path.
#[derive(Debug, Args)]
pub struct RepoArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
}

/// Shared by any `<noun> show <id>` subcommand.
#[derive(Debug, Args)]
pub struct RepoIdArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub id: String,
}

#[derive(Debug, Subcommand)]
pub enum ExperimentAction {
    /// Deterministic repository fault injection into an isolated fault workspace — SPEC.md §10
    /// Amendment. Simulates a broken repository state (missing file, broken config value, stale
    /// artifact, corrupted dependency pin) rather than a source-code logic bug.
    Fault {
        #[command(subcommand)]
        action: FaultAction,
    },
    /// Deterministic, sanity-gated source mutation testing — `create` applies a `MutationSpec`,
    /// runs the sanity gate, and creates a task only if the fault was detected; `show`/`replay`
    /// inspect the resulting embedded record — SPEC.md §10.
    Mutation {
        #[command(subcommand)]
        action: MutationAction,
    },
    /// Standalone, reproducible source mutation testing into an isolated mutant workspace —
    /// SPEC.md §10 Amendment (standalone mutation testing pass). Unlike `mutation create`,
    /// `apply` never gates or evaluates by itself — `evaluate` is a separate, later, independent
    /// step against the same materialized workspace.
    Mutant {
        #[command(subcommand)]
        action: MutantAction,
    },
}

#[derive(Debug, Args)]
pub struct MutationCreateArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    #[arg(long = "task-id")]
    pub task_id: String,
    #[arg(long)]
    pub spec: PathBuf,
    #[arg(long)]
    pub base: String,
    #[arg(long)]
    pub evaluator: String,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum MutationAction {
    /// Deterministically applies a `MutationSpec` on top of `--base` via git plumbing, commits
    /// to a dedicated ref, runs the sanity-gate `evaluate()` call, and — only if the mutant's
    /// verdict is **not** good (fault was detected) — creates the task named `--task-id`.
    Create(MutationCreateArgs),
    /// Prints the `MutationRef` embedded in `<task-id>`'s `TaskSpec` in full: spec, base/mutant
    /// commits, selected target, diff stats, mutant ref, sanity_checked, applied_at.
    Show(MutationTaskArgs),
    /// Re-applies the task's stored `(operator, target_glob, seed, operator_version,
    /// base_commit)` under a throwaway ref (deleted immediately after) and asserts it reproduces
    /// the identical `mutant_commit`/`selected_target`/`diff_stats` — SPEC.md §10's determinism
    /// contract, exercised directly.
    Replay(MutationTaskArgs),
}

#[derive(Debug, Args)]
pub struct MutationTaskArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub task_id: String,
}

#[derive(Debug, Subcommand)]
pub enum FaultAction {
    /// Select a candidate deterministically (`kind`, `target_glob`, `seed`, `fault_version`,
    /// `base`), materialize an isolated fault workspace at `base`, write the fault directly into
    /// it, and persist the resulting `FaultRef` under `--id`.
    Inject(FaultInjectArgs),
    /// Print a persisted fault's spec, selected target, description, diff stats, and workspace
    /// path.
    Show(RepoIdArgs),
    /// Reverse the fault in place inside its own workspace — the workspace itself stays alive.
    Restore(RepoIdArgs),
    /// Remove the fault's entire isolated workspace — the alternative to `restore` when the
    /// workspace is no longer needed.
    Discard(RepoIdArgs),
}

#[derive(Debug, Args)]
pub struct FaultInjectArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Fault id — ASCII letters, digits, `-`, `_` only. Both this record's `Store` key and the
    /// fault workspace's directory name.
    #[arg(long)]
    pub id: String,
    /// Path to a TOML-serialized `FaultSpec`.
    #[arg(long)]
    pub spec: PathBuf,
    /// Any git commit-ish; resolved and pinned to a concrete commit before injection.
    #[arg(long)]
    pub base: String,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum MutantAction {
    /// Select a candidate deterministically (`operator`, `target_glob`, `seed`,
    /// `operator_version`, `base`), materialize an isolated mutant workspace at `base`, write the
    /// mutation directly into it, and persist the resulting `MutantRef` under `--id`. Never
    /// evaluates or gates — `evaluate` is a separate command.
    Apply(MutantApplyArgs),
    /// Print a persisted mutant's spec, selected target, description, diff stats, workspace
    /// path, and recorded evaluation (or `none` if never evaluated).
    Show(RepoIdArgs),
    /// Run the evaluator against the mutant's still-materialized workspace, record the outcome
    /// onto the persisted `MutantRef` (`evaluation`), and write a real audit trail — unlike
    /// `mutation create`'s sanity check (`NullAuditSink`, immediate, gating), this never rejects:
    /// a surviving mutant is reported, not treated as a command failure.
    Evaluate(MutantEvaluateArgs),
    /// Reverse the mutation in place inside its own workspace — the workspace itself stays
    /// alive.
    Restore(RepoIdArgs),
    /// Remove the mutant's entire isolated workspace — the alternative to `restore` when the
    /// workspace is no longer needed.
    Discard(RepoIdArgs),
}

#[derive(Debug, Args)]
pub struct MutantApplyArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Mutant id — ASCII letters, digits, `-`, `_` only. Both this record's `Store` key and the
    /// mutant workspace's directory name.
    #[arg(long)]
    pub id: String,
    /// Path to a TOML-serialized `MutantSpec`.
    #[arg(long)]
    pub spec: PathBuf,
    /// Any git commit-ish; resolved and pinned to a concrete commit before applying.
    #[arg(long)]
    pub base: String,
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct MutantEvaluateArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub id: String,
    #[arg(long)]
    pub evaluator: String,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    #[arg(long)]
    pub task: String,
    /// `adapter[:model]`
    #[arg(long)]
    pub agent: String,
    /// A policy previously created via `policy add`; falls back to a generous, uniform built-in
    /// default (`PermissionPolicy::generous_default`) when omitted.
    #[arg(long)]
    pub policy: Option<String>,
    #[arg(long = "keep-worktree-on-fail")]
    pub keep_worktree_on_fail: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RaceArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    #[arg(long)]
    pub task: String,
    /// Comma-separated `adapter[:model]` list, in the order that determines `race_index`.
    #[arg(long, value_delimiter = ',')]
    pub agents: Vec<String>,
    #[arg(long, default_value_t = 1)]
    pub repeat: u32,
    /// Defaults to the full expanded participant count (`agents.len() * repeat`) — i.e.
    /// unbounded fan-out — when omitted.
    #[arg(long = "max-parallel")]
    pub max_parallel: Option<u32>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct BisectArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    #[arg(long)]
    pub task: String,
    /// `<good>..<bad>`
    #[arg(long)]
    pub range: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
#[command(group(clap::ArgGroup::new("verify_target").required(true).args(["ref", "experiment"])))]
pub struct VerifyArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    /// Required together with `--ref`; ignored when `--experiment` is given.
    #[arg(long)]
    pub evaluator: Option<String>,
    #[arg(long = "ref")]
    pub r#ref: Option<String>,
    #[arg(long = "apply-patch")]
    pub apply_patch: Option<PathBuf>,
    #[arg(long)]
    pub experiment: Option<String>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum ReportAction {
    /// Human-readable (or `--json`) report for an experiment, race, or bisect id.
    Show(ShowArgs),
    /// Recompute a `ScoreCard` from persisted `RawMetrics` — spawns no process.
    Score(ScoreArgs),
    /// Pretty-print an experiment's `audit.jsonl` in order; `--follow` tails a still-running
    /// experiment until its `RUNNING.lock` clears.
    Log(LogArgs),
}

#[derive(Debug, Args)]
pub struct ScoreArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub experiment_id: String,
    /// Path to a TOML-serialized `ScoringWeights`; defaults to `<repo>/.agentforge/scoring.toml`
    /// if present, else the built-in defaults (SPEC.md §14).
    #[arg(long)]
    pub weights: Option<PathBuf>,
    /// Show the full component breakdown, configured weights, threshold definitions, and failed
    /// checks, not just the composite score.
    #[arg(long)]
    pub verbose: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub id: String,
    /// Show scoring components, configured weights, threshold definitions, failed checks, and
    /// full audit/experiment identifiers — omitted by default to keep the report scannable.
    #[arg(long)]
    pub verbose: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    pub experiment_id: String,
    #[arg(long)]
    pub follow: bool,
}

#[derive(Debug, Subcommand)]
pub enum PolicyAction {
    /// Validates a `PermissionPolicy` (`max_wall_time_secs`/`max_output_bytes` both `> 0`) and
    /// copies it into `policies/` — mirrors `evaluator add`/`task add`.
    Add(AddFileArgs),
    List(RepoArgs),
    /// Prints every field tagged `Enforced`, `RequestedOnly`, or `Unsupported` — SPEC.md §17.
    Show(RepoIdArgs),
    /// Rejects `max_wall_time_secs == 0` or `max_output_bytes == 0`, naming the failing field.
    Validate(RepoIdArgs),
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    #[arg(long, default_value = ".")]
    pub repo: PathBuf,
    #[arg(long)]
    pub experiment: Option<String>,
    #[arg(long = "all-worktrees")]
    pub all_worktrees: bool,
    #[arg(long = "older-than")]
    pub older_than: Option<String>,
    #[arg(long)]
    pub force: bool,
}

/// Parses argv, dispatches, and maps the result to an exit code per SPEC.md §6's global table
/// and each command's precedence rule (e.g. `run`: Failed→1, TimedOut→124,
/// Completed+bad-verdict→3, Completed+good-verdict→0). `cli` never recomputes a verdict — it
/// only maps an already-structured result to a process exit code.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Init(args) => init_cmd(args),
        Command::Workspace { action } => dispatch_workspace(action),
        Command::Evaluator { action } => dispatch_evaluator(action),
        Command::Task { action } => dispatch_task(action),
        Command::Experiment { action } => dispatch_experiment(action),
        Command::Run(args) => run_cmd(args),
        Command::Race(args) => race_cmd(args),
        Command::Bisect(args) => bisect_cmd(args),
        Command::Verify(args) => verify_cmd(args),
        Command::Report { action } => dispatch_report(action),
        Command::Policy { action } => dispatch_policy(action),
        Command::Clean(args) => clean_cmd(args),
    }
}

/// Constructs every shared dependency most commands need from a bare repo path — `cli`'s job per
/// docs/ARCHITECTURE.md §14 ("no other module constructs its own dependencies").
struct CliDeps {
    exec: Arc<dyn Executor>,
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    store: Arc<Store>,
}

fn build_deps(repo: &Path) -> std::result::Result<CliDeps, String> {
    let exec: Arc<dyn Executor> = Arc::new(SystemExecutor::new());
    let git = Arc::new(GitRepo::open(repo.to_path_buf(), exec.clone()).map_err(|e| e.to_string())?);
    let state_root = WorktreeManager::resolve_state_root(repo).map_err(|e| e.to_string())?;
    let worktrees = Arc::new(WorktreeManager::new(state_root.clone(), git.clone()));
    let store = Arc::new(Store::open(repo.join(".agentforge"), state_root));
    Ok(CliDeps {
        exec,
        git,
        worktrees,
        store,
    })
}

/// Constructs the shared dependencies a workspace command needs from a bare repo path — this
/// is `cli`'s job per docs/ARCHITECTURE.md §14 ("no other module constructs its own
/// dependencies"). Distinguishes application-level checks (is `repo` a real git repository at
/// all) from anything OS-level — there is no sandboxing here, only the same validated-path and
/// validated-git-state checks `GitRepo::open`/`WorktreeManager` already enforce.
fn build_workspace_manager(repo: &Path) -> std::result::Result<WorkspaceManager, String> {
    let exec: Arc<dyn Executor> = Arc::new(SystemExecutor::new());
    let git = Arc::new(GitRepo::open(repo.to_path_buf(), exec.clone()).map_err(|e| e.to_string())?);
    let state_root = WorktreeManager::resolve_state_root(repo).map_err(|e| e.to_string())?;
    let worktrees = Arc::new(WorktreeManager::new(state_root, git.clone()));
    Ok(WorkspaceManager::new(git, worktrees, exec))
}

/// Every command function returns `ExitCode` directly (clap dispatch expects it, not a
/// `Result`), so a `build_deps`/`build_workspace_manager` failure can't propagate via `?` —
/// this is the one place that early-return shape lives, used at every call site instead of
/// copy-pasting the same four-line match.
macro_rules! deps_or_exit {
    ($repo:expr) => {
        match build_deps($repo) {
            Ok(deps) => deps,
            Err(e) => return report_setup_error(&e),
        }
    };
}

macro_rules! workspace_manager_or_exit {
    ($repo:expr) => {
        match build_workspace_manager($repo) {
            Ok(mgr) => mgr,
            Err(e) => return report_setup_error(&e),
        }
    };
}

/// Reads and TOML-deserializes a caller-supplied spec/policy/weights file — the "read file, then
/// `toml::from_str` it, mapping both errors to the same setup-error shape" pattern every
/// `--file`/`--spec` CLI arg shares.
fn load_toml_arg<T: serde::de::DeserializeOwned>(path: &Path) -> std::result::Result<T, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    toml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

macro_rules! load_toml_or_exit {
    ($path:expr) => {
        match load_toml_arg($path) {
            Ok(v) => v,
            Err(e) => return report_setup_error(&e),
        }
    };
}

/// The "refuse a collision unless `--force`" rule every `add`/`inject`/`apply`-style command
/// enforces before persisting a new `Store` record under a caller-chosen id.
macro_rules! reject_if_exists {
    ($force:expr, $noun:literal, $exists:expr, $id:expr) => {
        if !$force && $exists {
            eprintln!("error: {} {} already exists (use --force)", $noun, $id);
            return ExitCode::from(2);
        }
    };
}

// ---- init -----------------------------------------------------------------------------------

/// Must be run inside a git repo. Fails (exit 2) if `.agentforge/` already exists. Scaffolds the
/// tracked spec directory (SPEC.md §5) and resolves — without requiring a prior `init` — the
/// external state root every other command already computes via `WorktreeManager::
/// resolve_state_root`, then caches it in `config.toml` so it's visible via a plain `cat` (§5).
fn init_cmd(args: InitArgs) -> ExitCode {
    let exec: Arc<dyn Executor> = Arc::new(SystemExecutor::new());
    if let Err(e) = GitRepo::open(args.repo.to_path_buf(), exec) {
        return report_setup_error(&format!(
            "{} is not a git repository: {e}",
            args.repo.display()
        ));
    }

    let agentforge_dir = args.repo.join(".agentforge");
    if agentforge_dir.exists() {
        return report_setup_error(&format!("{} already exists", agentforge_dir.display()));
    }
    let state_root = match WorktreeManager::resolve_state_root(&args.repo) {
        Ok(p) => p,
        Err(e) => return report_setup_error(&e.to_string()),
    };

    for dir in ["tasks", "policies", "evaluators"] {
        if let Err(e) = std::fs::create_dir_all(agentforge_dir.join(dir)) {
            return report_setup_error(&format!("creating {dir}/: {e}"));
        }
    }
    if let Err(e) = std::fs::create_dir_all(&state_root) {
        return report_setup_error(&format!("creating state root: {e}"));
    }

    let config_toml = format!("state_root = {:?}\n", state_root.display().to_string());
    if let Err(e) = std::fs::write(agentforge_dir.join("config.toml"), config_toml) {
        return report_setup_error(&format!("writing config.toml: {e}"));
    }
    let scoring_toml =
        toml::to_string_pretty(&scoring::default_weights()).expect("ScoringWeights serializes");
    if let Err(e) = std::fs::write(agentforge_dir.join("scoring.toml"), scoring_toml) {
        return report_setup_error(&format!("writing scoring.toml: {e}"));
    }

    if args.json {
        print_json_line(&serde_json::json!({
            "agentforge_dir": agentforge_dir.display().to_string(),
            "state_root": state_root.display().to_string(),
        }));
    } else {
        println!("initialized {}", agentforge_dir.display());
        println!("state_root:  {}", state_root.display());
    }
    ExitCode::SUCCESS
}

// ---- workspace --------------------------------------------------------------------------------

fn dispatch_workspace(action: WorkspaceAction) -> ExitCode {
    match action {
        WorkspaceAction::Create(args) => workspace_create(args),
        WorkspaceAction::List(args) => workspace_list(args),
        WorkspaceAction::Show(args) => workspace_show(args),
        WorkspaceAction::Exec(args) => workspace_exec(args),
        WorkspaceAction::Remove(args) => workspace_remove(args),
        WorkspaceAction::Clean(args) => workspace_clean(args),
    }
}

fn workspace_create(args: WorkspaceCreateArgs) -> ExitCode {
    let mgr = workspace_manager_or_exit!(&args.repo);
    match mgr.create(&args.id, &args.base) {
        Ok(info) => {
            println!("created workspace {} at {}", info.id, info.path.display());
            println!("  head: {}", info.head);
            ExitCode::SUCCESS
        }
        Err(e) => report_workspace_error(&e),
    }
}

fn workspace_list(args: WorkspaceRepoArgs) -> ExitCode {
    let mgr = workspace_manager_or_exit!(&args.repo);
    match mgr.list() {
        Ok(workspaces) => {
            if workspaces.is_empty() {
                println!("no workspaces");
            }
            for w in workspaces {
                println!(
                    "{}\t{}\t{}\t{}",
                    w.id,
                    w.path.display(),
                    &w.head[..w.head.len().min(12)],
                    if w.locked { "locked" } else { "idle" }
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_workspace_error(&e),
    }
}

fn workspace_show(args: WorkspaceIdArgs) -> ExitCode {
    let mgr = workspace_manager_or_exit!(&args.repo);
    match mgr.show(&args.id) {
        Ok(info) => {
            println!("id:     {}", info.id);
            println!("path:   {}", info.path.display());
            println!("head:   {}", info.head);
            println!("locked: {}", info.locked);
            if let Ok(log_path) = mgr.audit_log_path(&args.id) {
                println!("audit:  {}", log_path.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_workspace_error(&e),
    }
}

fn workspace_exec(args: WorkspaceExecArgs) -> ExitCode {
    let mgr = workspace_manager_or_exit!(&args.repo);
    let Some((program, rest)) = args.command.split_first() else {
        eprintln!("error: no command given after `--`");
        return ExitCode::from(2);
    };
    let spec = ProcessSpec {
        program: program.clone(),
        args: rest.to_vec(),
        extra_env: vec![],
    };
    // Not a named, persisted policy — an ad hoc PermissionPolicy built straight from this
    // invocation's own flags, distinct from `run`'s `--policy <name>` lookup.
    let policy = PermissionPolicy {
        name: "cli-adhoc".to_string(),
        extra_readonly_paths: vec![],
        deny_network: true,
        env_passthrough: args.env_passthrough.clone(),
        max_wall_time_secs: args.timeout_secs,
        max_output_bytes: args.max_output_bytes,
        allowed_programs: args.allowed_programs.clone(),
        denied_programs: args.denied_programs.clone(),
        allowed_roots: args.allowed_roots.clone(),
        max_memory_bytes: None,
    };
    match mgr.exec(&args.id, spec, &policy) {
        Ok(outcome) => {
            eprintln!("stdout: {}", outcome.stdout_path.display());
            eprintln!("stderr: {}", outcome.stderr_path.display());
            if outcome.timed_out {
                eprintln!("error: command exceeded its {}s timeout", args.timeout_secs);
                return ExitCode::from(124);
            }
            match outcome.exit_code {
                Some(code) => exit_code_from_child(code),
                None => {
                    eprintln!("error: command terminated without an exit code (killed by signal)");
                    ExitCode::from(1)
                }
            }
        }
        Err(e) => report_workspace_error(&e),
    }
}

fn workspace_remove(args: WorkspaceRemoveArgs) -> ExitCode {
    let mgr = workspace_manager_or_exit!(&args.repo);
    match mgr.remove(&args.id, args.force) {
        Ok(()) => {
            println!("removed workspace {}", args.id);
            ExitCode::SUCCESS
        }
        Err(e) => report_workspace_error(&e),
    }
}

fn workspace_clean(args: WorkspaceCleanArgs) -> ExitCode {
    let mgr = workspace_manager_or_exit!(&args.repo);
    match mgr.clean(args.force) {
        Ok(removed) => {
            if removed.is_empty() {
                println!("nothing to clean");
            } else {
                for id in &removed {
                    println!("removed workspace {id}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_workspace_error(&e),
    }
}

// ---- evaluator ----------------------------------------------------------------------------

fn dispatch_evaluator(action: EvaluatorAction) -> ExitCode {
    match action {
        EvaluatorAction::Add(args) => evaluator_add(args),
        EvaluatorAction::List(args) => evaluator_list(args),
        EvaluatorAction::Show(args) => evaluator_show(args),
    }
}

fn evaluator_add(args: AddFileArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let spec: EvaluatorSpec = load_toml_or_exit!(&args.file);
    if let Err(msg) = spec.validate_fields() {
        return report_setup_error(&msg);
    }
    match deps.store.save_evaluator(&spec, args.force) {
        Ok(()) => {
            println!("added evaluator {}", spec.id);
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn evaluator_list(args: RepoArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    match deps.store.list_evaluators() {
        Ok(ids) => {
            if ids.is_empty() {
                println!("no evaluators");
            }
            for id in ids {
                println!("{id}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn evaluator_show(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    match deps.store.load_evaluator(&args.id) {
        Ok(spec) => {
            println!("id:               {}", spec.id);
            println!("timeout_secs:     {}", spec.timeout_secs);
            println!("budget_secs:      {}", spec.budget_secs);
            println!("size_budget_lines:{}", spec.size_budget_lines);
            println!("setup_cmds:       {}", spec.setup_cmds.len());
            println!(
                "test_cmd:         {} {}",
                spec.test_cmd.program,
                spec.test_cmd.args.join(" ")
            );
            println!("metric_extractors:{}", spec.metric_extractors.len());
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

// ---- task -----------------------------------------------------------------------------------

fn dispatch_task(action: TaskAction) -> ExitCode {
    match action {
        TaskAction::Add(args) => task_add(args),
        TaskAction::List(args) => task_list(args),
        TaskAction::Show(args) => task_show(args),
    }
}

fn task_add(args: AddFileArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let mut task: TaskSpec = load_toml_or_exit!(&args.file);
    if task.prompt.trim().is_empty() {
        return report_setup_error("task.prompt must not be empty");
    }
    let evaluator_spec = match deps.store.load_evaluator(&task.evaluator) {
        Ok(spec) => spec,
        Err(e) => return report_setup_error(&format!("evaluator {:?}: {e}", task.evaluator)),
    };
    reject_if_exists!(
        args.force,
        "task",
        deps.store.load_task(&task.id).is_ok(),
        task.id
    );
    let resolved_base = match deps.git.resolve_commit(&task.base_ref) {
        Ok(sha) => sha,
        Err(e) => return report_setup_error(&e.to_string()),
    };
    task.base_ref = resolved_base.clone();

    match capture_baseline(&deps, &resolved_base, &evaluator_spec) {
        Ok(baseline) => task.baseline = baseline,
        Err(e) => return report_setup_error(&e.to_string()),
    }
    task.created_at = chrono::Utc::now();

    match deps.store.save_task(&task, args.force) {
        Ok(()) => {
            println!("created task {}", task.id);
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

/// The same worktree-create → evaluate → worktree-remove path `experiment mutation create`'s
/// sanity gate uses (`MutationEngine::sanity_check`), reused here directly since `task add`'s
/// baseline capture has no `MutationRef` to go through `MutationEngine` for — SPEC.md §11.
fn capture_baseline(
    deps: &CliDeps,
    commit: &str,
    evaluator_spec: &EvaluatorSpec,
) -> crate::error::Result<crate::domain::EvaluatorVerdict> {
    let handle = deps.worktrees.create_evaluation_worktree(commit)?;
    let evaluator = Evaluator::new(deps.exec.clone());
    let outcome = evaluator.evaluate(&handle.path, evaluator_spec, &crate::audit::NullAuditSink);
    let cleanup = deps.worktrees.remove(&handle);
    let verdict = outcome?;
    cleanup?;
    Ok(verdict)
}

fn task_list(args: RepoArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    match deps.store.list_tasks() {
        Ok(ids) => {
            if ids.is_empty() {
                println!("no tasks");
            }
            for id in ids {
                println!("{id}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn task_show(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    match deps.store.load_task(&args.id) {
        Ok(task) => {
            println!("id:        {}", task.id);
            println!("name:      {}", task.name);
            println!("base_ref:  {}", task.base_ref);
            println!("evaluator: {}", task.evaluator);
            println!(
                "baseline:  build_succeeded={} exit_code={} tests={:?}/{:?}",
                task.baseline.build_succeeded,
                task.baseline.exit_code,
                task.baseline.tests_passed,
                task.baseline.tests_total,
            );
            match &task.mutation {
                Some(m) => println!(
                    "mutation:  {:?} at {}:{}:{} (seed={})",
                    m.spec.operator,
                    m.selected_target.file,
                    m.selected_target.line,
                    m.selected_target.column,
                    m.spec.seed
                ),
                None => println!("mutation:  none"),
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

// ---- experiment: fault / mutation / mutant -------------------------------------------------

fn dispatch_experiment(action: ExperimentAction) -> ExitCode {
    match action {
        ExperimentAction::Fault { action } => dispatch_fault(action),
        ExperimentAction::Mutation { action } => dispatch_mutation(action),
        ExperimentAction::Mutant { action } => dispatch_mutant(action),
    }
}

/// SPEC.md §10: applies a `MutationSpec` on top of `--base`, sanity-gates it, and — only if the
/// fault was detected (verdict not good) — creates `--task-id`. An undetectable fault (good
/// verdict) is rejected: no task is created, and the mutant is re-homed under
/// `refs/agentforge/mutants/rejected/<id>` for inspection rather than left under the task-id ref
/// no task will ever reference.
fn mutation_create(args: MutationCreateArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let evaluator_spec = match deps.store.load_evaluator(&args.evaluator) {
        Ok(spec) => spec,
        Err(e) => return report_setup_error(&format!("evaluator {:?}: {e}", args.evaluator)),
    };
    let spec: MutationSpec = load_toml_or_exit!(&args.spec);
    reject_if_exists!(
        args.force,
        "task",
        deps.store.load_task(&args.task_id).is_ok(),
        args.task_id
    );
    let base_commit = match deps.git.resolve_commit(&args.base) {
        Ok(sha) => sha,
        Err(e) => return report_setup_error(&e.to_string()),
    };

    let evaluator = Arc::new(Evaluator::new(deps.exec.clone()));
    let engine = MutationEngine::new(deps.git.clone(), deps.worktrees.clone(), evaluator);

    let mut mutation_ref = match engine.apply(&base_commit, &spec, &args.task_id) {
        Ok(m) => m,
        Err(e) => return report_mutation_error(&e),
    };
    let verdict = match engine.sanity_check(&mutation_ref, &evaluator_spec) {
        Ok(v) => v,
        Err(e) => return report_mutation_error(&e),
    };

    if verdict.is_good() {
        let rejected_ref = format!(
            "refs/agentforge/mutants/rejected/{}",
            crate::domain::ids::new_id(chrono::Utc::now())
        );
        if let Err(e) = deps
            .git
            .update_ref(&rejected_ref, &mutation_ref.mutant_commit)
        {
            return report_setup_error(&e.to_string());
        }
        let _ = deps.git.delete_ref(&mutation_ref.mutant_ref);
        eprintln!(
            "error: mutation rejected — fault undetectable by evaluator {:?}; mutant left at {rejected_ref} ({}) for inspection",
            args.evaluator, mutation_ref.mutant_commit
        );
        return ExitCode::from(2);
    }

    mutation_ref.sanity_checked = true;
    let task = TaskSpec {
        id: args.task_id.clone(),
        name: args.task_id.clone(),
        prompt: format!(
            "A fault was injected via {:?} at {}:{}:{}. Find and fix it so the evaluator's tests pass again.",
            spec.operator,
            mutation_ref.selected_target.file,
            mutation_ref.selected_target.line,
            mutation_ref.selected_target.column
        ),
        repo_path: args.repo.clone(),
        base_ref: mutation_ref.mutant_commit.clone(),
        mutation: Some(mutation_ref.clone()),
        evaluator: args.evaluator.clone(),
        agent_timeout_secs: DEFAULT_MUTATION_AGENT_TIMEOUT_SECS,
        baseline: verdict,
        created_at: chrono::Utc::now(),
    };
    match deps.store.save_task(&task, args.force) {
        Ok(()) => {
            println!(
                "created task {} (mutant {} at {}:{}:{})",
                task.id,
                &mutation_ref.mutant_commit[..mutation_ref.mutant_commit.len().min(12)],
                mutation_ref.selected_target.file,
                mutation_ref.selected_target.line,
                mutation_ref.selected_target.column
            );
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn dispatch_mutation(action: MutationAction) -> ExitCode {
    match action {
        MutationAction::Create(args) => mutation_create(args),
        MutationAction::Show(args) => mutation_show(args),
        MutationAction::Replay(args) => mutation_replay(args),
    }
}

fn mutation_show(args: MutationTaskArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let task = match deps.store.load_task(&args.task_id) {
        Ok(task) => task,
        Err(e) => return report_store_error(&e),
    };
    let Some(m) = &task.mutation else {
        eprintln!("error: task {} has no mutation record", args.task_id);
        return ExitCode::from(2);
    };
    println!("task:             {}", task.id);
    println!("operator:         {:?}", m.spec.operator);
    println!("target_glob:      {}", m.spec.target_glob);
    println!("seed:             {}", m.spec.seed);
    println!("operator_version: {}", m.spec.operator_version);
    println!("base_commit:      {}", m.base_commit);
    println!("mutant_commit:    {}", m.mutant_commit);
    println!("mutant_ref:       {}", m.mutant_ref);
    println!(
        "selected_target:  {}:{}:{}",
        m.selected_target.file, m.selected_target.line, m.selected_target.column
    );
    println!(
        "diff:             +{} -{} across {} file(s)",
        m.diff_stats.lines_added, m.diff_stats.lines_removed, m.diff_stats.files_changed
    );
    println!("sanity_checked:   {}", m.sanity_checked);
    println!("applied_at:       {}", m.applied_at);
    ExitCode::SUCCESS
}

fn mutation_replay(args: MutationTaskArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let task = match deps.store.load_task(&args.task_id) {
        Ok(task) => task,
        Err(e) => return report_store_error(&e),
    };
    let Some(original) = task.mutation.clone() else {
        eprintln!(
            "error: task {} has no mutation record — nothing to replay",
            args.task_id
        );
        return ExitCode::from(2);
    };

    let evaluator = Arc::new(Evaluator::new(deps.exec.clone()));
    let engine = MutationEngine::new(deps.git.clone(), deps.worktrees.clone(), evaluator);
    // A distinct, throwaway ref name so replay never clobbers the original task's own
    // `mutant_ref` — deleted again immediately after, regardless of outcome.
    let replay_task_id = format!(
        "{}-replay-{}",
        args.task_id,
        crate::domain::ids::new_id(chrono::Utc::now())
    );
    let replayed = match engine.apply(&original.base_commit, &original.spec, &replay_task_id) {
        Ok(r) => r,
        Err(e) => return report_mutation_error(&e),
    };
    let _ = deps.git.delete_ref(&replayed.mutant_ref);

    if replayed.mutant_commit == original.mutant_commit
        && replayed.selected_target == original.selected_target
        && replayed.diff_stats == original.diff_stats
    {
        println!(
            "replay OK: task {} reproduces mutant {}",
            args.task_id,
            &original.mutant_commit[..original.mutant_commit.len().min(12)]
        );
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "error: replay mismatch for task {} — original mutant {} vs replayed {}",
            args.task_id, original.mutant_commit, replayed.mutant_commit
        );
        ExitCode::from(1)
    }
}

fn dispatch_fault(action: FaultAction) -> ExitCode {
    match action {
        FaultAction::Inject(args) => fault_inject(args),
        FaultAction::Show(args) => fault_show(args),
        FaultAction::Restore(args) => fault_restore(args),
        FaultAction::Discard(args) => fault_discard(args),
    }
}

fn fault_inject(args: FaultInjectArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let spec: FaultSpec = load_toml_or_exit!(&args.spec);
    reject_if_exists!(
        args.force,
        "fault",
        deps.store.load_fault(&args.id).is_ok(),
        args.id
    );
    let base_commit = match deps.git.resolve_commit(&args.base) {
        Ok(sha) => sha,
        Err(e) => return report_setup_error(&e.to_string()),
    };

    let injector = FaultInjector::new(deps.git.clone(), deps.worktrees.clone());
    let fault_ref = match injector.inject(&base_commit, &spec, &args.id) {
        Ok(f) => f,
        Err(e) => return report_fault_error(&e),
    };

    match deps.store.save_fault(&fault_ref, args.force) {
        Ok(()) => {
            println!(
                "injected fault {} ({:?} at {})",
                fault_ref.id, fault_ref.spec.kind, fault_ref.selected_target.file
            );
            println!("  {}", fault_ref.description);
            println!("  workspace: {}", fault_ref.worktree_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn fault_show(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    match deps.store.load_fault(&args.id) {
        Ok(f) => {
            println!("id:            {}", f.id);
            println!("kind:          {:?}", f.spec.kind);
            println!("target_glob:   {}", f.spec.target_glob);
            println!("seed:          {}", f.spec.seed);
            println!("fault_version: {}", f.spec.fault_version);
            println!("base_commit:   {}", f.base_commit);
            println!(
                "target:        {}{}",
                f.selected_target.file,
                f.selected_target
                    .line
                    .map(|l| format!(":{l}"))
                    .unwrap_or_default()
            );
            println!("description:   {}", f.description);
            println!("workspace:     {}", f.worktree_path.display());
            println!(
                "diff:          +{} -{} across {} file(s)",
                f.diff_stats.lines_added, f.diff_stats.lines_removed, f.diff_stats.files_changed
            );
            println!("applied_at:    {}", f.applied_at);
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn fault_restore(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let fault_ref = match deps.store.load_fault(&args.id) {
        Ok(f) => f,
        Err(e) => return report_store_error(&e),
    };
    let injector = FaultInjector::new(deps.git.clone(), deps.worktrees.clone());
    match injector.restore(&fault_ref) {
        Ok(()) => {
            println!(
                "restored {} in workspace {}",
                fault_ref.selected_target.file,
                fault_ref.worktree_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => report_fault_error(&e),
    }
}

fn fault_discard(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let fault_ref = match deps.store.load_fault(&args.id) {
        Ok(f) => f,
        Err(e) => return report_store_error(&e),
    };
    let injector = FaultInjector::new(deps.git.clone(), deps.worktrees.clone());
    match injector.discard(&fault_ref) {
        Ok(()) => {
            println!(
                "discarded fault workspace {}",
                fault_ref.worktree_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => report_fault_error(&e),
    }
}

fn dispatch_mutant(action: MutantAction) -> ExitCode {
    match action {
        MutantAction::Apply(args) => mutant_apply(args),
        MutantAction::Show(args) => mutant_show(args),
        MutantAction::Evaluate(args) => mutant_evaluate(args),
        MutantAction::Restore(args) => mutant_restore(args),
        MutantAction::Discard(args) => mutant_discard(args),
    }
}

fn mutant_apply(args: MutantApplyArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let spec: MutantSpec = load_toml_or_exit!(&args.spec);
    reject_if_exists!(
        args.force,
        "mutant",
        deps.store.load_mutant(&args.id).is_ok(),
        args.id
    );
    let base_commit = match deps.git.resolve_commit(&args.base) {
        Ok(sha) => sha,
        Err(e) => return report_setup_error(&e.to_string()),
    };

    let evaluator = Arc::new(Evaluator::new(deps.exec.clone()));
    let tester = MutantTester::new(deps.git.clone(), deps.worktrees.clone(), evaluator);
    let mutant_ref = match tester.apply(&base_commit, &spec, &args.id) {
        Ok(m) => m,
        Err(e) => return report_mutant_error(&e),
    };

    match deps.store.save_mutant(&mutant_ref, args.force) {
        Ok(()) => {
            println!(
                "applied mutant {} ({:?} at {}:{}:{})",
                mutant_ref.id,
                mutant_ref.spec.operator,
                mutant_ref.selected_target.file,
                mutant_ref.selected_target.line,
                mutant_ref.selected_target.column
            );
            println!("  {}", mutant_ref.description);
            println!("  workspace: {}", mutant_ref.worktree_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn mutant_show(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    match deps.store.load_mutant(&args.id) {
        Ok(m) => {
            println!("id:               {}", m.id);
            println!("operator:         {:?}", m.spec.operator);
            println!("target_glob:      {}", m.spec.target_glob);
            println!("seed:             {}", m.spec.seed);
            println!("operator_version: {}", m.spec.operator_version);
            println!("base_commit:      {}", m.base_commit);
            println!(
                "selected_target:  {}:{}:{}",
                m.selected_target.file, m.selected_target.line, m.selected_target.column
            );
            println!("description:      {}", m.description);
            println!("workspace:        {}", m.worktree_path.display());
            println!(
                "diff:             +{} -{} across {} file(s)",
                m.diff_stats.lines_added, m.diff_stats.lines_removed, m.diff_stats.files_changed
            );
            println!("applied_at:       {}", m.applied_at);
            match &m.evaluation {
                None => println!("evaluation:    none"),
                Some(ev) => println!(
                    "evaluation:    build_succeeded={} exit_code={} tests={:?}/{:?} evaluated_at={}",
                    ev.verdict.build_succeeded,
                    ev.verdict.exit_code,
                    ev.verdict.tests_passed,
                    ev.verdict.tests_total,
                    ev.evaluated_at
                ),
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

/// Runs the evaluator directly against the mutant's already-materialized workspace (no new
/// worktree created or removed — `mutant apply` left it alive for exactly this), records the
/// verdict onto the persisted `MutantRef`, and writes a real per-mutant `JsonlAuditSink` trail
/// under `<state_root>/mutants/<id>/audit.jsonl` — unlike `mutation create`'s sanity check, which
/// uses `NullAuditSink` for its throwaway evaluation worktree. Never rejects: SURVIVED is a
/// reported outcome, not a command failure.
fn mutant_evaluate(args: MutantEvaluateArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let mut mutant_ref = match deps.store.load_mutant(&args.id) {
        Ok(m) => m,
        Err(e) => return report_store_error(&e),
    };
    let evaluator_spec = match deps.store.load_evaluator(&args.evaluator) {
        Ok(spec) => spec,
        Err(e) => return report_setup_error(&format!("evaluator {:?}: {e}", args.evaluator)),
    };

    let audit_path = deps
        .worktrees
        .state_root()
        .join("mutants")
        .join(&args.id)
        .join("audit.jsonl");
    let audit = match crate::audit::JsonlAuditSink::open(audit_path.clone()) {
        Ok(sink) => sink,
        Err(e) => return report_setup_error(&e.to_string()),
    };

    let evaluator = Arc::new(Evaluator::new(deps.exec.clone()));
    let tester = MutantTester::new(deps.git.clone(), deps.worktrees.clone(), evaluator);
    let verdict = match tester.evaluate(&mutant_ref, &evaluator_spec, &audit) {
        Ok(v) => v,
        Err(e) => return report_mutant_error(&e),
    };

    mutant_ref.evaluation = Some(MutantEvaluation {
        verdict: verdict.clone(),
        evaluated_at: chrono::Utc::now(),
    });

    match deps.store.save_mutant(&mutant_ref, true) {
        Ok(()) => {
            println!(
                "evaluated mutant {} — {}",
                mutant_ref.id,
                if verdict.is_good() {
                    "SURVIVED (not detected)"
                } else {
                    "KILLED (detected)"
                }
            );
            println!(
                "  build_succeeded={} exit_code={} tests={:?}/{:?}",
                verdict.build_succeeded,
                verdict.exit_code,
                verdict.tests_passed,
                verdict.tests_total
            );
            println!("  audit: {}", audit_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn mutant_restore(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let mutant_ref = match deps.store.load_mutant(&args.id) {
        Ok(m) => m,
        Err(e) => return report_store_error(&e),
    };
    let evaluator = Arc::new(Evaluator::new(deps.exec.clone()));
    let tester = MutantTester::new(deps.git.clone(), deps.worktrees.clone(), evaluator);
    match tester.restore(&mutant_ref) {
        Ok(()) => {
            println!(
                "restored {} in workspace {}",
                mutant_ref.selected_target.file,
                mutant_ref.worktree_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => report_mutant_error(&e),
    }
}

fn mutant_discard(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let mutant_ref = match deps.store.load_mutant(&args.id) {
        Ok(m) => m,
        Err(e) => return report_store_error(&e),
    };
    let evaluator = Arc::new(Evaluator::new(deps.exec.clone()));
    let tester = MutantTester::new(deps.git.clone(), deps.worktrees.clone(), evaluator);
    match tester.discard(&mutant_ref) {
        Ok(()) => {
            println!(
                "discarded mutant workspace {}",
                mutant_ref.worktree_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => report_mutant_error(&e),
    }
}

// ---- run / race / bisect ----------------------------------------------------------------------

/// Parses `adapter[:model]` — SPEC.md §6's `run --agent`/`race --agents` shape.
fn parse_agent_spec(spec: &str) -> (String, Option<String>) {
    match spec.split_once(':') {
        Some((adapter, model)) => (adapter.to_string(), Some(model.to_string())),
        None => (spec.to_string(), None),
    }
}

fn run_cmd(args: RunArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let task = match deps.store.load_task(&args.task) {
        Ok(t) => t,
        Err(e) => return report_store_error(&e),
    };
    let (adapter_name, model) = parse_agent_spec(&args.agent);
    let adapter = match crate::adapter::resolve(&adapter_name) {
        Ok(a) => a,
        Err(e) => return report_setup_error(&e.to_string()),
    };
    let policy = match &args.policy {
        Some(name) => match deps.store.load_policy(name) {
            Ok(p) => p,
            Err(e) => return report_store_error(&e),
        },
        None => PermissionPolicy::generous_default("cli-default"),
    };
    let agent_config = crate::domain::AgentConfig {
        adapter: adapter_name,
        model,
        extra_args: vec![],
        policy: policy.name.clone(),
    };

    let evaluator = Arc::new(Evaluator::new(deps.exec.clone()));
    let runner = ExperimentRunner::new(
        deps.git.clone(),
        deps.worktrees.clone(),
        deps.exec.clone(),
        evaluator,
        deps.store.clone(),
    );
    let result = if args.keep_worktree_on_fail {
        runner.run_keep_worktree_on_fail(&task, adapter.as_ref(), &agent_config, &policy)
    } else {
        runner.run(&task, adapter.as_ref(), &agent_config, &policy)
    };
    report_run_result(&deps, result, args.json)
}

fn report_run_result(
    deps: &CliDeps,
    result: crate::experiment::Result<crate::domain::ExperimentRecord>,
    json: bool,
) -> ExitCode {
    match result {
        Ok(record) => {
            let reporter = Reporter::new(deps.store.as_ref());
            if json {
                match reporter.experiment_json(&record.id) {
                    Ok(value) => print_json_line(&value),
                    Err(e) => return report_report_error(&e),
                }
            } else {
                match reporter.render_experiment(&record.id, false) {
                    Ok(text) => print!("{text}"),
                    Err(e) => return report_report_error(&e),
                }
            }
            experiment_exit_code(&record)
        }
        Err(e) => report_experiment_error(&e),
    }
}

/// SPEC.md §6's `run` precedence table, checked top-to-bottom: `Failed`→1, `TimedOut`→124,
/// `Completed` with a gated (bad) score→3, `Completed` with an ungated (good) score→0.
fn experiment_exit_code(record: &crate::domain::ExperimentRecord) -> ExitCode {
    match record.status {
        ExperimentStatus::Failed => ExitCode::from(1),
        ExperimentStatus::TimedOut => ExitCode::from(124),
        ExperimentStatus::Running => ExitCode::from(1),
        ExperimentStatus::Completed => {
            let good = record.score.as_ref().map(|s| !s.gated).unwrap_or(false);
            if good {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            }
        }
    }
}

fn report_experiment_error(e: &crate::experiment::Error) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::from(1)
}

fn race_cmd(args: RaceArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let task = match deps.store.load_task(&args.task) {
        Ok(t) => t,
        Err(e) => return report_store_error(&e),
    };
    if args.agents.is_empty() {
        return report_setup_error("--agents must list at least one adapter[:model]");
    }

    let mut agents = Vec::with_capacity(args.agents.len());
    for spec in &args.agents {
        let (adapter_name, model) = parse_agent_spec(spec);
        let adapter: Arc<dyn crate::adapter::AgentAdapter> =
            match crate::adapter::resolve(&adapter_name) {
                Ok(a) => Arc::from(a),
                Err(e) => return report_setup_error(&e.to_string()),
            };
        let agent_config = crate::domain::AgentConfig {
            adapter: adapter_name,
            model,
            extra_args: vec![],
            policy: "race-default".to_string(),
        };
        agents.push((agent_config, adapter));
    }

    let evaluator = Arc::new(Evaluator::new(deps.exec.clone()));
    let runner = Arc::new(ExperimentRunner::new(
        deps.git.clone(),
        deps.worktrees.clone(),
        deps.exec.clone(),
        evaluator,
        deps.store.clone(),
    ));
    let race_runner = RaceRunner::new(runner, deps.store.clone());
    let repeat = args.repeat.max(1);
    // Unbounded (every participant at once) when `--max-parallel` is omitted.
    let max_parallel = args
        .max_parallel
        .unwrap_or(agents.len() as u32 * repeat)
        .max(1);

    match race_runner.run_race(&task, &agents, repeat, max_parallel) {
        Ok(record) => report_race_result(&deps, &record, args.json),
        Err(e) => report_race_error(&e),
    }
}

fn report_race_result(deps: &CliDeps, record: &crate::domain::RaceRecord, json: bool) -> ExitCode {
    let reporter = Reporter::new(deps.store.as_ref());
    if json {
        match reporter.race_json(&record.id) {
            Ok(value) => print_json_line(&value),
            Err(e) => return report_report_error(&e),
        }
    } else {
        match reporter.render_race(&record.id, false) {
            Ok(text) => print!("{text}"),
            Err(e) => return report_report_error(&e),
        }
    }
    // SPEC.md §6: exit 0 if at least one participant reached `Completed`, else 1.
    let any_completed = record.participants.iter().any(|(experiment_id, _)| {
        deps.store
            .load_experiment(experiment_id)
            .map(|r| r.status == ExperimentStatus::Completed)
            .unwrap_or(false)
    });
    if any_completed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn report_race_error(e: &crate::race::Error) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::from(1)
}

fn bisect_cmd(args: BisectArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let task = match deps.store.load_task(&args.task) {
        Ok(t) => t,
        Err(e) => return report_store_error(&e),
    };
    let Some((good, bad)) = args.range.split_once("..") else {
        return report_setup_error("--range must be in the form <good>..<bad>");
    };

    let evaluator = Arc::new(Evaluator::new(deps.exec.clone()));
    let runner = crate::bisect::BisectRunner::new(
        deps.git.clone(),
        deps.worktrees.clone(),
        evaluator,
        deps.store.clone(),
    );
    match runner.run_bisect(&task, good, bad) {
        Ok(record) => report_bisect_result(&deps, &record, args.json),
        Err(e) => report_bisect_error(&e),
    }
}

fn report_bisect_result(
    deps: &CliDeps,
    record: &crate::domain::BisectRecord,
    json: bool,
) -> ExitCode {
    let reporter = Reporter::new(deps.store.as_ref());
    if json {
        match reporter.bisect_json(&record.id) {
            Ok(value) => print_json_line(&value),
            Err(e) => return report_report_error(&e),
        }
    } else {
        match reporter.render_bisect(&record.id) {
            Ok(text) => print!("{text}"),
            Err(e) => return report_report_error(&e),
        }
    }
    // SPEC.md §6: exit 0 with a culprit found, exit 3 if the whole range shares one verdict.
    if record.culprit.is_some() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    }
}

fn report_bisect_error(e: &crate::bisect::Error) -> ExitCode {
    eprintln!("error: {e}");
    match e {
        crate::bisect::Error::NotLinear { .. } => ExitCode::from(2),
        _ => ExitCode::from(1),
    }
}

// ---- verify -----------------------------------------------------------------------------------

fn verify_cmd(args: VerifyArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);

    let outcome = if let Some(experiment_id) = &args.experiment {
        verify_experiment(&deps, experiment_id)
    } else {
        let (Some(evaluator_id), Some(r#ref)) = (&args.evaluator, &args.r#ref) else {
            return report_setup_error(
                "--evaluator and --ref are both required unless --experiment is given",
            );
        };
        verify_ref(&deps, evaluator_id, r#ref, args.apply_patch.as_deref())
    };

    match outcome {
        Ok(verdict) => {
            if args.json {
                print_json_line(&serde_json::to_value(&verdict).expect("verdict serializes"));
            } else {
                print!("{}", format_verdict(&verdict));
            }
            if verdict.is_good() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(3)
            }
        }
        Err(e) => report_agentforge_error(&e),
    }
}

fn verify_ref(
    deps: &CliDeps,
    evaluator_id: &str,
    r#ref: &str,
    patch: Option<&Path>,
) -> crate::error::Result<EvaluatorVerdict> {
    let evaluator_spec = deps.store.load_evaluator(evaluator_id)?;
    let base = deps.git.resolve_commit(r#ref)?;
    let handle = deps.worktrees.create_evaluation_worktree(&base)?;
    let result = run_verify_evaluation(deps, &handle.path, patch, &evaluator_spec);
    let _ = deps.worktrees.remove(&handle);
    result
}

fn verify_experiment(
    deps: &CliDeps,
    experiment_id: &str,
) -> crate::error::Result<EvaluatorVerdict> {
    let record = deps.store.load_experiment(experiment_id)?;
    let task = deps.store.load_task(&record.task_id)?;
    let evaluator_spec = deps.store.load_evaluator(&task.evaluator)?;
    let handle = deps
        .worktrees
        .create_evaluation_worktree(&record.base_ref)?;
    let result = run_verify_evaluation(
        deps,
        &handle.path,
        Some(record.patch_path.as_path()),
        &evaluator_spec,
    );
    let _ = deps.worktrees.remove(&handle);
    result
}

/// Applies `patch` (if given and non-empty) inside `worktree_path`, then runs one standalone
/// `evaluate()` call — the shared body behind both `verify` shapes (SPEC.md §6's `--ref` and
/// `--experiment` forms), against a throwaway evaluation worktree (§7's third flavor). Uses
/// `NullAuditSink` — no `ExperimentRecord` exists for this call to attach an audit trail to,
/// mirroring `task add`'s baseline capture and `experiment mutation create`'s sanity gate.
fn run_verify_evaluation(
    deps: &CliDeps,
    worktree_path: &Path,
    patch: Option<&Path>,
    evaluator_spec: &EvaluatorSpec,
) -> crate::error::Result<EvaluatorVerdict> {
    if let Some(patch_path) = patch {
        let raw = std::fs::read_to_string(patch_path).map_err(|e| {
            crate::error::AgentForgeError::Validation(format!(
                "reading {}: {e}",
                patch_path.display()
            ))
        })?;
        if !raw.trim().is_empty() {
            deps.git.apply_patch(worktree_path, &raw)?;
        }
    }
    let evaluator = Evaluator::new(deps.exec.clone());
    Ok(evaluator.evaluate(worktree_path, evaluator_spec, &crate::audit::NullAuditSink)?)
}

fn format_verdict(verdict: &EvaluatorVerdict) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Build           {}\n",
        if verdict.build_succeeded {
            "succeeded"
        } else {
            "FAILED"
        }
    ));
    out.push_str(&format!(
        "Tests           {}\n",
        match (verdict.tests_passed, verdict.tests_total) {
            (Some(p), Some(t)) => format!("{p}/{t}"),
            _ => "-".to_string(),
        }
    ));
    out.push_str(&format!("Exit code       {}\n", verdict.exit_code));
    out.push_str(&format!("Timed out       {}\n", verdict.timed_out));
    out.push_str(&format!(
        "Wall time       {}s\n",
        verdict.wall_time_secs.round() as i64
    ));
    out.push_str(&format!(
        "Verdict         {}\n",
        if verdict.is_good() { "GOOD" } else { "BAD" }
    ));
    out
}

fn report_agentforge_error(e: &crate::error::AgentForgeError) -> ExitCode {
    eprintln!("error: {e}");
    let code = match e {
        crate::error::AgentForgeError::Store(store::Error::NotFound(_)) => 2,
        crate::error::AgentForgeError::Validation(_) => 2,
        _ => 1,
    };
    ExitCode::from(code)
}

// ---- report: show / score / log --------------------------------------------------------------

fn dispatch_report(action: ReportAction) -> ExitCode {
    match action {
        ReportAction::Show(args) => report_show(args),
        ReportAction::Score(args) => report_score(args),
        ReportAction::Log(args) => report_log(args),
    }
}

/// Recomputes and prints a `ScoreCard` from persisted `RawMetrics` — spawns no process, touches
/// no worktree — SPEC.md §14/§6. `--weights` overrides the repo's `scoring.toml` (or the
/// built-in default); a `formula_version` mismatch is a warning, never a silent failure, since
/// weights stay portable numbers even when the formula's shape changes.
fn report_score(args: ScoreArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let record = match deps.store.load_experiment(&args.experiment_id) {
        Ok(r) => r,
        Err(e) => return report_store_error(&e),
    };
    let metrics = match &record.raw_metrics {
        Some(m) => m,
        None => {
            return report_setup_error(&format!(
                "experiment {} has not been evaluated yet — no raw metrics recorded",
                args.experiment_id
            ))
        }
    };
    let task = match deps.store.load_task(&record.task_id) {
        Ok(t) => t,
        Err(e) => return report_store_error(&e),
    };
    let evaluator = match deps.store.load_evaluator(&task.evaluator) {
        Ok(e) => e,
        Err(e) => return report_store_error(&e),
    };

    let weights = match &args.weights {
        Some(path) => {
            let mut weights: ScoringWeights = load_toml_or_exit!(path);
            weights.source = path.display().to_string();
            weights
        }
        None => match deps.store.load_scoring_weights() {
            Ok(w) => w,
            Err(e) => return report_store_error(&e),
        },
    };
    if weights.formula_version != scoring::FORMULA_VERSION {
        eprintln!(
            "warning: weights formula_version {:?} does not match this binary's {:?} — \
             proceeding anyway (weights are portable numbers even when the formula's shape \
             changes)",
            weights.formula_version,
            scoring::FORMULA_VERSION
        );
    }

    let card = scoring::score(metrics, &task.baseline, &evaluator, &weights);

    if args.json {
        let failed_checks = scoring::failed_checks(metrics, &task.baseline);
        let value = serde_json::json!({
            "experiment_id": record.id,
            "raw_metrics": metrics,
            "score": card,
            "thresholds": {
                "evaluator": {
                    "timeout_secs": evaluator.timeout_secs,
                    "budget_secs": evaluator.budget_secs,
                    "size_budget_lines": evaluator.size_budget_lines,
                },
                "rating_bands": weights.rating_bands,
            },
            "failed_checks": failed_checks,
        });
        print_json_line(&value);
    } else {
        print!(
            "{}",
            report::format_scorecard(
                metrics,
                Some(&task.baseline),
                Some(&evaluator),
                &card,
                args.verbose
            )
        );
    }
    ExitCode::SUCCESS
}

/// `agentforge report show <experiment-id|race-id|bisect-id>` — the id's collection is unknown
/// up front, so this tries `experiment`, then `race`, then `bisect`, in SPEC.md §6's own listed
/// order, falling through only on a "no such id here" `NotFound` and surfacing any other error
/// (a genuinely corrupt record) immediately rather than masking it behind a misleading final
/// "not found".
fn report_show(args: ShowArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let reporter = Reporter::new(deps.store.as_ref());

    if args.json {
        match reporter.experiment_json(&args.id) {
            Ok(value) => return print_json(&value),
            Err(e) if is_unknown_id(&e) => {}
            Err(e) => return report_report_error(&e),
        }
        match reporter.race_json(&args.id) {
            Ok(value) => return print_json(&value),
            Err(e) if is_unknown_id(&e) => {}
            Err(e) => return report_report_error(&e),
        }
        match reporter.bisect_json(&args.id) {
            Ok(value) => print_json(&value),
            Err(e) => report_report_error(&e),
        }
    } else {
        match reporter.render_experiment(&args.id, args.verbose) {
            Ok(text) => {
                print!("{text}");
                return ExitCode::SUCCESS;
            }
            Err(e) if is_unknown_id(&e) => {}
            Err(e) => return report_report_error(&e),
        }
        match reporter.render_race(&args.id, args.verbose) {
            Ok(text) => {
                print!("{text}");
                return ExitCode::SUCCESS;
            }
            Err(e) if is_unknown_id(&e) => {}
            Err(e) => return report_report_error(&e),
        }
        match reporter.render_bisect(&args.id) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(e) => report_report_error(&e),
        }
    }
}

fn report_log(args: LogArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let record = match deps.store.load_experiment(&args.experiment_id) {
        Ok(r) => r,
        Err(e) => return report_store_error(&e),
    };

    let mut printed = 0usize;
    loop {
        printed = print_new_audit_lines(&record.audit_log_path, printed);
        if !args.follow || !deps.worktrees.is_locked(&args.experiment_id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    ExitCode::SUCCESS
}

/// Prints every audit line beyond `already_printed`, falling back to the raw JSON line for
/// anything that doesn't parse as a known `AuditEvent` (forward-compat, never a hard failure)
/// — returns the new total line count so `report_log --follow` can resume from where it left off.
fn print_new_audit_lines(path: &Path, already_printed: usize) -> usize {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    for line in lines.iter().skip(already_printed) {
        match serde_json::from_str::<crate::domain::AuditEvent>(line) {
            Ok(event) => println!("{}", format_audit_event(&event)),
            Err(_) => println!("{line}"),
        }
    }
    lines.len()
}

fn format_audit_event(event: &crate::domain::AuditEvent) -> String {
    use crate::domain::AuditEvent;
    match event {
        AuditEvent::ProcessSpawn {
            command,
            args,
            cwd,
            at,
        } => format!(
            "[{at}] spawn      {command} {} (cwd={})",
            args.join(" "),
            cwd.display()
        ),
        AuditEvent::ProcessExit {
            exit_code,
            timed_out,
            at,
        } => format!("[{at}] exit       code={exit_code:?} timed_out={timed_out}"),
        AuditEvent::PermissionCheck {
            action,
            allowed,
            reason,
            at,
        } => format!("[{at}] permission {action} allowed={allowed} ({reason})"),
        AuditEvent::WorktreeCreated {
            path,
            base_commit,
            at,
        } => format!(
            "[{at}] worktree   created {} (base {base_commit})",
            path.display()
        ),
        AuditEvent::WorktreeRemoved { path, at } => {
            format!("[{at}] worktree   removed {}", path.display())
        }
    }
}

fn print_json_line(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("json serializes")
    );
}

fn print_json(value: &serde_json::Value) -> ExitCode {
    print_json_line(value);
    ExitCode::SUCCESS
}

fn is_unknown_id(e: &report::Error) -> bool {
    matches!(e, report::Error::Store(store::Error::NotFound(_)))
}

fn report_report_error(e: &report::Error) -> ExitCode {
    match e {
        report::Error::Store(se) => report_store_error(se),
    }
}

// ---- policy -----------------------------------------------------------------------------------

fn dispatch_policy(action: PolicyAction) -> ExitCode {
    match action {
        PolicyAction::Add(args) => policy_add(args),
        PolicyAction::List(args) => policy_list(args),
        PolicyAction::Show(args) => policy_show(args),
        PolicyAction::Validate(args) => policy_validate(args),
    }
}

fn policy_add(args: AddFileArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    let policy: PermissionPolicy = load_toml_or_exit!(&args.file);
    if let Err(msg) = policy.validate_fields() {
        return report_setup_error(&msg);
    }
    match deps.store.save_policy(&policy, args.force) {
        Ok(()) => {
            println!("added policy {}", policy.name);
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn policy_list(args: RepoArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    match deps.store.list_policies() {
        Ok(ids) => {
            if ids.is_empty() {
                println!("no policies");
            }
            for id in ids {
                println!("{id}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

/// Prints every field tagged exactly `Enforced`, `RequestedOnly`, or `Unsupported`, matching
/// SPEC.md §17 — the live, testable backing is `PermissionPolicy::enforcement_report`.
fn policy_show(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    match deps.store.load_policy(&args.id) {
        Ok(policy) => {
            println!("name: {}", policy.name);
            for row in policy.enforcement_report() {
                println!("  {:<20} {:?}  {}", row.field, row.level, row.detail);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_store_error(&e),
    }
}

fn policy_validate(args: RepoIdArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);
    match deps.store.load_policy(&args.id) {
        Ok(policy) => match policy.validate_fields() {
            Ok(()) => {
                println!("policy {} is valid", policy.name);
                ExitCode::SUCCESS
            }
            Err(msg) => report_setup_error(&msg),
        },
        Err(e) => report_store_error(&e),
    }
}

// ---- clean --------------------------------------------------------------------------------

/// Always reconciles first (any experiment with `status == Running` and no `RUNNING.lock`
/// present is marked `Failed`), then performs the requested removal — SPEC.md §6.
fn clean_cmd(args: CleanArgs) -> ExitCode {
    let deps = deps_or_exit!(&args.repo);

    let reconciled = match reconcile_running_experiments(&deps) {
        Ok(n) => n,
        Err(e) => return report_store_error(&e),
    };
    if reconciled > 0 {
        println!("reconciled {reconciled} orphaned running experiment(s) to Failed");
    }

    let older_than = match args.older_than.as_deref().map(parse_duration).transpose() {
        Ok(d) => d,
        Err(msg) => return report_setup_error(&msg),
    };

    let candidate_ids: Vec<String> = if let Some(id) = &args.experiment {
        vec![id.clone()]
    } else if args.all_worktrees || older_than.is_some() {
        match deps.store.list_experiments() {
            Ok(ids) => ids,
            Err(e) => return report_store_error(&e),
        }
    } else {
        Vec::new()
    };

    let mut removed = Vec::new();
    for id in candidate_ids {
        let Ok(record) = deps.store.load_experiment(&id) else {
            continue;
        };
        if let Some(threshold) = older_than {
            if chrono::Utc::now() - record.started_at < threshold {
                continue;
            }
        }
        if deps.worktrees.is_locked(&id) && !args.force {
            eprintln!("skipping {id}: experiment is still running (use --force)");
            continue;
        }
        let handle = WorktreeHandle {
            path: record.worktree_path.clone(),
            kind: WorktreeKind::Experiment,
        };
        match deps.worktrees.remove(&handle) {
            Ok(()) => removed.push(id),
            Err(e) => eprintln!("error removing worktree for {id}: {e}"),
        }
    }

    if removed.is_empty() {
        println!("nothing to clean");
    } else {
        for id in &removed {
            println!("removed worktree for {id}");
        }
    }
    ExitCode::SUCCESS
}

/// SPEC.md §7's run-lock protocol: an experiment with `status == Running` but no `RUNNING.lock`
/// was orphaned by a crash (AgentForge itself died before rewriting the status to a terminal
/// value), not genuinely still executing — reconciled to `Failed` before any removal is
/// considered. Returns the number reconciled.
fn reconcile_running_experiments(deps: &CliDeps) -> store::Result<usize> {
    let mut count = 0;
    for id in deps.store.list_experiments()? {
        let Ok(mut record) = deps.store.load_experiment(&id) else {
            continue;
        };
        if record.status == ExperimentStatus::Running && !deps.worktrees.is_locked(&id) {
            record.status = ExperimentStatus::Failed;
            record.ended_at = Some(chrono::Utc::now());
            deps.store.save_experiment(&record)?;
            count += 1;
        }
    }
    Ok(count)
}

/// Parses a simple `<n><unit>` duration, unit one of `s`/`m`/`h`/`d` — SPEC.md §6's
/// `--older-than <duration>`.
fn parse_duration(spec: &str) -> Result<chrono::Duration, String> {
    let spec = spec.trim();
    if spec.len() < 2 {
        return Err(format!(
            "invalid --older-than duration {spec:?} (expected e.g. \"30m\", \"24h\", \"7d\")"
        ));
    }
    let (num, unit) = spec.split_at(spec.len() - 1);
    let value: i64 = num.parse().map_err(|_| {
        format!("invalid --older-than duration {spec:?} (expected e.g. \"30m\", \"24h\", \"7d\")")
    })?;
    match unit {
        "s" => Ok(chrono::Duration::seconds(value)),
        "m" => Ok(chrono::Duration::minutes(value)),
        "h" => Ok(chrono::Duration::hours(value)),
        "d" => Ok(chrono::Duration::days(value)),
        _ => Err(format!(
            "invalid --older-than duration unit in {spec:?} (expected suffix s/m/h/d)"
        )),
    }
}

// ---- shared error → exit-code mapping ----------------------------------------------------

/// `NoCandidates`/`InvalidGlob`/`EmptyId`/`InvalidId`/`PathEscapesWorkspace` are usage errors
/// (2, the caller gave a spec/id that can't select or address anything); `Git`/`Io`/`Evaluator`
/// are internal errors (1) — mirrors `report_fault_error`'s split, plus the `Evaluator` variant
/// `fault::Error` doesn't have.
fn report_mutant_error(e: &mutant::Error) -> ExitCode {
    eprintln!("error: {e}");
    let code = match e {
        mutant::Error::NoCandidates { .. }
        | mutant::Error::InvalidGlob { .. }
        | mutant::Error::EmptyId
        | mutant::Error::InvalidId(_)
        | mutant::Error::PathEscapesWorkspace(_)
        | mutant::Error::TargetIsSymlink(_) => 2,
        mutant::Error::Git(_) | mutant::Error::Io(_) | mutant::Error::Evaluator(_) => 1,
    };
    ExitCode::from(code)
}

/// `NoCandidates`/`InvalidGlob`/`EmptyId`/`InvalidId`/`PathEscapesWorkspace` are usage errors
/// (2, the caller gave a spec/id that can't select or address anything); `Git`/`Io` are internal
/// errors (1) — mirrors `report_mutation_error`'s split.
fn report_fault_error(e: &fault::Error) -> ExitCode {
    eprintln!("error: {e}");
    let code = match e {
        fault::Error::NoCandidates { .. }
        | fault::Error::InvalidGlob { .. }
        | fault::Error::EmptyId
        | fault::Error::InvalidId(_)
        | fault::Error::PathEscapesWorkspace(_)
        | fault::Error::TargetIsSymlink(_) => 2,
        fault::Error::Git(_) | fault::Error::Io(_) => 1,
    };
    ExitCode::from(code)
}

fn report_setup_error(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(2)
}

/// Validation-shaped `store::Error`s (not found, already-exists-without-force) are usage errors
/// (2); everything else (TOML/JSON/IO failure) is an internal error (1) — SPEC.md §6's exit code
/// table, same split `report_workspace_error` uses.
fn report_store_error(e: &store::Error) -> ExitCode {
    eprintln!("error: {e}");
    let code = match e {
        store::Error::NotFound(_)
        | store::Error::AlreadyExists(_)
        | store::Error::EmptyId
        | store::Error::InvalidId(_) => 2,
        store::Error::Io(_)
        | store::Error::TomlDe(_)
        | store::Error::TomlSer(_)
        | store::Error::Json(_) => 1,
    };
    ExitCode::from(code)
}

/// `NoCandidates`/`InvalidGlob` are usage errors (2, the caller gave a spec that can't select
/// anything); `Git`/`Evaluator` are internal errors (1). The reject-on-good-verdict decision
/// itself is handled inline by `mutation create`'s own dispatch, not expressed as an `Err`.
fn report_mutation_error(e: &mutation::Error) -> ExitCode {
    eprintln!("error: {e}");
    let code = match e {
        mutation::Error::NoCandidates { .. } | mutation::Error::InvalidGlob { .. } => 2,
        mutation::Error::Git(_) | mutation::Error::Evaluator(_) => 1,
    };
    ExitCode::from(code)
}

/// Maps a `workspace::Error` to an exit code: validation-shaped failures (bad id, not found,
/// locked, already exists, path escaped the workspace root, or a spawn refused by the
/// permission-policy layer) are usage errors (2); everything else (git/exec/audit/io failure) is
/// treated as an internal error (1) — SPEC.md §6's exit code table. A policy denial is
/// specifically not lumped in with the generic `Exec` bucket: it's a "you asked for something
/// your own configured policy disallows" outcome, not an AgentForge-internal failure.
fn report_workspace_error(e: &workspace::Error) -> ExitCode {
    eprintln!("error: {e}");
    let code = match e {
        workspace::Error::EmptyId
        | workspace::Error::InvalidId(_)
        | workspace::Error::PathOutsideRoots(_)
        | workspace::Error::AlreadyExists(_)
        | workspace::Error::EmptyCommand
        | workspace::Error::NotFound(_)
        | workspace::Error::Locked(_) => 2,
        workspace::Error::Exec(crate::exec::Error::PolicyDenied(_)) => 2,
        workspace::Error::Git(_)
        | workspace::Error::Exec(_)
        | workspace::Error::Audit(_)
        | workspace::Error::Io(_) => 1,
    };
    ExitCode::from(code)
}

/// A child's exit code is a full integer on some platforms; `ExitCode` only holds a `u8`. Wraps
/// rather than panicking on an out-of-range value — this is display/plumbing, not a place where
/// silently losing information is worse than a hard error.
fn exit_code_from_child(code: i32) -> ExitCode {
    ExitCode::from(code.rem_euclid(256) as u8)
}
