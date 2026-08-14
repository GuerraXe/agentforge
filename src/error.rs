//! Top-level, aggregating error type. See docs/ARCHITECTURE.md §15.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentForgeError {
    #[error(transparent)]
    Git(#[from] crate::git::Error),

    #[error(transparent)]
    Exec(#[from] crate::exec::Error),

    #[error(transparent)]
    Audit(#[from] crate::audit::Error),

    #[error(transparent)]
    Adapter(#[from] crate::adapter::Error),

    #[error(transparent)]
    Evaluator(#[from] crate::evaluator::Error),

    #[error(transparent)]
    Mutation(#[from] crate::mutation::Error),

    #[error(transparent)]
    Fault(#[from] crate::fault::Error),

    #[error(transparent)]
    Experiment(#[from] crate::experiment::Error),

    #[error(transparent)]
    Race(#[from] crate::race::Error),

    #[error(transparent)]
    Bisect(#[from] crate::bisect::Error),

    #[error(transparent)]
    Store(#[from] crate::store::Error),

    #[error(transparent)]
    Report(#[from] crate::report::Error),

    #[error(transparent)]
    Workspace(#[from] crate::workspace::Error),

    #[error("validation error: {0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, AgentForgeError>;
