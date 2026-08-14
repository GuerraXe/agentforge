//! `AgentConfig` / `PermissionPolicy` — SPEC.md §4.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::domain::exec::{CommandPolicy, EnforcementLevel};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// e.g. "claude-code"
    pub adapter: String,
    pub model: Option<String>,
    pub extra_args: Vec<String>,
    /// Name of a `PermissionPolicy`.
    pub policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionPolicy {
    pub name: String,
    /// Adapter-requested, best-effort — SPEC.md §16.
    pub extra_readonly_paths: Vec<PathBuf>,
    /// Adapter-requested, best-effort — SPEC.md §16. Default true.
    pub deny_network: bool,
    /// Executor-enforced allowlist of host env vars exposed to a spawned process —
    /// SPEC.md §20 (X2).
    pub env_passthrough: Vec<String>,
    /// Executor-enforced; must be > 0 — SPEC.md §20 (S4).
    pub max_wall_time_secs: u64,
    /// Executor-enforced; must be > 0 — SPEC.md §20 (S4).
    pub max_output_bytes: u64,
    /// Executor-enforced allowlist of programs this policy's spawns may run. Empty means no
    /// restriction — see `CommandPolicy`'s doc for why empty here is permissive rather than the
    /// fail-closed convention `env_passthrough` uses.
    #[serde(default)]
    pub allowed_programs: Vec<String>,
    /// Executor-enforced denylist, checked before `allowed_programs` — a program listed in both
    /// is refused.
    #[serde(default)]
    pub denied_programs: Vec<String>,
    /// Executor-enforced: a spawn's cwd must resolve inside one of these roots. Empty means no
    /// restriction. Defense-in-depth on top of the caller's own path validation (e.g.
    /// `workspace::WorkspaceManager`'s own state-root confinement) — cwd is always the relevant
    /// worktree already, never adapter-suppliable, so this mainly guards against AgentForge's
    /// own misconfiguration, not an adversarial agent.
    #[serde(default)]
    pub allowed_roots: Vec<PathBuf>,
    /// Representation only — no MVP platform enforces this (SPEC.md §3.1's Job Objects/cgroups
    /// non-goal). Always reports `EnforcementLevel::Unsupported` via `enforcement_report()`;
    /// carried here so a policy author's intent is recorded even though nothing acts on it yet.
    #[serde(default)]
    pub max_memory_bytes: Option<u64>,
}

/// One row of `enforcement_report()` — the structural, testable form of SPEC.md §16's security
/// table: which fields the Executor genuinely checks/applies (`Enforced`), which are relayed to
/// an adapter as a best-effort request only (`RequestedOnly`), and which are pure representation
/// with no backing enforcement in MVP (`Unsupported`). Nothing outside this function may claim a
/// stronger guarantee level than what's returned here for a given field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyFieldEnforcement {
    pub field: &'static str,
    pub level: EnforcementLevel,
    pub detail: &'static str,
}

impl PermissionPolicy {
    /// A generous, uniform, built-in-default policy for callers that need one before any named
    /// policy has been persisted (`Store::load_policy`'s absence is not a hard blocker) —
    /// originally `race::default_policy()`'s own private literal, factored out here so `run`'s
    /// CLI-level `--policy`-omitted fallback shares the exact same shape rather than maintaining
    /// a second, easily-drifting copy.
    pub fn generous_default(name: &str) -> Self {
        PermissionPolicy {
            name: name.to_string(),
            extra_readonly_paths: vec![],
            deny_network: true,
            env_passthrough: vec![],
            max_wall_time_secs: 3600,
            max_output_bytes: 10_000_000,
            allowed_programs: vec![],
            denied_programs: vec![],
            allowed_roots: vec![],
            max_memory_bytes: None,
        }
    }

    /// Pure, in-memory field validation — no I/O. SPEC.md §17: `policy validate` rejects
    /// `max_wall_time_secs == 0` or `max_output_bytes == 0`, naming the failing field.
    pub fn validate_fields(&self) -> Result<(), String> {
        if self.max_wall_time_secs == 0 {
            return Err("policy.max_wall_time_secs must be > 0".to_string());
        }
        if self.max_output_bytes == 0 {
            return Err("policy.max_output_bytes must be > 0".to_string());
        }
        Ok(())
    }

    /// The subset of this policy the Executor itself checks before spawning — SPEC.md §16.
    pub fn command_policy(&self) -> CommandPolicy {
        CommandPolicy {
            allowed_programs: self.allowed_programs.clone(),
            denied_programs: self.denied_programs.clone(),
            allowed_roots: self.allowed_roots.clone(),
        }
    }

    /// Every field's enforcement level and why — the live, testable backing for `policy show`
    /// (SPEC.md §6, §17) and for keeping this doc-comment-and-code pairing from drifting the way
    /// a prose-only table could (SPEC.md §20, M2).
    pub fn enforcement_report(&self) -> Vec<PolicyFieldEnforcement> {
        vec![
            PolicyFieldEnforcement {
                field: "env_passthrough",
                level: EnforcementLevel::Enforced,
                detail: "the Executor builds the child's environment from exactly this \
                         allowlist, never the full host environment",
            },
            PolicyFieldEnforcement {
                field: "max_wall_time_secs",
                level: EnforcementLevel::Enforced,
                detail: "the Executor kills the directly-spawned process if it outlives this \
                         budget",
            },
            PolicyFieldEnforcement {
                field: "max_output_bytes",
                level: EnforcementLevel::Enforced,
                detail: "the Executor truncates captured stdout/stderr at this cap",
            },
            PolicyFieldEnforcement {
                field: "allowed_programs",
                level: EnforcementLevel::Enforced,
                detail: "the Executor refuses to spawn a program absent from a non-empty \
                         allowlist",
            },
            PolicyFieldEnforcement {
                field: "denied_programs",
                level: EnforcementLevel::Enforced,
                detail: "the Executor refuses to spawn any program on this list, checked before \
                         allowed_programs",
            },
            PolicyFieldEnforcement {
                field: "allowed_roots",
                level: EnforcementLevel::Enforced,
                detail: "the Executor refuses to spawn into a cwd outside every configured root",
            },
            PolicyFieldEnforcement {
                field: "deny_network",
                level: EnforcementLevel::RequestedOnly,
                detail: "relayed to the adapter as a request only; no firewall/network-namespace \
                         enforcement exists in MVP (SPEC.md §3.1)",
            },
            PolicyFieldEnforcement {
                field: "extra_readonly_paths",
                level: EnforcementLevel::RequestedOnly,
                detail: "relayed to the adapter as a request only; AgentForge does not \
                         independently verify it was honored",
            },
            PolicyFieldEnforcement {
                field: "max_memory_bytes",
                level: EnforcementLevel::Unsupported,
                detail: "representation only — memory/CPU caps need OS-level facilities (Job \
                         Objects/cgroups) that are explicitly out of scope for MVP (SPEC.md §3.1)",
            },
        ]
    }
}
