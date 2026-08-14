//! An independent record of what the Executor observed — never populated by adapter code.
//! SPEC.md §16, docs/ARCHITECTURE.md §5.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use thiserror::Error;

use crate::domain::AuditEvent;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A trait so `exec`/`evaluator` can emit events without knowing whether they're running inside
/// a full experiment (`JsonlAuditSink`) or a throwaway evaluation worktree with no experiment
/// record at all (`NullAuditSink`) — SPEC.md §11 (`task add` baseline capture, `mutate`'s sanity
/// gate, `eval`).
pub trait AuditSink: Send + Sync {
    fn record(&self, event: &AuditEvent) -> Result<()>;
}

/// Append-only, flushed after every line — SPEC.md §16. The file handle is behind a `Mutex` so
/// `record` can take `&self` (required by the `AuditSink` trait, shared as `&dyn AuditSink`
/// across concurrent callers) while still safely interleaving writes from multiple threads
/// without corrupting a line.
pub struct JsonlAuditSink {
    file: Mutex<File>,
}

impl JsonlAuditSink {
    /// Creates the parent directory if needed and opens `path` in append mode, creating it if
    /// it doesn't exist.
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(JsonlAuditSink {
            file: Mutex::new(file),
        })
    }
}

impl AuditSink for JsonlAuditSink {
    fn record(&self, event: &AuditEvent) -> Result<()> {
        let line = serde_json::to_string(event)?;
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }
}

/// For calls with no `ExperimentRecord` to attach an audit trail to.
pub struct NullAuditSink;

impl AuditSink for NullAuditSink {
    fn record(&self, _event: &AuditEvent) -> Result<()> {
        Ok(())
    }
}
