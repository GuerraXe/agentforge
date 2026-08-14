# Security Model

AgentForge runs code an autonomous agent wrote, against a real repository, on your machine. This
document says precisely what protects you when it does that — and, just as importantly, what
doesn't. Every claim below is either a specific, tested mechanism in `src/`, or explicitly marked
as not provided. Nothing here is aspirational.

**The one-sentence version:** AgentForge isolates *where* an agent's process runs and *what
AgentForge itself* does to your repository. It does not sandbox the agent process itself — a
malicious (not just buggy) agent binary can still do anything your OS user account can do. Don't
point AgentForge at an untrusted agent binary, task spec, or target repository and rely on it
alone for containment.

If you find a real gap in what's claimed **Enforced** below, please open an issue describing the
concrete exploit — see [CONTRIBUTING.md](../CONTRIBUTING.md).

---

## How to read this document

Every protection AgentForge provides falls into one of four tiers. Mixing these up is exactly how
a security document becomes misleading, so they're kept separate everywhere in this project —
including in `agentforge policy show`'s own output, which tags every policy field with one of the
first three labels below at the point you're about to rely on it.

| Tier | Meaning |
|---|---|
| **Enforced** | AgentForge's own code guarantees this, structurally, on every supported platform. Verified by an automated test that exercises the real code path, not just a unit test of a helper. |
| **Application-level restriction** | AgentForge checks and refuses before acting (e.g. rejecting a path-traversal id, refusing to spawn a denied program). Real protection against careless input and the failure modes AgentForge itself can cause — not protection against a process that's already running and actively hostile. |
| **Platform-dependent** | Real on the platforms AgentForge implements it for, using OS facilities (Job Objects, process groups) — but not a portable guarantee, and not equivalent to a sandbox. |
| **Not provided / unsupported** | No enforcement exists. If a field or setting *looks* like it should provide this, its own field description says so, and this document says so too. |

---

## Tier 1 — Enforced

These hold regardless of policy configuration, on every supported platform, and are covered by
regression tests.

- **Working directory of every spawned process.** The `Executor` sets it, always, to the
  relevant isolated worktree. Never adapter-suppliable, never the caller's real checkout.
- **Environment variables exposed to a spawned process.** Built from exactly
  `policy.env_passthrough` plus the adapter's own literal additions — never the full host
  environment.
- **Wall-clock timeout on the directly-spawned process** (agent process and every evaluator
  `setup_cmds`/`test_cmd` step alike). The `Executor` owns the process handle from spawn to
  exit/kill.
- **Captured output size cap.** Stdout/stderr are capped at `max_output_bytes` *during* capture
  (a bounded reader thread per stream, not a post-hoc truncation) — fixed 2026-08-13 after an
  adversarial review found a runaway command could previously fill the OS temp directory before
  the old post-exit truncation step ever ran (`ADVERSARIAL_REVIEW.md` finding 4).
- **State root lives outside the target repository's directory tree.** Verified structurally —
  `state_root` never begins with the repo's own canonical path. An agent operating inside its
  isolated worktree cannot reach the *source* repository's working tree or `.git` through it.
- **The caller's real checkout is never mutated.** No AgentForge command runs a mutating git
  operation against the repository you pointed it at — every mutation happens in a disposable
  worktree under the state root. No `push`, `push --force`, branch deletion, or history rewrite
  runs against your branches or remotes; the only refs AgentForge writes are its own
  `refs/agentforge/...` namespace.
- **No shell interpolation anywhere in the process-spawn path.** Every command (git, the agent
  process, evaluator steps) is built as an argv array (`Command::args`) and spawned directly —
  there is no `sh -c`/`cmd /C` wrapping a string. A prompt containing shell metacharacters is
  passed as one literal argv element, not interpreted.
- **Ids that become filesystem paths are validated before use.** Every `Store`-persisted
  collection (tasks, evaluators, policies, faults, mutants, experiments, races, bisects) and
  every workspace/fault/mutant id is restricted to `[A-Za-z0-9_-]` before it's ever joined onto a
  path. This closed a critical, previously-real gap — see "What was found and fixed" below.
- **Fault/mutant injection never follows a symlink out of its own worktree.** A hostile target
  repository that tracks a path as a symlink pointing outside the isolated worktree is rejected
  (`symlink_metadata` checked, never followed) before any read/write/remove.

## Tier 2 — Application-level restrictions (policy-configurable)

Real, tested, and on by configuration — but they're AgentForge checking its own actions before
taking them, not OS-level containment of a process that's already running.

- **Command program allowlist/denylist.** `PermissionPolicy.allowed_programs`/`denied_programs`
  are checked by the Executor before every single spawn (the agent's top-level command, every
  evaluator step, and git) and fail closed — a denied program is never spawned, with zero side
  effects. Denylist is checked first, so a program on both lists is refused. This governs *which
  program the Executor is asked to run*, not what that program does internally once running —
  see "Mediated internal tool calls," below, for the boundary this does **not** cover.
- **Spawn cwd confined to configured allowed roots.** `PermissionPolicy.allowed_roots`, checked
  before every spawn. Defense-in-depth on top of cwd always being the assigned worktree — this
  guards against AgentForge's own misconfiguration, not agent containment.
- **`workspace exec`'s own allow/deny/root flags** (`--allow-program`, `--deny-program`,
  `--allowed-root`) are the same Executor-level checks, exposed directly for ad hoc use.

## Tier 3 — Platform-dependent (real, not portable, not a sandbox)

- **Timeout kill reaches a detached grandchild process, not just the direct child.** Fixed
  2026-08-13 after an adversarial review found the original implementation only ever killed the
  one directly-spawned process, so a detached grandchild survived a timeout indefinitely on every
  platform (`ADVERSARIAL_REVIEW.md` finding 3). Now: on Windows, the child is assigned to a Job
  Object (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, terminated on timeout); on Unix, the child is
  placed in its own process group (`kill(-pgid, SIGKILL)` on timeout). This is real on both
  platforms AgentForge supports — but it is not a guarantee for every possible platform, and a
  process that deliberately breaks away from its job/group (where the OS allows that) is not
  covered.
- **Network access, filesystem-write confinement beyond cwd, memory/CPU limits** are requested
  via `AdapterCapabilities` where an adapter supports asking for them, and reported in
  `agentforge policy show` — but AgentForge does not independently verify an adapter honored the
  request. Whether these are actually enforced depends entirely on the adapter and the platform,
  not on AgentForge.

## Tier 4 — Not provided (do not rely on these)

Say this plainly, because it's the part a security document is tempted to blur:

- **No process-level sandbox.** No containers, no Windows Job Object beyond the timeout-kill use
  above, no seccomp/AppArmor, no namespace isolation. AgentForge does not confine what the agent
  process — or any process it spawns — is allowed to do on your machine beyond the mechanisms
  listed above.
- **No mediation of an agent's internal tool calls.** The Executor governs what program
  AgentForge itself asks to run (Tier 2, above). It has no visibility into, and does not restrict,
  what a spawned agent does *inside its own process* — shell commands Claude Code invokes as part
  of its own tool use, for example, are invisible to AgentForge entirely. An earlier design
  (`CommandPolicy`) tried to mediate this and was cut because it had zero real enforcement behind
  it; the roadmap item is a genuinely mediating adapter, not a resurrection of that approach.
- **No memory/CPU resource limits.** `PermissionPolicy.max_memory_bytes` is carried and reported
  in every policy snapshot, but never enforced. A real cap needs the same OS-level facilities
  (Job Objects/cgroups) that process-tree sandboxing needs, and is out of scope for the same
  reason.
- **No network firewalling.** `deny_network` is relayed to the adapter as a request only; nothing
  in AgentForge blocks or observes actual network traffic.
- **No protection against a malicious (not just buggy) agent.** Every mechanism above assumes the
  agent under test is imperfect, not adversarial. AgentForge's isolation raises the cost of an
  agent's *mistake* damaging your real checkout; it does not stop a process that is actively
  trying to escape.
- **No filesystem write confinement beyond the starting cwd.** The Executor sets where a process
  *starts*; it cannot stop that process from writing anywhere else the OS permits (traversal,
  absolute paths, symlinks it creates itself after starting).
- **No cross-process lock on worktree operations.** Two separate `agentforge` invocations
  (e.g. two terminals, or a CI matrix) running `run`/`race`/`bisect`/`experiment fault`/`mutant`
  against the *same* target repository at the same time can race on `.git/worktrees/` metadata —
  in-process concurrency (e.g. `race`'s own parallel fan-out) is safe; cross-process concurrency
  against a shared repo is not. Run concurrent AgentForge invocations against separate clones, or
  serialize them yourself, until this is addressed.

---

## What was found and fixed

A full adversarial security and correctness review (posture: hostile reviewer, assuming a
malicious or careless user, target repository, or spec file) ran across the whole codebase on
2026-08-13. It found and fixed 5 independently-exploitable issues, each with a regression test
that exercises the real vulnerable code path — full detail and reproduction steps in
[`ADVERSARIAL_REVIEW.md`](ADVERSARIAL_REVIEW.md):

1. **Critical — path traversal via unvalidated ids in `store::Store`.** A task/evaluator/policy
   TOML file (or a bare CLI id argument) with an id like `../../../../somewhere` could write or
   read arbitrary files outside `.agentforge/`; chained through `clean --experiment <id>`, the
   read side escalated into an arbitrary `git worktree remove --force`. Fixed with a single
   `validate_id` choke point inside `Store` itself, covering every current and future caller.
2. **High — malicious repository symlinks escaped the isolated fault/mutant worktree.** A hostile
   target repository tracking a path as a symlink to something outside the worktree (e.g. a dotfile
   in your home directory) would have that target overwritten. Fixed with a shared symlink check
   before any write.
3. **High — the documented timeout process-tree-kill guarantee wasn't actually implemented.** See
   Tier 3, above, for the fix.
4. **Medium — subprocess output capture was unbounded on disk during a run.** See Tier 1, above,
   for the fix.
5. **Medium — a panic in one race participant could lose every other participant's already-collected
   result.** Fixed with a per-participant panic guard that converts a caught panic into a normal
   `Failed` result for that one participant, rather than losing the whole race.

A few lower-severity items were reviewed and **deliberately left undone**, documented rather than
silently ignored — see `ADVERSARIAL_REVIEW.md`'s "Documented, not fixed" section for the reasoning
on each: no cross-process worktree lock (Tier 4, above); `scoring::correctness_ratio` defaulting to
full credit when an evaluator's `metric_extractors` find no test counts (a real scoring-manipulation
vector for a hostile/misconfigured evaluator spec, left as-is pending a product decision rather than
silently changed); a TOCTOU window between the symlink check and the write it guards (no portable
atomic primitive exists in Rust's standard library for this); `race`'s unbounded default
parallelism (self-inflicted only — entirely under the caller's own control).

## Practical guidance

- **Do** run AgentForge against repositories and agent configurations you already trust to some
  degree — it raises the cost of an agent mistake, it doesn't replace trusting the agent.
- **Do** treat `agentforge policy show <name>` as the source of truth for what a given policy
  actually restricts, right before you rely on it — the tags there are kept in sync with this
  document by test.
- **Don't** run an untrusted agent binary, or point AgentForge at a repository you don't trust to
  contain malicious build/test scripts, expecting the isolation described here to contain it.
- **Don't** run concurrent AgentForge invocations against the same target repository from separate
  processes (Tier 4's cross-process lock gap).
- **Report a gap:** if you find a case where something claimed **Enforced** or **Application-level**
  above isn't, that's a real bug — please open an issue with a concrete reproduction (see
  [CONTRIBUTING.md](../CONTRIBUTING.md)).
