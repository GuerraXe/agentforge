# Contributing

## Setup

Requires a recent stable Rust toolchain (2021 edition) — no other runtime dependency.

```bash
git clone <this-repo>
cd agentforge
cargo build
```

Windows, macOS, and Linux are all supported; the platform-specific pieces (`exec::tree`'s
timeout process-tree kill) are `cfg`-gated per platform and covered by the same test suite either
way.

## Before opening a PR

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

All three must pass clean — zero clippy warnings, zero test failures. [`.github/workflows/ci.yml`](.github/workflows/ci.yml)
runs the same three (plus `cargo build`) on every push/PR, matrixed over `ubuntu-latest` and
`windows-latest` — run them locally first rather than relying on CI to catch it. `cargo run
--example demo` is also worth running for any change that touches CLI output shape, the adapter,
or scoring — it exercises the whole documented command surface end to end with no external
dependency.

Write tests before implementation where practical, and prefer a test that exercises the real code
path (a CLI integration test driving the compiled binary, a real temp git repo) over a unit test
of an isolated helper — that's the standard the existing suite holds itself to, and what
[docs/ADVERSARIAL_REVIEW.md](docs/ADVERSARIAL_REVIEW.md)'s fixes all did.

## Where things live

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the full module map and the reasoning
behind it. The one rule that generates the whole layout: **`domain` holds nouns (data, no I/O);
every other module holds exactly one verb, operating on those nouns.** In practice:

- **A new agent adapter** implements the `AgentAdapter` trait (`src/adapter/mod.rs`) — see
  `src/adapter/claude_code.rs` for the reference shape. `command_for` returns a `ProcessSpec`
  *value* only; it must never spawn a process, block, or touch the audit sink itself — that
  split is structural, not a convention (see [docs/ARCHITECTURE.md §7](docs/ARCHITECTURE.md)
  for why). Register the new adapter's name in `adapter::resolve` (`src/adapter/mod.rs`).
- **A new mutation operator** (for `mutation`/`mutant`'s shared source-mutation scanning) is a
  new variant in `mutation`'s operator enum plus its regex — see `src/mutation/mod.rs`.
- **A new fault kind** (for `experiment fault`) is a new `FaultKind` variant in
  `src/domain/fault.rs` plus its handling in `src/fault/mod.rs`.
- **Anything that spawns a process** goes through `exec::Executor` — no module except `exec`
  itself calls `std::process::Command`. This is enforced by the dependency graph, not just
  convention: nothing outside `exec` has a way to reach the OS process API.
- **Anything that touches git** goes through `git::GitRepo`/`git::worktree::WorktreeManager` —
  the one safe git abstraction every other module composes rather than reimplements.
- **A new CLI command** is wiring, not logic — it should compose existing library-level
  primitives (`cli` is deliberately the one module allowed to depend on everything else) rather
  than growing new behavior of its own. See `src/cli/mod.rs`'s doc comment and
  [docs/ARCHITECTURE.md §14](docs/ARCHITECTURE.md).

## Proposing a design change

[docs/SPEC.md](docs/SPEC.md) is the product contract and [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
is the Rust-level design derived from it — both carry a dated history of deliberate decisions
(non-goals, cut features, resolved review findings), not just the current state. If a change
you're proposing would reopen one of those decisions rather than fill an unimplemented gap, open
an issue describing the tension before sending a PR — several of AgentForge's own boundaries
(what `PermissionPolicy` restricts vs. what it doesn't, why `fault`/`mutation`/`mutant` are three
sibling mechanisms instead of one) exist because that tension was worked through explicitly
rather than silently reinterpreted. If your change lands, update the relevant `SPEC.md`/
`ARCHITECTURE.md` section with an explicit note — don't leave the docs silently stale relative to
the code.

## Reporting a security issue

See [docs/SECURITY.md](docs/SECURITY.md) for exactly what AgentForge does and doesn't guarantee.
If you find a case where something documented as **Enforced** or **Application-level** isn't —
that's a real bug, not a known limitation. Open an issue with a concrete, minimal reproduction
(the same standard [docs/ADVERSARIAL_REVIEW.md](docs/ADVERSARIAL_REVIEW.md) held its own findings
to). This is a portfolio project with no production deployment and no bug bounty, so there's no
formal disclosure embargo process — a public issue with a clear repro is the right way to report,
the same as any other bug.

## Commit / PR conventions

- Keep commits focused; a PR that fixes a bug and refactors an unrelated area is two PRs.
- Describe *why*, not just *what* — the diff already shows what changed.
- Run the full gate (`fmt --check`, `clippy -D warnings`, `test`) before pushing, not just before
  the final version — a red CI-equivalent run partway through a PR is normal; leaving one at the
  end isn't.
