# AgentForge — Adversarial Review of `docs/SPEC.md`

Status: review only. No implementation exists to change; this critiques the spec itself before
any code is written. References are to `SPEC.md` section numbers as of the version reviewed
(2026-08-11, the version containing §1–§16).

## Executive summary

The spec is internally consistent in its prose but has four issues serious enough that building
against it as-is would produce a tool that doesn't deliver what it promises, specifically around
isolation, timeout/audit integrity, and the one property the spec claims to care about most —
correctness-weighted scoring being resistant to gaming. Fix these four before writing code; the
rest can be resolved during implementation without re-architecting.

1. **Worktrees are nested inside the very repo they're supposed to isolate from** (§13, finding
   U1) — the stated "isolation" is bypassable by a single `../` and every experiment can see
   every other experiment's data.
2. **The `AgentAdapter` trait gives the adapter (not the harness) ownership of process spawning**
   (§5, finding U2), which contradicts the "Enforced, adapter-independent" timeout claim in §13
   and means the audit log's `ProcessSpawn`/`ProcessExit` events for the agent process are a
   self-report from the thing being evaluated, not an independent observation.
3. **The correctness scoring gate doesn't check `evaluator_exit_code`, and nothing detects a
   shrinking test count** (§11, finding S1) — an agent can score near-perfect by deleting or
   weakening the failing test, which is the single most common failure mode in this exact
   product category and the spec has no defense against it.
4. **The bisect oracle's exit-code convention (git-bisect's 0/1/125) is never reconciled with
   the CLI's own global exit-code table** (§4 vs §10, finding C1) — either two divergent
   implementations exist despite the "single code path" acceptance criterion, or the spec
   contradicts itself about what exit code 1 means.

Findings below are grouped under the ten lenses requested, each tagged Critical / High / Medium
/ Low. Some findings are load-bearing for more than one lens; where that happens they're written
up once, in their primary section, and cross-referenced rather than repeated.

---

## 1. Architecture that duplicates infrastructure

**D1 (High) — No single "run the evaluator against a commit in a real working tree" primitive,
despite four call sites needing exactly that.** §8 states the evaluator is "used identically" by
`run`, `race`, `bisect`, and `mutate`'s sanity gate, and §4 adds a fifth caller,
`agentforge eval --ref <commit>`. But §6 ("Isolated Git Worktrees") only defines two worktree
lifecycles: one worktree per experiment, and one dedicated worktree for a whole bisect session.
It says nothing about how `mutate`'s sanity check gets a working tree to build/test the mutant in,
or how `eval --ref` gets one for an arbitrary commit not tied to any experiment. As written, this
leaves at least two more worktree-creation code paths unspecified, which is exactly how
"identical" evaluator usage silently forks into three or four slightly-different implementations
during implementation. *Recommendation:* name the primitive explicitly — something like
`with_evaluation_worktree(base_ref, evaluator) -> RawMetrics` — and have `run`, `bisect`'s
oracle, `mutate`'s gate, and `eval` all call it, with §6 documenting all of its call sites, not
just two of them.

**D2 (Medium) — Two logs for evaluator activity with no stated relationship.** §3's layout has
both `experiments/<id>/audit.jsonl` (which per §12 contains `EvaluatorStep` events) and
`experiments/<id>/eval/log.jsonl`. The spec never says what's in one that isn't in the other. If
they're the same events written twice, that's redundant infrastructure that can drift out of
sync; if they're genuinely different (e.g. one is AgentForge-level steps, the other is raw
evaluator stdout/stderr framing), the spec should say so explicitly instead of leaving two
same-named-sounding JSONL streams for an implementer to reconcile.

**D3 (Medium) — `RaceRecord.leaderboard` denormalizes `ScoreCard`s that already live at
`experiments/<id>/score.json`.** §2 stores `Vec<(experiment_id, ScoreCard)>` on the race record
itself. Since §4's `score` command can recompute an individual experiment's `ScoreCard` with
different weights after the fact, nothing says whether the race's embedded leaderboard is a
frozen snapshot (in which case it will silently disagree with a later `agentforge score`
recomputation) or a live view (in which case it's not actually stored data, contradicting §2's
struct). Pick one and say so.

**D4 (Low) — Diff-stat computation (`diff_files_changed`/`diff_lines_added`/`diff_lines_removed`)
has three consumers** — patch capture, the parsimony score component, and the audit log's
`FileChangeSummary` event — with no statement that they all go through one function. Low
severity because the fix is trivial, but worth naming so it doesn't get implemented three times.

---

## 2. Unclear ownership between sandbox, experiment, evaluator, agent, Git, and reporting

**U1 (Critical) — Worktrees live inside the repo they're meant to be isolated from.** §3 places
`worktrees/<experiment-id>/` under `.agentforge/`, and §3 says `.agentforge/` is created "at the
target repo's root." That means an experiment's cwd
(`<repo>/.agentforge/worktrees/<experiment-id>/`) is a *subdirectory* of the very checkout it's
supposed to be isolated from — not a sibling, not an unrelated path. §13 already, honestly,
admits filesystem confinement is best-effort and not OS-enforced. Combined with nesting, this
means:
- An agent process that does nothing more sophisticated than `cd ../../../` or use an absolute
  path reaches the main repo's working tree — no exotic escape needed, just relative path
  arithmetic, from a cwd that's *guaranteed* to be a few `..`s away from the thing being
  protected.
- Every other experiment's worktree is a sibling directory, reachable the same way — one
  experiment's agent can read or write another concurrently-running experiment's worktree.
- AgentForge's own control plane (`tasks/`, `policies/`, `audit.jsonl` files, `score.json`
  files) is similarly reachable from any experiment's cwd — an agent that decides to "helpfully"
  glob `**/*` or walk upward can see and potentially tamper with the very audit trail and scoring
  records meant to evaluate it.

This isn't a hypothetical OS-permission gap like the ones §13 already discloses; it's a
structural design choice (nesting) making the *distance* to the protected data effectively zero.
*Recommendation:* worktrees (and the whole `.agentforge/` control plane) should live outside the
target repo's own tree — e.g. a per-repo state directory under a user-level cache/data
directory, keyed by a hash of the repo's canonical path — so that "isolated" at least means "not
a parent-relative `cd` away." If keeping `.agentforge/` inside the repo is kept for other
reasons (simpler discovery, matches the workspace's own per-project convention), §13's table
needs a new row spelling out exactly this risk, because it's currently uncovered by any of the
existing rows.

**U2 (Critical) — The `AgentAdapter` trait makes the adapter, not the harness, own process
spawning — which contradicts the "Enforced" timeout claim and weakens audit integrity.** §5's
trait is `fn run(&self, ctx: AgentRunContext) -> Result<AgentRunOutcome>` — a single blocking
call. Whatever spawns the actual `claude` subprocess (and therefore holds the OS process handle
needed to kill it) happens *inside* the adapter's `run()`, because the trait gives the adapter
the context and expects a completed outcome back. Two consequences:

1. §13 lists "Wall-clock timeout on the agent process" as **Enforced, adapter-independent** —
   but if the adapter owns the spawn and is blocking inside its own `run()`, AgentForge has no
   process handle to kill until `run()` returns, which is exactly when a timeout would no longer
   need enforcing. Enforcing a real timeout against a blocking call requires either (a) the
   harness spawning the process itself and the adapter only supplying a command/argument
   specification, or (b) the trait exposing a killable handle/PID the harness can act on
   independently of the adapter's control flow. Neither exists in the current trait. As written,
   §13's "Enforced" claim for the one boundary the spec is proudest of guaranteeing is not
   actually achievable by the architecture that's supposed to deliver it.
2. §12 says "every subprocess AgentForge itself spawns... emits a matching `ProcessSpawn` and
   `ProcessExit` pair," but §5's comment on `AgentRunContext.audit_sink` says "adapter reports
   ProcessSpawn/ProcessExit through this" — i.e. the adapter self-reports. If the adapter (the
   thing invoking the agent under evaluation) is also the thing responsible for telling
   AgentForge that it spawned a process and how it exited, the audit log is not an independent
   observation — a buggy or adversarial adapter can simply omit or misreport events. That
   directly undercuts §1's goal 6 ("a structured audit log of everything the agent **and
   AgentForge itself** did") — as designed, the agent-process portion of the log is provided by
   a party with every incentive (bugs or otherwise) to under-report.

*Recommendation:* invert the trait so the harness owns process spawning end-to-end for every
subprocess (agent and evaluator alike) — the adapter's job becomes producing a command
specification (program, args, cwd, env) the harness executes, not executing it itself. This one
change fixes both the timeout-enforceability gap and the audit-integrity gap, and is also the
natural place to enforce `max_output_bytes` and `cwd` confinement uniformly instead of trusting
each adapter to remember to do it (a second adapter could easily forget to honor
`ctx.worktree_path`, and nothing in the trait signature stops that).

**U3 (Medium) — No named "sandbox" component at all.** The word doesn't appear in the spec as a
module or owner. Responsibility for process spawning, timeout, output capture, and fs
confinement is currently split across "the harness" (never named), "the adapter" (per U2), and
"the worktree" (a path, not a component). Recommend making the fix in U2 concrete: introduce an
explicit process-execution owner (e.g. a `ProcessSandbox`/`Executor` type) that every subprocess
— agent and evaluator alike — goes through, so timeout/output-cap/audit emission are implemented
once and are guaranteed regardless of which adapter or evaluator is involved, rather than
partially delegated per-caller.

**U4 (Medium) — `mutate`'s relationship to Git ownership is unclear.** §7/§4 say `mutate`
"commits the result to a dedicated ref" but never say whether this happens via a worktree (which
§6 doesn't cover for `mutate`, per D1) or via direct git plumbing (reading a blob at `base_ref`,
writing a new tree/commit object, no checkout needed). Both are legitimate approaches but they
have different failure modes and different ownership stories (plumbing-only touches nothing but
`.git/objects` and refs; a worktree-based approach needs the lifecycle U2/D1 leave undefined).
Pick one and state it.

---

## 3. Features too ambitious for a solid MVP

**A1 (Medium) — §1.1 says every capability ships together; `CONTEXT.md`'s own MVP priorities say
otherwise, and the second one is right.** §1.1: "In scope for MVP, all of it required to ship
together." But `CONTEXT.md`'s "MVP priorities" section (written from this same spec) sequences
work as worktree lifecycle → evaluator → adapter/fake-adapter → `run` end-to-end → scoring → real
Claude Code adapter → mutation → race → bisect — i.e., not together at all, and correctly so.
Nine non-trivial subsystems (mutation engine with a mini text-pattern lexer, real `git bisect`
orchestration, a bounded-parallelism race scheduler, a permission-policy schema, an adapter
trait, structured audit logging, a configurable scoring model, a persistence layer, a
multi-command CLI) is a lot to call one indivisible MVP delivery. The spec should say plainly
that "coherent design, phased delivery" is the intent — design all nine together (which is
correctly the point of §1.1's "share infrastructure" framing), but don't gate "MVP done" on all
nine being simultaneously shipped. Leaving this contradiction unresolved risks either a stalled
first release (waiting for everything) or a rushed one (cutting corners under self-imposed "all
or nothing" pressure).

**A2 (Medium) — Real `git bisect` subprocess integration is more machinery than the MVP needs.**
§10 drives actual `git bisect start`/`git bisect run` rather than an in-process binary search
over the commit range. This inherits git's own bisect state machine (with its own on-disk state
under `.git/BISECT_*`), requires the internal oracle to be invoked as an external
process — almost certainly the compiled `agentforge` binary recursively invoking itself — and
creates a real cleanup hazard (see F2 in §8 below) and a real testability hazard (see §9 below)
for a feature whose actual requirement ("find the commit where the evaluator's verdict flips, in
a linear range") doesn't need git's bisect machinery at all. *Recommendation:* implement bisect
as an in-process binary search that checks out each candidate commit in the one dedicated
worktree and calls the shared evaluation primitive (D1) directly — same asymptotic behavior,
no recursive self-invocation, no dangling git-internal state to clean up, dramatically easier to
unit test.

**A3 (Medium) — The permission-policy schema is more configurable than anything in MVP can
enforce.** §5 declares the one shipped adapter (`claude-code`) `RequestedOnly` across all three
`AdapterCapabilities` fields. That means `CommandPolicy`'s glob/prefix allowlist and the three-way
`NetworkMode` enum (`DenyRequested` / `AllowLoggedRequested` / `UnrestrictedLogged`) are pure
configuration surface with zero enforcement behind them in MVP — every value just gets forwarded
as a request and trusted. Building and documenting a rich schema for a capability tier nobody can
actually redeem yet is speculative generality; see U3/§10 (unnecessary abstractions) for the same
point from a different angle. *Recommendation:* either cut `CommandPolicy`/`NetworkMode` down to
a minimal boolean-ish shape for MVP ("network requested off" / "on"), or keep the richer schema
but label the unenforced tiers experimental in the CLI output itself, not just in this spec.

---

## 4. Unsafe or misleading security assumptions

The two sharpest findings in this category are U1 and U2 above (worktree nesting, and the
timeout/audit-integrity gap from adapter-owned process spawning) — both are misleading precisely
because §13's table marks the properties they undermine as **Enforced**, which is the one claim
in the whole spec that's supposed to be unconditionally trustworthy. Additional findings:

**M1 (Medium) — `clean --all-worktrees` has no guard against removing a worktree that's still in
use.** §4 describes `clean` as explicit and non-implicit, which is the right instinct, but
nothing stops `agentforge clean --all-worktrees` from running concurrently with a long `race` and
deleting a worktree out from under a live agent process — no lock, no "skip anything with status
`Running`" check is mentioned. This is a realistic scenario (two terminal windows), not an edge
case, and its failure mode is silent corruption of an in-flight experiment rather than a clean
error.

**M2 (Low) — §13's table is good discipline but has no enforcement mechanism of its own.** The
closing line ("Anything not marked Enforced must not be described as a security guarantee
anywhere else...") is a policy statement with nothing checking it — CLI help text, README
copy, and `policy show` output could drift from this table over time with nothing catching it.
Worth a lightweight test that greps for guarantee-sounding language outside this table, or at
minimum a comment in the implementation pointing back to §13 wherever a boundary is described to
a user.

---

## 5. Scoring that could reward incorrect patches

**S1 (Critical) — The correctness gate checks `build_succeeded` and `timed_out`, but never
`evaluator_exit_code`, and nothing detects a shrinking test count.** §11's gate: "if
`!build_succeeded || timed_out`, correctness is forced to 0." A patch where the build succeeds,
the test command's combined output happens to match the `tests_passed`/`tests_total` regex with
a high ratio, but the evaluator's own process exits nonzero for an unrelated reason (a
post-test lint step, a crash after printing a partial summary, a leftover setup process) sails
through this gate with a high correctness score, because exit code is only consulted in the
*else* branch — when no test counts were extracted at all. Worse, and more specific to this
product: **nothing compares `tests_total` against any prior/expected value.** The best-known
failure mode for automated coding agents under test-based evaluation is deleting or weakening the
failing test rather than fixing the bug. Under the current formula, an agent that deletes the one
failing test turns `tests_passed/tests_total` from, say, `9/10` into a "perfect" `9/9` — a
strictly *better* correctness score for doing *less* correct work. And because `efficiency` and
`parsimony` also reward small, fast changes, all three non-gated components actively reward
exactly this degenerate patch: deleting a test is fast, tiny, and (per the current formula)
"correct." This is not a corner case; it's the shortest path to a high score under the stated
formula and it directly contradicts the spec's own top-line design goal that correctness must
heavily outweigh everything else. *Recommendation:* (a) gate on `evaluator_exit_code != 0` in
addition to `!build_succeeded`, not only as a fallback when no counts are present; (b) add an
`expected_tests_total` (or "minimum tests") field to `EvaluatorSpec`, or always run the baseline
evaluator once per task and compare, and treat a drop in `tests_total` as a hard-gated failure
(same treatment as a build failure), not a silently-accepted normalization input.

**S2 (Medium) — Regex-based metric extraction is inherently gameable beyond test deletion.**
Anything that changes a test runner's summary-line formatting (including incidental changes an
agent might make while "fixing" something adjacent) can desync the extractor from reality without
either a build failure or a nonzero exit code — the evaluator would then fall back to
`evaluator_exit_code == 0` → correctness `1.0`, based on an exit code that may not mean what the
extractor assumed it meant. Not fixable in general (this is the tradeoff of not using a
structured test-result format), but worth flagging in the evaluator-authoring guidance once
written, and reinforces why S1's exit-code-as-a-gate fix matters more than the extractor's
precision.

**S3 (Low) — `hygiene`'s 5 points are close to unconditional.** "1.0 if no timeout, no policy
violations, and the evaluator ran to completion" is true for the overwhelming majority of runs,
including bad ones — a broken-but-non-timing-out, non-policy-violating patch still gets full
hygiene credit. This doesn't reward incorrectness on its own, but it means the real discriminating
scale in practice is closer to 95 points than 100, which should be an intentional calibration
choice, not an artifact.

**S4 (Low) — Divide-by-zero / degenerate budgets aren't validated.** `efficiency` divides by
`budget_secs` and `parsimony` divides by `size_budget_lines`, both sourced from `EvaluatorSpec`,
but §4's `task add` validation ("evaluator exists, base_ref resolves, prompt non-empty") never
checks that the *evaluator's own* numeric fields are positive. A `size_budget_lines: 0` produces
a `NaN`/`inf` division whose behavior under `clamp` isn't specified.

---

## 6. CLI inconsistencies

**C1 (Critical) — The bisect oracle's exit codes contradict the CLI's own global exit-code
table.** §4's legend: `1` = generic/internal error, `3` = evaluator verdict is bad. §8/§10, for
the bisect oracle, explicitly adopt **git-bisect's** convention: `0` = good, `1`–`124` = bad,
`125` = skip. Under §4's legend, `1` means "AgentForge itself broke," not "the evaluator said
bad" — under §10's convention for the same kind of evaluator invocation, `1` means exactly "the
evaluator said bad." If the bisect oracle is the same code path as the public `eval`/`run`
commands (as §14's acceptance criteria require — "single code path, not parallel
implementations"), it cannot simultaneously return `3` for "bad" when invoked directly and `1`
for "bad" when invoked as a bisect oracle without an explicit translation layer the spec never
mentions. If it's *not* the same code path, that directly violates the acceptance criterion. This
needs one explicit resolution: either the internal oracle entrypoint is a distinct, documented
wrapper that translates the shared evaluator's result into git-bisect's convention (keeping the
underlying evaluation function shared, only the CLI-facing exit-code mapping differing by
entrypoint), or the two are reconciled some other way — but right now the spec asserts both
conventions for what sounds like the same operation.

**C2 (High) — Exit-code semantics for `run`/`race` on evaluator-bad, timeout, and policy
violation are unspecified per command.** §4's exit-code legend is global, but no per-command row
says which of `ExperimentStatus`'s four outcomes (`Completed`, `Failed`, `TimedOut`,
`PolicyViolation`) map to which process exit code for `agentforge run` itself — does `run` exit
`0` because *AgentForge* completed its job (regardless of the agent's patch quality), or `3`
because the patch it judged was bad? This matters a great deal for scripting/CI use (the
CONTEXT.md-noted use case of gating on agent success), and it compounds for `run --repeat N` (is
the process exit code based on the worst repeat? any failure? majority?) and for `race` (does the
winner's or a majority's or "all failed" verdict decide the aggregate exit code?). None of this is
defined.

**C3 (Medium) — `MutationRef` has no independent persistence location, so `mutate` without
`--task-id` produces an unspecified result.** §3's directory layout has `tasks/`, `policies/`,
`evaluators/` but no `mutations/`. `MutationRef` (§2) is modeled as reusable, inspectable data,
but the only place it's shown being stored is embedded inside a `TaskSpec`. If `agentforge
mutate` is invoked without `--task-id`, the mutation still happens (a real commit gets created on
a real ref in the user's repo) but there's no stated record of it in AgentForge's own state —
the user is left with a git ref and terminal output but no way to `mutate show` or later attach
it to a task. Either make `--task-id` mandatory (simplest fix) or give `MutationRef` its own
`mutations/<id>.toml` slot in §3.

**C4 (Low) — `race`'s "defensive" evaluator-unset check is unreachable as specified.** §4: race
"fails with exit 2 if the task's evaluator is unset (can't happen given §2, but re-checked
defensively)." Since `TaskSpec.evaluator` is a non-optional `String` in the same spec, this
describes dead code. Harmless, but worth removing or rephrasing so the spec doesn't document
defending against a state its own data model has already made impossible.

**C5 (Low) — `run --repeat N` and `race --agents <single-agent>` overlap without a stated
distinction.** A `race` with one entry in `--agents` and `--repeat N` produces the same N
experiments as `run --agent <that> --repeat N`, except the former also gets a leaderboard and the
latter doesn't. It's unclear which is the "canonical" way to just repeat one config N times, or
whether `run --repeat N` should also emit ranking-style output for consistency.

**C6 (Low) — `task add` collision behavior is unspecified.** §4 says `task add` "copies it into
`tasks/`" after validation; nothing says what happens if `<task-id>.toml` already exists
(overwrite, error, version bump).

---

## 7. Missing reproducibility guarantees

**R1 (High) — `base_ref` is a "commit-ish," not a pinned SHA.** §2's `TaskSpec.base_ref: String`
is documented as "commit-ish the worktree is created from." If a task points at a branch name
rather than a fixed SHA, two `run --repeat 2` invocations — or even the two halves of one long
`race` — aren't guaranteed to start from the same commit if something else moves that branch in
the meantime. Given the emphasis elsewhere on reproducibility (mutation determinism, evaluator
determinism), leaving the one field that anchors *what state everything starts from* mutable is
an inconsistency. *Recommendation:* resolve `base_ref` to a concrete SHA at `task add` /
`mutate` time and persist the resolved SHA, not the ref name.

**R2 (High) — Race leaderboard tie-break uses a field the spec itself declares non-deterministic,
directly contradicting the "fully deterministic ordering" claim in the same section.** §9:
"ties broken by lower `wall_time_secs`... fully deterministic ordering, no ambiguous ties in
output." But §8 states plainly: "`wall_time_secs` is explicitly exempt from [the determinism
contract] (timing varies by machine load)." A field explicitly called out elsewhere in the same
document as machine-load-dependent cannot also be relied on for "fully deterministic" tie-break
ordering — two identical `race` invocations on a loaded machine can plausibly order tied
participants differently. The documented secondary tie-break (`experiment_id` ascending) doesn't
rescue this either: per §3's ID scheme (compact ISO8601 timestamp + 6 random hex chars),
experiments launched in the same wall-clock second — the normal case for parallel race
participants — sort only by their random suffix, which isn't derived from anything reproducible.
So both the primary and secondary tie-break keys are effectively non-deterministic for the exact
scenario (parallel race, tied scores) the rule exists to handle. *Recommendation:* either accept
and document that tie order among truly-tied scores is not guaranteed stable across runs (drop
the "fully deterministic" claim), or pick a genuinely deterministic tie-break (e.g. a
declaration-order index assigned at race-construction time, before any parallelism starts).

**R3 (Medium) — Evaluator determinism assumes a stable toolchain, which is never pinned or even
flagged as an assumption.** §8's determinism contract covers AgentForge's own behavior (same
working tree → same verdict) but says nothing about the environment the `setup_cmds`/`test_cmd`
run in — compiler/toolchain/package versions are entirely the target repo's concern, but a spec
this careful about reproducibility elsewhere should at least name this as a boundary: "same
verdict" is only promised for a fixed toolchain, and AgentForge doesn't pin or verify one.
Concretely relevant here: `CONTEXT.md` records that Rust itself isn't even installed on the
authoring machine yet, underscoring that toolchain state isn't something to assume stable.

**R4 (Medium) — Mutation candidate discovery's determinism depends on filesystem walk order and
path casing, which differ between the dev machine (Windows, per `CONTEXT.md`) and the likely CI
target (Linux, by convention of the sibling `crie` project's `ubuntu-latest` CI).** §7's
determinism contract says candidates are sorted "by `(file_path, line, column)`" with paths
"lexicographic," but doesn't specify byte-wise vs. locale-aware comparison, or how path
separators and case-folding are normalized. Windows filesystems are case-insensitive by default;
Linux ones aren't. Without an explicit normalization rule (forward-slash-joined, byte-wise,
case-sensitive comparison regardless of host OS), a mutation authored on the Windows dev machine
and replayed in Linux CI is not guaranteed to select the same candidate — a concrete,
environment-specific risk given this project's actual dev/CI split, not a theoretical one.

**R5 (Low) — No stated behavior when a persisted `formula_version` or `operator_version` doesn't
match the currently-installed binary.** Both fields exist (§2, §7) specifically to mark
reproducibility boundaries, but nothing says what `agentforge score`/`mutate --spec` do when
replaying an old record against a newer binary — silently reinterpret with current logic, warn,
or refuse. Given the fields exist precisely to prevent silent reinterpretation, leaving their
consumption behavior unstated undermines the reason they were added.

---

## 8. Missing failure/cleanup behavior

**F1 (High) — No reconciliation path for interrupted experiments.** If `agentforge run` is
killed (Ctrl-C, crash, host reboot) mid-experiment, nothing describes what happens to the
worktree, to the `ExperimentRecord`'s `status` (stuck at `Running` forever, since nothing ever
transitions it), or to `ended_at` (never set). §4's `clean` command only describes removing
worktrees, not reconciling stale `Running` records. *Recommendation:* `clean` (or a dedicated
`status --reconcile`) should detect and mark orphaned `Running` experiments (e.g., no live
process for the recorded PID) as `Failed`, and the acceptance criteria/tests should cover this
directly.

**F2 (High) — No described cleanup for an interrupted bisect.** Because §10 drives real `git
bisect start`/`git bisect run` (see A2), a crash or kill mid-bisect leaves that dedicated
worktree's `.git` metadata in an active bisect state (`git bisect` refs/state present) with no
described `git bisect reset` on recovery, and no `bisect --abort`/`--resume` command in §4's
table. This is a direct consequence of choosing real git-bisect machinery over an in-process
search (A2) — an in-process implementation wouldn't have this class of dangling state at all.

**F3 (Medium) — Race partial failure is unspecified.** If one experiment in a race panics
AgentForge's own process (not just the agent under test), it's not stated whether the whole race
aborts, or whether remaining experiments continue and the `RaceRecord` reflects a partial
leaderboard. Also unspecified: whether in-flight worktrees from *other*, still-running
experiments get cleaned up if the race-level command itself is killed.

**F4 (Low) — `setup_cmds` partial failure isn't defined.** If the first of several `setup_cmds`
succeeds and the second fails, it's not stated whether `test_cmd` still runs, is skipped, or the
whole evaluator run short-circuits with `build_succeeded = false`. Any of these is reasonable;
the spec should pick one.

**F5 (Low) — No retention/disk-usage policy for accumulated worktrees.** `--keep-worktree-on-fail`
plus repeated debugging of a consistently-failing config can accumulate worktrees indefinitely
with only manual `clean` as a backstop — fine for MVP, but worth a one-line acknowledgment rather
than silence.

---

## 9. Designs that will be difficult to test deterministically

**T1 (Critical, restates R2) — Race leaderboard tie-break tests are testing a formula the spec
says is non-deterministic.** §15's test 12 asks for "correct leaderboard order including a
constructed tie fixture," but per R2 the documented tie-break keys (`wall_time_secs`, then
`experiment_id`) are both effectively non-reproducible for genuinely parallel, genuinely tied
participants. The test can only be made deterministic by injecting fixed/fake timing through the
`FakeAdapter` — which §15 doesn't call out as a requirement. Until R2 is resolved, this test
either can't be written honestly or is testing a fake-adapter-injected value rather than the real
tie-break logic.

**T2 (High) — Real `git bisect` integration requires integration tests against a compiled binary,
not in-process unit tests.** Per A2, if the bisect oracle is invoked as an external process
(almost certainly the `agentforge` binary recursively invoking itself via `git bisect run
agentforge ...`), §15's test 13 can't be a pure in-process test — it needs to build/locate the
actual binary (e.g. via `assert_cmd`'s `cargo_bin`), spin up a real subprocess tree, and interact
with real `.git` bisect state. That's slower, more environment-sensitive, and harder to keep
flake-free than testing an in-process binary-search function directly, which is the concrete
payoff of the A2 recommendation.

**T3 (Medium) — Timeout-margin tests are inherently timing-flaky.** §14/§15's "killed within a
bounded margin (e.g. ≤2s)" is a wall-clock assertion that will be flaky on loaded/shared CI
runners regardless of implementation correctness. *Recommendation:* either use a generous margin
explicitly chosen for CI headroom, or design the timeout mechanism to be tested via an injectable
clock/signal rather than a real sleep-and-measure test.

**T4 (Low) — Mutation's "best-effort" string/comment-skipping heuristic is a moving target for
tests.** Regex-based skipping of string/comment literals (§7) will have known blind spots (e.g. a
`//` inside a string literal, raw strings, multi-line comments) that are inherent to not using an
AST (an explicit, accepted non-goal). Tests here can only pin down the heuristic's *current*
behavior, not a spec-defined "correct" behavior — worth acknowledging so future test failures
from edge cases aren't mistaken for regressions.

---

## 10. Unnecessary abstractions

**X1 (restates A3) — `CommandPolicy`/`NetworkMode`'s richer variants are unenforceable in MVP.**
See §3 above ("Features too ambitious") — the same finding is also, from a different angle, an
unnecessary-abstraction problem: building multi-variant enums and glob-pattern schemas for
enforcement tiers that have zero real implementations behind them (`RequestedOnly` everywhere) is
speculative generality the MVP doesn't need yet.

**X2 (Low) — `env_passthrough` lives on `AgentConfig` rather than `PermissionPolicy`.** §2 splits
"what this agent run is allowed to touch" across two structs: filesystem/commands/network live on
`PermissionPolicy`, but environment-variable exposure — arguably the most security-sensitive of
the four (it's how API keys reach the agent process) — lives on `AgentConfig` instead. Not wrong,
but it means there are two places to look for "what can this run access," and future policy
tooling (`policy show`/`validate`) would need to reach across both structs. Consider
consolidating under `PermissionPolicy` for a single source of truth, or explicitly justify the
split if there's a reason `env_passthrough` is meant to vary per-agent rather than per-policy.

**X3 (Low) — Two same-purpose "PolicyViolation" names.** `ExperimentStatus::PolicyViolation` (a
terminal experiment outcome) and `AuditEvent::PolicyViolation` (a single logged event) share a
name but aren't declared to have a defined relationship — does *any* logged violation force the
terminal status, or only some subset? If the latter, what's the threshold? This reads like an
abstraction that was named once and reused without fully specifying the mapping between the two
levels it now exists at.

---

## Findings index (by severity)

| Severity | Findings |
|---|---|
| Critical | U1, U2, S1, C1, T1 (restates R2) |
| High | D1, R1, R2, F1, F2, C2, T2 |
| Medium | D2, D3, U3, U4, A1, A2, A3, M1, S2, C3, R3, R4, F3, T3 |
| Low | D4, M2, S3, S4, C4, C5, C6, R5, F4, F5, T4, X2, X3 |

Nothing here requires re-architecting the spec wholesale — the core composition (worktrees +
shared evaluator + adapter trait + scoring + audit log) is sound. The critical findings are
concentrated in a few specific decisions (where worktrees physically live, who owns process
spawning, whether the correctness gate checks exit code and test count, and how bisect's exit
codes reconcile with the rest of the CLI) and are addressable without touching anything else in
the spec.
