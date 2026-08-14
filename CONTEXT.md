# AgentForge — Project Context

Compact, factual context for future agent sessions. Update this file as decisions get made;
don't re-derive it by re-reading the whole workspace each time.

## Purpose

A portfolio-quality Rust CLI for safely running, testing, comparing, and evaluating autonomous
coding agents (e.g. Claude Code, other agentic coding tools). Scope not yet narrowed beyond that
— see "Unresolved architectural decisions" below.

## Current repository state (as of 2026-08-13)

- **The whole product is implemented and wired end to end: the shared foundation, isolated
  task-worktree CLI management (`workspace`), the evaluator, the task-embedded reproducible
  mutation framework (`mutation`), standalone repository fault injection (`fault`), standalone
  source mutation testing (`mutant`), scoring, evaluation reporting/persistence, the agent adapter
  interface (`adapter`, incl. the real `ClaudeCodeAdapter`), real experiment/race orchestration
  (`experiment::ExperimentRunner::run`, `race::RaceRunner::run_race`), real semantic bisect
  (`bisect::BisectRunner::run_bisect`), `store::Store`'s `policy` persistence (the last gap), and
  every `cli::run()` command — nothing is left `todo!()` or falls through to a generic
  "not implemented" error.** The 2026-08-13 "CLI integration and cleanup" pass regrouped the
  command surface for discoverability (`Mutate`/`Mutation`/`Fault`/`Mutant` → one
  `Experiment{Fault,Mutation,Mutant}` namespace with `Mutate` folded into `Mutation::Create`;
  `Eval` → `Verify`; `Score`/`Show`/`Log` → one `Report{Show,Score,Log}` namespace; `Policy`
  gained `Add`/`List`) and wired `Init`/`Run`/`Race`/`Bisect`/`Verify`/`Report::Log`/`Policy`/
  `Clean`, all previously either unimplemented or dispatch-stubbed. See
  `docs/TEST_STATUS.md`'s "CLI integration and cleanup" pass entry for the full detail, and
  `docs/SPEC.md` §6's new Amendment for the command-rename rationale. `projects/AgentForge/` has:
  `docs/SPEC.md` (v2 — the MVP contract, with three dated Amendments under §10 plus one under
  §6), `docs/SPEC_REVIEW.md` (historical record only), `docs/ARCHITECTURE.md` (the Rust module
  design, §9/§9a/§9b/§10/§14), `docs/TEST_STATUS.md` (a recorded snapshot of `cargo test` results
  — **276 passed / 0 failed, 276 total** as of the latest pass, up from 230/230), and a real
  `Cargo.toml` + `src/` + `tests/` + `examples/` tree. A fully local, zero-paid-API end-to-end
  demo now exists too (2026-08-13, "fully local end-to-end demo" pass) — `cargo run --example
  demo` (narrated) or `cargo test --test demo_e2e`, both driving the real compiled `agentforge`
  binary through its documented CLI, with `src/bin/mock_claude.rs` standing in for the real
  `claude` executable via `ClaudeCodeAdapter`'s own pre-existing `AGENTFORGE_CLAUDE_EXECUTABLE`
  override — `adapter::resolve` itself is untouched. See `docs/TEST_STATUS.md`'s matching entry.
  Real, non-stub implementations exist for:
  `domain::evaluator::EvaluatorSpec::validate_fields` /
  `domain::policy::PermissionPolicy::validate_fields`, `domain::ids::new_id`,
  `exec::SystemExecutor::spawn`, `git::GitRepo` (all methods, including `write_commit`'s
  plumbing — fixed, non-wall-clock commit date for reproducibility — plus
  `head_commit`/`prune_worktrees`/`list_tree_files`/`diff_stats_between`/`update_ref`/
  `delete_ref`/`restore_path`), `git::worktree::WorktreeManager` (all methods incl. the
  `RUNNING.lock` protocol, idempotent `remove`, a `state_root()` accessor, and five worktree
  flavors: Experiment/Bisect/Evaluation/Fault/Mutant), `audit::JsonlAuditSink`,
  **`evaluator::Evaluator::evaluate`** (SPEC §11, incl. `regex`-based metric extraction),
  **`mutation::MutationEngine`** (`find_candidates`/`apply`/`sanity_check`/`discard`, SPEC §10 —
  deterministic seeded candidate selection, pure-git-plumbing application, embedded-only
  `MutationRef` per §20 C3), **`fault::FaultInjector`** (`find_candidates`/`inject`/`restore`/
  `discard`, SPEC §10 Amendment — standalone `FaultRef`, working-tree-based repository-state
  faults), **`mutant::MutantTester`** (`find_candidates`/`apply`/`evaluate`/`restore`/`discard`,
  SPEC §10 Amendment — standalone `MutantRef`, sibling to `fault` but reusing `mutation`'s own
  operator-scanning code; `evaluate` is a separate, later, non-gating step with a real per-mutant
  `JsonlAuditSink`, unlike `mutation`'s `NullAuditSink` sanity gate), **`scoring::score`/
  `rating_for`/`default_weights`/`failed_checks`** (SPEC §14 — 80/10/10 composite, 90/70/45/20
  rating bands, `FORMULA_VERSION`), **`store::Store`**'s task/evaluator/fault/mutant/experiment/
  race/bisect/scoring-weights persistence (TOML under `.agentforge/{tasks,evaluators,faults,
  mutants}/`; experiment/race/bisect are always-overwrite, no `--force` guard), **`report::
  Reporter`** (`render_experiment`/`render_race`/`render_bisect` + `_json` variants, wired to
  `agentforge report score`/`agentforge report show`), **`workspace::WorkspaceManager`** (create/list/show/exec/
  remove/clean for isolated task worktrees, with path-traversal-proof id validation and idempotent
  cleanup), **`adapter::AgentAdapter`/`adapter::resolve`/`adapter::claude_code::ClaudeCodeAdapter`/
  `adapter::fake::FakeAdapter`** (SPEC §9 — `command_for` returns a `ProcessSpec` value only, never
  spawns; `ClaudeCodeAdapter` takes a `ClaudeCodeConfig { executable, permission_mode,
  extra_default_args }`, defaulting from `AGENTFORGE_CLAUDE_EXECUTABLE`/
  `AGENTFORGE_CLAUDE_PERMISSION_MODE`; `resolve("claude-code")` is real, any other name is
  `Error::UnknownAdapter`), **`experiment::ExperimentRunner::run`/`race::RaceRunner::run_race`**
  (SPEC §8/§12 — worktree → spawn → `git diff` capture → evaluate → score → finalize status →
  cleanup, `Failed`-not-`Err` for internal errors per §12 F3; `race` fans participants out via
  bounded `std::thread::scope`, zero race-specific scoring), **`bisect::BisectRunner::run_bisect`**
  (SPEC §13 — resolve/require-linear/candidate-list/one-dedicated-worktree/binary-search over
  `evaluate()`, `culprit: None` not `Err` for an inconclusive range, unconditional worktree
  cleanup), and **`cli::run()`'s complete dispatch** — every `Command` variant is real now:
  `Init`, `Workspace`, `Evaluator`, `Task`, `Experiment { Fault, Mutation, Mutant }` (the
  regrouped home for what used to be `Mutate`/`Mutation`/`Fault`/`Mutant` as four separate
  top-level commands), `Run`, `Race`, `Bisect`, `Verify` (renamed from `Eval`),
  `Report { Show, Score, Log }` (regrouped from three top-level commands), `Policy` (now with
  `Add`/`List`, not just `Show`/`Validate`), `Clean`. `cargo fmt`, `cargo clippy --all-targets
  --all-features -- -D warnings`, and `cargo test` all run clean (clippy: zero warnings; test:
  275/275, zero failures). Still no git repo, no README — the binary itself is now feature-complete
  end to end (every documented command works, not just a subset).
- **Read order for a new session:** `docs/SPEC.md` (product contract) → `docs/ARCHITECTURE.md`
  (module boundaries, dependency graph, exact trait/struct signatures) → `docs/TEST_STATUS.md`
  (which tests are red and exactly which function each is waiting on — this doubles as an
  implementation checklist) → `src/` and `tests/` (already typed/written) → this file
  (environment/state only). Don't re-derive the architecture from SPEC.md again — it's already
  done. Update `docs/SPEC.md` for product/behavior decisions, `docs/ARCHITECTURE.md` for
  Rust-level design decisions, this file for environment/state facts only — don't duplicate
  across the three. Re-run `cargo test` rather than trusting `TEST_STATUS.md`'s numbers once
  implementation has started — that file is a point-in-time snapshot, not a live status.
- This workspace (`C:\Users\AJ GUERRA\Desktop\VSCode`) is a portfolio of independent projects
  under `projects/`. Each project is **self-contained with its own git repository** — no shared
  source, no workspace-level manifest, no cross-project imports. AgentForge should follow this
  pattern: its own `git init`, own README, own CI, own dependency management, all scoped to
  `projects/AgentForge/`.
- The workspace root itself has a `.git/` directory but it is empty/uninitialized (`git status`
  fails with "not a git repository"). This is a known, long-standing, intentional-ish state
  (documented in `documents/HOUSEKEEPING_REPORT_2026-08-10.md`) — not something to fix as part
  of this task, and irrelevant to AgentForge's own repo.
- Reference sibling project: `projects/code-risk-intelligence-engine/` ("crie") is the most
  recently built, most mature project in this workspace and a good structural template —
  Python/Typer CLI with its own git repo (GitHub remote: `GuerraXe/code-risk-intelligence-engine`),
  `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, `LICENSE`, `pyproject.toml`, `docs/`, `examples/`,
  `tests/`, and `.github/workflows/ci.yml` (GitHub Actions, matrix build, lint/typecheck/test/build
  jobs). AgentForge's polish bar (README quality, CI, changelog discipline) should match this, with
  Rust-native equivalents (`Cargo.toml`, `cargo clippy`, `cargo test`, etc. in place of the Python
  tooling).

**Adversarial security/correctness review (2026-08-13).** A hostile-reviewer pass across the
whole `src/` tree — full write-up in `docs/ADVERSARIAL_REVIEW.md`. Found and fixed 5
independently-exploitable issues, each with its own regression test: a critical path-traversal
gap in `store::Store` (task/evaluator/policy/experiment/race/bisect ids were never validated
before being joined onto a filesystem path, unlike `workspace`/`fault`/`mutant`, which already had
this protection), a symlink-following write in `fault inject`/`mutant apply` that let a hostile
target repository escape its isolated worktree, a real gap between SPEC.md §8's documented
timeout-kill guarantee and the implementation (no process group/Job Object was ever created, so a
detached grandchild survived a timeout indefinitely — now fixed for real on both platforms),
unbounded disk growth during subprocess output capture (now bounded during the write, not just
truncated after), and a single race participant's panic that could have discarded every other
participant's already-collected result (now isolated via `catch_unwind`). Two lower-severity
issues were documented but deliberately not silently fixed (no cross-process lock around worktree
mutation; `scoring::correctness_ratio`'s full-credit default when an evaluator finds no test
counts — the latter reopens a SPEC.md §15 product decision, flagged for confirmation rather than
changed). **284/284** tests (up from 276/276), `cargo fmt`/`clippy -D warnings` clean. Two new
`cfg`-gated dependencies, each scoped to its own platform only: `libc` (Unix) and `windows-sys`
(Windows).

## Confirmed tooling / environment facts

- **Rust IS installed** (as of this session): rustup-managed `stable-x86_64-pc-windows-msvc`,
  rustc/cargo 1.97.1, via `rustup-init.exe -y --default-toolchain stable`. `cargo`/`rustc` are on
  `%USERPROFILE%\.cargo\bin`, which is on PATH for new shells after a restart — if a shell in this
  session doesn't see them, run `$env:Path += ";$env:USERPROFILE\.cargo\bin"` (PowerShell) first
  rather than re-installing.
- **MSVC C++ Build Tools ARE installed** (`Microsoft.VisualStudio.2022.BuildTools`, VCTools
  workload, via winget) — required because the MSVC Rust target needs `link.exe`, which nothing
  on this machine provided beforehand (confirmed no Visual Studio, no mingw gcc). Without this,
  `cargo check`/`build` fail at the proc-macro-crate linking step even though `rustc`/`cargo`
  themselves run. Both installs are system-wide and durable across sessions — don't reinstall,
  just re-verify with `cargo --version` if a fresh shell doesn't see them yet.
- OS: Windows 11 Pro. Primary shells: Git Bash (POSIX-ish, via the Bash tool) and Windows
  PowerShell 5.1 (via the PowerShell tool).
- `git` 2.55.0 is installed and working (used per-project, not at workspace root).
- `gh` (GitHub CLI) 2.97.0 is installed and authenticated as `GuerraXe` with `repo`/`workflow`
  scopes — creating a GitHub remote for AgentForge later is straightforward.
- Node.js v24.18.0 / npm 11.16.0 installed. Python not on PATH directly (`python`/`python3` fail —
  Microsoft Store alias stub), but the `py` launcher works (Python 3.13.3), and a root-level
  `.venv/` exists (undocumented purpose, left untouched per housekeeping report).
- No Docker (`docker` not found).

## Important constraints

- Keep AgentForge fully self-contained under `projects/AgentForge/`, matching sibling-project
  conventions — own repo, own README, no dependency on anything at the workspace root.
- "Safely running" agents is a stated goal — implies the CLI will likely shell out to or sandbox
  other agent CLIs/processes. Any implementation must consider process isolation, resource limits,
  and not-destructive-by-default behavior for whatever it launches (consistent with this session's
  general operating principle of caution around risky/hard-to-reverse actions).
- Workspace convention: dated, source-only backups exist for other projects under root
  `backups/<project>/snapshot-<date>/` — not yet set up for AgentForge, not urgent pre-code.

## Unresolved architectural decisions

**Resolved for MVP** — see `docs/SPEC.md` §16 for the full resolution of what used to be six
open questions here (agent-invocation model → trait-based `AgentAdapter`; sandboxing strategy →
application-level only, honestly scoped in SPEC.md §14; evaluation model → shared
`EvaluatorSpec`; output format → terminal + `--json`; CI matrix → deferred; distribution → local
binary only). Nothing below is still open at the product-design level; anything that comes up
during implementation that isn't covered by the spec should get added to the spec, not answered
ad hoc.

## MVP priorities

1. ~~Install Rust toolchain~~ — done.
2. ~~Write a first test suite against the skeleton~~ — done (`tests/*.rs`, 57 tests across the
   10 highest-risk areas; see `docs/TEST_STATUS.md`).
3. ~~Implement the shared foundation~~ — done: config validation, error types, `exec::SystemExecutor`,
   `git::GitRepo` + `git::worktree::WorktreeManager`, `audit::JsonlAuditSink`, `domain::ids::new_id`.
4. ~~Implement isolated task worktrees + repository execution CLI~~ — done: `src/workspace/mod.rs`
   (`WorkspaceManager`) plus `cli::run()`'s `Command::Workspace` dispatch
   (`create`/`list`/`show`/`exec`/`remove`/`clean`), all safety requirements (traversal-proof ids,
   validated refs, idempotent cleanup, app-level-vs-OS-level distinction) covered by tests.
   65/88 tests pass; `cargo fmt`/`clippy -D warnings`/`test` all clean. See `docs/TEST_STATUS.md`.
5. ~~Add `regex` to `Cargo.toml` and implement `evaluator::Evaluator::evaluate`~~ — done (2026-08-12).
6. ~~Implement the shared reproducible mutation/fault-injection framework~~ — done (2026-08-12):
   `mutation::MutationEngine` (`find_candidates`/`apply`/`sanity_check`/`discard`), `MutationRef`
   extended with `selected_target`/`mutant_ref`/`diff_stats`/`applied_at`, `store::Store`'s
   task/evaluator persistence, and `cli::run()`'s `Evaluator`/`Task`/`Mutate`/`Mutation{Show,Replay}`
   dispatch. **This was pulled forward ahead of item 6 (below) as a deliberate, user-directed
   exception** — confirmed via `AskUserQuestion` before implementation (see
   `docs/TEST_STATUS.md`'s "Scoping note" under the 2026-08-12 pass) — not a silent reordering of
   the plan. `race`/`bisect`/`experiment::ExperimentRunner::run` were explicitly left untouched.
6a. ~~Implement standalone repository fault injection~~ — done (2026-08-12): `fault::FaultInjector`,
    standalone `FaultRef`, `store::Store`'s fault persistence, `cli::run()`'s
    `Command::Fault{Inject,Show,Restore,Discard}`. Also a confirmed `AskUserQuestion` exception
    to the previous pass's own "not two" framing — see `docs/TEST_STATUS.md`.
6b. ~~Implement standalone source mutation testing~~ — done (2026-08-12): `mutant::MutantTester`
    (sibling to `fault`, reusing `mutation`'s operator-scanning code directly), standalone
    `MutantRef` with a deferred, non-gating `evaluate` step and a real per-mutant
    `JsonlAuditSink`, `store::Store`'s mutant persistence, `cli::run()`'s
    `Command::Mutant{Apply,Show,Evaluate,Restore,Discard}`. A third confirmed `AskUserQuestion`
    exception (fourth instance overall, see `feedback_agentforge_process` in memory) — see
    `docs/TEST_STATUS.md`'s "standalone mutation testing" pass entry.
7. ~~Implement `scoring::score`/`rating_for` and `store::Store`'s `scoring_weights`/`experiment`/
   `race`/`bisect` persistence plus `report::Reporter`~~ — done (2026-08-12, "evaluation
   reporting" pass): `scoring::score`/`rating_for`/`default_weights`/`failed_checks`,
   `Store::save_experiment`/`load_experiment`/`save_race`/`load_race`/`save_bisect`/`load_bisect`/
   `load_scoring_weights`, `report::Reporter`, `cli::run()`'s `Command::Score`/`Command::Show`.
   `store::Store`'s `policy` persistence remains the one still-`todo!()` gap in `store`.
7a. ~~Implement the agent adapter interface + real `ClaudeCodeAdapter`~~ — done (2026-08-12,
    "agent adapter interface" pass): `adapter::claude_code::ClaudeCodeAdapter::command_for`,
    `adapter::resolve`, `tests/adapter_contract.rs` (14 tests, written first). A confirmed
    `AskUserQuestion` exception (fifth instance, see `feedback_agentforge_process` in memory) —
    the brief's "receive a task in a workspace, return a structured run result" phrasing
    described `experiment::ExperimentRunner::run`'s job (SPEC.md §20 U2), not the adapter's; user
    picked the narrower, non-reopening scope. `ExperimentRunner::run`/`race`/`bisect` were
    explicitly left untouched.
8. ~~Implement `experiment::ExperimentRunner::run` end-to-end on `FakeAdapter`, then
   `race::RaceRunner::run_race`~~ — done (2026-08-12, "race orchestration" pass):
   `ExperimentRunner::run` (worktree → spawn → `git diff` capture → evaluate → score → finalize
   status → cleanup, `Failed`-status-not-`Err` for internal errors per SPEC.md §12 F3) and
   `RaceRunner::run_race` (participant expansion, bounded `std::thread::scope` fan-out, zero
   race-specific scoring — ranking stays entirely in the pre-existing `report::Reporter`).
   `ExperimentRunner` still uses `WorktreeManager` directly, not `workspace::WorkspaceManager` —
   revisited and left as-is; `WorkspaceManager` is the CLI-facing, task/evaluator-independent
   primitive (SPEC.md §7's `workspace` commands), while `ExperimentRunner` needs its own
   `Running`/lock/audit/patch bookkeeping tied to an `ExperimentRecord`, which doesn't fit
   `WorkspaceManager`'s simpler `WorkspaceInfo` shape without reshaping one of the two. Tests:
   `tests/experiment_run.rs` (new), `tests/race.rs` (new), `tests/experiment_reproducibility.rs`'s
   3 pre-existing tests now pass for real (previously passed only because their assertions were
   trivially satisfiable, see `docs/TEST_STATUS.md`'s "race orchestration" pass entry).
9. ~~Implement `bisect::BisectRunner::run_bisect`~~ — done (2026-08-12, "semantic bisect" pass):
   resolve/require-linear/candidate-list/one-dedicated-worktree/binary-search over `evaluate()`
   (SPEC.md §13), `culprit: None` (not `Err`) for an inconclusive range, unconditional worktree
   cleanup on every path. Gained a `store: Arc<Store>` field beyond ARCHITECTURE.md's original
   sketch (needed for `load_evaluator` and the live `steps.jsonl`-append behavior `Store`'s own
   doc comment already assigned to this runner) — not an `AskUserQuestion` case, since the sketch
   was still an un-"Implemented" `todo!()` stub, not a resolved decision. Tests:
   `tests/bisect.rs` (new, 5 tests, real temporary git repos with a scripted behavioral flip).
10. ~~Implement `store::Store`'s remaining `policy` persistence, then the rest of `cli::run()`'s
    dispatch~~ — done (2026-08-13, "CLI integration and cleanup" pass):
    `load_policy`/`save_policy`/`list_policies` (mirrors `evaluator`'s TOML-file persistence
    exactly), plus real dispatch for `Init`/`Run`/`Race`/`Bisect`/`Verify` (renamed from
    `Eval`)/`Report::Log`/`Policy`/`Clean` — nothing in `cli::run()` falls through to a generic
    "not implemented" error anymore. Bundled with a command-tree regroup for discoverability
    (`Mutate`/`Mutation`/`Fault`/`Mutant` → `Experiment{Fault,Mutation,Mutant}`;
    `Score`/`Show`/`Log` → `Report{Show,Score,Log}`; `Policy` gained `Add`/`List`) — explicitly
    the brief's own ask ("redesign names/subcommands... do not preserve earlier conceptual
    command names"), not a reinterpretation needing `AskUserQuestion`. Two small additive changes
    to primitives below `cli`: `ExperimentRunner::run_keep_worktree_on_fail` (new method,
    `run`'s own signature/behavior untouched) so `run --keep-worktree-on-fail`'s
    always-documented-but-never-honored flag has something real to call;
    `PermissionPolicy::generous_default` (factored out of `race::default_policy()`'s own private
    literal) so `run`'s `--policy`-omitted fallback shares one definition with `race`'s. Found and
    fixed one latent, previously-uncaught bug: `EvalArgs`'/`VerifyArgs`' clap `ArgGroup`
    referenced the field id `"r#ref"` (the raw-identifier source spelling) instead of the id clap
    actually registers, `"ref"` — panicked on every invocation via a debug assertion, latent since
    `eval` was first written because nothing had ever exercised its `Cli::parse` until this pass's
    new `tests/cli_verify.rs`. New CLI integration tests: `tests/cli_init.rs`, `tests/cli_bisect.rs`,
    `tests/cli_verify.rs`, `tests/cli_policy.rs`, `tests/cli_clean.rs`, `tests/cli_run.rs`,
    `tests/cli_race.rs`, `tests/cli_log.rs` (45 tests total), plus updates to the four pre-existing
    CLI test files for the new command shape. `cargo test`: **275/275** (up from 230/230). See
    `docs/TEST_STATUS.md`'s matching pass entry for full detail.
11. Own git repo + minimal README, mirroring crie's structural bar — the CLI is now feature-complete
    end to end; this is the next real gap.
12. ~~Build a fully local end-to-end demo, no paid API~~ — done (2026-08-13, "fully local
    end-to-end demo" pass): `src/bin/mock_claude.rs` (a deterministic stand-in agent, substituted
    via `ClaudeCodeAdapter`'s own pre-existing `AGENTFORGE_CLAUDE_EXECUTABLE` override — no change
    to `adapter::resolve`'s "only `claude-code` is resolvable by name" boundary), plus
    `tests/support/demo_scenario.rs` (the one shared implementation), runnable narrated via
    `cargo run --example demo` and wired in as `cargo test --test demo_e2e`. Walks the entire
    documented CLI end to end against one fixture repo. See `docs/TEST_STATUS.md`'s matching pass
    entry for full detail.
