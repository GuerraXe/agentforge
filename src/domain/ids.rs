//! The `<UTC compact ISO8601><6 hex chars>` ID scheme — SPEC.md §5.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use chrono::{DateTime, Utc};

/// Generates a sortable, collision-resistant-enough-for-local-use id, e.g.
/// `20260811T193000Z-9f3a2b`.
///
/// The random suffix comes from `RandomState` (std's OS-seeded SipHash keys) rather than a
/// `rand` dependency — the ID scheme's uniqueness only needs to survive collisions within a
/// single machine's concurrent experiments, not cryptographic unpredictability.
pub fn new_id(at: DateTime<Utc>) -> String {
    let timestamp = at.format("%Y%m%dT%H%M%SZ");
    format!("{timestamp}-{}", random_hex_suffix())
}

fn random_hex_suffix() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    hasher.write_u32(std::process::id());
    format!("{:06x}", hasher.finish() & 0xFF_FFFF)
}

/// A minimal, module-agnostic error for the shared `validate_id` check below — each caller
/// (`workspace`, `fault`, `mutant`) converts it into its own `EmptyId`/`InvalidId` variant rather
/// than exposing this type on its public API.
#[derive(Debug)]
pub(crate) enum IdError {
    Empty,
    Invalid(String),
}

/// The `[A-Za-z0-9_-]`-only rule every AgentForge-chosen id must satisfy before it becomes a
/// filesystem path component — shared by `workspace::WorkspaceManager`, `fault::FaultInjector`,
/// and `mutant::MutantTester` rather than kept as three independently-maintained copies.
pub(crate) fn validate_id(id: &str) -> std::result::Result<(), IdError> {
    if id.is_empty() {
        return Err(IdError::Empty);
    }
    let is_safe = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !is_safe {
        return Err(IdError::Invalid(id.to_string()));
    }
    Ok(())
}
