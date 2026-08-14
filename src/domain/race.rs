//! `RaceRecord` — SPEC.md §4, §12.
//!
//! Stores only the declared participant list, not a leaderboard: the leaderboard is always
//! computed live from each participant's own `score.json` (SPEC.md §20, D3).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RaceParticipant {
    pub race_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceRecord {
    pub id: String,
    pub task_id: String,
    pub max_parallel: u32,
    /// Fixed at construction time, before any execution starts.
    pub participants: Vec<(String /* experiment_id */, RaceParticipant)>,
}
