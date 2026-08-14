//! MVP's only production adapter — SPEC.md §9.
//!
//! Declares `RequestedOnly` for both capabilities: filesystem/network restriction is relayed to
//! Claude Code's own permission-mode flags, never independently verified by AgentForge.

use crate::domain::{EnforcementLevel, ProcessSpec};

use super::{AdapterCapabilities, AgentAdapter};

/// Overrides the default `claude` executable lookup — `adapter::resolve` takes only a name
/// (SPEC.md §9, ARCHITECTURE.md §7), so this environment variable is the one configuration point
/// available without threading a config value through that signature.
const CLAUDE_EXECUTABLE_ENV: &str = "AGENTFORGE_CLAUDE_EXECUTABLE";

/// Overrides the default (absent) `--permission-mode` flag. Unset means "don't pass the flag at
/// all" — never a silently-assumed permission mode.
const CLAUDE_PERMISSION_MODE_ENV: &str = "AGENTFORGE_CLAUDE_PERMISSION_MODE";

/// Construction-time configuration for `ClaudeCodeAdapter` — deliberately separate from the
/// per-call `model`/`extra_args` the `AgentAdapter` trait already takes, since those vary per
/// `command_for` call while these vary per adapter instance.
#[derive(Debug, Clone, Default)]
pub struct ClaudeCodeConfig {
    /// Program name or path resolved via `PATH`, exactly like any other `ProcessSpec.program` —
    /// never a shell command line.
    pub executable: String,
    /// Requests filesystem/network restriction through Claude Code's own `--permission-mode`
    /// flag where present (`AdapterCapabilities::RequestedOnly` — relayed only, never
    /// independently verified by AgentForge).
    pub permission_mode: Option<String>,
    /// Appended after every flag this adapter itself builds, before the per-call `extra_args`
    /// passed to `command_for`.
    pub extra_default_args: Vec<String>,
}

pub struct ClaudeCodeAdapter {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeAdapter {
    pub fn new(config: ClaudeCodeConfig) -> Self {
        ClaudeCodeAdapter { config }
    }
}

impl Default for ClaudeCodeAdapter {
    /// `AGENTFORGE_CLAUDE_EXECUTABLE`/`AGENTFORGE_CLAUDE_PERMISSION_MODE` override the plain
    /// `claude`-on-`PATH`, no-permission-mode-flag defaults.
    fn default() -> Self {
        ClaudeCodeAdapter::new(ClaudeCodeConfig {
            executable: std::env::var(CLAUDE_EXECUTABLE_ENV)
                .unwrap_or_else(|_| "claude".to_string()),
            permission_mode: std::env::var(CLAUDE_PERMISSION_MODE_ENV).ok(),
            extra_default_args: vec![],
        })
    }
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            can_confine_filesystem: EnforcementLevel::RequestedOnly,
            can_restrict_network: EnforcementLevel::RequestedOnly,
        }
    }

    fn command_for(&self, prompt: &str, model: Option<&str>, extra_args: &[String]) -> ProcessSpec {
        // A non-interactive (print/headless) invocation: `prompt` is one discrete argv element,
        // never interpolated into a shell string — there is no `sh -c`/`cmd /C` anywhere in this
        // path, and the Executor spawns `program`+`args` directly (SPEC.md §20, U2).
        let mut args = vec![
            "-p".to_string(),
            prompt.to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ];
        if let Some(model) = model {
            args.push("--model".to_string());
            args.push(model.to_string());
        }
        if let Some(mode) = &self.config.permission_mode {
            args.push("--permission-mode".to_string());
            args.push(mode.clone());
        }
        args.extend(self.config.extra_default_args.iter().cloned());
        args.extend_from_slice(extra_args);

        ProcessSpec {
            program: self.config.executable.clone(),
            args,
            extra_env: Vec::new(),
        }
    }
}
