//! `AuditEvent` — SPEC.md §4, §16.
//!
//! `PermissionCheck` is emitted only for checks the Executor itself performs and can verify
//! (cwd assignment, env filtering, output-cap enforcement) — never for adapter-relayed
//! best-effort requests, so the log never implies a verification that didn't happen.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditEvent {
    ProcessSpawn {
        command: String,
        args: Vec<String>,
        cwd: PathBuf,
        at: DateTime<Utc>,
    },
    ProcessExit {
        exit_code: Option<i32>,
        timed_out: bool,
        at: DateTime<Utc>,
    },
    PermissionCheck {
        action: String,
        allowed: bool,
        reason: String,
        at: DateTime<Utc>,
    },
    /// A worktree ("task workspace") was created — emitted by `workspace::WorkspaceManager`,
    /// docs/ARCHITECTURE.md's worktree-lifecycle layer.
    WorktreeCreated {
        path: PathBuf,
        base_commit: String,
        at: DateTime<Utc>,
    },
    /// A worktree was removed — emitted whether the removal was a real `git worktree remove`
    /// or a no-op because it was already gone (idempotent cleanup still records that the
    /// caller asked).
    WorktreeRemoved { path: PathBuf, at: DateTime<Utc> },
}
