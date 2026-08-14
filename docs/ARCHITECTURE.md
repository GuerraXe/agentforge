# AgentForge — Rust Architecture

**Status: implemented and tested end to end.** Every module and command described below is real,
not a sketch — 293/293 tests passing, `cargo clippy --all-targets --all-features -- -D warnings`
and `cargo fmt --check` both clean, plus a from-scratch adversarial security review
(`docs/ADVERSARIAL_REVIEW.md`) with its findings fixed. This document is also the project's design
history: it was written and kept current pass by pass as the implementation landed, so most
sections carry a dated **"Implemented"**/**"Hardened"** note pinning exactly which pass shipped
that piece and why any deviation from the original sketch happened. Treat those notes as the
authoritative record of what's real; treat the surrounding prose as the stable design rationale
around it. Derived directly from `docs/SPEC.md` (v2), the product contract this document turns
into Rust-level module boundaries, trait/struct signatures, and dependency structure.

**If you just want the shape of the system, not the full derivation:** read §1 (the one rule),
§2 (dependency graph), and §3 (crate layout) below, then stop — that's the whole architecture in
about 150 lines. The remaining sections are the module-by-module detail and the dated
implementation record, useful when you're about to touch a specific module or want to understand
why a particular design choice was made, not required reading to understand the system as a whole.

---

## 1. One rule that generates the whole module graph

**`domain` holds nouns. Every other module holds exactly one verb, operating on those nouns.**

`TaskSpec`, `EvaluatorVerdict`, `ExperimentRecord`, `ScoreCard` — these are data, with no I/O and
(beyond trivial field checks) no logic. `evaluator` turns an `EvaluatorSpec` into an
`EvaluatorVerdict`. `scoring` turns a `RawMetrics` into a `ScoreCard`. `git` turns a command into a
repository state change or query. Nothing computes or spawns anything by way of a struct method
buried in `domain` — if it does I/O or makes a decision, it isn't domain code.

This is what keeps the four "share infrastructure" requirements from the task brief honest rather
than aspirational:

- **Race and bisect share evaluator infrastructure** because both are *callers* of one
  `evaluator::Evaluator::evaluate()` — race via `experiment::ExperimentRunner` (§8), bisect
  directly (§10) — there is no second evaluation code path either could have drifted into.
- **Mutation and experiments share reproducibility infrastructure** because mutation's sanity
  check is the same `evaluate()` call again, and `git` is the same single abstraction mutation
  uses for its plumbing commit as `experiment` uses for patch capture (§9, §6).
- **All subprocess execution goes through one layer** — `exec::Executor` — because `git` (§6)
  and `evaluator` (§9) and `experiment` (§8) never call `std::process::Command` themselves; they
  all hold a reference to the same `Executor` trait object.
- **Reporting consumes structured results, never terminal output** — `report` (§13) only ever
  reads `ExperimentRecord`/`RaceRecord`/`BisectRecord`/`ScoreCard` back out of `store` (§12); it
  has no code path that opens `agent-stdout.log` or re-parses evaluator output. Only `evaluator`
  (§9) is allowed to look at raw process output, and only to populate the structured
  `EvaluatorVerdict` — after that point, raw text is gone from the system.

## 2. Module dependency graph

Arrows point from dependent to dependency (A → B means "A's code calls into B"). This is a DAG —
no cycles, checked by the fact that `cargo check` (a workspace-wide borrow/type/lifetime pass)
would fail to compile a cyclic `mod`/`use` graph in a single crate the way this is laid out.

```
                                   cli
                                    │
        ┌───────────┬──────────┬───┼────────────┬───────┬────────────┬────────────┐
        ▼           ▼          ▼   ▼             ▼       ▼            ▼            ▼
      report      race       bisect mutation    fault  mutant      experiment   workspace
        │           │          │      │           │       │            │            │
        │           └────┬─────┴──┬───┴─────┬─────┴───┬───┘            │            │
        │                ▼        ▼         ▼          ▼                │            │
        │            experiment evaluator  git::worktree ◄───────────────┴────────────┘
        │                │           │             │
        ▼                └─────┬─────┴─────┬───────┘
      store                    ▼           ▼
        │                    exec         git
        │                      │           │
        │                      └─────┬─────┘
        │                            ▼
        │                          audit
        │                            │
        └──────────────┬─────────────┘
                        ▼
                     domain
                        │
                        ▼
                      error
```

`adapter` sits beside `evaluator` (both depended on by `experiment`) and depends only on `domain`
and `exec`'s `ProcessSpec` type — it is a leaf with respect to everything except the two things it
must produce a value shaped like. `scoring` is a pure leaf depended on by `experiment` and
`report`, touching only `domain`. `workspace` (§6.1) depends on `git::worktree`, `exec`, and
`audit` directly, the same layer `experiment` sits on — but deliberately *not* on `experiment`
itself, `Store`, or `Evaluator`: a workspace is a disposable worktree plus an audit trail, not a
full experiment, so it doesn't need any of the machinery a `TaskSpec`/`EvaluatorSpec`/score
requires. `fault` (§9a) sits beside `mutation` and depends only on `git`/`git::worktree` and
`domain` — no `evaluator` dependency, unlike `mutation` (which needs it for the sanity gate),
since this pass gives fault workspaces no sanity gate of their own. `mutant` (§9b) sits beside
both: like `fault`, it depends on `git`/`git::worktree`/`domain` and materializes a real worktree
rather than using pure git plumbing; like `mutation`, it also depends on `evaluator` (for its own,
deferred `evaluate` step) and reuses `mutation`'s operator-scanning code directly rather than
duplicating it.

Every module's `Cargo`/`mod` boundary matches one row of SPEC.md's §2 ownership table. No module
is introduced here that doesn't correspond to a named concept in SPEC.md.

---

## 3. Crate layout

Single binary crate (`agentforge`), not a multi-crate workspace — SPEC.md's own "smallest
coherent architecture" instruction applies here too: a workspace only pays for itself once
something needs an independent release cadence or a genuinely separate consumer, and nothing in
MVP does. `src/lib.rs` exposes every module as a library surface; `src/main.rs` is a two-line
entry point (`agentforge::cli::run()`), which is what makes the CLI's own integration tests able
to drive the real command dispatch without spawning a subprocess.

```
Cargo.toml
src/
  lib.rs                 re-exports every top-level module
  main.rs                thin binary entry point
  error.rs                AgentForgeError (top-level, aggregating)
  domain/                 nouns only — see §4
    mod.rs
    ids.rs
    task.rs
    policy.rs
    evaluator.rs
    mutation.rs
    fault.rs
    mutant.rs
    experiment.rs
    race.rs
    bisect.rs
    scoring.rs
    audit.rs
    exec.rs
  exec/mod.rs              the Executor — §5
  audit/mod.rs             AuditSink — §5
  git/
    mod.rs                 GitRepo — §6
    worktree.rs             WorktreeManager, 4 worktree flavors — §6
  workspace/mod.rs          WorkspaceManager — §6.1, the CLI-facing worktree layer
  adapter/
    mod.rs                 AgentAdapter trait, AdapterCapabilities, resolve() — §7
    claude_code.rs
    fake.rs
  evaluator/mod.rs          Evaluator::evaluate() — §9
  scoring/mod.rs            score() — §9
  mutation/mod.rs           MutationEngine — §9
  fault/mod.rs               FaultInjector — §9a
  mutant/mod.rs               MutantTester — §9b
  experiment/mod.rs         ExperimentRunner — §8
  race/mod.rs               RaceRunner — §10
  bisect/mod.rs             BisectRunner — §10
  store/mod.rs               Store — §12
  report/mod.rs              Reporter — §13
  cli/mod.rs                 clap command tree + dispatch — §14
```

---

## 4. `domain` — the nouns

Every domain type is `#[derive(Debug, Clone, Serialize, Deserialize)]` (plus `PartialEq` where
equality is meaningful for tests, e.g. `EvaluatorVerdict`, `RawMetrics`). Nothing in `domain` does
I/O. The one exception to "no logic" is cheap, pure, in-memory validation (e.g.
`EvaluatorSpec::validate_fields()` checking `budget_secs > 0`) — still a pure function of the
struct's own fields, nothing reaching outside the process.

| Submodule | Types |
|---|---|
| `domain::ids` | `pub fn new_id(prefix_time: DateTime<Utc>) -> String` — the `<ISO8601><6 hex>` scheme (SPEC §5). Kept in `domain` rather than a top-level module because it's a small pure function every other module needs, not a subsystem of its own. |
| `domain::task` | `TaskSpec` |
| `domain::policy` | `AgentConfig`, `PermissionPolicy` |
| `domain::evaluator` | `EvaluatorSpec`, `EvaluatorVerdict`, `MetricExtractor`, `Cmd` |
| `domain::mutation` | `MutationOperator`, `MutationSpec`, `MutationRef` |
| `domain::fault` | `FaultKind`, `FaultSpec`, `FaultTarget`, `FaultRef` |
| `domain::mutant` | `MutantSpec`, `MutantTarget`, `MutantEvaluation`, `MutantRef` — reuses `domain::mutation::MutationOperator` rather than a second operator enum |
| `domain::experiment` | `ExperimentRecord`, `ExperimentStatus`, `RawMetrics`, `DiffStats` |
| `domain::race` | `RaceRecord`, `RaceParticipant` |
| `domain::bisect` | `BisectRecord`, `BisectStep` |
| `domain::scoring` | `ScoreCard`, `ScoreComponent`, `Rating`, `ScoringWeights`, `RatingBands` |
| `domain::audit` | `AuditEvent` |
| `domain::exec` | `ProcessSpec`, `ExecutionBudget`, `ProcessOutcome`, `EnforcementLevel`, `CommandPolicy` |

`EvaluatorVerdict::is_good()` (SPEC §4) lives here as an inherent method — it's a pure predicate
over the struct's own fields, exactly the class of "logic" `domain` is allowed to hold.

IDs are plain `String` type aliases (`pub type ExperimentId = String`, etc.), not newtypes. A
newtype per ID kind (`ExperimentId(String)`, `TaskId(String)`, `RaceId(String)`...) was considered
and rejected for MVP: nothing in SPEC.md's acceptance criteria depends on the compiler catching an
`ExperimentId` passed where a `TaskId` was expected, and five newtypes' worth of `Display`/`Deref`
boilerplate isn't justified by an MVP need — see SPEC.md's own §19 discipline about cutting
unjustified abstractions.

---

## 5. `exec` and `audit` — the shared foundation

### `exec`

```rust
pub trait Executor: Send + Sync {
    fn spawn(
        &self,
        spec: &ProcessSpec,
        cwd: &Path,
        env_passthrough: &[String],
        budget: &ExecutionBudget,
        command_policy: &CommandPolicy,
        audit: &dyn AuditSink,
    ) -> exec::Result<ProcessOutcome>;
}

pub struct SystemExecutor; // real impl: std::process::Command + a timeout watchdog thread
```

**Hardened (2026-08-13, adversarial security review — `docs/ADVERSARIAL_REVIEW.md` findings 3-4).**
Two real gaps in `SystemExecutor::spawn`, both closed in the same pass: (1) the timeout kill used
to reach only the directly-spawned process (`Child::kill()`) — a detached grandchild survived
indefinitely. A new `exec::tree` module now assigns the child to a Windows Job Object
(`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, terminated on timeout) or places it in its own Unix process
group (`kill(-pgid, SIGKILL)` on timeout), matching what SPEC.md §8 already claimed but the
implementation didn't back up. (2) `budget.max_output_bytes` was only enforced by truncating the
capture file *after* the process exited — stdio is now piped, with a bounded reader thread per
stream that stops writing (while still draining, so the child never blocks on a full pipe) once
the cap is reached, bounding disk growth during the run itself. `cfg`-gated new dependencies:
`libc` (Unix only) and `windows-sys` (Windows only) — neither affects the other platform's build.

**Amendment (permission-policy layer pass):** `command_policy` is a new parameter — `spec.program`
and `cwd` are checked against it (denylist first, then a non-empty allowlist; then cwd against
`allowed_roots`) before anything else happens, and each check records a `PermissionCheck` audit
event on both the allow and deny outcome. A denial returns `Error::PolicyDenied` with zero
`ProcessSpawn`/`ProcessExit` events — nothing was actually spawned. This stays a separate
parameter from `env_passthrough`/`budget` rather than folding into a single `&PermissionPolicy`
argument deliberately: those two are already derived from different sources depending on the
caller (an agent's `PermissionPolicy` for the agent process, `EvaluatorSpec.timeout_secs` for an
evaluator step), and `CommandPolicy` is the narrow slice every caller can supply regardless of
which of those it has in scope — `PermissionPolicy::command_policy()` extracts it when a caller
does have a full policy. `git::GitRepo`'s own internal spawns pass `CommandPolicy::unrestricted()`
— git is AgentForge's own trusted plumbing, not agent/evaluator-controlled.

This is a trait, not a bare struct, for exactly one reason traceable to SPEC.md: §18's acceptance
criteria require a "spy `Executor` that fails the test if invoked" for the `score` command (which
must recompute from disk without spawning anything). A trait object (`Arc<dyn Executor>`) is the
whole justification — every other consumer only ever sees the trait, never `SystemExecutor`
directly, so tests can substitute `NullExecutor`/`SpyExecutor` without touching production code.

`cwd` is a parameter to `spawn`, not a field of `ProcessSpec` — this is the direct encoding of
SPEC §8's "cwd is not adapter-suppliable": there is no field for an adapter to set. `spec` (which
adapters build) and `cwd`/`env_passthrough`/`budget` (which the caller — `experiment`, `git`, or
`evaluator` — supplies from policy/context, never from the adapter) are separate parameters on
purpose, so the type signature itself prevents the v1 ownership confusion SPEC.md §20 (U2)
documents resolving.

### `audit`

```rust
pub trait AuditSink: Send + Sync {
    fn record(&self, event: &AuditEvent) -> audit::Result<()>;
}

pub struct JsonlAuditSink { /* append-only file handle */ }
pub struct NullAuditSink;  // for evaluation-worktree calls that have no ExperimentRecord to attach to
```

Also a trait, for two reasons: (1) `Executor` and `evaluator::Evaluator` both need to emit events
into it without knowing whether they're running inside a full experiment (`JsonlAuditSink`) or a
throwaway evaluation worktree with no experiment record at all — `task add`'s baseline capture,
`mutate`'s sanity gate, and `eval` all fall into the latter (`NullAuditSink`); (2) it keeps
`exec` and `evaluator` decoupled from `store`'s file-layout knowledge — `JsonlAuditSink` is
constructed by whoever *does* know the layout (`experiment`, `store`) and handed down as `&dyn
AuditSink`.

`exec` depends on `audit` (its `spawn` signature takes `&dyn AuditSink`) and on `domain` (for
`ProcessSpec`/`ExecutionBudget`/`ProcessOutcome`/`AuditEvent`). `audit` depends only on `domain`.

---

## 6. `git` — the one safe Git abstraction

```rust
pub struct GitRepo {
    root: PathBuf,
    exec: Arc<dyn Executor>,
}

impl GitRepo {
    pub fn open(root: impl Into<PathBuf>, exec: Arc<dyn Executor>) -> git::Result<Self>;

    pub fn resolve_commit(&self, commit_ish: &str) -> git::Result<String>;      // → 40-hex SHA
    pub fn common_dir(&self) -> git::Result<PathBuf>;                            // for repo-id hashing
    pub fn status_porcelain(&self, path: &Path) -> git::Result<String>;

    pub fn worktree_add(&self, path: &Path, commit: &str) -> git::Result<()>;
    pub fn worktree_remove(&self, path: &Path) -> git::Result<()>;
    pub fn worktree_checkout(&self, worktree_path: &Path, commit: &str) -> git::Result<()>;

    pub fn diff(&self, worktree_path: &Path, base_ref: &str) -> git::Result<String>;
    pub fn diff_stats(&self, worktree_path: &Path, base_ref: &str) -> git::Result<DiffStats>;
    pub fn apply_patch(&self, worktree_path: &Path, patch: &str) -> git::Result<()>;

    pub fn rev_list_ancestry_path(&self, good: &str, bad: &str) -> git::Result<Vec<String>>;
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> git::Result<bool>;

    pub fn read_blob(&self, commit: &str, path: &str) -> git::Result<Vec<u8>>;
    pub fn write_commit(
        &self,
        parent: &str,
        changed_files: &[(String, Vec<u8>)],
        message: &str,
        update_ref: &str,
    ) -> git::Result<String>;   // plumbing: write blob(s)+tree+commit, move a ref, return new SHA
    // fixed, non-wall-clock author/committer date — write_commit's result is a pure function of
    // (parent, changed_files, message), required for mutation::MutationEngine::apply's
    // reproducibility contract (§10).

    // Added 2026-08-12 for mutation::MutationEngine (§9):
    pub fn list_tree_files(&self, commit: &str) -> git::Result<Vec<String>>;               // git ls-tree -r --name-only, never a worktree
    pub fn diff_stats_between(&self, from_commit: &str, to_commit: &str) -> git::Result<DiffStats>; // two-commit diff, unlike diff_stats' worktree-vs-ref form
    pub fn update_ref(&self, ref_name: &str, commit: &str) -> git::Result<()>;
    pub fn delete_ref(&self, ref_name: &str) -> git::Result<()>;                            // idempotent
}
```

**Every method shells out to the real `git` binary via `self.exec`** — there is no `git2`/`gix`
dependency. This was a deliberate MVP call, not an oversight: the target machine already has to
have a working `git` for AgentForge to be pointed at a repo at all, libgit2 bindings add a native
build dependency this project doesn't otherwise need, and — the architecturally relevant point —
routing git invocations through `Executor` is what makes "all subprocess execution goes through
one controlled layer" true of git operations too, not just agent/evaluator ones. `git`'s only
special status is that it's the one caller of `Executor` that also *parses* the child's stdout
into typed results (`DiffStats`, a list of SHAs, a blob's bytes) — everywhere else, raw process
output either becomes an `EvaluatorVerdict` (in `evaluator`, §9) or is left as an opaque captured
log nobody re-parses (§13's "structured results, not terminal output" rule).

`git::worktree` (submodule) is layered on top of `GitRepo`, not merged into it, because it encodes
policy `git` itself has no opinion about: *where* AgentForge puts worktrees and *which of five
flavors* it's creating (SPEC §7).

```rust
pub enum WorktreeKind { Experiment, Bisect, Evaluation, Fault, Mutant }

pub struct WorktreeHandle {
    pub path: PathBuf,
    pub kind: WorktreeKind,
}

pub struct WorktreeManager {
    state_root: PathBuf,
    git: Arc<GitRepo>,
}

impl WorktreeManager {
    pub fn new(state_root: PathBuf, git: Arc<GitRepo>) -> Self;

    /// Platform data dir + a stable hash of `git rev-parse --git-common-dir` — SPEC §5.
    pub fn resolve_state_root(repo_root: &Path) -> git::Result<PathBuf>;

    pub fn create_experiment_worktree(&self, experiment_id: &str, base_ref: &str) -> git::Result<WorktreeHandle>;
    pub fn create_bisect_worktree(&self, bisect_id: &str, base_ref: &str) -> git::Result<WorktreeHandle>;
    pub fn create_evaluation_worktree(&self, base_ref: &str) -> git::Result<WorktreeHandle>;
    /// Idempotent: a `handle.path` that's already gone is a no-op success, not an error.
    pub fn remove(&self, handle: &WorktreeHandle) -> git::Result<()>;
    /// Read-only access to the state root this manager places worktrees under, so callers
    /// (`workspace::WorkspaceManager`) can enumerate/validate paths without a second copy.
    pub fn state_root(&self) -> &Path;

    // RUNNING.lock protocol (SPEC §7 / §20 F1,M1) — keyed by experiment id, not by worktree path,
    // since a bisect/evaluation worktree has no long-running "is someone using this" question
    // the way an experiment's does (bisect and evaluation worktrees are held for one synchronous
    // call, not across a whole `run`).
    pub fn mark_running(&self, experiment_id: &str) -> git::Result<()>;
    pub fn clear_running(&self, experiment_id: &str) -> git::Result<()>;
    pub fn is_locked(&self, experiment_id: &str) -> bool;
}
```

`git worktree add`/`remove` calls across all three flavors serialize through an internal mutex
inside `WorktreeManager` (SPEC §7) — an implementation detail of the struct, not part of its
public interface, so it isn't in the signatures above.

`git` depends on `exec` and `domain` (for `DiffStats`). `git::worktree` depends on `git` and
`domain`.

---

## 6.1 `workspace` — the CLI-facing worktree layer

```rust
pub struct WorkspaceInfo {
    pub id: String,
    pub path: PathBuf,
    pub head: String,     // commit currently checked out
    pub locked: bool,     // RUNNING.lock present
}

pub struct WorkspaceManager {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    exec: Arc<dyn Executor>,
}

impl WorkspaceManager {
    pub fn new(git: Arc<GitRepo>, worktrees: Arc<WorktreeManager>, exec: Arc<dyn Executor>) -> Self;

    pub fn create(&self, id: &str, base_ref: &str) -> workspace::Result<WorkspaceInfo>;
    pub fn list(&self) -> workspace::Result<Vec<WorkspaceInfo>>;
    pub fn show(&self, id: &str) -> workspace::Result<WorkspaceInfo>;
    pub fn exec(
        &self,
        id: &str,
        command: ProcessSpec,
        budget: &ExecutionBudget,
        env_passthrough: &[String],
    ) -> workspace::Result<ProcessOutcome>;
    pub fn remove(&self, id: &str, force: bool) -> workspace::Result<()>;
    pub fn clean(&self, force: bool) -> workspace::Result<Vec<String>>;
    pub fn audit_log_path(&self, id: &str) -> workspace::Result<PathBuf>;
}
```

This is the layer `agentforge workspace {create,list,show,exec,remove,clean}` drives directly —
a "workspace" is exactly an `Experiment`-flavor worktree (§6), addressed by a caller-chosen id,
with its own audit trail. It exists to make isolated task worktrees and controlled repository
execution usable *before* `Store`/`Evaluator`/`scoring` exist: no `TaskSpec`, no `EvaluatorSpec`,
no score anywhere in this module.

**Safety properties, and exactly how each is structural rather than a convention:**

- **Never remove paths outside AgentForge-owned workspace roots.** Every path this module
  touches is `state_root.join("worktrees").join(id)` — never a raw path accepted from a caller.
  `id` is validated (`[A-Za-z0-9_-]+`, no separators, no `.`/`..`) *before* it's joined into any
  path, which makes traversal structurally impossible, not just checked-for. A second,
  independent `starts_with(state_root)` check runs on the constructed path as defense in depth,
  in case a future call site ever forgets the id validation step.
- **Validate Git refs and repository state.** `create` resolves `base_ref` through
  `GitRepo::resolve_commit` (§6) before creating anything — an unresolvable ref fails loudly,
  never silently creates a workspace pointed at nothing.
- **Avoid destructive Git operations unless explicitly required.** The only Git-destructive call
  is `worktree remove --force`, and only from `remove`/`clean`, which is the one operation
  explicitly asked to destroy something; nothing else in this module runs a mutating git command.
- **Cleanup is idempotent.** `remove` on an already-gone workspace is `Ok(())`, not an error
  (inherited directly from `WorktreeManager::remove`'s own idempotence, §6). `clean` is safe to
  run repeatedly for the same reason.
- **Application-level vs. OS-level, made explicit.** `exec` runs the given command with the same
  `Executor` guarantees as everywhere else (§5) — forced cwd, an exact env allowlist, a
  timeout-kill watchdog, output truncation, and (permission-policy layer pass) a command-program
  allow/denylist and cwd-root confinement — and nothing more. There is no container, no
  filesystem jail, no network isolation here; SPEC.md §16's Enforced/Best-effort/Not-provided
  table applies unchanged. `cli`'s `workspace` help text and doc comments say this plainly rather
  than letting "isolated workspace" imply a stronger boundary than what's actually enforced.
  `WorkspaceManager::exec` takes a full `&PermissionPolicy` (not separate budget/env-passthrough
  parameters) precisely because, unlike `evaluator`, it has no other source for those values —
  `cli`'s `workspace exec` builds one ad hoc from its own flags, the same way it built an ad hoc
  `ExecutionBudget` before this pass, since `Store`-backed named policies don't exist yet.

`exec`'s lock handling is worth being explicit about: `mark_running`/`clear_running` bracket the
call, and `clear_running` always runs — the `Executor::spawn` result is captured in a local
variable *before* clearing the lock, then propagated, so the lock never outlives the call
regardless of whether the command succeeded, failed, or the spawn itself errored.

`workspace` depends on `git`, `git::worktree`, `exec`, `audit`, and `domain` — not on
`experiment`, `Store`, or `Evaluator` (§2).

---

## 7. `adapter` — agent-independence, made structural

**Implemented (2026-08-12).** The trait/signatures below are exactly what shipped.

```rust
pub struct AdapterCapabilities {
    pub can_confine_filesystem: EnforcementLevel,
    pub can_restrict_network: EnforcementLevel,
}

pub trait AgentAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> AdapterCapabilities;
    fn command_for(&self, prompt: &str, model: Option<&str>, extra_args: &[String]) -> ProcessSpec;
}

pub fn resolve(name: &str) -> adapter::Result<Box<dyn AgentAdapter>>;
```

`command_for` is the entire trait surface that matters for agent-independence: it takes no
`worktree_path`, no `AuditSink`, no timeout — it cannot spawn anything and has nothing to spawn it
*with* even if it tried, because it returns a value (`ProcessSpec`) instead of taking control flow
(SPEC §20, U2). `experiment::ExperimentRunner` is the only thing that both calls `command_for` and
holds an `Executor` — an adapter implementation is structurally incapable of bypassing the
Executor's timeout/cwd/audit guarantees, not just conventionally discouraged from doing so.

`adapter::claude_code::ClaudeCodeAdapter` and `adapter::fake::FakeAdapter` are the two
implementations MVP ships (SPEC §9) — `FakeAdapter` carries a scripted `ProcessSpec` (or a small
built-in fixture script) so every integration test in SPEC §18 runs without the real `claude`
binary.

`ClaudeCodeAdapter::command_for` builds a non-interactive invocation (`-p <prompt> --output-format
json`, `--model <model>` when given, `--permission-mode <mode>` when configured) — `prompt` is
always one discrete `args` element, never interpolated into a shell string; nothing in this path
invokes `sh -c`/`cmd /C`. Construction-time configuration (`ClaudeCodeConfig { executable,
permission_mode, extra_default_args }`) is separate from the per-call `model`/`extra_args`
`command_for` itself takes. `adapter::resolve(name)` matches `"claude-code"` to
`ClaudeCodeAdapter::default()` (reading `AGENTFORGE_CLAUDE_EXECUTABLE`/
`AGENTFORGE_CLAUDE_PERMISSION_MODE` if set, else plain `claude` on `PATH` with no permission-mode
flag) and returns `Error::UnknownAdapter` for anything else. Adapter contract tests
(`tests/adapter_contract.rs`) cover command shape/determinism/no-shell-interpolation/
configurability for both adapters, `resolve`'s two branches, and — composed with the real
`SystemExecutor` — that a missing executable fails cleanly (`Error::SpawnFailed`, not a panic) and
that a successful spawn's exit status/stdout/stderr/audit timestamps are all captured; these tests
never depend on the real `claude` binary.

`adapter` depends on `domain` (`ProcessSpec`, `EnforcementLevel`) only.

---

## 8. `evaluator` — the one shared judgment

**Implemented (2026-08-12).** The signature below is exactly what shipped; `regex` (deferred per
§16 until a module needed it) is now a real dependency, used only inside this module's private
metric-extraction function.

```rust
pub struct Evaluator {
    exec: Arc<dyn Executor>,
}

impl Evaluator {
    pub fn new(exec: Arc<dyn Executor>) -> Self;

    /// The single function `run`, `race`, `bisect`, `mutate`'s sanity gate, `task add`'s
    /// baseline capture, and `eval` all call — SPEC §11, §20 (D1).
    pub fn evaluate(
        &self,
        worktree_path: &Path,
        spec: &EvaluatorSpec,
        audit: &dyn AuditSink,
    ) -> evaluator::Result<EvaluatorVerdict>;
}
```

`evaluate()` takes an already-checked-out worktree path, not a commit — checking out the right
commit is `git`/`WorktreeManager`'s job (§6), keeping `evaluator` from needing to know which of
the three worktree flavors it's being called against. This is exactly what lets `bisect` reuse it
directly (checkout a candidate into its one dedicated worktree, then call `evaluate()`) without
going through `experiment` at all — bisect steps aren't experiments (SPEC §4), and this signature
is why that's possible without a special case.

Metric extraction (running `metric_extractors` against `test_cmd`'s captured output) is a private
function inside this module, not exposed — `EvaluatorVerdict` is the only thing that crosses the
module boundary, which is the concrete mechanism behind "reporting consumes structured results,
not terminal output": raw evaluator stdout/stderr never leaves this module as a `String` that some
other module could be tempted to re-parse.

`evaluator` depends on `exec` (to run `setup_cmds`/`test_cmd`), `audit`, and `domain`. It does
**not** depend on `git` — worktree/commit setup is always done by the caller before `evaluate()`
is invoked, keeping this module a pure "run a command, extract a verdict" primitive.

---

## 9. `mutation` — reproducibility infrastructure shared with experiments

**Implemented (2026-08-12).** `MutationEngine` is the shared reproducible-experiment framework the
task brief asked for: fault injection (candidate selection + `apply`) and mutation testing (the
sanity-gate `evaluate()` call) are one feature, per SPEC.md §10, not two — there is no second
"experiment" abstraction sitting alongside `domain::experiment::ExperimentRecord` (which stays
scoped to one agent run against a task; bisect and mutation are both deliberately *not*
`Experiment`s — SPEC.md §4).

```rust
pub struct Candidate { pub file: String, pub line: u32, pub column: u32 }

pub struct MutationEngine {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    evaluator: Arc<Evaluator>,
}

impl MutationEngine {
    pub fn new(git: Arc<GitRepo>, worktrees: Arc<WorktreeManager>, evaluator: Arc<Evaluator>) -> Self;

    /// Reads blobs at `base_commit` via `GitRepo::list_tree_files`/`read_blob` — never a
    /// worktree. Filters by `glob::Pattern` against forward-slash-joined paths, sorts byte-wise,
    /// then scans each non-comment line with the operator's fixed `regex::Regex` after a
    /// best-effort string-literal mask (§20 T4's acknowledged heuristic).
    pub fn find_candidates(&self, base_commit: &str, spec: &MutationSpec) -> mutation::Result<Vec<Candidate>>;

    /// Pure git plumbing — no worktree created for this step (SPEC §10, §20 U4). Selects
    /// `candidates[seed % candidates.len()]`, re-derives its replacement (rather than threading a
    /// byte range through) so `apply` stays a pure function of its five identity inputs, and
    /// writes it via `git.write_commit` — whose author/committer date is now fixed, not the wall
    /// clock, which is required for `mutant_commit` to be reproducible at all.
    pub fn apply(&self, base_commit: &str, spec: &MutationSpec, task_id: &str) -> mutation::Result<MutationRef>;

    /// Uses an Evaluation-flavor worktree + the shared `Evaluator` — the exact same
    /// reproducibility path `task add`'s baseline capture uses (SPEC §10).
    pub fn sanity_check(&self, mutation_ref: &MutationRef, evaluator_spec: &EvaluatorSpec) -> mutation::Result<EvaluatorVerdict>;

    /// The entire cleanup surface for a mutant no longer needed: deletes `mutation_ref.mutant_ref`
    /// via `GitRepo::delete_ref`. Nothing else needs restoring — `apply` never touched a worktree
    /// or `HEAD`.
    pub fn discard(&self, mutation_ref: &MutationRef) -> mutation::Result<()>;
}
```

The "shared reproducibility infrastructure" the task brief asks for is concretely: `apply()` calls
`git.write_commit` (§6) — the same plumbing primitive nothing else currently needs but that lives
in the general-purpose `GitRepo`, not duplicated inside `mutation` — and `sanity_check()` calls
`evaluator.evaluate()` (§8) against a `WorktreeManager::create_evaluation_worktree` (§6), which is
the identical call `experiment::ExperimentRunner` and `store`'s baseline-capture path make. Three
existing primitives compose; `mutation` adds no new way to touch git, spawn a process, or judge a
patch. Two new `GitRepo` primitives back `find_candidates`/`discard`: `list_tree_files` (read
tracked paths at a commit without a worktree) and `update_ref`/`delete_ref` (move or remove a ref
— used both by `apply`'s own `write_commit` step and, standalone, by `mutate`'s CLI handler to
re-home a rejected mutant under `refs/agentforge/mutants/rejected/...`).

`mutation` depends on `git`, `git::worktree`, `evaluator`, and `domain`.

`cli`'s `agentforge experiment mutation show <task-id>`/`agentforge experiment mutation replay
<task-id>` (SPEC §6) are the CLI surface the task brief asked for: `show` reads
`store.load_task(id).mutation` and prints it; `replay` re-applies the same `(spec, base_commit)`
under a throwaway ref and asserts the result's `mutant_commit`/`selected_target`/`diff_stats`
match the original, exercising SPEC.md §10's determinism contract directly rather than only
asserting it in a unit test. (`create` — formerly the standalone `mutate` verb — lives in `cli`
alongside `show`/`replay` under the same `experiment mutation` subcommand, per the "CLI
integration and cleanup" pass's regrouping — see SPEC.md §6's Amendment.)

**Amendment (2026-08-12, "repository fault injection" pass):** the "one feature... not two" claim
above is now scoped more precisely — see §9a. It still holds for what it originally meant (fault
injection and mutation *testing*, i.e. the sanity-gated code-logic-bug path, are one mechanism,
`MutationEngine`), but a second, sibling mechanism — `fault::FaultInjector` — now exists for
fault kinds `MutationEngine` structurally cannot express (a missing file, a stale
untracked/gitignored generated artifact). The two share `git`/`git::worktree` plumbing and a
`candidates[seed % len]` determinism contract, but not an operator model or a record type.

---

## 9a. `fault` — repository-state fault injection, sibling to `mutation`

**Implemented (2026-08-12).** Where `mutation` scans tracked source lines for a small logic-bug
regex, `FaultInjector` simulates a broken repository/environment state: a missing file, a
corrupted config value, a stale generated artifact, a corrupted dependency version pin. SPEC.md
§10's Amendment has the full rationale for why this isn't a `MutationEngine` operator.

```rust
pub struct FaultCandidate { pub file: String, pub line: Option<u32> }  // None for whole-file kinds

pub struct FaultInjector {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
}

impl FaultInjector {
    pub fn new(git: Arc<GitRepo>, worktrees: Arc<WorktreeManager>) -> Self;

    /// Same determinism contract as `MutationEngine::find_candidates` (byte-wise sorted,
    /// forward-slash-normalized tracked paths). Whole-file kinds (`MissingFile`, `StaleArtifact`)
    /// produce one candidate per matched file; line-level kinds (`BrokenConfigValue`,
    /// `DependencyCorruption`) produce one per matching, non-comment line.
    pub fn find_candidates(&self, base_commit: &str, spec: &FaultSpec) -> fault::Result<Vec<FaultCandidate>>;

    /// Unlike `MutationEngine::apply` (pure git plumbing, never a worktree — SPEC.md §20 U4),
    /// this always materializes a fresh `WorktreeKind::Fault` worktree at `base_commit`
    /// (`WorktreeManager::create_fault_worktree`) and writes the fault directly into it via
    /// `std::fs` — the source repository is never opened for writing. `id` is validated
    /// (`[A-Za-z0-9_-]`-only, mirroring `workspace::validate_id`) before it becomes a filesystem
    /// path component.
    pub fn inject(&self, base_commit: &str, spec: &FaultSpec, id: &str) -> fault::Result<FaultRef>;

    /// `git checkout <base_commit> -- <file>` inside the fault workspace (new
    /// `GitRepo::restore_path`) — recreates a deleted file or reverts a rewritten one uniformly;
    /// the workspace itself stays alive for reuse.
    pub fn restore(&self, fault_ref: &FaultRef) -> fault::Result<()>;

    /// Removes the entire fault workspace — the alternative to `restore`.
    pub fn discard(&self, fault_ref: &FaultRef) -> fault::Result<()>;
}
```

`fault` depends on `git`, `git::worktree`, and `domain` — no `evaluator` dependency, since a fault
workspace has no sanity gate of its own in this pass (unlike `mutate`'s undetectable-fault
rejection, which needs an `EvaluatorSpec`); wiring that in is deferred to whichever future pass
integrates faults into `experiment`.

`cli`'s `agentforge experiment fault inject/show/restore/discard` (SPEC §6) are the CLI surface: `inject`
resolves `--base`, calls `FaultInjector::inject`, and persists the result via the new
`Store::save_fault`; `show`/`restore`/`discard` all load the persisted `FaultRef` by id first.
`FaultRef` is a standalone `Store` record — unlike `MutationRef` (embedded-only in `TaskSpec`,
SPEC.md §20 C3), it has no wrapping task to embed into yet, so it carries its own `id` and gets
its own `Store` collection (`save_fault`/`load_fault`/`list_faults`, mirroring `tasks`/
`evaluators` exactly).

No `ExperimentType` abstraction was introduced for this pass — `experiment::ExperimentRunner::run`
doesn't exist yet, so there is nothing to wire a `FaultRef` into. `fault` ships standalone,
mirroring how `mutation` shipped before `experiment` existed.

---

## 9b. `mutant` — standalone, reproducible source mutation testing, sibling to `fault`

**Implemented (2026-08-12, standalone mutation testing pass).** Built the same way `fault` was:
standalone-persisted, id-addressed, materializes a real worktree. Unlike `fault`, it targets the
same kind of thing `mutation` does (a source-line logic bug via `MutationOperator`) — so rather
than a third operator model, `mutant` calls `mutation`'s own candidate-scanning code directly
(`scan_line`/`mutate_file_contents`/`is_comment_line`/`Candidate`, bumped `pub(crate)`). SPEC.md
§10's Amendment (standalone mutation testing pass) has the full rationale and the two
`AskUserQuestion` decisions this shipped under.

```rust
pub struct MutantTester {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    evaluator: Arc<Evaluator>,
}

impl MutantTester {
    pub fn new(git: Arc<GitRepo>, worktrees: Arc<WorktreeManager>, evaluator: Arc<Evaluator>) -> Self;

    /// Identical shape to `mutation::MutationEngine::find_candidates`, calling its scanning code
    /// directly rather than a second copy of the five operator regexes.
    pub fn find_candidates(&self, base_commit: &str, spec: &MutantSpec) -> mutant::Result<Vec<Candidate>>;

    /// Unlike `MutationEngine::apply` (pure git plumbing, no worktree) but exactly like
    /// `FaultInjector::inject`: materializes a fresh `WorktreeKind::Mutant` worktree at
    /// `base_commit` and writes the mutation directly into it via `std::fs`. Id validation and
    /// path-safety checks call `fault::validate_id`/`fault::safe_join` directly (bumped
    /// `pub(crate)`), not a third copy. Never evaluates or gates — `evaluation` starts `None`.
    pub fn apply(&self, base_commit: &str, spec: &MutantSpec, id: &str) -> mutant::Result<MutantRef>;

    /// Runs `Evaluator::evaluate` directly against `mutant_ref.worktree_path` — no worktree
    /// created or removed here, unlike `MutationEngine::sanity_check`'s throwaway one. This is
    /// the one real behavioral difference from `mutation`: evaluation is separate, later, and
    /// non-gating. Does not persist — the caller records the returned verdict onto a
    /// `MutantEvaluation` and re-saves via `Store::save_mutant`.
    pub fn evaluate(&self, mutant_ref: &MutantRef, evaluator_spec: &EvaluatorSpec, audit: &dyn AuditSink) -> mutant::Result<EvaluatorVerdict>;

    /// `git checkout <base_commit> -- <file>` inside the mutant workspace — identical mechanism
    /// to `FaultInjector::restore`.
    pub fn restore(&self, mutant_ref: &MutantRef) -> mutant::Result<()>;

    /// Removes the entire mutant workspace — identical mechanism to `FaultInjector::discard`.
    pub fn discard(&self, mutant_ref: &MutantRef) -> mutant::Result<()>;
}
```

`mutant` depends on `git`, `git::worktree`, `evaluator`, and `domain` — unlike `fault`, it needs
`evaluator` for its own `evaluate` step (deferred rather than an inline sanity gate, so no
`Arc<Evaluator>` dependency was avoidable the way `fault`'s was).

`cli`'s `agentforge experiment mutant apply/show/evaluate/restore/discard` (SPEC §6) are the CLI surface,
structurally identical to `fault`'s: `apply` resolves `--base`, calls `MutantTester::apply`, and
persists via `Store::save_mutant`; `show`/`restore`/`discard` load the persisted `MutantRef` by id
first. `evaluate` additionally loads the target `EvaluatorSpec`, opens a dedicated
`JsonlAuditSink` at `<state_root>/mutants/<id>/audit.jsonl` (unlike `mutate`'s sanity gate, which
uses `NullAuditSink`), calls `MutantTester::evaluate`, and re-saves the record with `--force`
implied (an evaluation is expected to update an already-`apply`-created record).

`MutantRef` is a standalone `Store` record, exactly like `FaultRef` and unlike `MutationRef`
(embedded-only in `TaskSpec`, SPEC.md §20 C3) — new `Store::save_mutant`/`load_mutant`/
`list_mutants`, byte-for-byte mirroring `save_fault`/`load_fault`/`list_faults`.

No `ExperimentType` abstraction was introduced here either, for the same reason as `fault`:
`experiment::ExperimentRunner::run` doesn't exist yet. `mutant` ships standalone.

---

## 10. `experiment`, `race`, `bisect` — composition, not reimplementation

**`experiment`/`race` implemented (2026-08-12).** `ExperimentRunner::run` and
`RaceRunner::run_race` are exactly what shipped, with one addition beyond the sketch below:
`run_race` takes no `policy` parameter (matching its existing signature and the fact that
`race`'s own CLI row, SPEC.md §6, takes no `--policy` flag) — every participant runs under a
generous, uniform, built-in-default `PermissionPolicy` (`race::default_policy()`, private to the
module, now a thin wrapper over `PermissionPolicy::generous_default` — see the "CLI integration
and cleanup" pass's note below), the race-level analogue of `scoring::default_weights()`'s
"built-in-default" fallback; `run`'s own CLI row *does* take `--policy <name>` and falls back to
the same `generous_default` shape when omitted (`Store::load_policy` is real now — see §12).
`run` collapses every error surfaced
after its `Running` record is written (spawn refused/failed, `git diff`, `evaluate()`, a missing
evaluator) into a finalized `Failed`-status record rather than an `Err` — SPEC.md §12 (F3)
requires a real `ExperimentRecord` for a participant whose *experiment* failed internally, not a
`Result::Err` a caller like `race` would have to invent a placeholder record around; only a
failure before any record exists (worktree creation itself) propagates as `Err`. `run_race`'s
bounded fan-out is plain chunked `std::thread::scope` (never more than `max_parallel` `run` calls
in flight at once) — no new dependency, no work-stealing scheduler, since bounded-not-optimal
concurrency is all SPEC.md §12 asks for. `docs/TEST_STATUS.md`'s "race orchestration" pass entry
has the full test list. (`bisect` was still `todo!()` as of this pass — see its own "implemented"
note further down this section for the pass that closed it.)

```rust
// experiment
pub struct ExperimentRunner {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    exec: Arc<dyn Executor>,
    evaluator: Arc<Evaluator>,
    store: Arc<Store>,
}

impl ExperimentRunner {
    pub fn new(/* ... */) -> Self;

    /// The one `run` primitive. Everything else that produces an `ExperimentRecord` calls this.
    pub fn run(
        &self,
        task: &TaskSpec,
        agent: &dyn AgentAdapter,
        agent_config: &AgentConfig,
        policy: &PermissionPolicy,
    ) -> experiment::Result<ExperimentRecord>;
}
```

```rust
// race
pub struct RaceRunner {
    runner: Arc<ExperimentRunner>,
    store: Arc<Store>,
}

impl RaceRunner {
    pub fn new(runner: Arc<ExperimentRunner>, store: Arc<Store>) -> Self;

    /// Expands agents × repeat into a race_index-ordered participant list (SPEC §12, §20 R2/T1),
    /// then calls `ExperimentRunner::run` once per participant, bounded by max_parallel. No
    /// separate execution path — `race` cannot drift from what a bare `run` does, by construction.
    pub fn run_race(
        &self,
        task: &TaskSpec,
        agents: &[(AgentConfig, Arc<dyn AgentAdapter>)],
        repeat: u32,
        max_parallel: u32,
    ) -> race::Result<RaceRecord>;
}
```

`agents` takes already-resolved adapters paired with their configs, not bare `AgentConfig`s
resolved internally by name. This mirrors `ExperimentRunner::run`'s own `agent`/`agent_config`
split (§10, above) and was corrected during test-writing: name-based resolution
(`adapter::resolve`) belongs to `cli`, the only module that should ever turn a string into a
production adapter — `RaceRunner` resolving names itself would have made it impossible to drive
a race with `FakeAdapter` in tests, defeating the whole point of the trait split in §7.

**`bisect` implemented (2026-08-12).** `BisectRunner::run_bisect` is exactly what shipped, with
one addition beyond the sketch below: a `store: Arc<Store>` field/constructor param, absent from
the original sketch. Two independent reasons forced it, both mirroring `ExperimentRunner`'s own
established shape (§10, above) rather than inventing something new: (1) `TaskSpec` only carries
an evaluator *id* (`task.evaluator`), so turning it into a real `EvaluatorSpec` needs
`Store::load_evaluator`, exactly like `ExperimentRunner::run`'s own `execute` step; (2) SPEC.md
§13 point 6 ("`steps.jsonl` gets one entry per commit actually tested... appended as it happens")
needs somewhere to persist to as the search runs, not just once at the end — `store::Store`'s own
`save_bisect` doc comment already assigns that live, as-it-happens behavior to `BisectRunner`
specifically ("not `Store`, which only ever persists/reloads a complete snapshot"), so
`run_bisect` calls `Store::save_bisect` after every step, turning repeated full-snapshot writes
into an outside-observable append. `Store::load_evaluator`/`save_bisect` were already implemented
(the "evaluation reporting" pass) — this pass only consumes them.

A "no flip found" range (every tested candidate is good, so the whole range shares one verdict —
SPEC.md §13 point 5's exit-3 case) comes back as `culprit: None` on an `Ok(BisectRecord)`, not an
`Err` — the stub's original `Error::NoFlip` variant was removed in favor of this, to match the
same "a judgment is a normal result, not an internal failure" convention `experiment` already
established (SPEC.md §12 F3) and `mutate`'s undetectable-mutation verdict already demonstrates
(the caller inspects the value, it isn't handed an `Err`). `Error::NotLinear` (a genuine
precondition failure — `good` not an ancestor of `bad`) stays an `Err`, checked before any
worktree is created. The dedicated bisect worktree is unconditionally removed once the search
finishes, on every path (success, inconclusive, or a git/evaluator/store/audit error after the
worktree exists) — the same "AgentForge-managed isolated Git state, primary checkout never
touched" guarantee `run` gives. `docs/TEST_STATUS.md`'s "semantic bisect" pass entry has the full
test list. CLI wiring (`Command::Bisect`) was still open as of this pass — see §14 for the "CLI
integration and cleanup" pass that wired it, along with `run`/`race`/`verify`(formerly `eval`)/
`report log`/`policy`/`clean`, all at once.

```rust
// bisect
pub struct BisectRunner {
    git: Arc<GitRepo>,
    worktrees: Arc<WorktreeManager>,
    evaluator: Arc<Evaluator>,
    store: Arc<Store>,
}

impl BisectRunner {
    pub fn new(
        git: Arc<GitRepo>,
        worktrees: Arc<WorktreeManager>,
        evaluator: Arc<Evaluator>,
        store: Arc<Store>,
    ) -> Self;

    /// In-process binary search over `evaluate()` calls (SPEC §13) — no `experiment`
    /// dependency, since bisect steps are not experiments.
    pub fn run_bisect(&self, task: &TaskSpec, good: &str, bad: &str) -> bisect::Result<BisectRecord>;
}
```

`race` depends on `experiment` (and transitively nothing new — every dependency `race` needs, it
gets through `ExperimentRunner`). `bisect` depends on `git`, `git::worktree`, `evaluator`,
`store`, and `domain` directly — deliberately *not* on `experiment`, mirroring SPEC §4's decision
that a bisect step produces an `EvaluatorVerdict`, not an `ExperimentRecord`. This asymmetry (race
depends on experiment, bisect doesn't) is the concrete, checkable form of "race and bisect share
evaluator infrastructure" — they share the layer *below* experiment (`evaluator`, `git`, `store`),
not experiment itself, which is exactly right since only one of the two ever involves an agent.

---

## 11. `scoring` — pure function, no infrastructure

```rust
pub fn score(
    metrics: &RawMetrics,
    baseline: &EvaluatorVerdict,
    evaluator_spec: &EvaluatorSpec,
    weights: &ScoringWeights,
) -> ScoreCard;
```

No struct, no trait, no dependencies beyond `domain` — SPEC §15 requires this to be recomputable
from persisted data with zero I/O and zero process spawns (`score --weights alt.toml`'s
acceptance criterion is literally "spawns no process"), so the type signature enforces that by
construction: there is nothing in scope for this function to spawn a process *with*.

---

## 12. `store` — the persistence layer everything else reads/writes through

**All persistence implemented (2026-08-13, "CLI integration and cleanup" pass — `policy` was the
last gap).** `load_task`/`save_task`/`list_tasks`, `load_evaluator`/`save_evaluator`/
`list_evaluators`, `load_fault`/`save_fault`/`list_faults` (2026-08-12, "repository fault
injection" pass), `load_mutant`/`save_mutant`/`list_mutants` (2026-08-12, standalone mutation
testing pass), `load_scoring_weights` and `save_experiment`/`load_experiment`/`save_race`/
`load_race`/`save_bisect`/`load_bisect` (2026-08-12, "evaluation reporting" pass),
`load_policy`/`save_policy`/`list_policies` (2026-08-13) are all real: plain TOML files under
`<repo_root>/{tasks,evaluators,faults,mutants,policies}/<id-or-name>.toml` for the id-keyed
collections, `<state_root>/{experiments,races,bisects}/<id>/` (manifest + separate raw-data files,
so a still-`Running`/in-progress record's manifest never needs an `Option` placeholder) for the
external-state-root ones, `save_*`'s collision rule (`AlreadyExists` unless `force`) matching
SPEC §20 (C6) where it applies, ids listed sorted for deterministic CLI/`--json` output.
`load_policy`/`save_policy` mirror `load_evaluator`/`save_evaluator` exactly, keyed by
`PermissionPolicy.name` rather than a separately-supplied id. One further addition beyond the
original sketch below: `list_experiments()` (directory names under `<state_root>/experiments/`,
not `.toml` file stems like `list_tasks`/`list_evaluators` — each experiment is a directory, not a
single file), added for `clean`'s reconciliation pass and its `--all-worktrees`/`--older-than`
selection, both of which need every known experiment id, not just one.

**Hardened (2026-08-13, adversarial security review — `docs/ADVERSARIAL_REVIEW.md` finding 1).**
Every `load_*`/`save_*` above now validates its `id`/`name` argument (`[A-Za-z0-9_-]`-only,
non-empty) before joining it onto a path — mirroring the rule `workspace::validate_id`/
`fault::validate_id` already enforced at their own boundary, which `Store` itself never had. Before
this, a `task`/`evaluator`/`policy` id sourced from a user-authored spec file, or an `experiment`/
`race`/`bisect` id sourced from a bare CLI argument (`report show <id>`, `clean --experiment
<id>`), reached `format!("{id}.toml")`/`.join(id)` completely unchecked — a real, exploitable path
traversal (arbitrary file write with `--force` on the save side; on the read side, `clean
--experiment <id>` chains into a `git worktree remove --force` on whatever `worktree_path` the
loaded file happens to contain). `Store` is the one choke point every id-addressed collection
passes through regardless of caller, so this is enforced once, here, rather than at each of the
several CLI entry points that call into it.

```rust
pub struct Store {
    repo_root: PathBuf,   // <repo>/.agentforge
    state_root: PathBuf,  // external, resolved by WorktreeManager::resolve_state_root
}

impl Store {
    pub fn open(repo_root: PathBuf, state_root: PathBuf) -> Self;

    pub fn load_task(&self, id: &str) -> store::Result<TaskSpec>;
    pub fn save_task(&self, task: &TaskSpec, force: bool) -> store::Result<()>;
    pub fn list_tasks(&self) -> store::Result<Vec<String>>;

    pub fn load_evaluator(&self, id: &str) -> store::Result<EvaluatorSpec>;
    pub fn save_evaluator(&self, spec: &EvaluatorSpec, force: bool) -> store::Result<()>;
    pub fn list_evaluators(&self) -> store::Result<Vec<String>>;  // added 2026-08-12, for `evaluator list`

    // FaultRef is standalone (SPEC §20 C3 applies to MutationRef only) — added 2026-08-12,
    // "repository fault injection" pass.
    pub fn load_fault(&self, id: &str) -> store::Result<FaultRef>;
    pub fn save_fault(&self, fault: &FaultRef, force: bool) -> store::Result<()>;
    pub fn list_faults(&self) -> store::Result<Vec<String>>;

    // MutantRef is standalone too, mirroring FaultRef exactly — added 2026-08-12, standalone
    // mutation testing pass (§9b).
    pub fn load_mutant(&self, id: &str) -> store::Result<MutantRef>;
    pub fn save_mutant(&self, mutant: &MutantRef, force: bool) -> store::Result<()>;
    pub fn list_mutants(&self) -> store::Result<Vec<String>>;

    // Keyed by PermissionPolicy.name, not a separately-supplied id — added for real 2026-08-13.
    pub fn load_policy(&self, name: &str) -> store::Result<PermissionPolicy>;
    pub fn save_policy(&self, policy: &PermissionPolicy, force: bool) -> store::Result<()>;
    pub fn list_policies(&self) -> store::Result<Vec<String>>;

    pub fn load_scoring_weights(&self) -> store::Result<ScoringWeights>;

    pub fn save_experiment(&self, record: &ExperimentRecord) -> store::Result<()>;
    pub fn load_experiment(&self, id: &str) -> store::Result<ExperimentRecord>;
    // Directory names under <state_root>/experiments/, not .toml file stems — added 2026-08-13
    // for `clean`'s reconciliation pass and its --all-worktrees/--older-than selection.
    pub fn list_experiments(&self) -> store::Result<Vec<String>>;

    pub fn save_race(&self, record: &RaceRecord) -> store::Result<()>;
    pub fn load_race(&self, id: &str) -> store::Result<RaceRecord>;

    pub fn save_bisect(&self, record: &BisectRecord) -> store::Result<()>;
    pub fn load_bisect(&self, id: &str) -> store::Result<BisectRecord>;
}
```

`Store` is the only module that knows SPEC §5's directory layout and TOML-vs-JSON format split.
Every other module that needs a `TaskSpec`/`ExperimentRecord`/etc. gets it from `Store`, never by
reading a path it constructed itself — this is what makes `report`'s "structured results only"
guarantee checkable by inspection: `report` holds a `&Store` and nothing else that touches a
filesystem path.

Note `RaceRecord` (per SPEC §4/§12) stores only the participant list, not a leaderboard — `Store`
has no `save_leaderboard`/`load_leaderboard` method because there is no such artifact; `report`
computes the ranked view by calling `load_experiment` once per participant.

`store` depends on `domain` only.

---

## 13. `report` — structured-in, human-or-JSON-out

```rust
pub struct Reporter<'a> {
    store: &'a Store,
}

impl<'a> Reporter<'a> {
    pub fn new(store: &'a Store) -> Self;

    pub fn render_experiment(&self, id: &str) -> report::Result<String>;
    pub fn render_race(&self, id: &str) -> report::Result<String>;
    pub fn render_bisect(&self, id: &str) -> report::Result<String>;

    pub fn experiment_json(&self, id: &str) -> report::Result<serde_json::Value>;
    pub fn race_json(&self, id: &str) -> report::Result<serde_json::Value>;
    pub fn bisect_json(&self, id: &str) -> report::Result<serde_json::Value>;
}
```

`render_*` returns a `String` rather than writing directly to stdout so the SPEC §18 acceptance
tests (substring/structure checks against `show`'s output) can assert against it without capturing
process stdout. `Reporter` holds only `&Store` — no `Executor`, no `GitRepo` — which is the
type-level proof that reporting cannot shell out to anything or re-derive data the way it might be
tempted to (e.g. re-running `git diff` instead of reading the stored `patch.diff`).

`report` depends on `store`, `scoring` (the `score` command path recomputes a `ScoreCard` from
loaded `RawMetrics` before rendering), and `domain`.

---

## 14. `cli` — wiring, not logic

**Fully wired (2026-08-13, "CLI integration and cleanup" pass).** Every variant below has real
dispatch — the previous "`Workspace` is the only variant implemented, everything else reports a
clean not-implemented error" asymmetry is gone; `Init`/`Run`/`Race`/`Bisect`/`Verify`/`Report`/
`Policy`/`Clean` are all real now. This pass also regrouped the command surface for
discoverability (SPEC.md §6's Amendment has the full rationale): `Mutate`/`Mutation`/`Fault`/
`Mutant` — previously four commands at inconsistent altitudes (`Mutate` a bare verb, the other
three nouns-with-subcommands) despite being conceptually parallel repository-state test-fixture
mechanisms — became one `Experiment` namespace (`Fault`/`Mutation`/`Mutant`, with `Mutate` folded
into `Mutation` as its `Create` action); `Eval` was renamed `Verify`; `Score`/`Show`/`Log` became
one `Report` namespace (`Score`/`Show`/`Log`).

```rust
#[derive(clap::Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(clap::Subcommand)]
pub enum Command {
    Init(InitArgs),
    Workspace { #[command(subcommand)] action: WorkspaceAction },
    Evaluator { #[command(subcommand)] action: EvaluatorAction },
    Task { #[command(subcommand)] action: TaskAction },
    Experiment { #[command(subcommand)] action: ExperimentAction },
    Run(RunArgs),
    Race(RaceArgs),
    Bisect(BisectArgs),
    Verify(VerifyArgs),
    Report { #[command(subcommand)] action: ReportAction },
    Policy { #[command(subcommand)] action: PolicyAction },
    Clean(CleanArgs),
}

#[derive(clap::Subcommand)]
pub enum ExperimentAction {
    Fault { #[command(subcommand)] action: FaultAction },      // Inject, Show, Restore, Discard
    Mutation { #[command(subcommand)] action: MutationAction }, // Create, Show, Replay
    Mutant { #[command(subcommand)] action: MutantAction },     // Apply, Show, Evaluate, Restore, Discard
}

#[derive(clap::Subcommand)]
pub enum ReportAction {
    Show(ShowArgs),
    Score(ScoreArgs),
    Log(LogArgs),
}

pub fn run() -> std::process::ExitCode;
```

Every leaf variant maps 1:1 to a row in SPEC §6's command table — the exit-code precedence rules
defined there (e.g. `run`'s `Failed→1, TimedOut→124, Completed+gated→3, Completed+ungated→0`) are
implemented once, in `cli`'s dispatch for that command, by inspecting the `ExperimentStatus`/
`ScoreCard.gated`/`EvaluatorVerdict` that `experiment::ExperimentRunner::run`/
`bisect::BisectRunner::run_bisect`/`evaluator::Evaluator::evaluate` already returned — `cli` never
recomputes a verdict, it only maps an already-structured result to a process exit code.
`workspace exec` follows the same rule at a lower level: its exit code mirrors the child process's
own exit code (or `124` on timeout), read directly off the `ProcessOutcome`
`WorkspaceManager::exec` already returned — never recomputed. `run`/`race`/`bisect` print the same
report `report show`/`report score` would (human or `--json`) by constructing a `Reporter` over
the same `Store` they just wrote through — reusing `report`'s own formatting rather than a second,
CLI-local copy.

Two small additions this pass made to primitives below `cli`, both additive and non-breaking
(existing call sites/signatures untouched): `experiment::ExperimentRunner` gained
`run_keep_worktree_on_fail` (identical to `run` except it preserves the worktree when the
finalized status is `Failed`/`TimedOut` — `run --keep-worktree-on-fail`'s CLI flag existed before
this pass but had nothing real to call, since `run` itself always removed the worktree
unconditionally); `domain::policy::PermissionPolicy` gained `generous_default(name)`, factoring
out what was previously `race::default_policy()`'s own private literal so `run`'s
`--policy`-omitted fallback shares the exact same shape instead of a second, driftable copy.

`clean` reconciles (any `Running` experiment with no `RUNNING.lock` → `Failed`, via the new
`Store::list_experiments`) before performing its requested removal; `--older-than <duration>`
parses a simple `<n><unit>` string (`s`/`m`/`h`/`d`). `init` scaffolds `.agentforge/{tasks,
evaluators,policies}/`, `config.toml` (caching the resolved `state_root`), and `scoring.toml`
(the built-in default weights) — refuses (exit 2) if `.agentforge/` already exists or `--repo`
isn't a git repository.

`cli` is the only module allowed to depend on everything else; it constructs the `Arc<dyn
Executor>`, `Arc<GitRepo>`, `Arc<WorktreeManager>`, `Arc<Store>`, etc. once at startup and passes
them down into `ExperimentRunner`/`RaceRunner`/`BisectRunner`/`MutationEngine`/`Reporter`/
`WorkspaceManager`. No other module constructs its own dependencies — this is the
dependency-injection seam that makes substituting `FakeAdapter`/`SpyExecutor`/`NullAuditSink` in
tests possible without touching production wiring beyond this one module. `run`/`race` are the
one place `cli` calls `adapter::resolve` — mapping a bare `adapter[:model]` string to a real
`Box<dyn AgentAdapter>` is exclusively `cli`'s job, never a library-level module's (§7).

---

## 15. Error types

Two layers, matching the module graph:

```rust
// error.rs — top-level, what `cli` and public library consumers see
#[derive(Debug, thiserror::Error)]
pub enum AgentForgeError {
    #[error(transparent)] Git(#[from] git::Error),
    #[error(transparent)] Exec(#[from] exec::Error),
    #[error(transparent)] Audit(#[from] audit::Error),
    #[error(transparent)] Adapter(#[from] adapter::Error),
    #[error(transparent)] Evaluator(#[from] evaluator::Error),
    #[error(transparent)] Mutation(#[from] mutation::Error),
    #[error(transparent)] Experiment(#[from] experiment::Error),
    #[error(transparent)] Race(#[from] race::Error),
    #[error(transparent)] Bisect(#[from] bisect::Error),
    #[error(transparent)] Store(#[from] store::Error),
    #[error(transparent)] Report(#[from] report::Error),
    #[error(transparent)] Workspace(#[from] workspace::Error),
    #[error("validation error: {0}")] Validation(String),
}

pub type Result<T> = std::result::Result<T, AgentForgeError>;
```

```rust
// each subsystem module, e.g. git/mod.rs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")] Io(#[from] std::io::Error),
    #[error("git command failed: {0}")] CommandFailed(String),
    #[error("not a valid commit-ish: {0}")] InvalidCommitish(String),
    // ...
}
pub type Result<T> = std::result::Result<T, Error>;
```

This two-layer shape (rather than one flat enum, or one error type per function) was asked for
explicitly by the task brief's "define... error types" — plural — and earns its keep here for a
concrete reason: `git::Result<T>`/`evaluator::Result<T>`/etc. let each module's own function
signatures stay precise (a `GitRepo` method can only fail in ways `git::Error` enumerates), while
`?` still bubbles everything up into one `AgentForgeError` at the `cli` boundary via `#[from]`,
with `thiserror(transparent)` meaning `cli` doesn't need a second copy of every error message —
the leaf error's `Display` impl is used as-is.

---

## 16. Dependencies (`Cargo.toml`)

Chosen for what the *interfaces* in this document need to compile and be meaningfully typed, not
for behavior that isn't written yet:

| Crate | Why |
|---|---|
| `serde` (`derive`) | Every `domain` type needs `Serialize`/`Deserialize` — SPEC §5 requires TOML specs and JSON records, and "structured results" (§13) only means something if the structures are real serde types now, not added later. |
| `serde_json` | JSON records/`--json` output; `report`'s `serde_json::Value` return type. |
| `toml` | Spec files (`tasks/`, `policies/`, `evaluators/`, `config.toml`, `scoring.toml`). |
| `chrono` (`serde`) | `DateTime<Utc>` fields on `TaskSpec`/`ExperimentRecord`, and the ID scheme's timestamp component. |
| `thiserror` | The two-layer error design in §15. |
| `clap` (`derive`) | `cli`'s command tree (§14) — this *is* a public interface the task brief asked to define, not deferred behavior. |

Deliberately **not** included yet, because nothing in the current interfaces needs the type: a
regex crate (`MetricExtractor.pattern` is a plain `String`, compiled lazily inside `evaluator`'s
still-unwritten implementation), an async runtime (`race`'s bounded parallelism is designed as
synchronous `std::thread` fan-out, not `tokio` — SPEC.md never requires anything to be `async`,
and a CLI that mostly waits on child processes doesn't need one), and a native git binding
(§6 explains why `git` shells out instead). Adding any of these later is a `Cargo.toml` line, not
an architecture change.

---

## 17. Current implementation status

This document originally described a compiling module skeleton — every struct/enum/trait real,
every function body a `todo!()` pointing at the SPEC.md section that would govern its real
implementation. That phase is long over. As of the 2026-08-13 "CLI integration and cleanup" pass,
every module in §1's dependency graph and every row in SPEC.md §6's command table is a real,
tested implementation with no `todo!()` or dispatch-stub remaining anywhere in `src/`. Since then,
two further passes hardened rather than extended it: a from-scratch adversarial security review
(`docs/ADVERSARIAL_REVIEW.md`, 5 fixed findings — see `docs/SECURITY.md` for the reader-facing
version) and a root-cause debugging pass that fixed a Windows-specific flaky-test source and a
subprocess temp-file leak. A fully local, zero-paid-API demo (`cargo run --example demo`,
`cargo test --test demo_e2e`) now exercises the entire documented CLI surface end to end.

`cargo test`: **284 passed, 0 failed.** `cargo clippy --all-targets --all-features -- -D
warnings` and `cargo fmt --check`: clean. `docs/TEST_STATUS.md` holds the full dated history of
every pass that got the project here; re-run `cargo test` rather than trusting any number in this
document or that one once you're actively changing code — both are point-in-time snapshots, not
live status.
