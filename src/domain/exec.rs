//! `ProcessSpec`, `ExecutionBudget`, `ProcessOutcome`, `EnforcementLevel` — SPEC.md §4, §8.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EnforcementLevel {
    Enforced,
    RequestedOnly,
    Unsupported,
}

/// Adapter-built. Deliberately has no `cwd` field — the Executor sets cwd, always, to the
/// caller-supplied worktree path. It is not adapter-suppliable — SPEC.md §20 (U2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Adapter-supplied literal additions (e.g. a model flag via env), merged on top of the
    /// Executor-filtered `env_passthrough` set.
    pub extra_env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExecutionBudget {
    pub timeout_secs: u64,
    pub max_output_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessOutcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
}

/// The subset of `PermissionPolicy` the Executor itself checks before spawning — separated out
/// from `env_passthrough`/`ExecutionBudget` (which stay per-call parameters, since a caller like
/// `evaluator::Evaluator` derives its timeout from `EvaluatorSpec.timeout_secs`, not from any
/// agent's `PermissionPolicy`) because these three fields are genuinely orthogonal: they gate
/// *whether* a spawn happens at all, not how the resulting child is shaped.
///
/// Empty `allowed_programs`/`allowed_roots` mean "no restriction configured" (unrestricted is
/// the default, matching a plain allow/deny-list convention) — this is deliberately not the same
/// fail-closed convention `env_passthrough` uses (empty there means "pass nothing"), because an
/// unconfigured command policy silently blocking every program or every cwd would break every
/// existing caller that has no opinion on this dimension. `denied_programs` always wins over
/// `allowed_programs` for a program listed in both.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandPolicy {
    pub allowed_programs: Vec<String>,
    pub denied_programs: Vec<String>,
    pub allowed_roots: Vec<PathBuf>,
}

impl CommandPolicy {
    /// No program or filesystem-root restriction — the default for callers that don't configure
    /// this dimension of the policy.
    pub fn unrestricted() -> Self {
        Self::default()
    }
}
