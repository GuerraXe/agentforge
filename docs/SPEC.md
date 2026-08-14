# AgentForge — MVP Specification (v2)

Status: draft, pre-implementation. Nothing in this repo is built yet (see `../CONTEXT.md` for
the environment/tooling snapshot this spec was written against). This is v2, revised against
`docs/SPEC_REVIEW.md`'s adversarial critique of v1 — every finding in that review is resolved
below, not merely acknowledged; §20 maps each finding to exactly where it was fixed.

Guiding principle, unchanged from v1: **correctness must heavily outweigh speed and patch
size.** v2 makes this a harder structural guarantee than v1 did (§15's scoring gate now also
checks the evaluator's exit code and a test-count regression, closing the "delete the failing
test" loophole the review found).

---

## 1. Goals

AgentForge is a Rust CLI that lets a developer:

1. Point one or more coding-agent configurations at a task defined against a real Git repo.
2. Run each agent in an isolated worktree so runs never collide or corrupt the source repo.
3. Judge the resulting patch with a deterministic, repo-defined evaluator (build + tests, not
   agent self-report).
4. Turn that same evaluator into an oracle for two more things: ranking multiple agents/configs
   against each other ("race"), and semantic bisect (find the commit where the evaluator's
   verdict flips).
5. Inject reproducible, seeded faults into a repo to generate meaningful tasks and mutation-style
   experiments.
6. See results as both raw metrics and a transparent, configurably-weighted 0–100 score, backed
   by a structured audit log that AgentForge itself observed — not one the agent self-reports.
7. Do all of this against Claude Code first, through an interface that doesn't assume Claude Code
   is the only agent that will ever exist.

---

## 2. Architecture at a Glance

The whole design is one separation, applied consistently: **the agent does the work; everything
else is deterministic software that controls, isolates, tests, and judges that work — and never
trusts the agent's own account of what it did.**

| Component | Owns | What it guarantees |
|---|---|---|
| **Worktree** (§8) | A disposable, isolated Git checkout per unit of work | The agent's filesystem starting point is never the caller's real checkout, and lives outside the target repo's own directory tree. |
| **Executor** (§9) | Spawning, bounding, and observing *every* subprocess AgentForge runs (agent process and evaluator steps alike) | Timeout, output caps, working directory, and environment exposure — for the process it directly spawns. The adapter never spawns anything itself. |
| **Adapter** (§10) | Translating a task+model into a command to run | Nothing about execution — only *what* to run, never *how* it's run. |
| **Evaluator** (§12) | Deciding, deterministically, whether a patch is good | The one judgment every other feature (race, bisect, mutation sanity-check) defers to. Never the agent's job. |
| **Scoring model** (§15) | Turning a judgment into a transparent, recomputable number | Correctness dominates; gamed or partial correctness cannot outscore genuine correctness (§15's gate). |
| **Audit log** (§16) | An independent record of what the Executor observed | Never populated by adapter code — the agent has no path to editing its own audit trail. |

Everything downstream (races, bisect, mutation) is this same set of primitives composed, not
reimplemented — see §20 (D1) for how v1 left this implicit and v2 makes it an explicit,
single-implementation contract.

---

## 3. MVP Scope & Phasing

v1 said every capability shipped "together," then contradicted itself by having `CONTEXT.md`
sequence delivery anyway (review finding A1). v2 resolves this directly: the architecture is
designed as one coherent whole (§2), but MVP completion happens in two phases, and "MVP done"
means both phases complete — phasing is a delivery order, not a scope cut.

**Phase 1 — the core loop (proves the central claim in §2):**
`init`, `evaluator add`, `task add` (with baseline capture), worktree lifecycle, the Executor,
the `AgentAdapter` trait plus `FakeAdapter` and the `claude-code` adapter, `run`, scoring, the
audit log, and reporting (`show`/`score`/`log`). This alone demonstrates "agents perform work,
but deterministic software controls, isolates, tests, and judges that work" end to end.

**Phase 2 — composition on top of Phase 1 (no new architecture, only reuse):**
`mutate`, `race`, `bisect`. Each is Phase 1's primitives composed differently: `race` is N `run`s
plus a deterministic ranking; `bisect` is repeated `evaluate()` calls plus a binary search;
`mutate` is a git-plumbing commit plus one `evaluate()` sanity check. None of the three introduces
a new way to spawn a process, isolate a filesystem, or judge a patch.

## 3.1 Non-Goals / Roadmap (explicitly out of MVP)

- **OS-level sandboxing** (containers, Windows Job Objects, seccomp/AppArmor, network
  firewalling). Documented honestly, not hidden (§17).
- **Process-tree resource limits.** The Executor bounds the process it directly spawns; a
  grandchild process the agent spawns and detaches is not guaranteed to be killed on timeout
  (§9, §17) — real process-tree containment needs Job Objects/cgroups, which is sandboxing
  territory and out of scope with it.
- **Mediated tool execution inside an agent's own process.** v1's `CommandPolicy` tried to
  restrict what the *agent* runs internally (e.g. shell commands Claude Code invokes as part of
  its own tool use) — cut for MVP (review A3, X1) because it had zero enforcement behind it for
  an adapter that's one opaque process from AgentForge's point of view. Still a roadmap item, for
  an adapter that genuinely mediates its own tool calls.
  **Amendment (permission-policy layer pass):** a narrower, genuinely enforceable version now
  exists at the `Executor` boundary instead — see §4's `PermissionPolicy.allowed_programs`/
  `denied_programs` and §16. This restricts which *programs the Executor itself is asked to
  spawn* (the agent's top-level command, every evaluator `setup_cmds`/`test_cmd` step, and git),
  not what a spawned agent process does internally — that distinction is exactly why this is
  enforceable where v1's mediated-tool-call version wasn't.
- **Adapters beyond Claude Code** (Codex CLI, Aider, etc.). The trait exists so these are
  additive later; none are implemented in MVP.
- **AST-aware mutation.** MVP mutators are language-agnostic text-pattern operators (§11); a
  real per-language AST mutation engine is a roadmap item.
- **Token/cost tracking and adapter-specific telemetry.**
- **Distributed/remote execution, multi-machine races.**
- **A web UI or dashboard.** Terminal output + on-disk JSON only.
- **crates.io publication.** MVP ships as a local binary.
- **Non-linear / merge-aware bisect**, or bisecting across agent-race branches.
- **Real `git bisect` integration.** v1 shelled into git's own bisect state machine; v2 replaces
  this with an in-process binary search (review C1, A2, T2, F2) — same result, none of the
  external-process/exit-code-convention/dangling-state problems.
- **Automatic detection of policy violations beyond what the Executor directly observes.** v1's
  `PolicyViolation` status/event implied a detection capability MVP doesn't have; it's cut (review
  X3) rather than left as an unbacked claim.
- **Full CI cross-platform matrix design.**
- **Opt-in real-Claude-Code test lane** (`AGENTFORGE_TEST_REAL_CLAUDE=1`, §18). Needs a real
  Claude Code installation and paid API access; every other test in this codebase deliberately
  substitutes a scripted stand-in (`FakeAdapter`, or `src/bin/mock_claude.rs` via
  `AGENTFORGE_CLAUDE_EXECUTABLE`) instead, so this was never built and isn't planned — a testing
  capability, not a product gap (§17/§20, verification pass 2026-08-14 — `docs/VERIFICATION.md`).
- **A `--policy` flag on `race`.** `race` always uses its own internal `race::default_policy()`
  (a generous built-in default); unlike `run`, there's no way to hand it a named, custom-denying
  policy through the CLI, which means `race`'s exit-1-when-zero-participants-completed branch
  can't currently be exercised through the compiled binary in a test (§17/§18 row 20,
  verification pass 2026-08-14 — `docs/VERIFICATION.md`). A small CLI surface addition, not a
  correctness gap in `report_race_result` itself.

---

## 4. Concepts & Data Model

All structures below are Rust-flavored pseudocode describing the MVP contract, not final
implementation. Every persisted struct is `serde`-serializable.

### EvaluatorVerdict — the one shared judgment type

```
struct EvaluatorVerdict {
    build_succeeded: bool,
    tests_total: Option<u32>,
    tests_passed: Option<u32>,
    exit_code: i32,        // -1 is reserved: "killed by timeout", never a real process exit code
    timed_out: bool,
    wall_time_secs: f64,
}

impl EvaluatorVerdict {
    fn is_good(&self) -> bool {
        self.build_succeeded && !self.timed_out && self.exit_code == 0
    }
}
```

This is the return type of the one `evaluate()` function (§12) that `run`, `race`'s participants,
`bisect`'s binary search, `mutate`'s sanity gate, `task add`'s baseline capture, and `eval` all
call — the single implementation review finding D1 asked for.

### Task

```
struct TaskSpec {
    id: String,
    name: String,
    prompt: String,
    repo_path: PathBuf,
    base_ref: String,             // always a resolved 40-hex commit SHA — see §20 (R1)
    mutation: Option<MutationRef>,
    evaluator: String,            // id of an EvaluatorSpec — MANDATORY, never optional
    agent_timeout_secs: u64,
    baseline: EvaluatorVerdict,   // captured once, at registration time, against base_ref — §20 (S1)
    created_at: DateTime<Utc>,
}
```

`base_ref` is resolved via `git rev-parse` at `task add`/`mutate` time and the resolved SHA is
what's persisted — the CLI accepts any commit-ish as input, but a moving branch name is never
what's stored, so two runs against the same task are guaranteed to start from the same commit
(§20, R1).

`baseline` exists specifically so the scoring gate can detect a shrinking test count without a
second, separate mechanism (§15) — it's produced by the same `evaluate()` call every other
capability uses, run once against `base_ref` when the task is registered. It is **not** required
to be a good verdict (a mutation task's baseline is expected to be bad by design) — it's purely
a reference point for comparison, not a gate on registration.

### MutationSpec / MutationRef

```
enum MutationOperator { NegateCondition, OffByOne, BooleanFlip, DeleteStatement, SwapOperator }

struct MutationSpec {
    operator: MutationOperator,
    target_glob: String,
    seed: u64,
    operator_version: u32,   // bumped whenever candidate-discovery behavior changes
}

struct MutationTarget {
    file: String,             // forward-slash-joined, repo-root-relative
    line: u32,
    column: u32,
}

struct MutationRef {
    spec: MutationSpec,
    base_commit: String,        // resolved SHA the mutation was applied on top of
    mutant_commit: String,      // resulting commit, on refs/agentforge/mutants/<task-id>
    sanity_checked: bool,       // true once the evaluator confirmed the mutant is "killed"
    selected_target: MutationTarget,  // the one candidate `apply()` selected
    mutant_ref: String,         // the exact git ref mutant_commit is reachable from
    diff_stats: DiffStats,      // structured diff of mutant_commit against base_commit
    applied_at: DateTime<Utc>,  // NOT part of the determinism/identity contract below
}
```

A `MutationRef` only ever exists embedded in a `TaskSpec` — `mutate` requires `--task-id` and
always creates that task on success (§20, C3). There is no standalone, task-less mutation record
in MVP.

**Amendment (2026-08-12):** `MutationRef` gained `selected_target`/`mutant_ref`/`diff_stats`/
`applied_at` — additive reproducibility metadata (what was mutated, where the mutant commit is
reachable from for inspection/cleanup, what changed, and when), not a reopening of the
"embedded-only, no standalone record" decision above: these fields are still only ever read
through the owning `TaskSpec`. `mutant_ref` is the entire restore/cleanup surface for a mutant no
longer needed (`MutationEngine::discard` deletes it) — pure git plumbing never creates a worktree
or touches `HEAD`, so nothing else needs restoring. `applied_at` is explicitly excluded from the
determinism contract in §10 below: replaying identical `(operator, target_glob, seed,
operator_version, base_commit)` must reproduce an identical `mutant_commit`/`selected_target`/
`diff_stats` regardless of when the replay happens. `agentforge mutation show <task-id>` prints
this record; `agentforge mutation replay <task-id>` re-applies it under a throwaway ref and
asserts the reproduction — SPEC.md §10's determinism contract exercised directly.

### FaultSpec / FaultRef

**Added 2026-08-12 (§10 Amendment) — a second, distinct mechanism from `MutationSpec`/
`MutationRef` above.** Repository fault injection simulates a broken repository/environment state
rather than a source-code logic bug, and its four fault kinds (a missing file, a stale generated
artifact) can't be expressed as a text-pattern regex over a tracked source line — see §10's
amendment for the full rationale.

```
enum FaultKind { MissingFile, BrokenConfigValue, StaleArtifact, DependencyCorruption }

struct FaultSpec {
    kind: FaultKind,
    target_glob: String,
    seed: u64,
    fault_version: u32,   // bumped whenever candidate-discovery/transformation behavior changes
}

struct FaultTarget {
    file: String,              // forward-slash-joined, repo-root-relative
    line: Option<u32>,         // None for the whole-file kinds (MissingFile, StaleArtifact)
}

struct FaultRef {
    id: String,                 // caller-chosen, validated — this record's Store key AND the
                                 // fault workspace's directory name
    spec: FaultSpec,
    base_commit: String,        // resolved SHA the fault was injected on top of
    selected_target: FaultTarget,
    description: String,        // exact, human-readable account of what changed
    worktree_path: PathBuf,     // serialized as `workspace_path` (serde rename) — the isolated
                                 // worktree the fault was written into directly
    diff_stats: DiffStats,
    applied_at: DateTime<Utc>,  // NOT part of the determinism/identity contract, same as MutationRef
}
```

Unlike `MutationRef`, `FaultRef` is a **standalone** record — it has no wrapping `TaskSpec` to
embed into (`experiment`/`ExperimentRunner` doesn't exist yet, so there is nothing to wire it
into), so it carries its own `id` and is persisted directly (`Store::save_fault`/`load_fault`/
`list_faults`, mirroring `tasks`/`evaluators`). This is a deliberate, narrower scope than
`MutationRef`'s embedded-only contract — not a reopening of §20 (C3)'s "no standalone, task-less
mutation record" finding, which was specifically about `MutationRef`.

### AgentConfig / PermissionPolicy

```
struct AgentConfig {
    adapter: String,          // e.g. "claude-code"
    model: Option<String>,
    extra_args: Vec<String>,
    policy: String,           // name of a PermissionPolicy
}

struct PermissionPolicy {
    name: String,
    extra_readonly_paths: Vec<PathBuf>,  // adapter-requested, best-effort (§17)
    deny_network: bool,                   // adapter-requested, best-effort (§17); default true
    env_passthrough: Vec<String>,         // Executor-enforced allowlist of host env vars — §20 (X2)
    max_wall_time_secs: u64,              // Executor-enforced; must be > 0 — §20 (S4)
    max_output_bytes: u64,                // Executor-enforced; must be > 0 — §20 (S4)
    allowed_programs: Vec<String>,        // Executor-enforced; empty = unrestricted
    denied_programs: Vec<String>,         // Executor-enforced; checked before allowed_programs
    allowed_roots: Vec<PathBuf>,          // Executor-enforced; empty = unrestricted
    max_memory_bytes: Option<u64>,        // representation only — Unsupported, no MVP platform enforces it
}
```

v1 had a separate `CommandPolicy`/`NetworkMode` with three enum variants and a glob allowlist;
all of it was `RequestedOnly` for the one adapter MVP ships, so v2 cuts it down to what's
actually load-bearing (§20, A3/X1). `env_passthrough` moved here from `AgentConfig` — it's a
permission concern, not an agent-identity concern (§20, X2).

**Amendment (permission-policy layer pass):** `allowed_programs`/`denied_programs`/
`allowed_roots` are a new, narrower `CommandPolicy` the `Executor` itself checks before every
spawn — the agent's command, every evaluator step, and git alike — via
`PermissionPolicy::command_policy()`. Unlike `env_passthrough`'s fail-closed convention (empty
means "pass nothing"), an empty `allowed_programs`/`allowed_roots` means "no restriction
configured" — the safer default for a dimension most callers won't opt into, since a config that
silently defaulted to zero permitted programs would break every existing caller. `denied_programs`
always wins over `allowed_programs` for a program listed in both. `max_memory_bytes` is
representation-only: no MVP platform enforces it (§3.1's Job Objects/cgroups non-goal still
applies), and `PermissionPolicy::enforcement_report()` always tags it `Unsupported` — it exists so
a policy author's intent is recorded, not acted on.

### Evaluator

```
struct EvaluatorSpec {
    id: String,
    setup_cmds: Vec<Cmd>,        // run once before test_cmd; first failure short-circuits — §20 (F4)
    test_cmd: Cmd,
    timeout_secs: u64,           // must be > 0 — validated at `evaluator add`, §20 (S4)
    budget_secs: u64,            // must be > 0 — efficiency score input
    size_budget_lines: u32,      // must be > 0 — parsimony score input
    metric_extractors: Vec<MetricExtractor>,  // regex-based: name + pattern + capture group
}
```

`Cmd` is `{ program: String, args: Vec<String>, cwd_relative: PathBuf }`.

### Experiment (the atomic unit)

```
struct ExperimentRecord {
    id: String,
    task_id: String,
    agent_config: AgentConfig,
    policy_snapshot: PermissionPolicy,
    base_ref: String,
    mutation_ref: Option<MutationRef>,
    race_id: Option<String>,     // set only if launched as part of a race
    race_index: Option<u32>,     // deterministic position, fixed before execution starts — §20 (R2/T1)
    worktree_path: PathBuf,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    status: ExperimentStatus,    // Running | Completed | Failed | TimedOut — §20 (X3) drops PolicyViolation
    patch_path: PathBuf,
    raw_metrics: Option<RawMetrics>,
    score: Option<ScoreCard>,
    audit_log_path: PathBuf,
}

struct RawMetrics {
    verdict: EvaluatorVerdict,     // from evaluating the agent's patch
    diff: DiffStats,               // files_changed, lines_added, lines_removed — one shared fn, §20 (D4)
    agent_timed_out: bool,         // the AGENT process itself was killed by the Executor
}
```

Patch capture is **adapter-independent by design**: after the agent process exits (or is killed),
AgentForge always runs `git diff <base_ref>` inside the worktree itself. It never trusts an
adapter to self-report its changes.

### Race / Bisect

```
struct RaceRecord {
    id: String,
    task_id: String,
    max_parallel: u32,
    participants: Vec<{ experiment_id: String, race_index: u32 }>,  // fixed at construction time
}
```

v1 stored a denormalized `leaderboard: Vec<(experiment_id, ScoreCard)>` on the race record
itself, which could silently disagree with an experiment's own `score.json` after an individual
rescore. v2 stores only the declared participant list; `agentforge report show <race-id>` always
computes the ranked table live from each experiment's current `score.json` (§20, D3) — there is
exactly one place a `ScoreCard` lives on disk.

```
struct BisectStep { commit: String, verdict: EvaluatorVerdict, is_good: bool }

struct BisectRecord {
    id: String,
    task_id: String,
    evaluator_id: String,
    range: (String /* good sha */, String /* bad sha */),
    worktree_path: PathBuf,   // serialized as `bisect_worktree` (serde rename) — unified Rust-level
                              // name with ExperimentRecord/FaultRef/MutantRef's own worktree_path
    steps: Vec<BisectStep>,
    culprit: Option<String>,
}
```

Bisect steps are direct `evaluate()` calls, not full experiments — there's no agent and no
`ScoreCard` involved, so bisect doesn't manufacture `ExperimentRecord`s for something that isn't
an experiment (a v1 ambiguity; see §20, D1).

### ScoreCard (see §15 for the full model)

```
struct ScoreCard {
    total: u8,                        // 0-100
    rating: Rating,                   // Excellent | Good | Fair | Poor | Fail
    gated: bool,                      // true if the correctness gate applied
    components: Vec<ScoreComponent { name: String, raw: f64, normalized: f64, weight: f64, contribution: f64 }>,
    weights_source: String,
    formula_version: String,
}
```

### AuditEvent

```
enum AuditEvent {
    ProcessSpawn      { command: String, args: Vec<String>, cwd: PathBuf, at: DateTime<Utc> },
    ProcessExit       { exit_code: Option<i32>, timed_out: bool, at: DateTime<Utc> },
    PermissionCheck   { action: String, allowed: bool, reason: String, at: DateTime<Utc> },
    WorktreeCreated   { path: PathBuf, base_commit: String, at: DateTime<Utc> },
    WorktreeRemoved   { path: PathBuf, at: DateTime<Utc> },
}
```

Amendment (simplification pass): `EvaluatorStep`/`FileChangeSummary` were removed — declared and
even rendered by the CLI's log formatter, but nothing in the evaluator or experiment pipeline ever
constructed either, so they could never actually appear in a real audit log. D2/D4 in
`docs/SPEC_REVIEW.md` had already flagged this exact gap (undefined relationship between
`audit.jsonl` and `eval/log.jsonl`; diff-stat computation with no single stated producer). If a
real evaluator/file-change audit trail is wanted later, it should be added back only once something
actually produces the events, not re-declared as an unproduced placeholder.

`PermissionCheck` is emitted **only** for checks the Executor itself performs and can verify —
cwd assignment, env-var filtering, output-cap enforcement. It is never emitted for adapter-relayed
best-effort requests (network denial, readonly paths), so the log never implies verification that
didn't happen. v1's `PolicyViolation` event is cut — MVP has no mechanism that can honestly
detect one (§20, X3).

Every event carries the owning `experiment_id` implicitly (one audit log file per experiment).

---

## 5. Repository Layout & Persistence Format

`agentforge init` creates two things: a small, git-tracked spec directory inside the target repo,
and an external, untracked state root **outside** the target repo's own directory tree (§20, U1).

```
<repo>/.agentforge/                  # TRACKED in the target repo's git history
  config.toml                        # state_root path, default policy/evaluator ids, max_parallel_experiments
  scoring.toml                       # weights, budgets, rating bands (§15)
  tasks/<task-id>.toml
  policies/<policy-name>.toml
  evaluators/<evaluator-id>.toml

<state_root>/                        # NOT inside <repo>; see below for how it's located
  worktrees/<experiment-id>/          # ephemeral
  bisect-worktrees/<bisect-id>/       # ephemeral
  tmp/                                 # ephemeral
  experiments/<experiment-id>/
    manifest.toml            # ExperimentRecord (minus large blobs)
    RUNNING.lock              # present iff status == Running; see §8's lock protocol
    patch.diff
    audit.jsonl
    agent-stdout.log
    agent-stderr.log          # both truncated per policy.max_output_bytes
    metrics.json               # RawMetrics
    score.json                 # ScoreCard, weights snapshot embedded
  races/<race-id>/
    manifest.toml             # RaceRecord
  bisects/<bisect-id>/
    manifest.toml
    steps.jsonl
    result.json
```

**Where `<state_root>` lives:** a platform data directory (`%LOCALAPPDATA%` on Windows,
`$XDG_DATA_HOME` or `~/.local/share` on Unix) under
`agentforge/state/<repo-id>/`, where `repo-id` is a stable hash of the canonical, symlink-resolved
absolute path of `git rev-parse --git-common-dir` for the target repo. This is deterministic
without needing `config.toml` to exist, but `config.toml` also caches the resolved path so it's
always visible with `cat .agentforge/config.toml` — moving state outside the repo must not also
make it hard to find, so `agentforge init` prints the resolved path on success and it's included
in `--json` output.

**Format rules:** human-authored specs (`tasks/`, `policies/`, `evaluators/`, `config.toml`,
`scoring.toml`) are TOML. Machine-generated single records are JSON. `audit.jsonl` and
`bisects/*/steps.jsonl` are JSON Lines, flushed line-by-line so a crash mid-run leaves a valid,
parseable prefix. Patches are unified diff text. v1 had a separate `eval/log.jsonl` alongside
`audit.jsonl` with no stated difference between them; v2 drops it — evaluator steps are audit
events, full stop (§20, D2).

**ID scheme:** `<UTC compact ISO8601><6 hex chars>`, e.g. `20260811T193000Z-9f3a2b`. Sortable by
creation time. Not used for anything that requires strict ordering guarantees within the same
second — see §13's `race_index`, which exists precisely because IDs alone don't provide that.

**`.agentforge/` inside the repo is meant to be committed** — specs are code, reviewed like code.
The state root is never committed (it isn't even inside the repo to accidentally commit).

---

## 6. CLI Commands

Global exit codes, used consistently by every command with no per-command exceptions:

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Generic/internal error (AgentForge itself failed, not a judgment about a patch) |
| `2` | Usage or validation error |
| `3` | A judged patch/commit was bad (evaluator verdict not good) — see the precedence tables below for exactly which commands can return this |
| `124` | An agent process was killed for exceeding its timeout |

**Command tree** (top-level nouns/verbs; every leaf below maps 1:1 to a row in the table that
follows): `init`, `workspace {create,list,show,exec,remove,clean}`, `evaluator {add,list,show}`,
`task {add,list,show}`, `experiment {fault,mutation,mutant}` (repository-state test-fixture
mechanisms — sibling to one another, none produce an `ExperimentRecord`), `run`, `race`, `bisect`,
`verify`, `report {show,score,log}`, `policy {add,list,show,validate}`, `clean`. Every command that
reads repo-tracked or state-root state takes a consistent `--repo` (default `.`); every command
that can produce a structured result takes a consistent `--json`.

| Command | Behavior |
|---|---|
| `agentforge init [--repo <path>] [--json]` | Must be run inside a Git repo. Fails (exit 2) if `.agentforge/` already exists. Scaffolds the layout in §5 (`tasks/`, `evaluators/`, `policies/`, `config.toml`, `scoring.toml`), resolves and prints `state_root`. |
| `agentforge evaluator add <file.toml> [--force]` | Validates an `EvaluatorSpec` (`timeout_secs`, `budget_secs`, `size_budget_lines` all `> 0`; `test_cmd` non-empty) and copies it into `evaluators/`. Exit 2, naming the field, on failure. Exit 2 on an existing id without `--force`. |
| `agentforge evaluator list` / `evaluator show <id>` | Listing / detail. `--json` on both. |
| `agentforge task add <file.toml> [--force]` | Validates a `TaskSpec` (evaluator id resolves via `evaluator show`; `prompt` non-empty), resolves `base_ref` to a SHA via `git rev-parse`, captures `baseline` by calling `evaluate()` once against that SHA in a throwaway evaluation worktree (§8), then writes the task. Exit 2 on validation failure or an existing id without `--force`. |
| `agentforge task list` / `task show <id>` | Listing / detail, including the stored baseline. `--json` on both. |
| `agentforge experiment mutation create --task-id <id> --spec <file.toml> --base <ref> --evaluator <id> [--force]` | Deterministically applies a `MutationSpec` on top of `<ref>` via git plumbing (§11), commits to a dedicated ref, runs the sanity-gate `evaluate()` call, and — only if the mutant's verdict is **not** good (fault was detected) — creates the task named `--task-id`, reusing the sanity-gate's own `EvaluatorVerdict` as that task's `baseline` (no second evaluator invocation). If the mutant's verdict **is** good (fault undetected), exits 2, creates no task, and leaves the mutant commit under `refs/agentforge/mutants/rejected/...` for inspection. `--task-id` is required; `--force` follows the same collision rule as `task add`. |
| `agentforge experiment mutation show <task-id>` | Prints the `MutationRef` embedded in `<task-id>`'s `TaskSpec` in full: spec, base/mutant commits, `selected_target`, `diff_stats`, `mutant_ref`, `sanity_checked`, `applied_at`. Exit 2 if the task has no mutation record. |
| `agentforge experiment mutation replay <task-id>` | Re-applies the task's stored `(operator, target_glob, seed, operator_version, base_commit)` under a throwaway ref (deleted immediately after) and asserts it reproduces the identical `mutant_commit`/`selected_target`/`diff_stats` — SPEC.md §10's determinism contract exercised directly. Exit `0` on a match, `1` on a mismatch, `2` if the task has no mutation record. |
| `agentforge experiment fault inject --id <id> --spec <file.toml> --base <ref> [--force]` | §10's Amendment: deterministically selects a candidate for the `FaultSpec`'s `kind`/`target_glob`/`seed`, materializes an isolated fault workspace at `<ref>`, and writes the fault directly into it (never the source repository). Persists the resulting `FaultRef` under `--id`. Exit 2 on zero candidates, an invalid glob, or an invalid/colliding `--id` without `--force`. |
| `agentforge experiment fault show <id>` | Prints the persisted `FaultRef`: spec, selected target, description, diff stats, workspace path. Exit 2 if `<id>` doesn't exist. |
| `agentforge experiment fault restore <id>` | Reverses the fault in place (`git checkout <base_commit> -- <file>` inside the fault workspace); the workspace itself stays alive. |
| `agentforge experiment fault discard <id>` | Removes the fault's entire isolated workspace — the alternative to `restore` when it's no longer needed. |
| `agentforge experiment mutant apply --id <id> --spec <file.toml> --base <ref> [--force]` | §10's Amendment (standalone mutation testing pass): deterministically selects a candidate for the `MutantSpec`'s `operator`/`target_glob`/`seed` (reusing `mutation`'s own operator regexes), materializes an isolated mutant workspace at `<ref>`, and writes the mutation directly into it (never the source repository). Persists the resulting `MutantRef` under `--id`, unevaluated. Never runs the evaluator. Exit 2 on zero candidates, an invalid glob, or an invalid/colliding `--id` without `--force`. |
| `agentforge experiment mutant show <id>` | Prints the persisted `MutantRef`: spec, selected target, description, diff stats, workspace path, and the recorded `evaluation` (`none` if `mutant evaluate` hasn't run yet). Exit 2 if `<id>` doesn't exist. |
| `agentforge experiment mutant evaluate <id> --evaluator <eval-id>` | Runs `evaluate()` against the mutant's still-materialized workspace (no new worktree created or removed), writes a real `JsonlAuditSink` trail to `<state_root>/mutants/<id>/audit.jsonl`, and re-saves the `MutantRef` with `evaluation` set. Never gates or rejects — a surviving mutant is reported, not a command failure. |
| `agentforge experiment mutant restore <id>` | Reverses the mutation in place (`git checkout <base_commit> -- <file>` inside the mutant workspace); the workspace itself stays alive. |
| `agentforge experiment mutant discard <id>` | Removes the mutant's entire isolated workspace — the alternative to `restore` when it's no longer needed. |
| `agentforge run --task <id> --agent <adapter[:model]> [--policy <name>] [--keep-worktree-on-fail] [--json]` | Exactly one experiment: create worktree → Executor spawns the adapter's command → capture patch via `git diff` → `evaluate()` → score → write record → remove worktree (unless the experiment did not complete — `Failed`/`TimedOut` — and `--keep-worktree-on-fail` was passed). `--policy` names a policy previously registered via `policy add`; omitted, `run` falls back to a generous built-in default (`PermissionPolicy::generous_default`). Prints the same report `report show` would (human or `--json`). Exit code precedence, checked top-to-bottom: `status == Failed` → `1`; `status == TimedOut` → `124`; `status == Completed && score.gated` → `3`; `status == Completed && !score.gated` → `0`. |
| `agentforge race --task <id> --agents <adapter[:model]>[,...] [--repeat N] [--max-parallel N] [--json]` | Expands `--agents` (in listed order) × `--repeat` (default 1) into a flat, ordered participant list **before any execution starts**; each entry's position in that list is its `race_index`. Runs them as independent `run`s bounded by `max-parallel` (defaults to the full expanded participant count — i.e. unbounded fan-out — when omitted). A participant's internal failure doesn't abort the others. Prints the same leaderboard `report show` would. Exit `0` if at least one participant reached `Completed`; exit `1` if none did. |
| `agentforge bisect --task <id> --range <good>..<bad> [--json]` | Resolves both ends to SHAs, requires `good` to be an ancestor of `bad` (exit 2 otherwise), builds the ordered candidate list via `git rev-list --ancestry-path --reverse good..bad`, binary-searches it calling `evaluate()` per candidate in one dedicated worktree (§8, §14). Exit `0` with `result.json.culprit` set on finding a single flip; exit `3` if the whole range shares one verdict. |
| `agentforge verify --evaluator <id> --ref <commit> [--apply-patch <file>] [--json]` **or** `agentforge verify --experiment <id> [--json]` | Runs `evaluate()` standalone — either against an arbitrary commit (optionally with a patch applied) in a throwaway evaluation worktree, or by re-evaluating a stored experiment's already-captured patch. No `ScoreCard` is produced (scoring only applies to experiments). Exit `0`/`3` based on the resulting verdict. |
| `agentforge report score <experiment-id> [--weights <file>] [--verbose] [--json]` | Recomputes and prints a `ScoreCard` from persisted `RawMetrics` — spawns no process. If `--weights`' `formula_version` doesn't match the running binary's, prints a warning (never silent) and proceeds, since weights are portable numbers even when the formula's shape changes. |
| `agentforge report show <experiment-id\|race-id\|bisect-id> [--verbose] [--json]` | Human-readable report. For a race id, computes the leaderboard live from each participant's current `score.json`, sorted by `total` desc then `race_index` asc; non-`Completed` participants sort last. `--json` for the underlying record(s). |
| `agentforge report log <experiment-id> [--follow]` | Pretty-prints `audit.jsonl` in order. `--follow` tails a currently-running experiment (polls until its `RUNNING.lock` clears). |
| `agentforge policy add <file.toml> [--force]` | Validates a `PermissionPolicy` (`max_wall_time_secs`/`max_output_bytes` both `> 0`) and copies it into `policies/`, keyed by its own `name` field — mirrors `evaluator add`/`task add`. Exit 2 on an existing name without `--force`. |
| `agentforge policy list` | Listing. |
| `agentforge policy show <name>` | Prints resolved fields tagged exactly `Enforced`, `RequestedOnly`, or `Unsupported`, matching §17. |
| `agentforge policy validate <name>` | Rejects a policy with `max_wall_time_secs == 0` or `max_output_bytes == 0` (exit 2, field named). |
| `agentforge clean [--experiment <id> \| --all-worktrees \| --older-than <duration>] [--force]` | Always reconciles first: any experiment with `status == Running` and no `RUNNING.lock` present is marked `Failed` ("interrupted: no active lock found"). Then performs the requested removal (`--older-than` takes a simple `<n><unit>` duration, unit one of `s`/`m`/`h`/`d`). Refuses to remove a worktree whose experiment still has `RUNNING.lock` present unless `--force` (§20, M1). |

**Amendment (2026-08-13, "CLI integration and cleanup" pass):** the command surface above was
regrouped for discoverability and had its remaining gaps wired up; no product behavior changed
except where noted. `fault`, `mutation` (formerly the standalone `mutate` verb plus a separate
`mutation show`/`replay` pair), and `mutant` moved under a new `experiment` namespace — they are
repository-state test-fixture mechanisms, siblings to one another, and were previously three
inconsistently-shaped top-level verbs (`mutate` acted, `mutation`/`fault`/`mutant` were nouns with
subcommands) despite being conceptually parallel. `eval` was renamed `verify` (clearer: it runs
checks and reports pass/fail, not generic "evaluation" of code). `show`/`score`/`log` moved under
a new `report` namespace. `policy` gained `add`/`list` (previously only `show`/`validate` existed,
with no way to actually register a named policy for `run --policy <name>` to reference — a real
gap, not a deliberate cut). `init`, `run`, `race`, `bisect`, `verify`, `policy`, `clean` are now
fully wired (`cli::run()`'s dispatch previously fell through to a generic "not implemented" error
for all of these); `store::Store::load_policy`/`save_policy` are real (TOML files under
`policies/`, mirroring `evaluators/`/`tasks/`). `run`/`race`/`bisect`/`verify`/`log`/`clean` gained
a consistent `--repo` (previously only some commands had it); `run`/`race`/`bisect`/`verify`
gained a consistent `--json`. `ExperimentRunner` gained an additive
`run_keep_worktree_on_fail` method (alongside the unchanged `run`) so `run --keep-worktree-on-fail`
has something real to call — SPEC.md §7 already documented this flag's intended behavior, but no
code path honored it. See `docs/ARCHITECTURE.md` §10/§12/§14 for the corresponding Rust-level
detail.

---

## 7. Isolated Git Worktrees & External State Root

Five worktree lifecycles exist in MVP, all built on the same `create_worktree`/`remove_worktree`
helper (§20, D1):

1. **Experiment worktree** — one per `run`/race-participant. `git worktree add <path>
   <base_ref>` in `<state_root>/worktrees/<experiment-id>/`, used for both the agent's process and
   the subsequent evaluator run against its patch. Removed after the experiment finalizes unless
   preserved on failure.
2. **Bisect worktree** — exactly one per bisect session, at
   `<state_root>/bisect-worktrees/<bisect-id>/`, revisited via `git -C <path> checkout <commit>`
   for each candidate. Created once, removed once.
3. **Evaluation worktree** — a throwaway worktree used by `task add`'s baseline capture,
   `mutate`'s sanity gate, and `eval`. Created immediately before one `evaluate()` call, removed
   immediately after — no agent involved, so there's no keep-on-fail concept for this flavor.
4. **Fault worktree** (§10 Amendment) — one per `fault inject`, at
   `<state_root>/fault-worktrees/<fault-id>/`. Unlike the other three flavors, `FaultInjector`
   writes directly into this worktree's files via the filesystem (not just git commands run
   against it) — the materialized working-tree state a repository fault kind needs to express
   ("this file is missing") is the entire point of this flavor existing. Created by `inject`,
   reversible in place via `restore`, removed via `discard`.
5. **Mutant worktree** (§10 Amendment, standalone mutation testing pass) — one per `mutant
   apply`, at `<state_root>/mutant-worktrees/<mutant-id>/`. Identical shape to the fault worktree:
   `MutantTester` writes the mutation directly into it via the filesystem and leaves it
   materialized so `mutant evaluate` can run a real test command against it later. Created by
   `apply`, reversible in place via `restore`, removed via `discard`.

**Locking:** `git worktree add`/`remove` calls across all flavors are serialized behind a
single mutex/lock — `git`'s worktree metadata under `.git` is not safe for concurrent mutation —
but execution (the Executor running processes, `evaluate()` running) is fully parallel once a
worktree exists.

**Run-lock protocol** (resolves review F1 and M1 together): the instant an `ExperimentRecord` is
written with `status = Running`, a `RUNNING.lock` file is created next to it containing the
current process's PID (informational only). It is removed as the last step before the status is
rewritten to a terminal value. `clean`'s reconciliation pass (§6) and `--all-worktrees` removal
both key off this file's presence, not off the `status` field alone — the field can be stale if
AgentForge itself crashed before updating it, but the lock file's *absence* after a crash is what
lets `clean` tell "orphaned" apart from "genuinely still running" without inspecting OS process
state.

**Never touched:** the caller's primary checkout (working tree + index) — nothing in `run`,
`race`, `bisect`, `mutate`, `fault`, or `eval` runs a git command against it. Verified by test
(§19).

**On the residual risk this doesn't fully close:** moving the state root outside the target
repo (§5) closes the worst part of review finding U1 — an agent can no longer reach the actual
source repo or its history via `../`. It does **not** make sibling experiment directories
unreachable from each other; an agent whose cwd is one experiment's worktree can still, via `../`,
see other experiments' worktrees and the state root's own control-plane files, if the OS-level
filesystem confinement (best-effort only — §17) doesn't hold. This residual is now honestly
scoped to AgentForge's own state, not the user's source repo, and is listed explicitly in §17's
table rather than left as an unnamed gap.

---

## 8. Execution & Isolation — the Executor

**The Executor is the single component that spawns every subprocess AgentForge runs — the
agent's process and every evaluator `setup_cmds`/`test_cmd` step alike. No other code path spawns
a process.** This directly resolves review finding U2: v1's adapter trait let the adapter spawn
its own process, which meant AgentForge had no handle to enforce a timeout against a blocking
call, and the adapter self-reported its own audit events. Neither is true of the Executor.

**Hardened (2026-08-13, adversarial security review — `docs/ADVERSARIAL_REVIEW.md` finding 3).**
The process-group/Job-Object parenthetical below used to be aspirational, not implemented: a
timeout only ever killed the one directly-spawned process, so a detached grandchild survived
indefinitely on every platform. It's real now — Windows assigns the child to a Job Object
(`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, terminated on timeout) and Unix places it in its own
process group (`kill(-pgid, SIGKILL)` on timeout) — see `src/exec/mod.rs`'s `tree` module. Also
hardened in the same pass: `budget.max_output_bytes` is now enforced by a bounded reader thread
per stream *during* capture (piped stdio), not just by post-hoc truncation after the process
exits — a runaway command could previously fill the OS temp directory before the old truncation
step ever ran.

```
struct ProcessSpec {
    program: String,
    args: Vec<String>,
    extra_env: Vec<(String, String)>,   // adapter-supplied additions (e.g. a model flag via env)
    // cwd is NOT a field here — the Executor sets it, always, to the caller-supplied worktree path.
    // It is not adapter-suppliable and cannot be overridden by extra_env.
}

struct ExecutionBudget { timeout_secs: u64, max_output_bytes: u64 }

struct ProcessOutcome { exit_code: Option<i32>, timed_out: bool, stdout_path: PathBuf, stderr_path: PathBuf }

impl Executor {
    fn spawn(&self, spec: ProcessSpec, cwd: &Path, env_passthrough: &[String],
              budget: ExecutionBudget, audit: &AuditSink) -> ProcessOutcome;
}
```

`Executor::spawn` unconditionally:
- sets the child's working directory to `cwd` (always the relevant worktree — never adapter- or
  evaluator-config-suppliable);
- builds the child's environment from exactly the host variables named in `env_passthrough`, plus
  `spec.extra_env`, and nothing else from the host process's environment;
- kills the child (and its process group / job object, where the platform provides one) if it
  outlives `budget.timeout_secs`; the direct child is always killed — a grandchild the child
  spawns and detaches is only killed where the platform lets AgentForge reach it, which is not
  guaranteed on every platform without the OS-sandboxing capability that's explicitly out of
  scope (§3.1) — see §17 for the precise guarantee boundary;
- truncates captured stdout/stderr at `budget.max_output_bytes` with an explicit marker;
- emits exactly one `ProcessSpawn` and one `ProcessExit` audit event, itself, regardless of caller.

Because the Executor is the only thing that ever calls the underlying OS process API, adapters
and evaluators are both just *callers* that supply a `ProcessSpec` — an adapter cannot forget to
confine its cwd (there's no cwd field for it to set), and cannot suppress an audit event (it has
no access to the audit sink).

---

## 9. Agent Adapter Interface

**Implemented (2026-08-12).** The trait below is exactly what shipped; `adapter::resolve` is a
real name→adapter lookup (`"claude-code"` only; any other name is `Error::UnknownAdapter`).
`ClaudeCodeAdapter` takes a `ClaudeCodeConfig { executable, permission_mode, extra_default_args }`
at construction — `resolve`'s signature takes only a name (unchanged from the sketch below), so
`ClaudeCodeAdapter::default()` reads `AGENTFORGE_CLAUDE_EXECUTABLE`/
`AGENTFORGE_CLAUDE_PERMISSION_MODE` as its one available override point; nothing about the
executable path or permission mode is hard-coded beyond those defaults. See
`docs/ARCHITECTURE.md` §7 for the exact `command_for` behavior.

```
trait AgentAdapter {
    fn name(&self) -> &str;
    fn capabilities(&self) -> AdapterCapabilities;
    fn command_for(&self, prompt: &str, model: Option<&str>, extra_args: &[String]) -> ProcessSpec;
}

struct AdapterCapabilities {
    can_confine_filesystem: EnforcementLevel,  // Enforced | RequestedOnly | Unsupported
    can_restrict_network:   EnforcementLevel,
}
```

This is the concrete shrink from v1's trait: `run()` returned an outcome and implicitly owned
execution; `command_for()` only builds a command specification and returns immediately — it does
not block, does not spawn, does not see the worktree path (the Executor supplies `cwd`
independently of anything the adapter returns), and does not receive an audit sink. Everything
about *how* the resulting command executes belongs to the Executor (§8), not the adapter. A
second adapter implementation only needs to answer "what program/args/env represents this
prompt+model" — it cannot get isolation or auditing wrong, because it never touches either.

`AdapterCapabilities` drops v1's `can_restrict_commands` — with `CommandPolicy` cut (§3.1), there
is nothing left for it to describe.

**Claude Code adapter (`claude-code`), MVP's only production adapter:** `command_for` builds a
non-interactive (print/headless) invocation of the `claude` CLI, mapping `model` to `--model` and
requesting filesystem/network restriction through Claude Code's own permission-mode flags where
they exist. Declares `can_confine_filesystem = RequestedOnly`, `can_restrict_network =
RequestedOnly` — both are the underlying tool's own settings, relayed as a request, never verified
independently by AgentForge (§17).

**`FakeAdapter`, test-only:** returns a scripted `ProcessSpec` (e.g. a small script that writes a
fixed diff into its cwd and exits with a scripted code) so every integration test in §19 can run
with zero dependency on the real `claude` binary.

---

## 10. Fault Injection & Mutation

MVP mutation is **language-agnostic and text-pattern based**, not AST-aware (§3.1). Five
operators ship: `NegateCondition`, `OffByOne`, `BooleanFlip`, `DeleteStatement`, `SwapOperator`.

**Determinism contract:**
1. Candidate sites for `(operator, target_glob, operator_version)` are discovered by walking
   matched files and scanning each top-to-bottom with the operator's fixed regex, best-effort
   skipping string/comment literals.
2. **Cross-platform normalization (§20, R4):** file paths are compared using forward-slash-joined,
   byte-wise (ordinal), case-sensitive comparison of the path relative to the repo root —
   regardless of the host OS's native separator or case sensitivity. This applies uniformly
   whether AgentForge runs on the Windows machine mutation specs are authored on or the Linux CI
   they may be replayed on, so the same spec selects the same candidate on both.
3. Candidates are sorted by `(normalized_path, line, column)`.
4. Selection is `candidates[seed % candidates.len()]` — a pure function of the inputs.
5. `operator_version` is bumped whenever the regex or discovery order changes. Replaying a
   `MutationSpec` whose `operator_version` doesn't match the currently-installed binary's
   implementation is an error (exit 2), never a silent reinterpretation (§20, R5) — mutation
   reproducibility is only guaranteed within one AgentForge binary version, and that boundary is
   enforced, not just documented.
6. Zero candidates found is an error (exit 2), never a silent no-op.

**Mutation creation is pure git plumbing** — reading the blob(s) at `base_ref`, applying the
text transformation, writing new blob/tree/commit objects, and updating
`refs/agentforge/mutants/<task-id>`. No worktree is created for this step (§20, U4); a worktree
is only needed for the sanity-gate *evaluation* that follows, and that reuses the evaluation
worktree flavor from §8. The commit's author/committer date is fixed (not the wall clock) and its
message never includes `task_id` — both are required for `mutant_commit` to be a genuine pure
function of `(operator, target_glob, seed, operator_version, base_commit)`, not merely
deterministic-if-replayed-within-the-same-second; `mutation replay` (§6) exercises this directly.

**Sanity gate:** `mutate` always calls `evaluate()` against the mutant in a throwaway evaluation
worktree before it's usable as a task. A **good** verdict means the fault went undetected — the
mutation is rejected (§6's `mutate` row) since an undetectable fault makes a meaningless task (an
agent that changes nothing would also "pass"). A **not-good** verdict — including a build
failure, since `DeleteStatement` is expected to sometimes produce a non-compiling mutant — is a
successfully "killed" mutant and the task is created, reusing this exact `EvaluatorVerdict` as
the new task's `baseline` (no redundant second evaluator call).

**Known limitation, stated rather than hidden (§20, T4):** the regex-based comment/string-literal
skip is a heuristic, not a parser — it will have blind spots (e.g. a `//` inside a string
literal) that are inherent to the accepted non-goal of not building an AST engine for MVP. Tests
pin the heuristic's *current* behavior via fixtures; they are not a claim of universal
correctness.

**Amendment (2026-08-12) — repository fault injection is a second, distinct mechanism.**
docs/ARCHITECTURE.md §9 previously stated that fault injection and mutation testing "are one
feature... not two," resolving an earlier scoping question by pointing both at `MutationEngine`.
A later request for concrete repository-state faults — a missing file, a broken/modified config
value, a stale generated artifact (or timestamp-independent stale marker), and reversible
dependency/config corruption — reopened that question, since none of these fit `MutationEngine`'s
model: they aren't a regex-matched logic bug on one tracked source line, and a stale generated
artifact is typically untracked/gitignored, not a git blob `find_candidates` can even discover.
Flagged to the user via `AskUserQuestion` before implementation; resolved as follows, rather than
either forcing these into `MutationEngine` or silently reinterpreting the "not two" claim:

- **A new sibling module, `fault::FaultInjector`** (see `FaultSpec`/`FaultRef` above), shares
  `git`/`git::worktree` plumbing with `mutation` but has its own operator set (`FaultKind`) and its
  own record type (`FaultRef`, standalone rather than embedded). "Not two" now reads as "one
  mechanism for code-mutation faults (`mutate`), a second for repository-state faults (`fault
  inject`), sharing the same plumbing layer" — not "one mechanism, full stop."
- **Working-tree-based, not pure plumbing, unlike `MutationEngine::apply`.** "This file is
  missing" and "this generated artifact is stale" are working-tree states a git blob/tree/commit
  alone can't carry, so `FaultInjector::inject` always materializes an isolated `WorktreeKind::
  Fault` worktree at `base_commit` and writes the fault directly into it via the filesystem — the
  source repository is never opened for writing (the task brief's explicit requirement). Each of
  the four fault kinds selects a candidate deterministically the same way `MutationEngine` does
  (`candidates[seed % len]`, over byte-wise-sorted, forward-slash-normalized tracked paths) and is
  reversible via a single `git checkout <base_commit> -- <file>` inside that same workspace
  (`FaultInjector::restore`, `GitRepo::restore_path`) or disposable via removing the whole
  workspace (`FaultInjector::discard`) — never by touching the source repository.
- **Determinism contract**, mirroring §10's mutation contract above: candidate discovery and fault
  application are pure functions of `(kind, target_glob, seed, fault_version, base_commit)`. Which
  `id` a fault is injected under changes only `FaultRef.worktree_path`, never
  `selected_target`/`description`/`diff_stats` — the same "id is plumbing, not content" split
  `MutationRef.mutant_ref` vs. `mutant_commit` already establishes. `StaleArtifact`'s marker
  content is a fixed constant, never derived from the wall clock — the concrete form of the task
  brief's "timestamp-independent" requirement.
- **No `ExperimentType` abstraction introduced.** `experiment::ExperimentRunner::run` doesn't
  exist yet, so `fault::FaultInjector` ships standalone with its own CLI surface (`agentforge
  fault inject/show/restore/discard`, §6) and its own `Store` collection — mirroring how `mutation`
  shipped before `experiment` existed. Wiring a `FaultRef` into `ExperimentRecord`/
  `ExperimentRunner` as an actual "first experiment type" is deferred to whichever future pass
  builds `ExperimentRunner::run` for real, rather than speculatively designed now.

**Amendment (2026-08-12, standalone mutation testing pass) — a third mutation-adjacent mechanism,
`mutant::MutantTester`.** A later request asked for "reproducible source mutation testing using
the same experiment infrastructure," recording operator/file/location/seed/diff/evaluator-outcome,
and reusing fault injection's storage/selection/audit/cleanup code. On inspection this collided
with two things `mutation::MutationEngine`/`MutationRef` already resolve deliberately: §20 (C3)
("no standalone, task-less mutation record" — `MutationRef` stays embedded-only in `TaskSpec`) and
`sanity_check`'s immediate, gating, `NullAuditSink` evaluation model. Flagged to the user via
`AskUserQuestion` (batched with the audit-trail question below) before implementing; both
recommended options confirmed:

- **A new sibling module, `mutant::MutantTester`**, built the same way `fault::FaultInjector` was
  (not a replacement for `mutation::MutationEngine`, which is untouched): `apply` materializes an
  isolated `WorktreeKind::Mutant` worktree at `base_commit` and writes the mutation directly into
  it via the filesystem (never pure git plumbing, unlike `MutationEngine::apply`), and the
  resulting `MutantRef` (`MutantSpec`/`MutantTarget`/`MutantEvaluation` — new structs) is
  standalone-persisted via `Store::save_mutant`/`load_mutant`/`list_mutants`, mirroring
  `save_fault`/`load_fault`/`list_faults` exactly rather than reopening (C3) for `MutationRef`
  itself. `mutant`'s own id-safety and path-safety checks are the exact same code
  `fault::FaultInjector` uses (`validate_id`/`safe_join`/`is_safe_relative_path`, bumped to
  `pub(crate)` and called directly, not re-implemented) — the literal "reuse fault injection's
  selection/cleanup code" the brief asked for. Candidate discovery and the mutation transform
  itself reuse `mutation::MutationEngine`'s own operator-scanning code directly
  (`mutation::scan_line`/`mutate_file_contents`/`is_comment_line`, `mutation::Candidate`, and
  `MutationOperator`, all bumped to `pub(crate)`) rather than a second copy of the five operator
  regexes — one set of "mutations whose intent can be clearly reported," shared by both
  mechanisms, per the task brief's explicit MVP framing (a small, safe, deterministic set, not a
  full AST-aware framework).
- **Evaluation is a separate, later, non-gating step — the one real behavioral difference from
  `sanity_check`.** `apply` never runs the evaluator and never rejects; the mutant workspace stays
  alive afterward specifically so `mutant evaluate <id> --evaluator <id>` can run independently,
  whenever a caller asks, recording the outcome (`MutantEvaluation { verdict, evaluated_at }`) onto
  the persisted record via a re-save rather than discarding it. A surviving (undetected) mutant is
  reported, not treated as a command failure — there is no reject-and-rehome-under-`rejected/...`
  behavior here, unlike `mutate`.
- **A real audit trail, not `NullAuditSink`** — the second confirmed `AskUserQuestion` answer.
  `mutant evaluate` opens a dedicated `JsonlAuditSink` at
  `<state_root>/mutants/<id>/audit.jsonl` and passes it through to `Evaluator::evaluate`, unlike
  `sanity_check`'s throwaway evaluation worktree (SPEC.md §11, deliberately `NullAuditSink` since
  there's no `ExperimentRecord` to attach a trail to). A `MutantRef` has no such record either, but
  the brief specifically asked for a recorded "evaluator outcome," so it gets its own log file
  rather than reusing the null sink by default.
- **`WorktreeKind::Mutant`/`WorktreeManager::create_mutant_worktree`** — a fifth worktree flavor,
  identical shape to `create_fault_worktree` (§7).

---

## 11. Evaluator & Shared Deterministic Evaluation

One function, `evaluate(commit_or_worktree, EvaluatorSpec) -> EvaluatorVerdict`, is called,
unmodified, by: `run`, `race`'s participants (each is a `run`), `bisect`'s binary search,
`mutate`'s sanity gate, `task add`'s baseline capture, and `eval`. This is the single
implementation review finding D1 asked for, made explicit rather than merely implied by four
separate call sites each assumed to agree.

`evaluate()`:
1. Runs `setup_cmds` in order inside the target worktree via the Executor. **The first failing
   setup command short-circuits the rest** — remaining `setup_cmds` and `test_cmd` do not run,
   `build_succeeded = false`, and `exit_code` is that failing command's exit code (§20, F4).
2. If all `setup_cmds` succeed, runs `test_cmd` via the Executor, subject to
   `EvaluatorSpec.timeout_secs`.
3. Applies `metric_extractors` (regex + capture group) against `test_cmd`'s combined
   stdout/stderr to populate `tests_total`/`tests_passed` where present. An evaluator with no
   extractors still yields `build_succeeded` and `exit_code` — extractors add granularity, they
   aren't required.
4. Returns an `EvaluatorVerdict`.

**Determinism contract:** for a fixed commit/working-tree state, repeated `evaluate()` calls
produce identical `build_succeeded`, `tests_total`, `tests_passed`, `exit_code`.
`wall_time_secs` is explicitly exempt (machine-load-dependent) and is a scoring input only,
never a pass/fail signal, and — per §20 (R2) — never used for ordering either.

**Toolchain stability is an assumed boundary, not something AgentForge controls or verifies**
(§20, R3): `evaluate()`'s determinism contract covers AgentForge's own behavior given a fixed
working tree; it says nothing about the compiler/toolchain/package state `setup_cmds`/`test_cmd`
run against, which is entirely the target repo's concern. A task authored against one toolchain
version and replayed after that toolchain drifts is not guaranteed to reproduce its original
verdict — this is stated here explicitly rather than left as an implicit, unstated assumption.

**Metric extraction is inherently gameable beyond what §15's gate closes** (§20, S2): a regex
against free-form test-runner output can desync from reality if a patch changes output
formatting incidentally. Not fully fixable without a structured test-result protocol (out of
scope for MVP); evaluator-authoring guidance (to be written alongside implementation) should
recommend test runners with a stable, parseable summary format.

For `bisect`, "good" is exactly `EvaluatorVerdict::is_good()`. There is no separate "skip" tier
in MVP (§3.1) — a commit that fails to build counts as bad, same as `run`'s convention, which is
a deliberate simplification enabled by dropping real `git bisect` machinery (§14).

---

## 12. Multi-Agent Races

- `--agents a:m1,a:m2 --repeat 2` expands, in exactly this order, to `race_index` 0–3:
  `(a:m1, repeat 0)`, `(a:m1, repeat 1)`, `(a:m2, repeat 0)`, `(a:m2, repeat 1)` — outer loop over
  the listed agents, inner loop over repeat count. This assignment happens once, before any
  worktree is created or any process spawned (§20, R2/T1) — it does not depend on execution
  order, wall-clock timing, or which participant happens to finish first.
- Execution is parallel up to `max_parallel_experiments` (config default, overridable); worktree
  creation/removal is still serialized (§8).
- All participants share the task's one `EvaluatorSpec` — a race cannot mix evaluators.
- **Leaderboard order:** `score.total` descending; ties broken by `race_index` ascending — a
  value fixed at construction time, immune to timing, unlike v1's `wall_time_secs`-based
  tie-break (§20, R2/T1). Non-`Completed` participants sort after all `Completed` ones.
- **Partial failure (§20, F3):** if one participant's `ExperimentRecord` ends `Failed` (an
  AgentForge-internal error, not a bad agent patch), the remaining participants are unaffected and
  still run to completion; the leaderboard simply includes the failed participant at the bottom
  with no score. The race's own process exit code is `0` if at least one participant reached
  `Completed`, `1` if none did (§6).
- The leaderboard is never persisted as its own artifact — `show`/`--json` on a race id compute
  it live from each participant's `score.json` every time (§20, D3), so it can never disagree
  with an experiment that was individually rescored afterward.

---

## 13. Semantic Bisect

v1 drove real `git bisect start`/`git bisect run`, which required an externally-invoked oracle
process using git-bisect's own `0`/`1`/`125` exit-code convention — directly conflicting with the
CLI's global exit-code table (review C1), and leaving dangling `.git/BISECT_*` state on a crash
(review F2), and requiring integration tests to shell out to a compiled binary (review T2). v2
replaces all of this with an in-process binary search over `evaluate()` calls:

1. Resolve `good` and `bad` to SHAs; require `good` to be an ancestor of `bad` via
   `git merge-base --is-ancestor` (exit 2 otherwise — linear-range-only remains the stated
   limitation, §3.1).
2. Build the ordered candidate list: `git rev-list --ancestry-path --reverse <good>..<bad>`
   (excludes `good`, includes `bad`, chronological order).
3. Create **one** dedicated worktree for the whole session (§8, flavor 2).
4. Binary-search the candidate list: for the midpoint commit, `git -C <worktree> checkout
   <commit>`, call `evaluate()`, record a `BisectStep`, and narrow the search range based on
   `is_good()` — exactly the same halving logic as a textbook binary search, entirely in-process,
   with no external process, no recursive self-invocation, and no dependency on git's own bisect
   state machine.
5. Result: the first candidate whose verdict is bad, written to `result.json.culprit`. Exit `0`
   on finding it; exit `3` if the whole range shares one verdict — now consistent with every
   other command's exit-code table, because there is no longer a second, git-bisect-flavored
   convention in play anywhere in the codebase (§20, C1).
6. `steps.jsonl` gets one entry per commit actually tested, in the order tested, appended as it
   happens.

Because this never touches git's own bisect state, there is nothing to clean up on interruption
beyond the one dedicated worktree, which follows the same lock-file protocol as any other
worktree (§8).

---

## 14. Scoring Model

Three components — v1's `hygiene` is cut (§20, S3): with the gate below now covering timeout and
non-good exit codes directly, hygiene had nothing left to discriminate on that wasn't already
implied elsewhere, and contributed close to a constant 5 points regardless of patch quality.

| Component | Default weight | Normalized value (0.0–1.0) |
|---|---|---|
| `correctness` | 80 | `0` if gated (see below); else `tests_passed / tests_total` if the evaluator reports counts, else `1.0` (verdict is good by construction when not gated and counts are absent) |
| `efficiency` | 10 | `clamp(1 - wall_time_secs / budget_secs, 0, 1)` |
| `parsimony` | 10 | `clamp(1 - diff_lines_changed / size_budget_lines, 0, 1)` |

`contribution_i = weight_i * normalized_i`; `total = round(sum(contribution_i))`, clamped to
`[0, 100]`.

**Correctness gate — now closes the loophole review finding S1 identified.** `gated` is true, and
correctness is forced to `0` **and** `total` is hard-capped at `5` regardless of
efficiency/parsimony, if **any** of:
- `!verdict.build_succeeded`
- `verdict.timed_out`
- `agent_timed_out`
- **`verdict.exit_code != 0`** — v1 only consulted this as a fallback when no test counts were
  present; v2 checks it unconditionally, so a build that succeeds but whose test command exits
  nonzero (a crash after printing a partial summary, a failing post-test step) can no longer earn
  a high correctness score just because an extracted ratio looked good.
- **A test-count regression** — `task.baseline.tests_total` and `verdict.tests_total` are both
  `Some`, and `verdict.tests_total < task.baseline.tests_total`. This is the direct fix for the
  most realistic gaming path in this product category: an agent that "fixes" a failing test by
  deleting it. Deleting the failing test no longer produces a better `tests_passed/tests_total`
  ratio in a way that escapes the gate — it's caught before the ratio is even computed, exactly
  like a build failure. (If either side lacks a test count, this specific check is skipped — it
  can't compare what it doesn't have — but every other gate condition above still applies.)

A fast, tiny, gamed patch cannot outscore a slow, large, genuinely correct one: the cap is a
structural guarantee, not a weighting bias, and it is now wide enough to actually catch the
known failure mode it exists to catch.

**Rating bands** (`scoring.toml`, defaults): `90–100 Excellent`, `70–89 Good`, `45–69 Fair`,
`20–44 Poor`, `0–19 Fail`.

**Validation (§20, S4):** `evaluator add` rejects an `EvaluatorSpec` with `budget_secs <= 0` or
`size_budget_lines <= 0` — both are scoring divisors, and a zero value is rejected before it can
reach the formula, not handled by clamping undefined behavior after the fact.

**Transparency & reproducibility:** every `ScoreCard` embeds `weights_source` and
`formula_version`; `score.json` stores the full component breakdown. `agentforge report score
--weights alt.toml` recomputes a full alternate `ScoreCard` from persisted `RawMetrics` with zero
re-execution — no worktree touched, no process spawned (§18 makes this an explicit,
spy-verifiable acceptance criterion).

---

## 15. Structured Audit Log

- One `audit.jsonl` per experiment, append-only, flushed after every line.
- Every subprocess the Executor spawns — the agent process, every `setup_cmds`/`test_cmd` step —
  produces a matching `ProcessSpawn`/`ProcessExit` pair, emitted by the Executor itself (§8). No
  adapter code path has access to the audit sink, so no adapter can omit or falsify these events
  (§20, U2) — this is a structural property of the trait (§9), not a runtime policy.
- `PermissionCheck` events cover only what the Executor itself verifies (env filtering, and — as
  of the permission-policy layer pass — the command-program allow/denylist and cwd-root
  confinement checks, each recorded on both the allow and the deny outcome) — never
  adapter-relayed best-effort requests (network denial, readonly paths, memory limits), so the
  log never implies a verification that didn't happen.
- Content is metadata (commands/args/exit codes/byte counts), not full transcripts — those live in
  `agent-stdout.log`/`agent-stderr.log`, separately capped.

---

## 16. Security Boundaries & Limitations

Same purpose as v1's table: say precisely what's real. Several rows move from best-effort to
**Enforced** in v2 because the Executor, not the adapter, now owns execution (§20, U2); one new
row reflects the external state root (§20, U1); the process-tree caveat is now explicit rather
than folded silently into a blanket "Enforced" claim.

| Boundary | MVP guarantee level | Detail |
|---|---|---|
| Working directory of every spawned process | **Enforced** | Set by the Executor, always, to the relevant worktree — not adapter-suppliable (§8, §9). Upgraded from best-effort in v1. |
| Environment variables exposed to a spawned process | **Enforced** | Built by the Executor from exactly `policy.env_passthrough` plus adapter-supplied literal additions — never the full host environment. |
| Wall-clock timeout on the directly-spawned agent process | **Enforced** | The Executor owns the process handle from spawn to exit/kill. |
| Wall-clock timeout on a grandchild process the agent detaches | **Not provided** | The Executor can only reliably reach the process it directly spawned; process-tree containment needs OS-level facilities (Job Objects/cgroups) that are explicitly out of scope (§3.1). |
| Wall-clock timeout on evaluator steps | **Enforced** | Same mechanism as the agent process. |
| Captured output size cap | **Enforced** | Truncated at `max_output_bytes`, marker recorded. |
| State root (worktrees, experiments, audit logs) is outside the target repo's directory tree | **Enforced** | Verified structurally — `state_root` never begins with the repo's canonical path (§5). Closes the worst part of v1's isolation gap. |
| Main repo working tree/index untouched by any command | **Enforced** | No command runs a mutating git operation against the caller's primary checkout; verified by test (§19). |
| No destructive git operations against the user's branches/remotes | **Enforced** | Nothing runs `push`, `push --force`, branch deletion, or history rewrite outside AgentForge's own ephemeral refs/worktrees. |
| Sibling experiment/state-root directories reachable from within a worktree | **Not fully isolated — best-effort only** | Moving the state root outside the repo stops an agent reaching the *source* repo; it does not stop an agent from reaching another experiment's worktree or AgentForge's own control-plane files via relative paths, absent real OS sandboxing (§7). |
| Agent filesystem writes beyond its assigned cwd (traversal, absolute paths, symlinks) | **Best-effort / adapter-dependent** | The Executor sets the *starting* cwd; it cannot stop a process from writing elsewhere if the OS permits it. Requested via the adapter's own permission settings where supported (`AdapterCapabilities.can_confine_filesystem`). |
| Command program the Executor itself is asked to spawn (agent process, evaluator steps, git) | **Enforced** | `PermissionPolicy.allowed_programs`/`denied_programs`, checked by the Executor before every spawn, fail closed (`Error::PolicyDenied`, zero side effects) — §4, §16 amendment (permission-policy layer pass). Distinct from mediating what an agent does *inside* its own process, which remains **Not provided** (below). |
| Mediated tool execution inside an agent's own process (e.g. shell commands Claude Code invokes internally) | **Not provided in MVP** | v1's `CommandPolicy` tried this and is cut (§3.1) — AgentForge has no visibility into an adapter's internal tool calls, only the one top-level process it spawns. Roadmap item for a mediating adapter. |
| Spawn cwd confined to configured allowed roots | **Enforced** | `PermissionPolicy.allowed_roots`, checked by the Executor before every spawn. Defense-in-depth on top of cwd always being the caller-assigned worktree (§8) — guards against AgentForge's own misconfiguration, not agent containment. |
| Memory/CPU resource limits on a spawned process | **Not provided in MVP — representation only** | `PermissionPolicy.max_memory_bytes` is carried and reported but never enforced; OS-level facilities (Job Objects/cgroups) needed for a real cap are explicitly out of scope (§3.1). |
| Network access restriction | **Best-effort, requested only** | No firewall/network-namespace enforcement. Requested via `AdapterCapabilities.can_restrict_network` where the adapter supports it; not independently verified. |
| Protection against a malicious (not just buggy) agent | **Not provided** | MVP assumes the agent under test is imperfect, not adversarial. Don't run untrusted agent binaries against sensitive repos/hosts based on AgentForge's isolation alone. |

Anything not marked **Enforced** must not be described as a security guarantee anywhere else in
this project's docs, CLI help text, or output. `agentforge policy show` exists to surface this
table's per-adapter reality at the point someone is about to rely on it, and §19 includes a test
that checks the tagging in that output stays in sync with this table (§20, M2).

---

## 17. Acceptance Criteria

Every checklist item below is written to be checked by a specific command, fixture, or assertion
— not by judgment call. **Verification status as of 2026-08-14: see `docs/VERIFICATION.md` for
the authoritative, evidence-cited per-criterion record** (verified / partially verified / not
implemented, each with the actual test or code reference). The checkboxes below are kept in sync
with that record's top-line verdict; `docs/VERIFICATION.md` is the source of truth for nuance.

**Isolated Git worktrees & external state root**
- [x] `agentforge init`'s printed output and `config.toml`'s `state_root` field are an absolute
      path that is not a prefix-match of the repo's canonical root path. *(Partially verified —
      "not a prefix-match" is tested directly; "absolute" holds by construction but is untested.
      See `docs/VERIFICATION.md`.)*
- [x] `race --max-parallel 2` with 2+ participants produces distinct `worktree_path` values, each
      listed by `git worktree list` until individually cleaned.
- [x] `git status --porcelain` in the primary checkout is byte-identical before and after every
      one of: `init`, `task add`, `run`, `race`, `bisect`, `experiment mutation create`, `verify`,
      `clean`.
- [x] A `Completed` experiment's worktree directory does not exist after `run` returns unless
      `--keep-worktree-on-fail` was passed and the status was not `Completed`.

**Execution & isolation (the Executor)**
- [x] A fixture command that echoes its own cwd, spawned via the Executor with worktree path `W`,
      reports exactly `W`, in 100% of runs — cwd is never adapter-suppliable by construction.
- [x] A fixture process configured with `timeout_secs = 2` that sleeps 30s is killed such that
      `ended_at - started_at <= 5` seconds (2s budget + 3s margin).
- [x] A fixture process configured with `max_output_bytes = B` that writes `10*B` bytes produces
      a captured file of at most `B + len(TRUNCATION_MARKER)` bytes, with the marker present.
- [x] A fixture command that dumps its full environment, run with `env_passthrough = ["FOO"]` and
      host env containing `FOO=1, BAR=2`, reports an environment containing `FOO=1` and not
      containing `BAR`.
- [x] Every Executor-spawned process produces exactly one `ProcessSpawn` and one `ProcessExit`
      audit event; the adapter trait (§9) exposes no method capable of emitting either.

**Configurable permission policies where feasible**
- [x] `policy validate` rejects (exit 2, field named) a policy with `max_wall_time_secs == 0` or
      `max_output_bytes == 0` or a missing required field.
- [x] `policy show <name>` output tags every field exactly `Enforced`, `RequestedOnly`, or
      `Unsupported`, matching §16's table (golden-output test). *(Corrected 2026-08-14: this row
      previously read "`Enforced` or `Requested (best-effort)`", which never matched
      `policy_show`'s own already-deliberate, already-documented tag vocabulary — see
      `docs/VERIFICATION.md`.)*
- [x] Changing only `env_passthrough` between two otherwise-identical `run` invocations changes
      the observed spawned-process environment with no code change.
- [x] A program on `policy.denied_programs`, or absent from a non-empty `policy.allowed_programs`,
      is refused by the Executor before it is spawned (`Error::PolicyDenied`), with zero
      `ProcessSpawn`/`ProcessExit` events for the refused attempt — verified in
      `tests/exec_boundaries.rs` and, end-to-end through `workspace exec`, in
      `tests/workspace.rs`/`tests/cli_workspace.rs`.
- [x] A cwd outside every root in a non-empty `policy.allowed_roots` is refused the same way.
- [x] `PermissionPolicy::enforcement_report()` tags every field `Enforced`, `RequestedOnly`, or
      `Unsupported` consistent with §16's table — the live, testable backing `policy show` will
      render — verified in `tests/config_validation.rs`.

**Structured audit logs**
- [x] `audit.jsonl` parses as one JSON object per line, with no partial trailing line, for both a
      `Completed` and a `TimedOut` fixture experiment.
- [x] `ProcessSpawn` count equals `ProcessExit` count for any experiment whose status is not
      `Running`.

**Reproducible fault injection and source mutation experiments**
- [x] Two `mutate` runs with identical `(operator, target_glob, seed, operator_version, base_ref)`
      produce identical `git show <mutant_commit>:<file>` output and identical resulting tree SHAs.
- [x] Candidate-site selection is identical between a fixture run under a simulated
      case-insensitive filesystem and one under a simulated case-sensitive one, for the same
      inputs. *(Candidate discovery never reads filesystem directory entries — file names come
      exclusively from `git ls-tree` against a tree object — so this can't literally be exercised
      under two simulated filesystem modes; verified instead by proving the sort is byte-wise, not
      case-folded, the concrete property that guarantee depends on. See `docs/VERIFICATION.md`.)*
- [x] A mutation whose sanity-gate verdict is good exits 2 and writes no `TaskSpec`.
- [x] A mutation matching zero candidates exits 2, names the operator and glob, creates no ref.

**Multiple agent/configuration races**
- [x] `race --agents a:m1,a:m2 --repeat 2` produces `race_index` values `0,1,2,3` assigned to
      `(a:m1,0),(a:m1,1),(a:m2,0),(a:m2,1)` respectively, present in each `ExperimentRecord`
      before that experiment's worktree is created.
- [x] On a `FakeAdapter` fixture scripted so two participants produce identical `score.total`, the
      leaderboard orders them by ascending `race_index` in 20/20 repeated invocations.
- [x] A participant scripted to fail with an AgentForge-internal error does not stop the other
      participants from completing; the race process exits `0` if at least one participant
      completed. *(The exit-0-if-any-completed branch is verified through the real binary; the
      complementary exit-1-if-none-completed branch is not — `race` has no `--policy` flag to
      force that outcome through the CLI. See `docs/VERIFICATION.md`.)*

**Shared deterministic evaluation of patches**
- [x] `run`, `bisect`'s binary search, `mutate`'s sanity gate, `task add`'s baseline capture, and
      `eval` all resolve to one function in the implementation (a single `evaluate()` with no
      parallel reimplementation) — verified by an integration test where a metric-extractor
      regex fix, applied once, is observed identically by all five call sites against the same
      fixture commit. *(Verified by direct inspection of every call site rather than the specific
      described integration test, which doesn't exist as a standalone test — a structural
      guarantee at least as strong, since a second reimplementation would require a visible new
      function to exist. See `docs/VERIFICATION.md`.)*
- [x] Two `evaluate()` calls against an unchanged commit produce field-identical
      `build_succeeded`/`tests_total`/`tests_passed`/`exit_code` (`wall_time_secs` excluded from
      the comparison).

**Semantic bisect using the same evaluator infrastructure**
- [x] Against an 8-commit linear fixture with a single scripted verdict flip at a known commit,
      `bisect` reports exactly that commit as `culprit` and exits `0`.
- [x] `steps.jsonl`'s entry count matches the exact expected binary-search trace for that fixture
      (not a loose bound).
- [x] `git status --porcelain` in the primary checkout is unchanged before/after.
- [x] A range where both ends share one verdict exits `3` with no `culprit` written.

**Human-readable results with raw metrics plus transparent configurable scores**
- [x] `show <experiment-id>` output includes every `RawMetrics` field and every
      `ScoreComponent`'s name/raw/normalized/weight/contribution, plus `total` and `rating`.
- [x] `score <id> --weights alt.toml` run twice produces byte-identical `ScoreCard` JSON both
      times and spawns zero processes (asserted via a spy `Executor` that fails the test if
      invoked). *(Byte-identical output across repeated runs is verified end to end through the
      real binary; "spawns zero processes" is verified by code inspection — `report_score` never
      constructs an `Executor` — rather than a runtime spy, since that isn't reachable through the
      compiled binary's black-box CLI surface. See `docs/VERIFICATION.md`.)*
- [ ] Every `--json`-supporting command's output round-trips through its documented struct's
      deserializer with no unknown/missing-field errors. *(Not verified — most `--json` e2e tests
      parse output as a generic `serde_json::Value` and assert on specific fields, which is weaker
      than deserializing into the exact documented struct. A broad, mechanical pass across most
      CLI test files; not attempted this pass. See `docs/VERIFICATION.md`.)*

**Claude Code as first agent adapter, core agent-independent**
- [x] `AgentAdapter`'s only production-relevant method (`command_for`) returns a value and takes
      no ownership of execution — checked by trait-signature inspection (no method returns an
      outcome type or blocks on process completion).
- [ ] The same patch-capture call (`git diff <base_ref>` in the worktree) executes for both a
      `FakeAdapter`-driven experiment and (opt-in lane only) a `claude-code`-driven one.
      *(`FakeAdapter` half solidly verified; the opt-in real-Claude-Code lane
      (`AGENTFORGE_TEST_REAL_CLAUDE=1`) does not exist in this codebase — it needs a real Claude
      Code installation and paid API access this environment doesn't have, consistent with the
      project's deliberate zero-paid-API stance elsewhere. Moved to the roadmap — see §21/
      `docs/VERIFICATION.md`.)*

---

## 18. Test Plan

CI runs fully offline by default — no test may require the real `claude` binary, an API key, or
network access. `FakeAdapter` (§9) drives every `run`/`race`/`bisect` integration test. A
separate, explicitly opt-in test (env-gated, e.g. `AGENTFORGE_TEST_REAL_CLAUDE=1`) exercises the
real Claude Code adapter and is excluded from default CI.

Timeout-margin tests use a fixed, generous margin (budget `2s`, allowed overrun up to `3s`,
i.e. asserting completion within `5s`) rather than a tight one, specifically to avoid flakiness
under loaded CI runners (§20, T3) — the assertion is "killed within a bound," not "killed
instantly."

| # | Test | Validates |
|---|---|---|
| 1 | Two parallel `run`s never collide on worktree path; primary checkout `git status --porcelain` unchanged before/after | §7 |
| 2 | `state_root` never prefix-matches the repo's canonical root | §5, §16 (U1 resolution) |
| 3 | Fixture cwd-echo command run via the Executor reports exactly the assigned worktree path | §8 (U2 resolution) |
| 4 | Fixture env-dump command run with a restricted `env_passthrough` exposes exactly the allowlisted keys | §8 |
| 5 | Timeout enforcement: fixture sleeping 30s with `timeout_secs=2` is killed within 5s total | §8 |
| 6 | Output truncation at exactly `max_output_bytes`, marker present | §8 |
| 7 | Every Executor-spawned process yields exactly one `ProcessSpawn`/`ProcessExit` pair; adapter trait exposes no audit-capable method | §9, §15 (U2 resolution) |
| 8 | `audit.jsonl` valid JSONL end-to-end for a `Completed` and a `TimedOut` fixture, no partial trailing line | §15 |
| 9 | Mutation determinism: identical inputs twice → identical mutant tree SHA | §10 |
| 10 | Mutation candidate selection identical under simulated case-sensitive vs. case-insensitive path comparison | §10 (R4 resolution) |
| 11 | Mutation zero-candidates → exit 2, no ref created | §10 |
| 12 | Mutation sanity gate rejects a good-verdict (undetected) mutant, exit 2, no task written | §10 |
| 13 | `evaluate()` determinism: unchanged commit → identical pass/fail fields across repeated calls | §11 |
| 14 | `setup_cmds` partial failure: first failing command short-circuits remaining setup and `test_cmd` | §11 (F4 resolution) |
| 15 | `evaluator add` rejects `budget_secs=0` / `size_budget_lines=0` / `timeout_secs=0`, exit 2, field named | §14 (S4 resolution) |
| 16 | Scoring gate fires (correctness forced 0, total capped ≤5) for: build failure, timeout, nonzero exit code with high extracted pass ratio, **and a test-count regression against the task's baseline** | §14 (S1 resolution) — this is the direct regression test for the finding that mattered most |
| 17 | `score --weights alt.toml` reproduces a hand-computed alternate `ScoreCard` from a stored experiment; a spy `Executor` asserts zero processes spawned | §14 |
| 18 | Race with `FakeAdapter` × 2 agents × repeat 2 → `race_index` 0–3 in the documented order, assigned before execution starts | §12 (R2/T1 resolution) |
| 19 | Race leaderboard tie-break by `race_index` is stable across 20 repeated runs of a tied-score fixture | §12 (R2/T1 resolution) — a repetition-based flake check, not a timing-based one |
| 20 | Race continues after one participant's internal failure; exit code `0` iff at least one participant completed | §12 (F3 resolution) |
| 21 | Bisect against an 8-commit fixture with one scripted flip → exact culprit commit, exact expected step count, primary checkout untouched | §13 |
| 22 | Bisect range with no flip → exit 3, no culprit | §13 |
| 23 | `clean` reconciliation: an experiment with `status=Running` and no `RUNNING.lock` present is marked `Failed` on the next `clean` invocation | §7 (F1/M1 resolution) |
| 24 | `clean --all-worktrees` skips (does not remove) a worktree whose `RUNNING.lock` is present, unless `--force` | §7 (M1 resolution) |
| 25 | `run` exit-code precedence: `Failed`→1, `TimedOut`→124, `Completed`+bad verdict→3, `Completed`+good verdict→0, tested against one fixture per branch | §6 (C2 resolution) |
| 26 | `policy show` output tagging (`Enforced` / `RequestedOnly` / `Unsupported`) matches §16's table exactly (golden-output test, re-run whenever the table changes) | §16 (M2 resolution) |
| 27 | `task add` captures a `baseline` `EvaluatorVerdict` against the resolved `base_ref` without requiring it to be good | §11 |
| 28 | `base_ref` is persisted as a resolved 40-hex SHA even when the input was a branch name | §4 (R1 resolution) |
| 29 | End-to-end smoke: `init` → `evaluator add` → `task add` → `experiment mutation create` (sanity gate passes) → `run` with `FakeAdapter` scripted to fix the mutant → `report score` shows a high total → `bisect` across a range containing the mutant commit returns it | Cross-capability coherence (§2) |

Completion for MVP requires all 29 tests passing in CI, plus every acceptance-criteria checkbox
in §17 independently verified. **Verified 2026-08-14 — see `docs/VERIFICATION.md`** for the
full, evidence-cited record: all 29 rows above are now backed by real, executing tests (up from a
subset when this table was first written); every §17 checkbox is `[x]` except two, both moved to
the roadmap (§21) rather than silently left uncredited: the opt-in real-Claude-Code adapter test
lane, and full `--json`-struct-deserializer round-tripping for every command.

---

## 19. Unnecessary-Abstraction Cuts Applied

A short, explicit list of things v1 had that v2 removed outright, since "smallest coherent
architecture" is easiest to verify as a diff against what used to exist:

- `CommandPolicy` (glob-pattern command allowlist) and the 3-variant `NetworkMode` enum — replaced
  with a single `deny_network: bool` (§4, §20 A3/X1). **Amended by the permission-policy layer
  pass:** a plain (non-glob) program allow/denylist and cwd-root confinement were reintroduced at
  the `Executor` boundary — see §4, §16 — since that scope (what AgentForge itself spawns) is
  genuinely enforceable, unlike v1's attempt to mediate an agent's internal tool calls.
- `ExperimentStatus::PolicyViolation` and `AuditEvent::PolicyViolation` — MVP has no mechanism
  that can honestly detect one; cut rather than left as an unbacked claim (§20, X3).
- The `hygiene` scoring component — redundant with the strengthened gate, contributed near-constant
  points (§14, §20 S3).
- `run --repeat N` — fully subsumed by `race --agents <one-agent> --repeat N`, which now also
  gives a leaderboard; having two entry points for "repeat one config N times" was unmotivated
  (§20, C5).
- The separate `eval/log.jsonl` file — evaluator steps are audit events, logged once (§5, §20 D2).
- `RaceRecord.leaderboard` as persisted, denormalized data — computed live from each experiment's
  own `score.json` instead (§4, §20 D3).
- Real `git bisect` subprocess orchestration — replaced by an in-process binary search over the
  same `evaluate()` primitive everything else uses (§13, §20 C1/A2/T2/F2).
- `env_passthrough` as a field on `AgentConfig` — consolidated into `PermissionPolicy`, the one
  place "what this run may access" now lives (§4, §20 X2).

---

## 20. Resolution of `SPEC_REVIEW.md` Findings

Every finding from the review, and exactly where v2 resolves it. Nothing below is "documented as
a known issue" in place of a fix — each row points at a structural change.

| Finding | Resolution |
|---|---|
| **U1** — worktrees nested inside the target repo | State root moved outside the repo entirely, to a platform data directory keyed by repo identity (§5, §7). |
| **U2** — adapter owns process spawning; timeout/audit claims unbacked | New `Executor` component owns all subprocess execution; adapter trait shrunk to `command_for()`, which cannot spawn, block, or touch the audit sink (§8, §9). |
| **U3** — no named sandbox component | The Executor is that component, named and scoped explicitly (§8). |
| **U4** — `mutate`'s worktree/Git ownership unclear | Mutation creation is pure git plumbing (no worktree); the sanity-gate evaluation uses the named "evaluation worktree" flavor (§7, §10). |
| **D1** — no shared evaluation primitive despite 4+ callers | `evaluate()` named as the one function every caller uses; three worktree flavors named explicitly (§7, §11). |
| **D2** — duplicate evaluator logs | `eval/log.jsonl` removed; evaluator steps are audit events only (§5). |
| **D3** — race leaderboard duplicates per-experiment scores | `RaceRecord` stores only participant order; leaderboard computed live (§4, §12). |
| **D4** — diff-stat computation duplicated | Named as one `DiffStats` computation reused by patch capture, scoring, and the audit event (§4). |
| **A1** — "ship together" contradicted phased `CONTEXT.md` priorities | Explicit Phase 1 / Phase 2 split; phasing is delivery order, not scope cut (§3). |
| **A2** — real `git bisect` more machinery than needed | Replaced with in-process binary search (§13). |
| **A3 / X1** — permission-policy schema richer than any adapter can enforce | `CommandPolicy`/3-variant `NetworkMode` cut to one bool (§4, §19). Amended: a plain program allow/denylist and cwd-root confinement reintroduced at the `Executor` boundary, where enforcement is real (§4, §16, §19). |
| **M1** — `clean --all-worktrees` no guard against live experiments | `RUNNING.lock` protocol; removal skipped unless `--force` (§7, §6). |
| **M2** — security table could drift from CLI output with nothing catching it | Golden-output test added (§18, test 26). |
| **S1** — correctness gate doesn't check exit code or test-count regression | Gate now checks `exit_code != 0` unconditionally and a `tests_total` regression against a stored task baseline (§14). |
| **S2** — regex metric extraction gameable | Acknowledged explicitly as a residual limitation with authoring guidance to follow; the gate fix (S1) covers the exploitable case (§11). |
| **S3** — `hygiene` component near-constant | Cut; weight redistributed to correctness (§14, §19). |
| **S4** — no validation of evaluator numeric fields | `evaluator add` rejects non-positive `budget_secs`/`size_budget_lines`/`timeout_secs` (§6, §14). |
| **C1** — bisect exit codes contradict global CLI table | No longer relevant — bisect has no external oracle process or its exit-code convention (§13). |
| **C2** — `run`/`race` exit-code semantics on bad/timeout/failure unspecified | Explicit precedence table for `run`; explicit rule for `race` (§6). |
| **C3** — `MutationRef` has no persistence location without `--task-id` | `--task-id` made required (§4, §6). |
| **C4** — dead defensive evaluator-unset check in `race` | Removed from the spec's description (§6). |
| **C5** — `run --repeat` and `race` overlap | `run --repeat` cut (§19). |
| **C6** — `task add` collision behavior unspecified | `--force` flag defined; default is exit 2 on collision (§6). |
| **R1** — `base_ref` a mutable ref, not pinned | Resolved to a SHA and persisted as one at registration time (§4). |
| **R2 / T1** — race tie-break uses non-deterministic `wall_time_secs` | Replaced with `race_index`, fixed at construction before execution (§12). |
| **R3** — toolchain stability assumed, never stated | Stated explicitly as a boundary (§11). |
| **R4** — mutation determinism not normalized cross-platform | Explicit forward-slash/byte-wise/case-sensitive normalization rule (§10). |
| **R5** — no behavior defined for version mismatches on replay | `operator_version` mismatch is a hard error; `formula_version` mismatch is a warning, not silent (§10, §6). |
| **F1** — no reconciliation for interrupted experiments | `RUNNING.lock`-based reconciliation on `clean` (§7, §6). |
| **F2** — dangling `git bisect` state on interruption | Not applicable — no real git-bisect state exists in v2 (§13). |
| **F3** — race partial-failure behavior unspecified | Explicit: isolated per-participant failure, defined exit-code rule (§12). |
| **F4** — `setup_cmds` partial-failure behavior unspecified | Explicit: first failure short-circuits (§11). |
| **F5** — no worktree retention policy | Acknowledged as an accepted MVP limitation; `clean --older-than` remains the manual backstop (§3.1). |
| **T2** — real bisect needs binary-level integration tests | Not applicable — in-process, unit-testable (§13, §18). |
| **T3** — timeout tests inherently flaky | Fixed, generous margin specified (2s budget / 5s bound) (§18). |
| **T4** — mutation heuristic is a moving target for tests | Acknowledged explicitly; tests pin current behavior, not claimed universal correctness (§10). |
| **X2** — `env_passthrough` split from `PermissionPolicy` | Moved into `PermissionPolicy` (§4). |
| **X3** — `PolicyViolation` status/event naming and detection unclear | Both cut; MVP has no honest detection mechanism for one (§19). |

---

## 21. Open Questions Resolved From `CONTEXT.md`

Unchanged from v1 — still resolved as follows; see v1's §16 for the original mapping:

1. **Scope of "running" agents** → trait-based `AgentAdapter`, now narrowed to command
   construction only (§9).
2. **Sandboxing strategy** → application-level only, honestly scoped (§16); explicit non-goal
   (§3.1).
3. **Comparison/evaluation model** → shared `evaluate()` (§11).
4. **Output format** → terminal reports by default, `--json` everywhere, persisted JSON/TOML
   always (§6).
5. **CI matrix** → deferred (§3.1).
6. **Distribution** → local binary only for MVP (§3.1).
