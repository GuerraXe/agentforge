//! `ScoreCard`, `ScoreComponent`, `Rating`, `ScoringWeights` — SPEC.md §4, §15.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Rating {
    Excellent,
    Good,
    Fair,
    Poor,
    Fail,
}

impl std::fmt::Display for Rating {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Rating::Excellent => "Excellent",
            Rating::Good => "Good",
            Rating::Fair => "Fair",
            Rating::Poor => "Poor",
            Rating::Fail => "Fail",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RatingBands {
    pub excellent: u8,
    pub good: u8,
    pub fair: u8,
    pub poor: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringWeights {
    pub correctness: f64,
    pub efficiency: f64,
    pub parsimony: f64,
    pub rating_bands: RatingBands,
    pub formula_version: String,
    /// Where these weights came from — a path, or `"built-in-default"`. Carried on the weights
    /// themselves (rather than as a separate parameter to `score()`) so `ScoreCard.weights_source`
    /// can be populated without `scoring::score` needing to know anything about the filesystem —
    /// found while writing scoring tests, SPEC.md §15. `#[serde(default)]` so a hand-authored
    /// `scoring.toml`/`--weights` file never needs to state this — `Store::load_scoring_weights`
    /// always overwrites it with the resolved path (or `"built-in-default"`) after parsing, so
    /// nothing in the file itself is trusted to say where the file is.
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreComponent {
    pub name: String,
    pub raw: f64,
    pub normalized: f64,
    pub weight: f64,
    pub contribution: f64,
    /// Whether a larger `raw` value is preferable for this metric (e.g. `true` for a test-pass
    /// ratio, `false` for wall-clock time or diff size, where smaller is better).
    pub higher_is_better: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreCard {
    pub total: u8,
    pub rating: Rating,
    /// True if the correctness gate applied — SPEC.md §15.
    pub gated: bool,
    pub components: Vec<ScoreComponent>,
    pub weights_source: String,
    pub formula_version: String,
}
