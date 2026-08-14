# AgentForge

AgentForge is a command-line tool for **safely running, testing, comparing, and evaluating
autonomous coding agents** — Claude Code today, with a trait-based adapter interface for others
later. Point it at a task and a git repository; it runs the agent in an isolated worktree,
measures what actually changed against a deterministic test oracle, and produces a transparent,
recomputable score. No server, no web UI, no dashboard — a binary and on-disk JSON/TOML.

## The problem

Coding agents are good enough now that the interesting question has shifted from "can it write
code" to "did it, this time, on this task, without me watching it happen." Running an agent
directly against your working checkout answers none of that safely: you can't easily undo what it
did, you have no independent record of what actually ran, and "it says the tests pass" is the
agent's own unverified claim. Comparing two agents or two models on the same task, or finding
*which* commit an agent's fix actually depends on, means building all of that isolation and
measurement machinery yourself, every time.

## Core design principle

**Agents perform work. Deterministic software controls, isolates, tests, and judges it.**

That split is not a slogan — it's enforced structurally, module by module:

| Component | Owns | Never does |
|---|---|---|
| **Worktree** | A disposable, isolated Git checkout per unit of work, outside your repo's own directory tree | Persist agent state anywhere your real checkout can see |
| **Executor** | Spawning, timing out, and capturing output for *every* subprocess AgentForge runs — agent and evaluator alike | Let an adapter spawn a process itself |
| **Adapter** | Translating a task + model into a command to run | Anything about *how* that command executes — no cwd, no timeout, no audit access |
| **Evaluator** | Deciding, deterministically, whether a patch is good | Trust the agent's own account of what it did |
| **Scoring** | Turning a judgment into a transparent, recomputable number | Let gamed or partial correctness outscore genuine correctness |
| **Audit log** | An independent record of what the Executor actually observed | Get written to by adapter code |

The agent never has a code path to the audit trail, the scoring formula, or its own verdict. It
writes a patch; everything else about whether that patch was good happens without it.

## Architecture at a glance

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
```

Everything above `experiment` is that same primitive composed, never reimplemented: `race` is N
`run`s plus a deterministic ranking; `bisect` is repeated `evaluate()` calls plus a binary search;
`mutation`/`mutant`/`fault` reuse the same worktree and git plumbing to build test fixtures. Full
module-by-module detail, dependency rationale, and the dated implementation record live in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## What's actually implemented

Every command below is real, tested, and wired end to end — **293/293 tests passing**, `cargo
clippy --all-targets --all-features -- -D warnings` and `cargo fmt --check` both clean, plus a
from-scratch adversarial security review with every finding fixed (see
[docs/SECURITY.md](docs/SECURITY.md)):

```
agentforge init
agentforge workspace   {create, list, show, exec, remove, clean}
agentforge evaluator   {add, list, show}
agentforge task        {add, list, show}
agentforge experiment  {fault, mutation, mutant}   # repository-state test fixtures
agentforge run
agentforge race
agentforge bisect
agentforge verify
agentforge report      {show, score, log}
agentforge policy      {add, list, show, validate}
agentforge clean
```

## Install

**Prerequisites:** a recent stable Rust toolchain (2021 edition — install via
[rustup](https://rustup.rs) if you don't have one) and `git`. Nothing else — no database, no
Docker, no separate runtime to stand up.

AgentForge isn't published to crates.io (`publish = false` — see [Limitations](#limitations)); the
only way to get it today is building from source, which takes under a minute:

```
git clone https://github.com/GuerraXe/agentforge.git
cd agentforge
cargo build --release          # binary lands at target/release/agentforge (.exe on Windows)
./target/release/agentforge --help
```

Optional — put the binary on your `PATH` so plain `agentforge ...` works from anywhere, instead of
typing `./target/release/agentforge` every time:

```
# Linux / macOS
cp target/release/agentforge ~/.local/bin/       # or any directory already on your PATH

# Windows (PowerShell)
Copy-Item target\release\agentforge.exe $env:USERPROFILE\bin\   # any directory already on PATH
```

To run a real task against Claude Code specifically (as opposed to the zero-paid-API demo below),
separately install the `claude` CLI and reference it as `--agent claude-code[:model]`;
`ClaudeCodeAdapter` shells out to whatever `AGENTFORGE_CLAUDE_EXECUTABLE` points at, defaulting to
`claude` on your `PATH`. This is entirely optional — everything else on this page, including the
full CLI surface, works with no API key at all.

## A working example

Register an evaluator (the deterministic test oracle) and a task, then race three candidate
patches against it:

```
$ agentforge run --task fix-discount-bug --agent claude-code:goodfix --policy demo-policy
Experiment      20260814T013725Z-2bbfd9
Status          Completed
Tests           2/2
Score           99          Rating   Excellent          Gated   no

$ agentforge race --task fix-discount-bug \
    --agents claude-code:goodfix,claude-code:partialfix,claude-code:nofix --max-parallel 3
Candidate                  Tests  Time  Patch  Score  Rating
#0 claude-code:goodfix     2/2    0s    4L     99     Excellent
#1 claude-code:partialfix  1/2    0s    2L     60     Fair
#2 claude-code:nofix       0/2    0s    0L     5      Fail
```

The score is a transparent 80/10/10 blend of correctness, efficiency, and parsimony —
`report show <id> --verbose` breaks down every component, weight, and threshold that produced it,
never just a bare number.

**Next step:** [docs/USAGE.md](docs/USAGE.md) is the full setup-and-usage tutorial — `init` a real
repository, register your own evaluator/task/policy, and run your first agent against it, one
step at a time.

## Local demo — zero paid API

The entire CLI surface above (init through cleanup, including fault injection, mutation testing,
racing, and semantic bisect) runs against a small seeded "billing service" fixture repo with no
API key and no network call at all:

```
cargo run --example demo        # narrated walkthrough, ~10s
cargo test --test demo_e2e      # the same scenario, asserted
```

This isn't a separate mock mode bolted on for the demo — it's the real `agentforge` binary and
the real `ClaudeCodeAdapter`, pointed at a tiny deterministic stand-in binary
(`src/bin/mock_claude.rs`) via `ClaudeCodeAdapter`'s own `AGENTFORGE_CLAUDE_EXECUTABLE`
environment override, the same knob a CI pipeline would use to swap in a stub. `adapter::resolve`
itself is untouched.

## Development

The same prerequisites as [Install](#install), plus these before opening a PR (see
[CONTRIBUTING.md](CONTRIBUTING.md) for the full checklist):

```
cargo test                     # 293 tests
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

## Limitations

Said plainly, not buried — see [docs/SECURITY.md](docs/SECURITY.md) for the full, tiered
breakdown of exactly what's enforced versus not:

- **No process sandbox.** AgentForge isolates *where* an agent's process runs (a disposable
  worktree outside your repo) and *what AgentForge itself* does to your repository — it does not
  contain a malicious agent binary. Don't run an untrusted agent against a repo based on this
  isolation alone.
- **No mediation of an agent's internal tool calls** — only the one top-level process AgentForge
  spawns is visible to it.
- **No memory/CPU limits or network firewalling** — represented in policy configuration,
  reported honestly as unenforced, not actually enforced.
- **A detached grandchild process is only reliably killed on timeout on Windows and Unix** (Job
  Objects / process groups respectively) — real, but not a universal guarantee.
- **One adapter today** (Claude Code). The trait exists for more; none else are implemented.
- **No cross-process lock** — running two `agentforge` invocations against the same target repo
  concurrently can race on worktree metadata.

## Roadmap

Not started, listed honestly as not-yet rather than implied:

- Additional agent adapters (Codex CLI, Aider, etc.) behind the existing `AgentAdapter` trait.
- A mediating adapter capable of restricting an agent's *internal* tool calls, not just the
  top-level process AgentForge spawns.
- AST-aware mutation operators (today's mutators are language-agnostic text-pattern regexes).
- A cross-process advisory lock around worktree mutation, for safe concurrent CI usage against a
  shared repository.
- Token/cost tracking and adapter-specific telemetry.

## Documentation

- [docs/USAGE.md](docs/USAGE.md) — full setup-and-usage tutorial and command reference, with
  real, working examples.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — module design, dependency graph, and the dated
  record of what shipped when and why.
- [docs/SECURITY.md](docs/SECURITY.md) — exactly what's enforced, application-level,
  platform-dependent, or not provided at all.
- [CONTRIBUTING.md](CONTRIBUTING.md) — how to build, test, and propose a change.

## License

MIT — see [LICENSE](LICENSE).
