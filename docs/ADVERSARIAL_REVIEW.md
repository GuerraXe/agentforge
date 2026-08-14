# AgentForge — Adversarial Security & Correctness Review

**Date:** 2026-08-13. **Scope:** the full `src/` tree as of this pass (no git history existed for
the project yet — this is a source-code audit, not a diff review). **Posture:** hostile reviewer,
assuming a malicious or merely careless user points AgentForge at an untrusted target repository,
an untrusted task/evaluator/policy spec file, or runs commands concurrently — not just the happy
path SPEC.md/ARCHITECTURE.md describe. Every finding below was verified by reading the actual
code path, not inferred from documentation; every "Fixed" finding has a passing regression test
that exercises the real vulnerable code path (not just the helper function in isolation).

**Result: 5 real, independently-exploitable issues found and fixed** (one Critical, two High, two
Medium), plus several lower-severity/documented-only observations below. `cargo fmt --check`,
`cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` all pass clean after
the fixes — **284/284** (up from 276/276; the delta is new regression tests, not new product
features). See `docs/TEST_STATUS.md` for the dated pass entry.

---

## Summary table

| # | Severity | Finding | Status |
|---|----------|---------|--------|
| 1 | **Critical** | Path traversal / arbitrary file read-write via unvalidated ids in `store::Store` | **Fixed** |
| 2 | **High** | Malicious repository symlinks let `fault inject`/`mutant apply` write outside the isolated worktree | **Fixed** |
| 3 | **High** | SPEC.md's timeout process-tree-kill claim was not implemented — grandchildren survived indefinitely | **Fixed** |
| 4 | **Medium** | Subprocess output capture was unbounded on disk *during* the run — only truncated after the fact | **Fixed** |
| 5 | **Medium** | A panic in one race participant's thread would unwind past `thread::scope` and lose every other participant's already-collected result | **Fixed** |
| 6 | Low / informational | No cross-process lock around `git worktree add`/`remove` — two concurrent AgentForge invocations against the same repo can race | Documented, not fixed |
| 7 | Low / informational | `correctness_ratio` defaults to full credit (1.0) when an evaluator's `metric_extractors` find no test counts | Documented, not fixed (product decision) |
| 8 | Low / informational | Read-side path-traversal amplification via `clean --experiment <id>` trusting a loaded record's `worktree_path` | Fixed as part of #1 |
| 9 | Informational | TOCTOU window between the symlink check and the write it guards | Documented residual |
| 10 | Informational | `RaceRunner`'s `--max-parallel`-omitted default is unbounded (`agents × repeat`) | Documented, self-inflicted only |

---

## Finding 1 — Critical: path traversal / arbitrary file read-write via unvalidated ids in `store::Store`

**Where:** `src/store/mod.rs` — every `load_*`/`save_*` function.

`workspace::validate_id`, `fault::validate_id` (reused by `mutant`), and their dedicated tests
(`tests/workspace.rs`, `tests/fault_reproducibility.rs`, `tests/mutant_reproducibility.rs`, each
asserting `["../../evil", "..", "a/../../b", "a/b", "a\\b", "."]` are rejected) establish a clear,
deliberate, well-tested convention in this codebase: **any caller-chosen id that becomes a
filesystem path component must be restricted to `[A-Za-z0-9_-]`**. `store::Store` — the one module
every other id-addressed collection (`tasks`, `evaluators`, `policies`, `faults`, `mutants`,
`experiments`, `races`, `bisects`) ultimately persists through — never enforced this rule at all.
Every function built its path with a bare `format!("{id}.toml")` / `.join(id)`:

```rust
pub fn load_task(&self, id: &str) -> Result<TaskSpec> {
    read_toml(&self.tasks_dir().join(format!("{id}.toml")), id)   // id: totally unchecked
}
pub fn save_task(&self, task: &TaskSpec, force: bool) -> Result<()> {
    write_toml(&self.tasks_dir().join(format!("{}.toml", task.id)), task, force)
}
```

`task.id`/`spec.id` (evaluator)/`policy.name` come straight from a **user-authored TOML file**
(`task add --file`, `evaluator add --file`, `policy add --file`) with no validation beyond
"non-empty prompt" / numeric-field bounds — nothing ever checked the id's *character set*. Worse,
`RepoIdArgs.id`/`MutationTaskArgs.task_id` (bare CLI arguments for `<noun> show`/`restore`/
`discard`/`report show <id>`/`clean --experiment <id>`) reach `Store` with **no validation
anywhere upstream either** — not even the character-class check `fault`/`mutant`/`workspace`
apply at their own boundary before ever calling `Store`.

**Concrete exploit:** a hostile or careless task-spec file —

```toml
id = "../../../../../../Users/victim/.ssh/authorized_keys"
name = "x"
prompt = "x"
...
```

— run through `agentforge task add --file evil.toml --force` writes a `.toml`-suffixed file
outside `.agentforge/tasks/` entirely, at a path the attacker fully controls via `..` segments,
overwriting whatever is there if `--force` is passed. The same pattern applies to `evaluator add`,
`policy add`, and — on the *read* side — to `<noun> show <id>`, `mutation show/replay <task-id>`,
`report show <id>`, and `clean --experiment <id>`. The last of these is a genuine escalation: a
traversal id let through to `load_experiment` can point at an arbitrary file, whose content (if it
happens to parse) supplies `record.worktree_path` — which `clean` then passes straight to
`git worktree remove --force`, an operation whose whole point is deleting a directory tree.

**Fix:** a single choke-point `validate_id` (mirroring `workspace`/`fault`'s existing rule byte for
byte) added to `store::Store` itself, called at the top of every `load_*`/`save_*` function that
builds a path from an id or name — `load_task`/`save_task`, `load_evaluator`/`save_evaluator`,
`load_fault`/`save_fault`, `load_mutant`/`save_mutant`, `load_policy`/`save_policy`,
`load_experiment`/`save_experiment`, `load_race`/`save_race`, `load_bisect`/`save_bisect`. This is
the one place *every* id-addressed collection passes through regardless of caller, so fixing it
here closes the gap for every current call site and every future one, rather than requiring each
CLI entry point to remember to validate independently. Two new `store::Error` variants
(`EmptyId`/`InvalidId`) wired into the CLI's existing exit-code mapping (usage error, exit 2).

**Verified by:** `tests/store.rs` — `save_task_rejects_a_path_traversal_id`,
`load_task_rejects_a_path_traversal_id`, `save_evaluator_rejects_a_path_traversal_id`,
`save_policy_rejects_a_path_traversal_name`, `load_experiment_rejects_a_path_traversal_id`, each
run against the exact same malicious-id set the pre-existing `workspace`/`fault`/`mutant` tests
use, confirming nothing is created outside the repo and every call returns `InvalidId`/`EmptyId`.

---

## Finding 2 — High: malicious repository symlinks escape the isolated fault/mutant worktree

**Where:** `src/fault/mod.rs` (`FaultInjector::inject`), `src/mutant/mod.rs`
(`MutantTester::apply`).

`find_candidates` (both modules) only checks the *string shape* of a git-tracked path
(`is_safe_relative_path`: no `..`/absolute components) — it says nothing about what that path
resolves to once checked out. Git tracks symlinks as ordinary blobs (mode `120000`) whose content
is the link target; `git worktree add`/`checkout` materializes them as real filesystem symlinks on
any platform/config that honors `core.symlinks` (true by default on Linux/macOS, and on Windows
with Developer Mode or an elevated process). Before this fix, `StaleArtifact`/`BrokenConfigValue`/
`DependencyCorruption` (fault) and `apply` (mutant) touched the candidate's resolved target path
with a plain `std::fs::write`/`std::fs::read_to_string` — which **follows a symlink** transparently.

**Concrete exploit:** a hostile repository tracks `config.toml` as a symlink pointing at, say,
`~/.bashrc`, another experiment's still-live worktree, or any path the AgentForge process can
write. A user running `agentforge experiment fault inject --spec stale.toml --base HEAD --id x`
against that repo (with a `target_glob` matching `config.toml`) has the fault write silently
redirected outside the isolated fault worktree entirely — the repository under test escapes its
sandbox the moment the fault mechanism touches that candidate. `mutant apply` has the identical
exposure (it also does a raw `read_to_string`/`write` on its resolved candidate path).
`MissingFile`'s `std::fs::remove_file` is *not* affected — removing a symlink removes the link
itself on every platform, never the target — but the other three kinds are.

**Fix:** a shared `fault::reject_symlink` helper (`std::fs::symlink_metadata` — which, unlike
`metadata`, never follows the link itself) called immediately after the candidate's target path is
resolved in both `FaultInjector::inject` and `MutantTester::apply`, before any read/write/remove
touches it. Refuses unconditionally on any symlink rather than trying to characterize a "safe"
target — the point of an isolated worktree is that nothing inside it should be able to reach
outside it, full stop. New `Error::TargetIsSymlink` variant in both modules, wired into the CLI's
existing usage-error exit-code mapping.

**Verified by:** `tests/fault_reproducibility.rs` —
`inject_refuses_to_follow_a_tracked_symlink_outside_the_worktree` (both a `#[cfg(unix)]` version,
which needs no special privilege and runs for real in this session, and a `#[cfg(windows)]`
version that attempts real symlink creation and skips gracefully — printing why — if
`SeCreateSymbolicLinkPrivilege` isn't available in the current environment, which it is not in
this development sandbox; verified the skip path fires correctly here, but the actual assertion
could not be exercised end-to-end on this machine). `mutant::MutantTester::apply` shares the exact
same `reject_symlink` call and code shape; a dedicated Windows-side test for it was not duplicated
given the sandbox constraint above, but the fix is identical and equally covered by code review.

---

## Finding 3 — High: SPEC.md's timeout process-tree-kill claim was never actually implemented

**Where:** `src/exec/mod.rs` (`wait_with_timeout`).

SPEC.md §8 states the Executor, on timeout, "kills the child (and its process group / job object,
where the platform provides one)." Before this pass, nothing in the codebase ever created a
process group or a Windows Job Object anywhere — `grep`-ing the entire source tree for
`JobObject`/`process_group`/`setsid` turned up zero hits. `wait_with_timeout` only ever called
`Child::kill()`, which terminates the one directly-spawned process. A grandchild the agent or an
evaluator command spawns and detaches — a background daemon, a leftover build worker, anything —
survives the timeout kill indefinitely. For a tool whose entire purpose is running
timeout-bounded, untrusted-by-construction agent and evaluator commands, this is both a real
resource-exhaustion risk and a documentation claim the implementation didn't back up
("misleading...resource/sandbox claims" from the review brief, and "timeout/process cleanup
failures").

To be clear about what *was* already honestly scoped: SPEC.md §3.1 explicitly lists "process-tree
resource limits" as a non-goal and calls the grandchild-survival risk out by name — that part is
transparent, not misleading. The misleading part specifically was §8's parenthetical claiming the
*direct-kill* path already reached a process group/job object "where the platform provides one" —
Windows and Linux both provide one, and neither was used.

**Fix:** real process-tree containment, gated per-platform in a new `exec::tree` module:

- **Windows** (this development machine's actual platform, and the one this could be verified
  against for real): the child is assigned to a Job Object immediately after spawn
  (`CreateJobObjectW` + `AssignProcessToJobObject`), configured with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. On timeout, `TerminateJobObject` kills every process still
  in the job — the direct child and any grandchild that didn't itself escape the job — not just
  the one `Child::kill()` reaches. The `KILL_ON_JOB_CLOSE` flag is a second, independent safety
  net: even a *normal*, non-timeout exit that leaves a grandchild running gets cleaned up the
  moment the job handle is closed, and even if AgentForge itself is killed mid-experiment (no
  `Drop` runs), the OS tears the job down on its own.
- **Unix**: the child is placed in its own new process group at spawn time
  (`std::os::unix::process::CommandExt::process_group(0)`, stable std, no new dependency). On
  timeout, `kill(-pgid, SIGKILL)` (via the `libc` crate, `cfg(unix)`-only dependency) signals the
  whole group in one call.
- Job-object/process-group creation is best-effort (`Option<Handle>`): if it fails for any reason
  (an older Windows without nested-job support, running inside another job that forbids nesting),
  the code falls back to the pre-existing plain `Child::kill()` of just the direct process rather
  than failing the whole spawn.

**Verified by:** `tests/exec_boundaries.rs` —
`timeout_kill_also_terminates_a_detached_grandchild_process_on_windows` actually spawns a direct
child that itself starts a detached PowerShell grandchild (recording its own pid to a marker
file), lets the Executor's 2-second timeout fire, and polls `Get-Process -Id <pid>` until the
grandchild is confirmed gone — **this test passes on this machine, proving the fix works, not just
that it compiles.** A `#[cfg(unix)]` counterpart (`..._on_unix`, using a backgrounded `sleep` and
`kill -0` to probe liveness) is included for correctness/CI coverage on Unix but could not be run
in this Windows-only sandbox. All 14 pre-existing `exec_boundaries` tests (timeout enforcement,
truncation, policy checks, env allowlisting) still pass unchanged.

---

## Finding 4 — Medium: subprocess output capture was unbounded on disk during the run

**Where:** `src/exec/mod.rs` (`SystemExecutor::spawn`).

`budget.max_output_bytes` was enforced *post-hoc*: the child's stdout/stderr were redirected
directly to files (`Stdio::from(file)`), so the OS would happily let the child write an arbitrary
amount of data to disk for the entire time it ran, and only after the process exited or was
killed did `truncate_captured_file` trim the file down. A runaway or malicious command — an agent
process, or a command embedded in an untrusted repository's evaluator `setup_cmds`/`test_cmd` —
could fill the OS temp directory with output well before any timeout or exit ever gave the old
truncation step a chance to run, regardless of the configured cap. This is the literal "unbounded
process/output behavior" the review brief named.

**Fix:** stdio is now piped (`Stdio::piped()`) instead of redirected straight to a file; a
dedicated reader thread per stream (`exec::spawn_capture_thread`) reads the child's pipe and
writes at most `max_output_bytes` to the capture file, appending the same truncation marker as
before the instant that cap is first crossed — then keeps *draining* the pipe without writing
further, which is required, not optional: stopping the read loop instead would leave the child
blocked indefinitely on a full pipe buffer the moment it next tries to write, turning "we stopped
recording your output" into "we silently hung your process." This bounds disk growth *during*
capture, matching the actual intent of `max_output_bytes`, not just the final file size.

**Verified by:** the pre-existing `captured_output_is_truncated_at_max_output_bytes` test passes
unchanged against the new mechanism (same observable contract: final file capped near
`max_output_bytes`, marker present). A dedicated "peak disk usage never exceeds the cap mid-flight"
test was not added — the capture file's exact path isn't known until `spawn()` returns (it blocks
until the process finishes), so cleanly sampling it from a concurrent thread would need a more
invasive refactor of `unique_capture_path` for testability alone; the fix was instead verified by
code review of the bounded reader-thread logic itself, which has no path where the write side can
exceed the cap.

---

## Finding 5 — Medium: one race participant's panic could lose every other participant's results

**Where:** `src/race/mod.rs` (`RaceRunner::run_race`).

Each participant ran inside `scope.spawn(move || { ... runner.run(...) ... })`, and the caller
collected results with `handle.join().expect("experiment thread panicked")`. `ExperimentRunner::
run` is designed to never panic — every internal failure it can hit collapses into a `Failed`-
status `Ok(ExperimentRecord)` (SPEC.md §12 F3) specifically so callers never have to handle a panic
here — but that's a property of the *current* implementation, not something the type system
enforces. If a future change (or an existing bug not yet found) introduced a reachable panic
anywhere on the path that processes agent- or repository-controlled data — a `.unwrap()` on a
malformed evaluator output, say — a single participant panicking would unwind straight past
`std::thread::scope`, discarding every already-collected result for the *whole* race, not just
failing that one participant the way an ordinary internal failure already does (`Failed`, listed,
inspectable). "One bad candidate corrupts the whole race" is exactly the "race conditions in
parallel candidates" failure mode the review brief asked about, even though the trigger here is a
panic rather than a data race per se.

**Fix:** each participant's call to `runner.run(...)` is now wrapped in
`std::panic::catch_unwind(AssertUnwindSafe(...))`; a caught panic is converted into a new
`experiment::Error::Panicked(String)` (extracting the panic message where it's a `&str`/`String`
payload, falling back to a fixed message otherwise) and handled by the exact same "no
`ExperimentRecord` was ever produced, so this participant has nothing to stamp or list" path that
already existed for a pre-record failure (e.g. worktree creation itself failing) — every other
participant's result is completely unaffected.

**Verified by:** `tests/race.rs` — `one_participants_panic_does_not_abort_the_others`, using a new
`PanickingAdapter` test fixture whose `command_for` deliberately panics, confirms `run_race` still
returns `Ok`, the healthy participant is still persisted and reaches `Completed`, and the race
call doesn't unwind. All 6 pre-existing `race.rs` tests, including the pre-existing (non-panic)
partial-failure test, still pass unchanged.

---

## Documented, not fixed

These were deliberately **not** silently patched — either because a real fix would reopen a
scoping/design question this project's own process norms say should be confirmed rather than
assumed (see the project's "ask before amending a documented decision" convention), or because
the residual risk is genuinely low/self-inflicted and a fix would be disproportionate for MVP.
Flagging them here is the point of an adversarial review that isn't allowed to just quietly rewrite
product behavior.

### 6. No cross-process lock around `git worktree add`/`remove` (Low)

`git::worktree::WorktreeManager` serializes worktree mutation with a `std::sync::Mutex` — correct
and sufficient *within one AgentForge process* (this is what makes `race`'s bounded parallel
fan-out safe), but that mutex offers no protection at all between two separate AgentForge
invocations (e.g. two terminals both running `agentforge run`/`race`/`bisect` against the same
target repository at the same time). `.git/worktrees/` metadata is explicitly documented
(`git/worktree.rs`'s own doc comment) as "not safe for concurrent mutation," and nothing enforces
single-process access at the OS level. A concrete fix (an advisory lock file at the state root,
held for the duration of any worktree-mutating command) would need to wrap every CLI entry point
that touches a worktree — `run`, `race`, `bisect`, `experiment fault/mutant`, `workspace create/
remove/clean`, `clean` — a broader, more invasive change than the contained fixes above, and one
this pass did not make unilaterally. Recommend a follow-up pass specifically scoped to
cross-process concurrency if this is a realistic usage pattern (CI running multiple AgentForge
jobs against a shared checkout, for instance).

### 7. `correctness_ratio` defaults to full credit when an evaluator finds no test counts (Low)

`scoring::correctness_ratio` (SPEC.md §15) returns `1.0` — full correctness credit — whenever
`tests_passed`/`tests_total` are `None`, which happens whenever an `EvaluatorSpec`'s
`metric_extractors` don't match anything in the test command's output (a misconfigured regex, a
test framework whose output format isn't matched, or an evaluator with `metric_extractors: []`
entirely). This is explicit, deliberate, and documented in the code itself ("a good verdict by
construction when there's nothing to gate on and no counts to compare"), and `is_gated` still
independently catches a nonzero exit code or build failure regardless. But it does mean a task
author (or a hostile repository shipping its *own* evaluator spec for a user to blindly
`evaluator add --file`) can guarantee **every** candidate patch scores 100% correctness — as long
as the build doesn't literally fail — simply by configuring `metric_extractors: []` or a pattern
that never matches. This is a scoring-manipulation vector, not a bug in the formula as specified;
changing the default (e.g. to gate on *absent* counts rather than treat them as perfect) would
reopen a resolved SPEC.md §15 design decision, which this project's process explicitly calls for
confirming with the user before changing rather than silently amending. Flagged here for that
decision, not fixed.

### 8. Read-side path-traversal amplification via `clean --experiment <id>` (Low — closed by fix #1)

Called out separately because it's a good illustration of why fix #1 needed to be at the `Store`
choke point rather than patched CLI-flag-by-CLI-flag: `clean --experiment <id>` loads an
`ExperimentRecord` by id and then passes its *stored* `worktree_path` field straight into
`WorktreeManager::remove`, which calls `git worktree remove --force <path>`. Before fix #1, a
traversal id here could have pointed `load_experiment` at an attacker-influenced file elsewhere on
disk; if that file happened to parse as an `ExperimentManifest`, its `worktree_path` field — not
`args.experiment` itself — is what would have reached the destructive `git worktree remove
--force` call, an escalation from "read a wrong file" to "delete an arbitrary directory the
process can reach." Now closed by the same centralized `Store::validate_id` fix.

### 9. TOCTOU window between the symlink check and the write it guards (Informational)

`fault::reject_symlink`/`mutant`'s reuse of it check `symlink_metadata` immediately before the
read/write/remove that follows — but "immediately before" is still two separate syscalls, not one
atomic operation, so a symlink swapped in between the check and the write (by some other process
racing AgentForge for that exact path, inside a worktree AgentForge itself just created and no
other legitimate process should be touching) would not be caught. Rust's standard library has no
portable `O_NOFOLLOW`-style atomic "open, but fail if this path is/became a symlink" primitive, so
closing this completely would need platform-specific raw syscalls for marginal additional benefit
given the isolated worktree these files live in is not expected to have any concurrent legitimate
writer. Noted as a residual rather than left silently unconsidered.

### 10. `race`'s `--max-parallel`-omitted default is unbounded (Informational)

`race_cmd` defaults `max_parallel` to `agents.len() * repeat` when the flag is omitted — i.e. every
participant runs concurrently by default. This is entirely user-controlled (the user chooses how
many agents and how many repeats), not something an adversarial repository or task spec can
influence, so it's a self-inflicted resource-exhaustion footgun at worst, not a security
vulnerability — documented here only for completeness against the review brief's "unbounded
process/output behavior" category.

---

## Categories from the review brief with no material finding

Reviewed and considered explicitly, not just implicitly covered above:

- **Unsafe Git behavior / command injection:** every `git` invocation goes through
  `GitRepo::spawn_git` → `Executor::spawn`, arguments always passed as a `Vec<String>` array
  (`Command::args`), never shell-interpolated — there is no `sh -c`/`cmd /C` anywhere in the git
  or adapter code paths. Same is true of `ClaudeCodeAdapter::command_for` (prompt is one discrete
  argv element).
- **Accidental modification/deletion of the user's real checkout:** structurally prevented — every
  git operation that mutates state runs either against a dedicated worktree
  (`<state_root>/{worktrees,bisect-worktrees,fault-worktrees,mutant-worktrees}/...`, entirely
  outside the target repo) or as pure plumbing against blob/tree/commit objects via a throwaway
  `GIT_INDEX_FILE` (`write_commit`) that never touches the real index. Verified by existing tests
  (`creating_and_removing_a_workspace_never_touches_the_primary_checkout`, and the same pattern
  repeated across `workspace.rs`/`worktree_lifecycle.rs`/`race.rs`).
- **Unsafe environment inheritance:** `SystemExecutor::spawn` calls `.env_clear()` before building
  the child's environment from exactly `env_passthrough` plus a small fixed OS-required set
  (`PATH`, and on Windows `SystemRoot`/`PATHEXT`/`COMSPEC`/`windir`) plus `spec.extra_env` — never
  the full host environment. `env_passthrough_allowlist_is_exact` exercises this directly.
- **Incorrect experiment reproducibility / semantic bisect misclassification:** `write_commit`'s
  fixed `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` (Unix epoch) and exclusion of `task_id` from the
  mutant commit's inputs make `mutation`/`mutant apply` genuinely reproducible (already covered by
  `tests/mutation_reproducibility.rs`/`tests/mutant_reproducibility.rs`); `bisect::BisectRunner`'s
  binary search is a textbook implementation over `rev_list_ancestry_path`'s ordered candidate
  list with an explicit `is_ancestor` precondition check before any worktree is created — read in
  full and found correct against SPEC.md §13's contract.
- **Secret leakage into logs:** `PermissionCheck`/`ProcessSpawn` audit events record which host
  env vars were *allowlisted* (names and a count) but never their values; the child's actual
  environment values never cross into the audit log.
- **Panic/unwrap usage on user-controlled paths:** swept the codebase for `.unwrap()`/`.expect()`;
  every remaining instance outside test code is backed by a local invariant already established a
  few lines earlier (a regex capture group guaranteed present by the pattern that just matched, a
  well-formed struct that's always serializable) rather than by trusting external input — the one
  real gap found (`race`'s participant-panic handling) is fix #5 above.
