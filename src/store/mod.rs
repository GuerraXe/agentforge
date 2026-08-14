//! The persistence layer everything else reads/writes through — SPEC.md §5,
//! docs/ARCHITECTURE.md §12.
//!
//! The only module that knows the directory layout and TOML-vs-JSON format split. No other
//! module constructs a path into `.agentforge/` or the state root itself.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{
    AgentConfig, BisectRecord, BisectStep, EvaluatorSpec, ExperimentRecord, ExperimentStatus,
    FaultRef, MutantRef, MutationRef, PermissionPolicy, RaceRecord, RawMetrics, ScoreCard,
    ScoringWeights, TaskSpec,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml deserialize error: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("toml serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0} (use --force to overwrite)")]
    AlreadyExists(String),
    #[error("id must not be empty")]
    EmptyId,
    #[error("id {0:?} is invalid — only ASCII letters, digits, '-', and '_' are allowed")]
    InvalidId(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The same `[A-Za-z0-9_-]`-only rule `workspace::validate_id`/`fault::validate_id` enforce,
/// applied here too: every `Store` collection (`tasks`, `evaluators`, `policies`, `faults`,
/// `mutants`, `experiments`, `races`, `bisects`) turns a caller-supplied id/name directly into a
/// path component (`<dir>/<id>.toml`, or `<dir>/<id>/manifest.toml`) with no other sanitization
/// on this side. Unlike `workspace`/`fault`/`mutant`, which validate their own ids at the point
/// they're minted (before ever reaching `Store`), `task`/`evaluator`/`policy` ids arrive here
/// straight from a user-authored TOML file (`task add --file`, `evaluator add --file`, `policy
/// add --file`) or a bare CLI argument (`report show <id>`, `clean --experiment <id>`, ...) with
/// no upstream check at all — so `Store` is the one choke point every path-building id/name
/// actually passes through and is where this must be enforced, not each call site individually.
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(Error::EmptyId);
    }
    let is_safe = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !is_safe {
        return Err(Error::InvalidId(id.to_string()));
    }
    Ok(())
}

pub struct Store {
    /// `<repo>/.agentforge` — TRACKED in the target repo's git history.
    repo_root: PathBuf,
    /// External, untracked — resolved via `WorktreeManager::resolve_state_root`.
    state_root: PathBuf,
}

impl Store {
    pub fn open(repo_root: PathBuf, state_root: PathBuf) -> Self {
        Store {
            repo_root,
            state_root,
        }
    }

    pub fn load_task(&self, id: &str) -> Result<TaskSpec> {
        validate_id(id)?;
        read_toml(&self.tasks_dir().join(format!("{id}.toml")), id)
    }

    pub fn save_task(&self, task: &TaskSpec, force: bool) -> Result<()> {
        validate_id(&task.id)?;
        write_toml(
            &self.tasks_dir().join(format!("{}.toml", task.id)),
            task,
            force,
        )
    }

    pub fn list_tasks(&self) -> Result<Vec<String>> {
        list_toml_ids(&self.tasks_dir())
    }

    pub fn load_evaluator(&self, id: &str) -> Result<EvaluatorSpec> {
        validate_id(id)?;
        read_toml(&self.evaluators_dir().join(format!("{id}.toml")), id)
    }

    pub fn save_evaluator(&self, spec: &EvaluatorSpec, force: bool) -> Result<()> {
        validate_id(&spec.id)?;
        write_toml(
            &self.evaluators_dir().join(format!("{}.toml", spec.id)),
            spec,
            force,
        )
    }

    pub fn list_evaluators(&self) -> Result<Vec<String>> {
        list_toml_ids(&self.evaluators_dir())
    }

    /// A standalone record — unlike `MutationRef` (embedded-only in `TaskSpec`, SPEC.md §20 C3),
    /// `FaultRef` has no wrapping task to embed into yet (`experiment`/`ExperimentRunner` isn't
    /// built), so it gets its own `Store` collection, mirroring `tasks`/`evaluators` exactly.
    pub fn load_fault(&self, id: &str) -> Result<FaultRef> {
        validate_id(id)?;
        read_toml(&self.faults_dir().join(format!("{id}.toml")), id)
    }

    pub fn save_fault(&self, fault: &FaultRef, force: bool) -> Result<()> {
        validate_id(&fault.id)?;
        write_toml(
            &self.faults_dir().join(format!("{}.toml", fault.id)),
            fault,
            force,
        )
    }

    pub fn list_faults(&self) -> Result<Vec<String>> {
        list_toml_ids(&self.faults_dir())
    }

    /// A standalone record, mirroring `save_fault`/`load_fault`/`list_faults` exactly:
    /// `MutantRef` (SPEC.md §10 Amendment, standalone mutation testing) has no wrapping task to
    /// embed into either, same reasoning as `FaultRef`.
    pub fn load_mutant(&self, id: &str) -> Result<MutantRef> {
        validate_id(id)?;
        read_toml(&self.mutants_dir().join(format!("{id}.toml")), id)
    }

    pub fn save_mutant(&self, mutant: &MutantRef, force: bool) -> Result<()> {
        validate_id(&mutant.id)?;
        write_toml(
            &self.mutants_dir().join(format!("{}.toml", mutant.id)),
            mutant,
            force,
        )
    }

    pub fn list_mutants(&self) -> Result<Vec<String>> {
        list_toml_ids(&self.mutants_dir())
    }

    fn tasks_dir(&self) -> PathBuf {
        self.repo_root.join("tasks")
    }

    fn evaluators_dir(&self) -> PathBuf {
        self.repo_root.join("evaluators")
    }

    fn faults_dir(&self) -> PathBuf {
        self.repo_root.join("faults")
    }

    fn mutants_dir(&self) -> PathBuf {
        self.repo_root.join("mutants")
    }

    /// A standalone, id-keyed collection mirroring `evaluators`/`tasks` exactly (TOML files under
    /// `<repo_root>/policies/`, keyed by `PermissionPolicy.name`).
    pub fn load_policy(&self, name: &str) -> Result<PermissionPolicy> {
        validate_id(name)?;
        read_toml(&self.policies_dir().join(format!("{name}.toml")), name)
    }

    pub fn save_policy(&self, policy: &PermissionPolicy, force: bool) -> Result<()> {
        validate_id(&policy.name)?;
        write_toml(
            &self.policies_dir().join(format!("{}.toml", policy.name)),
            policy,
            force,
        )
    }

    pub fn list_policies(&self) -> Result<Vec<String>> {
        list_toml_ids(&self.policies_dir())
    }

    fn policies_dir(&self) -> PathBuf {
        self.repo_root.join("policies")
    }

    /// Directory listing under `<state_root>/experiments/` — unlike `list_tasks`/`list_evaluators`
    /// (a flat directory of `<id>.toml` files), each experiment is a directory
    /// (`manifest.toml`/`metrics.json`/`score.json`), so this lists directory names rather than
    /// `.toml` file stems. Used by `clean`'s reconciliation pass and its `--all-worktrees`/
    /// `--older-than` selection, both of which need every known experiment id, not just one.
    pub fn list_experiments(&self) -> Result<Vec<String>> {
        list_dir_ids(&self.state_root.join("experiments"))
    }

    /// Reads `<repo_root>/scoring.toml` if present; falls back to `scoring::default_weights()`
    /// (source `"built-in-default"`) when the repo hasn't scaffolded one — `init` (SPEC.md §5)
    /// isn't required just to run `score`/`show`. `source` is always overwritten to the resolved
    /// path (or left as `"built-in-default"`) rather than trusted from the file's own content, so
    /// a `ScoreCard.weights_source` can never lie about where its weights actually came from.
    pub fn load_scoring_weights(&self) -> Result<ScoringWeights> {
        let path = self.repo_root.join("scoring.toml");
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let mut weights: ScoringWeights = toml::from_str(&raw)?;
                weights.source = path.display().to_string();
                Ok(weights)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(crate::scoring::default_weights())
            }
            Err(e) => Err(Error::Io(e)),
        }
    }

    /// No `save_leaderboard`/`load_leaderboard` — there is no such artifact. `report` computes
    /// the ranked view live from each participant's `score.json` — SPEC.md §20 (D3).
    ///
    /// Always overwrites (no `--force`/collision guard, unlike `save_task`/`save_evaluator`): an
    /// `ExperimentRecord` is a result that gets rewritten as it progresses (`Running` →
    /// `Completed`/`Failed`/`TimedOut`), not a user-authored spec with collision semantics.
    pub fn save_experiment(&self, record: &ExperimentRecord) -> Result<()> {
        validate_id(&record.id)?;
        let dir = self.experiment_dir(&record.id);
        std::fs::create_dir_all(&dir)?;
        write_toml_always(
            &dir.join("manifest.toml"),
            &ExperimentManifest::from(record),
        )?;
        write_json_always(&dir.join("metrics.json"), &record.raw_metrics)?;
        write_json_always(&dir.join("score.json"), &record.score)?;
        Ok(())
    }

    pub fn load_experiment(&self, id: &str) -> Result<ExperimentRecord> {
        validate_id(id)?;
        let dir = self.experiment_dir(id);
        let manifest: ExperimentManifest = read_toml(&dir.join("manifest.toml"), id)?;
        let raw_metrics: Option<RawMetrics> = read_json(&dir.join("metrics.json"), id)?;
        let score: Option<ScoreCard> = read_json(&dir.join("score.json"), id)?;
        Ok(manifest.into_record(raw_metrics, score))
    }

    /// Always overwrites — same reasoning as `save_experiment`; a race's participant list is
    /// fixed at construction (SPEC.md §12) but the record itself may still be re-saved.
    pub fn save_race(&self, record: &RaceRecord) -> Result<()> {
        validate_id(&record.id)?;
        let dir = self.race_dir(&record.id);
        std::fs::create_dir_all(&dir)?;
        write_toml_always(&dir.join("manifest.toml"), record)?;
        Ok(())
    }

    pub fn load_race(&self, id: &str) -> Result<RaceRecord> {
        validate_id(id)?;
        read_toml(&self.race_dir(id).join("manifest.toml"), id)
    }

    /// Always overwrites — same reasoning as `save_experiment`. `steps.jsonl` is rewritten in
    /// full from `record.steps` on every call rather than appended to; the live, in-process
    /// append-as-tested behavior SPEC.md §13 describes belongs to `BisectRunner`, not `Store`,
    /// which only ever persists/reloads a complete snapshot.
    pub fn save_bisect(&self, record: &BisectRecord) -> Result<()> {
        validate_id(&record.id)?;
        let dir = self.bisect_dir(&record.id);
        std::fs::create_dir_all(&dir)?;
        write_toml_always(&dir.join("manifest.toml"), &BisectManifest::from(record))?;
        let mut steps_raw = String::new();
        for step in &record.steps {
            steps_raw.push_str(&serde_json::to_string(step)?);
            steps_raw.push('\n');
        }
        std::fs::write(dir.join("steps.jsonl"), steps_raw)?;
        write_json_always(&dir.join("result.json"), &BisectResult::from(record))?;
        Ok(())
    }

    pub fn load_bisect(&self, id: &str) -> Result<BisectRecord> {
        validate_id(id)?;
        let dir = self.bisect_dir(id);
        let manifest: BisectManifest = read_toml(&dir.join("manifest.toml"), id)?;
        let steps_raw = match std::fs::read_to_string(dir.join("steps.jsonl")) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(Error::Io(e)),
        };
        let mut steps = Vec::new();
        for line in steps_raw.lines().filter(|l| !l.trim().is_empty()) {
            steps.push(serde_json::from_str::<BisectStep>(line)?);
        }
        let result: Option<BisectResult> = read_json(&dir.join("result.json"), id)?;
        let culprit = result.and_then(|r| r.culprit);
        Ok(manifest.into_record(steps, culprit))
    }

    fn experiment_dir(&self, id: &str) -> PathBuf {
        self.state_root.join("experiments").join(id)
    }

    fn race_dir(&self, id: &str) -> PathBuf {
        self.state_root.join("races").join(id)
    }

    fn bisect_dir(&self, id: &str) -> PathBuf {
        self.state_root.join("bisects").join(id)
    }
}

/// `manifest.toml`'s shape — every `ExperimentRecord` field except `raw_metrics`/`score`, which
/// get their own JSON files so a still-`Running` experiment's manifest never needs an `Option`
/// placeholder for data that doesn't exist yet.
#[derive(Debug, Serialize, Deserialize)]
struct ExperimentManifest {
    id: String,
    task_id: String,
    agent_config: AgentConfig,
    policy_snapshot: PermissionPolicy,
    base_ref: String,
    mutation_ref: Option<MutationRef>,
    race_id: Option<String>,
    race_index: Option<u32>,
    worktree_path: PathBuf,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    status: ExperimentStatus,
    patch_path: PathBuf,
    audit_log_path: PathBuf,
}

impl From<&ExperimentRecord> for ExperimentManifest {
    fn from(record: &ExperimentRecord) -> Self {
        ExperimentManifest {
            id: record.id.clone(),
            task_id: record.task_id.clone(),
            agent_config: record.agent_config.clone(),
            policy_snapshot: record.policy_snapshot.clone(),
            base_ref: record.base_ref.clone(),
            mutation_ref: record.mutation_ref.clone(),
            race_id: record.race_id.clone(),
            race_index: record.race_index,
            worktree_path: record.worktree_path.clone(),
            started_at: record.started_at,
            ended_at: record.ended_at,
            status: record.status,
            patch_path: record.patch_path.clone(),
            audit_log_path: record.audit_log_path.clone(),
        }
    }
}

impl ExperimentManifest {
    fn into_record(
        self,
        raw_metrics: Option<RawMetrics>,
        score: Option<ScoreCard>,
    ) -> ExperimentRecord {
        ExperimentRecord {
            id: self.id,
            task_id: self.task_id,
            agent_config: self.agent_config,
            policy_snapshot: self.policy_snapshot,
            base_ref: self.base_ref,
            mutation_ref: self.mutation_ref,
            race_id: self.race_id,
            race_index: self.race_index,
            worktree_path: self.worktree_path,
            started_at: self.started_at,
            ended_at: self.ended_at,
            status: self.status,
            patch_path: self.patch_path,
            raw_metrics,
            score,
            audit_log_path: self.audit_log_path,
        }
    }
}

/// `manifest.toml`'s shape for a bisect — every `BisectRecord` field except `steps`/`culprit`,
/// which get their own files (`steps.jsonl`, `result.json`) per SPEC.md §13.
#[derive(Debug, Serialize, Deserialize)]
struct BisectManifest {
    id: String,
    task_id: String,
    evaluator_id: String,
    range: (String, String),
    #[serde(rename = "bisect_worktree")]
    worktree_path: PathBuf,
}

impl From<&BisectRecord> for BisectManifest {
    fn from(record: &BisectRecord) -> Self {
        BisectManifest {
            id: record.id.clone(),
            task_id: record.task_id.clone(),
            evaluator_id: record.evaluator_id.clone(),
            range: record.range.clone(),
            worktree_path: record.worktree_path.clone(),
        }
    }
}

impl BisectManifest {
    fn into_record(self, steps: Vec<BisectStep>, culprit: Option<String>) -> BisectRecord {
        BisectRecord {
            id: self.id,
            task_id: self.task_id,
            evaluator_id: self.evaluator_id,
            range: self.range,
            worktree_path: self.worktree_path,
            steps,
            culprit,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BisectResult {
    culprit: Option<String>,
}

impl From<&BisectRecord> for BisectResult {
    fn from(record: &BisectRecord) -> Self {
        BisectResult {
            culprit: record.culprit.clone(),
        }
    }
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &std::path::Path, id: &str) -> Result<T> {
    let raw = std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::NotFound(id.to_string()),
        _ => Error::Io(e),
    })?;
    Ok(toml::from_str(&raw)?)
}

/// SPEC.md §20 (C6): default is exit-2-on-collision; `--force` overwrites.
fn write_toml<T: serde::Serialize>(path: &std::path::Path, value: &T, force: bool) -> Result<()> {
    if !force && path.exists() {
        return Err(Error::AlreadyExists(
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = toml::to_string_pretty(value)?;
    std::fs::write(path, raw)?;
    Ok(())
}

/// No collision guard — used only for result records that are expected to be rewritten across
/// an experiment/race/bisect's lifecycle (`save_experiment`/`save_race`/`save_bisect`), unlike
/// `write_toml`'s user-authored-spec collision rule (SPEC.md §20 C6).
fn write_toml_always<T: serde::Serialize>(path: &std::path::Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = toml::to_string_pretty(value)?;
    std::fs::write(path, raw)?;
    Ok(())
}

/// Writes `value` as JSON, always overwriting — the JSON-file counterpart to `write_toml_always`.
/// `value` is typically an `Option<T>` (`null` on disk when absent), so `load_experiment` can
/// distinguish "not evaluated yet" from a read error without a file-existence check of its own.
fn write_json_always<T: serde::Serialize>(path: &std::path::Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(value)?;
    std::fs::write(path, raw)?;
    Ok(())
}

/// Mirrors `read_toml`, for the JSON side of a record's persisted files.
fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path, id: &str) -> Result<T> {
    let raw = std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::NotFound(id.to_string()),
        _ => Error::Io(e),
    })?;
    Ok(serde_json::from_str(&raw)?)
}

/// Ids are filenames minus the `.toml` extension — an unsorted directory listing, sorted for a
/// stable, deterministic CLI/`--json` output order.
fn list_toml_ids(dir: &std::path::Path) -> Result<Vec<String>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut ids: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "toml"))
        .filter_map(|entry| {
            entry
                .path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();
    ids.sort();
    Ok(ids)
}

/// Like `list_toml_ids`, but for a collection whose ids are subdirectory names rather than
/// `<id>.toml` file stems (`experiments/<id>/manifest.toml`, not `experiments/<id>.toml`).
fn list_dir_ids(dir: &std::path::Path) -> Result<Vec<String>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut ids: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    ids.sort();
    Ok(ids)
}
