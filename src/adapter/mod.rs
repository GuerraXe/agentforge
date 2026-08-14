//! Agent-independence, made structural — SPEC.md §9, docs/ARCHITECTURE.md §7.
//!
//! `command_for` returns a value instead of taking control flow: an adapter cannot spawn
//! anything, set its own cwd, or touch the audit sink, because it has none of those in scope.

pub mod claude_code;
pub mod fake;

use thiserror::Error;

use crate::domain::{EnforcementLevel, ProcessSpec};

#[derive(Debug, Error)]
pub enum Error {
    #[error("unknown adapter: {0}")]
    UnknownAdapter(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy)]
pub struct AdapterCapabilities {
    pub can_confine_filesystem: EnforcementLevel,
    pub can_restrict_network: EnforcementLevel,
}

pub trait AgentAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> AdapterCapabilities;
    fn command_for(&self, prompt: &str, model: Option<&str>, extra_args: &[String]) -> ProcessSpec;
}

/// Looks up a production adapter by name (e.g. "claude-code"). `FakeAdapter` is constructed
/// directly by tests, not resolved by name.
pub fn resolve(name: &str) -> Result<Box<dyn AgentAdapter>> {
    match name {
        "claude-code" => Ok(Box::new(claude_code::ClaudeCodeAdapter::default())),
        other => Err(Error::UnknownAdapter(other.to_string())),
    }
}
