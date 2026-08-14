# Usage Guide

A complete command reference for AgentForge, with real, working examples. Every example on this
page is copied from an actual `cargo run --example demo` run or the CLI's own `--help` output —
nothing here is aspirational or hand-typed from memory. See [README.md](../README.md) for a
30-second overview and [ARCHITECTURE.md](ARCHITECTURE.md) for how the pieces fit together.

## Before you start

You need a built `agentforge` binary — see [README.md's Install section](../README.md#install)
if you haven't built one yet (`cargo build --release`, under a minute, no external dependencies).
Every command below assumes `agentforge` is on your `PATH`; substitute
`./target/release/agentforge` (or `.\target\release\agentforge.exe` on Windows) if you haven't
added it to `PATH`.

Sections 1–4 below walk through real setup against your own git repository, end to end. If you'd
rather see the whole tool work *before* writing any config of your own, run `cargo run --example
demo` first (no setup, no API key) — it drives every command on this page against a small seeded
fixture repo; see [Running the whole thing locally](#running-the-whole-thing-locally-no-paid-api)
at the bottom of this page.

## Conventions used below

- Every command that reads repo-tracked or state-root state takes `--repo <path>` (default `.`).
- Every command that can produce a structured result takes `--json`.
- `<agent>` is always `adapter[:model]` — today, the only resolvable adapter name is
  `claude-code`, e.g. `--agent claude-code` or `--agent claude-code:opus`. See
  [README.md](../README.md#local-demo--zero-paid-api) for how to exercise the whole CLI without a
  paid API key at all.
- Global exit codes, used consistently, no per-command exceptions: `0` success, `1` generic/
  internal error, `2` usage/validation error, `3` a judged patch/commit was bad, `124` an agent
  process was killed for exceeding its timeout.

---

## 1. Initialize a repository

```
$ agentforge init --repo .
initialized ./.agentforge
state_root:  /home/you/.local/share/agentforge/state/6ad646ca8191629a
```

Must be run inside a git repository; fails (exit 2) if `.agentforge/` already exists. Scaffolds
`tasks/`, `evaluators/`, `policies/` under `.agentforge/`, and resolves + prints the **state
root** — a directory *outside* your repository's own directory tree where every worktree,
experiment record, and audit log lives (see [ARCHITECTURE.md §7](ARCHITECTURE.md) for why that
separation matters). `.agentforge/config.toml` caches the resolved path so it's visible with a
plain `cat` afterward.

## 2. Register an evaluator

An evaluator is the deterministic oracle every other command defers to — it decides pass/fail, no
agent or human judgment involved. It's a TOML file:

```toml
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
program = "sh"
args = ["evaluate.sh"]
cwd_relative = "."
```

```
$ agentforge evaluator add billing-tests.toml
added evaluator billing-tests
$ agentforge evaluator list
billing-tests
$ agentforge evaluator show billing-tests
```

`setup_cmds` run first (any failure short-circuits before `test_cmd` runs); `test_cmd`'s stdout is
scanned by each `metric_extractors` regex to populate `tests_passed`/`tests_total`. Both are the
only two metric names AgentForge understands today.

## 3. Register a policy (optional but recommended)

```toml
name = "demo-policy"
extra_readonly_paths = []
deny_network = true
env_passthrough = []
max_wall_time_secs = 120
max_output_bytes = 5000000
allowed_programs = []
denied_programs = []
allowed_roots = []
```

```
$ agentforge policy add demo-policy.toml
added policy demo-policy
$ agentforge policy show demo-policy
name: demo-policy
  env_passthrough      Enforced  the Executor builds the child's environment from exactly this allowlist, never the full host environment
  max_wall_time_secs   Enforced  the Executor kills the directly-spawned process if it outlives this budget
  max_output_bytes     Enforced  the Executor truncates captured stdout/stderr at this cap
  allowed_programs     Enforced  the Executor refuses to spawn a program absent from a non-empty allowlist
  denied_programs      Enforced  the Executor refuses to spawn any program on this list, checked before allowed_programs
  allowed_roots        Enforced  the Executor refuses to spawn into a cwd outside every configured root
  deny_network         RequestedOnly  relayed to the adapter as a request only; no firewall/network-namespace enforcement exists in MVP (SPEC.md §3.1)
  extra_readonly_paths RequestedOnly  relayed to the adapter as a request only; AgentForge does not independently verify it was honored
  max_memory_bytes     Unsupported  representation only — memory/CPU caps need OS-level facilities (Job Objects/cgroups) that are explicitly out of scope for MVP (SPEC.md §3.1)
```

`policy show` is the authoritative, always-current answer to "what does this policy actually
restrict?" — see [SECURITY.md](SECURITY.md) for the full model this output reflects. Omitting
`--policy` on `run`/`race` falls back to a generous built-in default, not to "no restrictions."

## 4. Register a task

```toml
id = "fix-discount-bug"
name = "fix-discount-bug"
prompt = "Fix billing/discount.txt: DISCOUNT_RATE must be 0.10 and LOGGING must be enabled."
repo_path = "."
base_ref = "HEAD"
evaluator = "billing-tests"
agent_timeout_secs = 30
created_at = "2026-01-01T00:00:00Z"

[baseline]
build_succeeded = true
exit_code = 1
timed_out = false
wall_time_secs = 0.0
```

```
$ agentforge task add fix-discount-bug.toml
created task fix-discount-bug
$ agentforge task show fix-discount-bug
id:        fix-discount-bug
name:      fix-discount-bug
base_ref:  a49f790b0e45aaf07fd2a29c43f088c982a34fdd
evaluator: billing-tests
baseline:  build_succeeded=true exit_code=1 tests=Some(0)/Some(2)
mutation:  none
```

`base_ref` is resolved to a concrete SHA at add time. `task add` **recaptures the baseline for
real** by running `evaluate()` once against that SHA in a throwaway worktree — the `[baseline]`
block in your TOML is a placeholder that gets overwritten with the true measured result, not
trusted as-is.

## 5. Run a single candidate

```
$ agentforge run --task fix-discount-bug --agent claude-code:goodfix --policy demo-policy --json
```

```json
{
  "id": "20260814T013725Z-2bbfd9",
  "task_id": "fix-discount-bug",
  "agent_config": { "adapter": "claude-code", "model": "goodfix", "policy": "demo-policy", "extra_args": [] },
  "status": "Completed",
  "score": { "total": 99.0, "rating": "Excellent", "gated": false },
  "raw_metrics": { "..." : "build/test/diff/timing measurements" },
  "patch_path": ".../experiments/20260814T013725Z-2bbfd9/patch.diff",
  "audit_log_path": ".../experiments/20260814T013725Z-2bbfd9/audit.jsonl"
}
```

Sequence, every time: create an isolated worktree → the Executor spawns the adapter's command in
it → capture the resulting patch via `git diff` → `evaluate()` the patch → score it → write the
record → remove the worktree (unless the run didn't complete and `--keep-worktree-on-fail` was
passed). Exit code precedence: `Failed` → `1`; `TimedOut` → `124`; `Completed` and gated → `3`;
`Completed` and not gated → `0`.

Without `--json`, `run` prints the same human-readable report `report show` would (see §11).

## 6. Race multiple agents/models

```
$ agentforge race --task fix-discount-bug --agents claude-code:goodfix,claude-code:partialfix,claude-code:nofix --max-parallel 3
```

```
Race            20260814T013659Z-06e47c
Task            fix-discount-bug
Max parallel    3

Candidate                  Tests  Time  Patch  Score  Rating
#0 claude-code:goodfix     2/2    0s    4L     99     Excellent
#1 claude-code:partialfix  1/2    0s    2L     60     Fair
#2 claude-code:nofix       0/2    0s    0L     5      Fail
```

`--agents` expands (in listed order) × `--repeat` (default 1) into a flat, ordered participant
list **before any execution starts** — an entry's position is its permanent `race_index`.
Participants run as independent `run`s, bounded by `--max-parallel` (unbounded fan-out if
omitted — see [SECURITY.md](SECURITY.md#tier-4--not-provided-do-not-rely-on-these)). One
participant's internal failure never aborts the others. Ranking is by `score.total` descending,
then `race_index` ascending; non-`Completed` participants sort last. Exit `0` if at least one
participant reached `Completed`, `1` if none did.

## 7. Semantic bisect

Binary-search a commit range for the exact commit that flipped a behavior, using the task's
evaluator as the oracle — no source diffing, no heuristics.

```
$ agentforge bisect --task bisect-regression-hunt --range 267a902b..9d8203f2
Bisect          20260814T013701Z-89b375
Task            bisect-regression-hunt
Evaluator       bisect-status
Range           267a902b..9d8203f2
Steps tested    3
    1  3bc884d1  BAD   (exit=1 build_succeeded=true)
    2  8235853465  GOOD  (exit=0 build_succeeded=true)
    3  5a57b2bf  GOOD  (exit=0 build_succeeded=true)
Culprit         3bc884d1ca8d4ab525a35b0be0ac6a485b6e5321
```

`good` must be an ancestor of `bad` (exit 2 otherwise, checked before any worktree is created).
Runs in one dedicated worktree, calling `evaluate()` per candidate — a real binary search, not
`git bisect run` shelling out. Exit `0` with `culprit` set on a single flip found; exit `3` if
every tested commit in the range shares one verdict (nothing to bisect).

## 8. Verify a ref standalone

Run the evaluator against an arbitrary commit — no task, no scoring, no worktree left behind —
useful for sanity-checking an evaluator itself or re-checking a stored experiment's patch:

```
$ agentforge verify --evaluator billing-tests --ref HEAD
Build           succeeded
Tests           0/2
Exit code       1
Timed out       false
Wall time       0s
Verdict         BAD
```

```
$ agentforge verify --experiment 20260814T013725Z-2bbfd9
```

Exactly one of `--ref` (with `--evaluator`, and optionally `--apply-patch <file>`) or
`--experiment <id>` is required. Exit `0`/`3` on the verdict; no `ScoreCard` is produced (scoring
only applies to experiments, which have a baseline to score against).

## 9. Reports

`report show <id>` works on an experiment, race, *or* bisect id — it figures out which:

```
$ agentforge report show 20260814T013725Z-2bbfd9 --verbose
Experiment      20260814T013725Z-2bbfd9
Task            fix-discount-bug
Agent           claude-code:goodfix
Status          Completed

Tests           2/2
Score           99
Rating          Excellent
Gated           no

Scoring components (configured weights)
  correctness  raw=1.000  normalized=1.000  weight=80.0  contribution=80.00
  efficiency   raw=0.057  normalized=0.998  weight=10.0  contribution=9.98
  parsimony    raw=4.000  normalized=0.920  weight=10.0  contribution=9.20

Failed checks   none
```

`report score <experiment-id> [--weights <file>]` recomputes a `ScoreCard` from the persisted raw
measurements — it spawns no process, so it's free to call repeatedly with a different weights
file to see how a formula change would have scored an existing result. `report log
<experiment-id> [--follow]` pretty-prints the experiment's `audit.jsonl`; `--follow` tails a
still-running experiment until its lock clears.

Add `--json` to `show`/`score` for the underlying record instead of the formatted report.

## 10. Repository-state test fixtures (`experiment fault` / `mutation` / `mutant`)

Three sibling mechanisms for building deterministic, reproducible test fixtures — none of them
produce an `ExperimentRecord` on their own; they exist to *set up* a known-bad state for an agent
or evaluator to be tested against.

**`experiment fault`** — inject a deterministic, non-code-logic failure (a missing file, a broken
config value, a stale artifact, a corrupted dependency pin) into an isolated worktree:

```
$ agentforge experiment fault inject --id billing-fault --spec fault-spec.toml --base HEAD
injected fault billing-fault (BrokenConfigValue at billing/discount.txt)
  billing/discount.txt:1: replaced `DISCOUNT_RATE=0.50` with `DISCOUNT_RATE=__AGENTFORGE_BROKEN__`
$ agentforge experiment fault show billing-fault
$ agentforge experiment fault restore billing-fault   # reverse it in place
$ agentforge experiment fault discard billing-fault    # remove the whole worktree
```

**`experiment mutant`** — standalone, reproducible source mutation testing. `apply` never gates
or scores; `evaluate` is a separate, later step:

```
$ agentforge experiment mutant apply --id billing-mutant --spec mutant-spec.toml --base HEAD
applied mutant billing-mutant (BooleanFlip at billing/flags.txt:1:9)
$ agentforge experiment mutant evaluate billing-mutant --evaluator flags-detector
evaluated mutant billing-mutant — KILLED (detected)
```

A mutant that survives evaluation (`SURVIVED (not detected)`) is reported, not treated as a
command failure — a surviving mutant means your evaluator has a real coverage gap, which is
useful information, not an error.

**`experiment mutation`** — the task-embedded variant: applies a mutation, runs an immediate
sanity-gate evaluation, and only creates the named task if the fault was actually detected
(otherwise exits 2 and creates nothing):

```
$ agentforge experiment mutation create --task-id my-mutation-task --spec mutation-spec.toml --base HEAD --evaluator billing-tests
```

All three select their candidate deterministically from `(kind/operator, target_glob, seed,
version, base)` — the same seed against the same base always selects the same target and produces
a byte-identical result. `experiment mutation replay <task-id>` re-applies a task's stored spec
and asserts this holds.

## 11. Isolated workspaces (low-level primitive)

A workspace is a disposable, isolated worktree with no task/evaluator/agent config attached —
useful for exploring a repo state or running an ad hoc command in isolation:

```
$ agentforge workspace create --id explore --base HEAD
created workspace explore at /home/you/.local/share/agentforge/state/.../worktrees/explore
$ agentforge workspace exec explore -- cargo test
$ agentforge workspace list
$ agentforge workspace remove explore
```

`workspace exec` passes everything after `--` as a literal argument array to the child process —
**never through a shell**, so shell metacharacters in a command argument are not interpreted. It
also accepts its own `--allow-program`/`--deny-program`/`--allowed-root`/`--env-passthrough`
flags for ad hoc, one-off Executor-level restriction — see
[SECURITY.md](SECURITY.md#tier-2--application-level-restrictions-policy-configurable).

## 12. Cleanup

```
$ agentforge clean --all-worktrees --force
removed worktree for 20260814T013658Z-6cc5ba
$ agentforge workspace clean --force
```

`clean` always reconciles first: any experiment recorded as `Running` with no live
`RUNNING.lock` present is marked `Failed` ("interrupted: no active lock found") before any
removal happens. `--experiment <id>`, `--all-worktrees`, or `--older-than <n><unit>` (`s`/`m`/`h`/
`d`) select what to remove; refuses a still-locked worktree unless `--force`. Note `clean` only
sweeps experiment/race/bisect worktrees — a plain `workspace` has its own separate `workspace
remove`/`workspace clean` lifecycle, shown above.

---

## Running the whole thing locally, no paid API

Every example above is drawn from `cargo run --example demo` — a narrated script that builds
`agentforge` plus a tiny deterministic stand-in binary (`mock_claude`), then drives the real
compiled `agentforge` binary through every command on this page against a small fixture "billing
service" repository with a seeded bug. Nothing is mocked at the AgentForge level — it's the same
binary, the same `ClaudeCodeAdapter`, redirected at a fake executable via the adapter's own
`AGENTFORGE_CLAUDE_EXECUTABLE` override (the same knob a CI pipeline would use). See
[README.md](../README.md#local-demo--zero-paid-api) to run it yourself.

```
cargo run --example demo        # narrated, ~10s
cargo test --test demo_e2e      # same scenario, asserted
```
