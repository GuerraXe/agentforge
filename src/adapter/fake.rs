//! Test-only adapter — SPEC.md §9, §18. Carries a scripted `ProcessSpec` so every
//! `run`/`race`/`bisect` integration test runs with zero dependency on the real `claude` binary.

use crate::domain::{EnforcementLevel, ProcessSpec};

use super::{AdapterCapabilities, AgentAdapter};

pub struct FakeAdapter {
    pub scripted_command: ProcessSpec,
}

impl AgentAdapter for FakeAdapter {
    fn name(&self) -> &str {
        "fake"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            can_confine_filesystem: EnforcementLevel::Enforced,
            can_restrict_network: EnforcementLevel::Enforced,
        }
    }

    fn command_for(
        &self,
        _prompt: &str,
        _model: Option<&str>,
        _extra_args: &[String],
    ) -> ProcessSpec {
        self.scripted_command.clone()
    }
}
