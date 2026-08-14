//! Pure data types — the "nouns" every other module operates on.
//!
//! Nothing in this module performs I/O or spawns a process. See docs/ARCHITECTURE.md §4.

pub mod audit;
pub mod bisect;
pub mod evaluator;
pub mod exec;
pub mod experiment;
pub mod fault;
pub mod ids;
pub mod mutant;
pub mod mutation;
pub mod policy;
pub mod race;
pub mod scoring;
pub mod task;

pub use audit::AuditEvent;
pub use bisect::{BisectRecord, BisectStep};
pub use evaluator::{Cmd, EvaluatorSpec, EvaluatorVerdict, MetricExtractor};
pub use exec::{CommandPolicy, EnforcementLevel, ExecutionBudget, ProcessOutcome, ProcessSpec};
pub use experiment::{DiffStats, ExperimentRecord, ExperimentStatus, RawMetrics};
pub use fault::{FaultKind, FaultRef, FaultSpec, FaultTarget};
pub use mutant::{MutantEvaluation, MutantRef, MutantSpec, MutantTarget};
pub use mutation::{MutationOperator, MutationRef, MutationSpec, MutationTarget};
pub use policy::{AgentConfig, PermissionPolicy};
pub use race::{RaceParticipant, RaceRecord};
pub use scoring::{Rating, RatingBands, ScoreCard, ScoreComponent, ScoringWeights};
pub use task::TaskSpec;
