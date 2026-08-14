//! Structured-in, human-or-JSON-out — SPEC.md §13, docs/ARCHITECTURE.md §13.
//!
//! Holds only `&Store` — no `Executor`, no `GitRepo` — the type-level proof that reporting
//! cannot shell out to anything or re-derive data instead of reading what was persisted. The one
//! exception is `scoring::score`/`scoring::failed_checks`, both pure functions over already-
//! loaded structured data (`RawMetrics`, `EvaluatorVerdict`) — recomputing a `ScoreCard` from
//! them is not re-deriving raw output, it's the documented `score` command path
//! (docs/ARCHITECTURE.md §13).

use std::cmp::Ordering;

use serde_json::{json, Value};
use thiserror::Error;

use crate::domain::{
    DiffStats, EvaluatorSpec, EvaluatorVerdict, ExperimentRecord, ExperimentStatus, RatingBands,
    RawMetrics, ScoreCard, TaskSpec,
};
use crate::store::Store;

#[derive(Debug, Error)]
pub enum Error {
    #[error("store error: {0}")]
    Store(#[from] crate::store::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Reporter<'a> {
    store: &'a Store,
}

impl<'a> Reporter<'a> {
    pub fn new(store: &'a Store) -> Self {
        Reporter { store }
    }

    /// Returns a `String` (rather than writing to stdout) so tests can assert against it
    /// directly — SPEC.md §18. `verbose` controls whether the scoring-component breakdown,
    /// configured weights, threshold definitions, failed checks, and full identifier set are
    /// included; raw measurements (tests/time/patch) and the composite score are always shown
    /// side by side — the composite score never hides them.
    pub fn render_experiment(&self, id: &str, verbose: bool) -> Result<String> {
        let record = self.store.load_experiment(id)?;
        Ok(self.render_experiment_record(&record, verbose))
    }

    /// Computes the ranked table live from each participant's current `score.json` — SPEC.md
    /// §20 (D3). Sorted by `total` desc then `race_index` asc; non-`Completed` participants last.
    pub fn render_race(&self, id: &str, verbose: bool) -> Result<String> {
        let race = self.store.load_race(id)?;
        let mut participants = Vec::with_capacity(race.participants.len());
        for (experiment_id, _) in &race.participants {
            participants.push(self.store.load_experiment(experiment_id)?);
        }
        rank_participants(&mut participants);

        let mut out = String::new();
        out.push_str(&format!("Race            {}\n", race.id));
        out.push_str(&format!("Task            {}\n", race.task_id));
        out.push_str(&format!("Max parallel    {}\n", race.max_parallel));
        out.push('\n');
        out.push_str(&render_comparison_table(&participants));

        if verbose {
            for record in &participants {
                out.push('\n');
                out.push_str(&"-".repeat(60));
                out.push('\n');
                out.push_str(&self.render_experiment_record(record, true));
            }
        }
        Ok(out)
    }

    pub fn render_bisect(&self, id: &str) -> Result<String> {
        let record = self.store.load_bisect(id)?;
        let mut out = String::new();
        out.push_str(&format!("Bisect          {}\n", record.id));
        out.push_str(&format!("Task            {}\n", record.task_id));
        out.push_str(&format!("Evaluator       {}\n", record.evaluator_id));
        out.push_str(&format!(
            "Range           {}..{}\n",
            record.range.0, record.range.1
        ));
        out.push_str(&format!("Steps tested    {}\n", record.steps.len()));
        for (i, step) in record.steps.iter().enumerate() {
            let verdict = if step.is_good { "GOOD" } else { "BAD " };
            out.push_str(&format!(
                "  {:>3}  {}  {}  (exit={} build_succeeded={})\n",
                i + 1,
                step.commit,
                verdict,
                step.verdict.exit_code,
                step.verdict.build_succeeded
            ));
        }
        match &record.culprit {
            Some(culprit) => out.push_str(&format!("Culprit         {culprit}\n")),
            None => out.push_str("Culprit         none (range shares one verdict)\n"),
        }
        Ok(out)
    }

    pub fn experiment_json(&self, id: &str) -> Result<Value> {
        let record = self.store.load_experiment(id)?;
        Ok(self.experiment_json_value(&record))
    }

    pub fn race_json(&self, id: &str) -> Result<Value> {
        let race = self.store.load_race(id)?;
        let mut participants = Vec::with_capacity(race.participants.len());
        for (experiment_id, _) in &race.participants {
            participants.push(self.store.load_experiment(experiment_id)?);
        }
        rank_participants(&mut participants);

        let leaderboard: Vec<Value> = participants
            .iter()
            .enumerate()
            .map(|(i, record)| {
                let mut entry = self.experiment_json_value(record);
                if let Value::Object(ref mut map) = entry {
                    map.insert("rank".to_string(), json!(i + 1));
                }
                entry
            })
            .collect();

        Ok(json!({
            "id": race.id,
            "task_id": race.task_id,
            "max_parallel": race.max_parallel,
            "leaderboard": leaderboard,
        }))
    }

    /// `serde_json::to_value(self.store.load_bisect(id)?)` — a `BisectRecord` has no `ScoreCard`
    /// to enrich with weights/thresholds, so unlike `experiment_json`/`race_json` there's nothing
    /// beyond the persisted record to add.
    pub fn bisect_json(&self, id: &str) -> Result<Value> {
        let record = self.store.load_bisect(id)?;
        Ok(serde_json::to_value(record).expect("BisectRecord always serializes"))
    }

    /// Loads the `TaskSpec`/`EvaluatorSpec` behind an experiment's threshold definitions
    /// (rating bands, evaluator budgets). Missing/unloadable context degrades to `None` rather
    /// than failing the whole report — an experiment's own raw measurements and score stand on
    /// their own even if the task or evaluator that produced them was since removed.
    fn context_for(&self, record: &ExperimentRecord) -> Option<(TaskSpec, EvaluatorSpec)> {
        let task = self.store.load_task(&record.task_id).ok()?;
        let evaluator = self.store.load_evaluator(&task.evaluator).ok()?;
        Some((task, evaluator))
    }

    fn render_experiment_record(&self, record: &ExperimentRecord, verbose: bool) -> String {
        let mut out = String::new();
        out.push_str(&format!("Experiment      {}\n", record.id));
        out.push_str(&format!("Task            {}\n", record.task_id));
        out.push_str(&format!("Agent           {}\n", agent_label(record)));
        out.push_str(&format!("Status          {:?}\n", record.status));
        if let (Some(race_id), Some(race_index)) = (&record.race_id, record.race_index) {
            out.push_str(&format!("Race            {race_id} (index {race_index})\n"));
        }
        out.push('\n');

        let context = self.context_for(record);
        let baseline = context.as_ref().map(|(task, _)| task.baseline.clone());
        let evaluator = context.as_ref().map(|(_, ev)| ev.clone());

        match (&record.raw_metrics, &record.score) {
            (Some(metrics), Some(score)) => {
                out.push_str(&format_scorecard(
                    metrics,
                    baseline.as_ref(),
                    evaluator.as_ref(),
                    score,
                    verbose,
                ));
            }
            _ => out.push_str("(no raw measurements yet — experiment has not been evaluated)\n"),
        }

        if verbose {
            out.push('\n');
            out.push_str("Identifiers\n");
            out.push_str(&format!("  experiment_id   {}\n", record.id));
            out.push_str(&format!("  task_id         {}\n", record.task_id));
            if let (Some(race_id), Some(race_index)) = (&record.race_id, record.race_index) {
                out.push_str(&format!(
                    "  race_id         {race_id} (index {race_index})\n"
                ));
            }
            out.push_str(&format!(
                "  worktree_path   {}\n",
                record.worktree_path.display()
            ));
            out.push_str(&format!(
                "  patch_path      {}\n",
                record.patch_path.display()
            ));
            out.push_str(&format!(
                "  audit_log_path  {}\n",
                record.audit_log_path.display()
            ));
        }

        out
    }

    fn experiment_json_value(&self, record: &ExperimentRecord) -> Value {
        let mut value = serde_json::to_value(record).expect("ExperimentRecord always serializes");
        let context = self.context_for(record);
        let failed_checks = match (&record.raw_metrics, context.as_ref()) {
            (Some(metrics), Some((task, _))) => {
                crate::scoring::failed_checks(metrics, &task.baseline)
            }
            _ => Vec::new(),
        };
        let thresholds = context.as_ref().map(|(_, evaluator)| {
            json!({
                "timeout_secs": evaluator.timeout_secs,
                "budget_secs": evaluator.budget_secs,
                "size_budget_lines": evaluator.size_budget_lines,
            })
        });
        let rating_bands = record
            .score
            .as_ref()
            .filter(|s| s.weights_source == "built-in-default")
            .map(|_| rating_bands_json(&crate::scoring::default_weights().rating_bands));

        if let Value::Object(ref mut map) = value {
            map.insert(
                "thresholds".to_string(),
                json!({
                    "evaluator": thresholds,
                    "rating_bands": rating_bands,
                }),
            );
            map.insert("failed_checks".to_string(), json!(failed_checks));
        }
        value
    }
}

/// SPEC.md §20 (D3): sorted by `total` desc then `race_index` asc; non-`Completed` participants
/// sort last regardless of any stale `score` they might carry.
fn rank_participants(participants: &mut [ExperimentRecord]) {
    participants.sort_by(|a, b| {
        let a_done = a.status == ExperimentStatus::Completed;
        let b_done = b.status == ExperimentStatus::Completed;
        match (a_done, b_done) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => {
                let a_total = a.score.as_ref().map(|s| s.total).unwrap_or(0);
                let b_total = b.score.as_ref().map(|s| s.total).unwrap_or(0);
                b_total.cmp(&a_total).then(a.race_index.cmp(&b.race_index))
            }
        }
    });
}

fn agent_label(record: &ExperimentRecord) -> String {
    match &record.agent_config.model {
        Some(model) => format!("{}:{}", record.agent_config.adapter, model),
        None => record.agent_config.adapter.clone(),
    }
}

/// The terminal comparison table — one row per candidate, immediately understandable without
/// opening a single record: tests, wall time, patch size, composite score, and rating side by
/// side. Raw measurements are never hidden behind the composite score — they're columns of their
/// own, not folded into it.
fn render_comparison_table(participants: &[ExperimentRecord]) -> String {
    let headers = ["Candidate", "Tests", "Time", "Patch", "Score", "Rating"];
    let rows: Vec<Vec<String>> = participants
        .iter()
        .map(|record| {
            let candidate = format!(
                "#{} {}",
                record.race_index.unwrap_or(0),
                agent_label(record)
            );
            match (&record.raw_metrics, &record.score) {
                (Some(metrics), Some(score)) => vec![
                    candidate,
                    tests_cell(&metrics.verdict),
                    time_cell(metrics.verdict.wall_time_secs),
                    patch_cell(&metrics.diff),
                    score.total.to_string(),
                    score.rating.to_string(),
                ],
                _ => vec![
                    candidate,
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    format!("{:?}", record.status),
                ],
            }
        })
        .collect();
    render_table(&headers, &rows)
}

fn tests_cell(verdict: &EvaluatorVerdict) -> String {
    match (verdict.tests_passed, verdict.tests_total) {
        (Some(passed), Some(total)) => format!("{passed}/{total}"),
        _ => "-".to_string(),
    }
}

fn time_cell(wall_time_secs: f64) -> String {
    format!("{}s", wall_time_secs.round() as i64)
}

fn patch_cell(diff: &DiffStats) -> String {
    format!("{}L", diff.lines_added + diff.lines_removed)
}

/// Raw measurements (tests/time/patch/build/exit) plus the composite score, always shown
/// together — SPEC.md's transparency requirement that a composite score never hides what it was
/// computed from. `verbose` adds the full component breakdown (name/raw/normalized/weight/
/// contribution — the configured weights), rating thresholds, evaluator budgets, and failed
/// checks.
pub fn format_scorecard(
    metrics: &RawMetrics,
    baseline: Option<&EvaluatorVerdict>,
    evaluator: Option<&EvaluatorSpec>,
    score: &ScoreCard,
    verbose: bool,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Tests           {}\n",
        tests_cell(&metrics.verdict)
    ));
    let budget_note = evaluator
        .map(|e| format!(" (budget {}s)", e.budget_secs))
        .unwrap_or_default();
    out.push_str(&format!(
        "Time            {}{budget_note}\n",
        time_cell(metrics.verdict.wall_time_secs)
    ));
    let size_budget_note = evaluator
        .map(|e| format!(" (budget {}L)", e.size_budget_lines))
        .unwrap_or_default();
    out.push_str(&format!(
        "Patch           {} lines, {} files{size_budget_note}\n",
        metrics.diff.lines_added + metrics.diff.lines_removed,
        metrics.diff.files_changed
    ));
    out.push_str(&format!(
        "Build           {}\n",
        if metrics.verdict.build_succeeded {
            "succeeded"
        } else {
            "FAILED"
        }
    ));
    out.push_str(&format!("Exit code       {}\n", metrics.verdict.exit_code));
    out.push('\n');
    out.push_str(&format!("Score           {}\n", score.total));
    out.push_str(&format!("Rating          {}\n", score.rating));
    out.push_str(&format!(
        "Gated           {}\n",
        if score.gated { "yes" } else { "no" }
    ));

    if verbose {
        out.push('\n');
        out.push_str("Scoring components (configured weights)\n");
        for c in &score.components {
            out.push_str(&format!(
                "  {:<12} raw={:<10.3} normalized={:<6.3} weight={:<6.1} contribution={:.2}\n",
                c.name, c.raw, c.normalized, c.weight, c.contribution
            ));
        }
        out.push('\n');
        out.push_str(&format!("Weights source  {}\n", score.weights_source));
        out.push_str(&format!("Formula version {}\n", score.formula_version));
        if score.weights_source == "built-in-default" {
            out.push_str(&format!(
                "Rating bands    {}\n",
                format_rating_bands(&crate::scoring::default_weights().rating_bands)
            ));
        }
        if let Some(evaluator) = evaluator {
            out.push_str(&format!(
                "Evaluator       timeout={}s budget={}s size_budget={}L\n",
                evaluator.timeout_secs, evaluator.budget_secs, evaluator.size_budget_lines
            ));
        }
        out.push('\n');
        match baseline {
            Some(baseline) => {
                let checks = crate::scoring::failed_checks(metrics, baseline);
                if checks.is_empty() {
                    out.push_str("Failed checks   none\n");
                } else {
                    out.push_str("Failed checks\n");
                    for check in checks {
                        out.push_str(&format!("  - {check}\n"));
                    }
                }
            }
            None => out.push_str("Failed checks   unknown (task baseline unavailable)\n"),
        }
    }

    out
}

fn format_rating_bands(bands: &RatingBands) -> String {
    format!(
        "Excellent>={} Good>={} Fair>={} Poor>={} Fail<{}",
        bands.excellent, bands.good, bands.fair, bands.poor, bands.poor
    )
}

fn rating_bands_json(bands: &RatingBands) -> Value {
    json!({
        "excellent": bands.excellent,
        "good": bands.good,
        "fair": bands.fair,
        "poor": bands.poor,
    })
}

/// A minimal left-aligned, dynamically-widened table — no external crate, since every column
/// here is a short scalar (an id, a count, a duration) rather than wrapped prose.
fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let mut out = String::new();
    let header_cells: Vec<String> = headers.iter().map(|s| s.to_string()).collect();
    render_row(&mut out, &header_cells, &widths);
    for row in rows {
        render_row(&mut out, row, &widths);
    }
    out
}

fn render_row(out: &mut String, cells: &[String], widths: &[usize]) {
    let mut line = String::new();
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            line.push_str("  ");
        }
        if i + 1 == cells.len() {
            line.push_str(cell);
        } else {
            line.push_str(&format!("{:<width$}", cell, width = widths[i]));
        }
    }
    out.push_str(line.trim_end());
    out.push('\n');
}
