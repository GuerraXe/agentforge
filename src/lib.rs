//! AgentForge — see docs/SPEC.md for the product contract and docs/ARCHITECTURE.md for the
//! module-by-module design this crate's layout follows directly.

pub mod adapter;
pub mod audit;
pub mod bisect;
pub mod cli;
pub mod domain;
pub mod error;
pub mod evaluator;
pub mod exec;
pub mod experiment;
pub mod fault;
pub mod git;
pub mod mutant;
pub mod mutation;
pub mod race;
pub mod report;
pub mod scoring;
pub mod store;
pub mod workspace;

pub use error::{AgentForgeError, Result};
