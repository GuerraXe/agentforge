//! `EvaluatorSpec`, `EvaluatorVerdict`, `MetricExtractor`, `Cmd` — SPEC.md §4, §11.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub cwd_relative: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricExtractor {
    pub name: String,
    pub pattern: String,
    pub capture_group: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatorSpec {
    pub id: String,
    /// Run once before `test_cmd`; first failure short-circuits — SPEC.md §20 (F4).
    pub setup_cmds: Vec<Cmd>,
    pub test_cmd: Cmd,
    /// Must be > 0 — validated at `evaluator add`, SPEC.md §20 (S4).
    pub timeout_secs: u64,
    /// Must be > 0 — efficiency score input.
    pub budget_secs: u64,
    /// Must be > 0 — parsimony score input.
    pub size_budget_lines: u32,
    pub metric_extractors: Vec<MetricExtractor>,
}

impl EvaluatorSpec {
    /// Pure, in-memory field validation — no I/O. SPEC.md §20 (S4).
    pub fn validate_fields(&self) -> Result<(), String> {
        if self.timeout_secs == 0 {
            return Err("evaluator.timeout_secs must be > 0".to_string());
        }
        if self.budget_secs == 0 {
            return Err("evaluator.budget_secs must be > 0".to_string());
        }
        if self.size_budget_lines == 0 {
            return Err("evaluator.size_budget_lines must be > 0".to_string());
        }
        Ok(())
    }
}

/// The one shared judgment type — SPEC.md §4, §11.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvaluatorVerdict {
    pub build_succeeded: bool,
    pub tests_total: Option<u32>,
    pub tests_passed: Option<u32>,
    /// `-1` is reserved: "killed by timeout", never a real process exit code.
    pub exit_code: i32,
    pub timed_out: bool,
    pub wall_time_secs: f64,
}

impl EvaluatorVerdict {
    pub fn is_good(&self) -> bool {
        self.build_succeeded && !self.timed_out && self.exit_code == 0
    }
}
