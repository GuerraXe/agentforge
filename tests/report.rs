//! `report::Reporter` — human-readable and JSON evaluation reporting, SPEC.md §13, §18.
//!
//! These construct `ExperimentRecord`/`RaceRecord`/`BisectRecord` fixtures directly and persist
//! them via `Store` (no `run`/`race`/`bisect` execution involved — those are separately
//! out-of-scope `todo!()`s per docs/TEST_STATUS.md), then assert on `Reporter`'s formatted
//! strings and JSON values.

mod common;

use agentforge::domain::{DiffStats, EvaluatorVerdict, ExperimentStatus, TaskSpec};
use agentforge::report::Reporter;

fn good_verdict(wall_time_secs: f64) -> EvaluatorVerdict {
    EvaluatorVerdict {
        build_succeeded: true,
        tests_total: Some(143),
        tests_passed: Some(143),
        exit_code: 0,
        timed_out: false,
        wall_time_secs,
    }
}

/// A task + evaluator + a `Completed` experiment scored via the real `scoring::score`, saved
/// through `Store` — the minimal, real (not hand-rolled) fixture chain `render_experiment`/
/// `experiment_json` read back.
fn seed_scored_experiment(
    store: &agentforge::store::Store,
    experiment_id: &str,
    task_id: &str,
    evaluator_id: &str,
    agent: &str,
    wall_time_secs: f64,
    diff: DiffStats,
) -> agentforge::domain::ExperimentRecord {
    let evaluator = common::noop_evaluator_spec(evaluator_id);
    store.save_evaluator(&evaluator, true).unwrap();
    let baseline = good_verdict(1.0);
    let task: TaskSpec = common::task_spec(task_id, "deadbeef", evaluator_id, baseline.clone());
    store.save_task(&task, true).unwrap();

    let metrics = common::raw_metrics(good_verdict(wall_time_secs), diff);
    let weights = agentforge::scoring::default_weights();
    let score = agentforge::scoring::score(&metrics, &baseline, &evaluator, &weights);

    let mut record =
        common::experiment_record(experiment_id, task_id, common::fake_agent_config(agent));
    record.status = ExperimentStatus::Completed;
    record.raw_metrics = Some(metrics);
    record.score = Some(score);
    store.save_experiment(&record).unwrap();
    record
}

// ---- render_experiment -----------------------------------------------------------------------

#[test]
fn render_experiment_shows_raw_measurements_alongside_the_composite_score() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let record = seed_scored_experiment(
        &store,
        "exp-1",
        "task-1",
        "eval-1",
        "claude-code",
        31.0,
        DiffStats {
            files_changed: 3,
            lines_added: 100,
            lines_removed: 84,
        },
    );

    let reporter = Reporter::new(&store);
    let text = reporter
        .render_experiment("exp-1", false)
        .expect("render_experiment");

    // Raw measurements are never hidden behind the composite score — both are present.
    assert!(text.contains("143/143"), "{text}");
    assert!(text.contains("31s"), "{text}");
    assert!(text.contains("184 lines"), "{text}");
    assert!(text.contains("Score"), "{text}");
    assert!(
        text.contains(&record.score.unwrap().rating.to_string()),
        "{text}"
    );
    // Non-verbose: no component breakdown.
    assert!(!text.contains("Scoring components"), "{text}");
}

#[test]
fn render_experiment_verbose_adds_components_weights_thresholds_and_failed_checks() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    seed_scored_experiment(
        &store,
        "exp-1",
        "task-1",
        "eval-1",
        "claude-code",
        31.0,
        DiffStats {
            files_changed: 3,
            lines_added: 100,
            lines_removed: 84,
        },
    );

    let reporter = Reporter::new(&store);
    let text = reporter
        .render_experiment("exp-1", true)
        .expect("render_experiment");

    assert!(text.contains("Scoring components"), "{text}");
    assert!(text.contains("correctness"), "{text}");
    assert!(text.contains("weight="), "{text}");
    assert!(text.contains("Weights source"), "{text}");
    assert!(text.contains("Rating bands"), "{text}");
    assert!(text.contains("Evaluator"), "{text}");
    assert!(text.contains("Failed checks"), "{text}");
    assert!(text.contains("none"), "{text}");
    assert!(text.contains("Identifiers"), "{text}");
    assert!(text.contains("audit_log_path"), "{text}");
}

#[test]
fn render_experiment_reports_failed_checks_when_gated() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let evaluator = common::noop_evaluator_spec("eval-1");
    store.save_evaluator(&evaluator, false).unwrap();
    let baseline = good_verdict(10.0);
    let task = common::task_spec("task-1", "deadbeef", "eval-1", baseline.clone());
    store.save_task(&task, false).unwrap();

    let bad_verdict = EvaluatorVerdict {
        build_succeeded: false,
        ..good_verdict(10.0)
    };
    let metrics = common::raw_metrics(bad_verdict, DiffStats::default());
    let weights = agentforge::scoring::default_weights();
    let score = agentforge::scoring::score(&metrics, &baseline, &evaluator, &weights);

    let mut record = common::experiment_record("exp-1", "task-1", common::fake_agent_config("a"));
    record.status = ExperimentStatus::Completed;
    record.raw_metrics = Some(metrics);
    record.score = Some(score);
    store.save_experiment(&record).unwrap();

    let reporter = Reporter::new(&store);
    let text = reporter
        .render_experiment("exp-1", true)
        .expect("render_experiment");

    assert!(text.contains("build did not succeed"), "{text}");
    assert!(text.contains("Gated           yes"), "{text}");
}

#[test]
fn render_experiment_handles_a_not_yet_evaluated_experiment_gracefully() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let record = common::experiment_record("exp-1", "task-1", common::fake_agent_config("a"));
    store.save_experiment(&record).unwrap();

    let reporter = Reporter::new(&store);
    let text = reporter
        .render_experiment("exp-1", false)
        .expect("render_experiment");

    assert!(text.contains("no raw measurements yet"), "{text}");
}

#[test]
fn render_experiment_errors_not_found_for_an_unknown_id() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let reporter = Reporter::new(&store);

    let result = reporter.render_experiment("does-not-exist", false);

    assert!(matches!(
        result,
        Err(agentforge::report::Error::Store(
            agentforge::store::Error::NotFound(_)
        ))
    ));
}

// ---- render_race (the terminal comparison table) ---------------------------------------------

#[test]
fn render_race_produces_a_scannable_comparison_table_with_a_header_row() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let a = seed_scored_experiment(
        &store,
        "exp-a",
        "task-1",
        "eval-1",
        "claude-code",
        31.0,
        DiffStats {
            files_changed: 2,
            lines_added: 100,
            lines_removed: 84,
        },
    );
    let mut a = a;
    a.race_id = Some("race-1".to_string());
    a.race_index = Some(0);
    store.save_experiment(&a).unwrap();

    let mut b = seed_scored_experiment(
        &store,
        "exp-b",
        "task-1",
        "eval-1",
        "gpt-5-codex",
        90.0,
        DiffStats {
            files_changed: 5,
            lines_added: 150,
            lines_removed: 60,
        },
    );
    b.race_id = Some("race-1".to_string());
    b.race_index = Some(1);
    store.save_experiment(&b).unwrap();

    let race = common::race_record("race-1", "task-1", &[("exp-a", 0), ("exp-b", 1)]);
    store.save_race(&race).unwrap();

    let reporter = Reporter::new(&store);
    let text = reporter.render_race("race-1", false).expect("render_race");

    assert!(
        text.contains("Candidate") && text.contains("Tests") && text.contains("Score"),
        "expected a header row, got:\n{text}"
    );
    assert!(text.contains("claude-code"), "{text}");
    assert!(text.contains("gpt-5-codex"), "{text}");
    assert!(
        !text.contains("Scoring components"),
        "compact by default: {text}"
    );
}

#[test]
fn render_race_ranks_by_total_desc_then_race_index_asc_with_non_completed_last() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    // exp-a: fast, small patch -> higher score. exp-b: slow, huge patch -> lower score.
    seed_scored_experiment(
        &store,
        "exp-a",
        "task-1",
        "eval-1",
        "agent-a",
        5.0,
        DiffStats {
            files_changed: 1,
            lines_added: 5,
            lines_removed: 0,
        },
    );
    seed_scored_experiment(
        &store,
        "exp-b",
        "task-1",
        "eval-1",
        "agent-b",
        59.0,
        DiffStats {
            files_changed: 20,
            lines_added: 190,
            lines_removed: 0,
        },
    );
    // exp-c never completed.
    let mut c = common::experiment_record("exp-c", "task-1", common::fake_agent_config("agent-c"));
    c.status = ExperimentStatus::Failed;
    store.save_experiment(&c).unwrap();

    let race = common::race_record(
        "race-1",
        "task-1",
        &[("exp-c", 0), ("exp-b", 1), ("exp-a", 2)],
    );
    store.save_race(&race).unwrap();

    let reporter = Reporter::new(&store);
    let text = reporter.render_race("race-1", false).expect("render_race");

    let pos_a = text.find("agent-a").expect("agent-a present");
    let pos_b = text.find("agent-b").expect("agent-b present");
    let pos_c = text.find("agent-c").expect("agent-c present");
    assert!(
        pos_a < pos_b && pos_b < pos_c,
        "expected order agent-a (higher score) < agent-b < agent-c (not Completed, last):\n{text}"
    );
}

#[test]
fn render_race_verbose_appends_per_candidate_breakdown() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    seed_scored_experiment(
        &store,
        "exp-a",
        "task-1",
        "eval-1",
        "agent-a",
        5.0,
        DiffStats::default(),
    );
    let race = common::race_record("race-1", "task-1", &[("exp-a", 0)]);
    store.save_race(&race).unwrap();

    let reporter = Reporter::new(&store);
    let compact = reporter.render_race("race-1", false).expect("compact");
    let verbose = reporter.render_race("race-1", true).expect("verbose");

    assert!(!compact.contains("Scoring components"));
    assert!(verbose.contains("Scoring components"));
    assert!(verbose.len() > compact.len());
}

// ---- render_bisect ------------------------------------------------------------------------

#[test]
fn render_bisect_shows_steps_in_order_and_the_culprit() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mut record = common::bisect_record("bisect-1", "task-1", "eval-1");
    record.steps = vec![
        common::bisect_step("sha-good", good_verdict(1.0), true),
        common::bisect_step(
            "sha-bad",
            EvaluatorVerdict {
                build_succeeded: false,
                ..good_verdict(1.0)
            },
            false,
        ),
    ];
    record.culprit = Some("sha-bad".to_string());
    store.save_bisect(&record).unwrap();

    let reporter = Reporter::new(&store);
    let text = reporter.render_bisect("bisect-1").expect("render_bisect");

    let pos_good = text.find("sha-good").unwrap();
    let pos_bad = text.find("sha-bad").unwrap();
    assert!(
        pos_good < pos_bad,
        "steps should render in tested order:\n{text}"
    );
    assert!(text.contains("Culprit"), "{text}");
    assert!(text.contains("sha-bad"), "{text}");
}

#[test]
fn render_bisect_with_no_culprit_says_so_explicitly() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let record = common::bisect_record("bisect-1", "task-1", "eval-1");
    store.save_bisect(&record).unwrap();

    let reporter = Reporter::new(&store);
    let text = reporter.render_bisect("bisect-1").expect("render_bisect");

    assert!(text.contains("none"), "{text}");
}

// ---- JSON output --------------------------------------------------------------------------

#[test]
fn experiment_json_includes_raw_metrics_score_thresholds_and_failed_checks() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    seed_scored_experiment(
        &store,
        "exp-1",
        "task-1",
        "eval-1",
        "claude-code",
        31.0,
        DiffStats {
            files_changed: 3,
            lines_added: 100,
            lines_removed: 84,
        },
    );

    let reporter = Reporter::new(&store);
    let value = reporter.experiment_json("exp-1").expect("experiment_json");

    // Raw measurements are not hidden behind the composite score in JSON either.
    assert!(value["raw_metrics"]["verdict"]["tests_passed"].as_u64() == Some(143));
    assert!(value["score"]["total"].as_u64().is_some());
    assert!(value["score"]["components"].as_array().is_some());
    assert_eq!(
        value["thresholds"]["evaluator"]["budget_secs"].as_u64(),
        Some(60)
    );
    assert_eq!(
        value["thresholds"]["rating_bands"]["excellent"].as_u64(),
        Some(90)
    );
    assert_eq!(value["failed_checks"].as_array().map(|a| a.len()), Some(0));
}

#[test]
fn experiment_json_reports_failed_checks_as_a_populated_array_when_gated() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let evaluator = common::noop_evaluator_spec("eval-1");
    store.save_evaluator(&evaluator, false).unwrap();
    let baseline = good_verdict(10.0);
    let task = common::task_spec("task-1", "deadbeef", "eval-1", baseline.clone());
    store.save_task(&task, false).unwrap();

    let bad_verdict = EvaluatorVerdict {
        exit_code: 1,
        ..good_verdict(10.0)
    };
    let metrics = common::raw_metrics(bad_verdict, DiffStats::default());
    let weights = agentforge::scoring::default_weights();
    let score = agentforge::scoring::score(&metrics, &baseline, &evaluator, &weights);
    let mut record = common::experiment_record("exp-1", "task-1", common::fake_agent_config("a"));
    record.status = ExperimentStatus::Completed;
    record.raw_metrics = Some(metrics);
    record.score = Some(score);
    store.save_experiment(&record).unwrap();

    let reporter = Reporter::new(&store);
    let value = reporter.experiment_json("exp-1").expect("experiment_json");

    let checks = value["failed_checks"].as_array().expect("array");
    assert_eq!(checks.len(), 1);
    assert!(checks[0].as_str().unwrap().contains("exit code"));
}

#[test]
fn race_json_ranks_the_leaderboard_and_includes_full_experiment_records() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    seed_scored_experiment(
        &store,
        "exp-a",
        "task-1",
        "eval-1",
        "agent-a",
        5.0,
        DiffStats::default(),
    );
    seed_scored_experiment(
        &store,
        "exp-b",
        "task-1",
        "eval-1",
        "agent-b",
        59.0,
        DiffStats {
            files_changed: 20,
            lines_added: 190,
            lines_removed: 0,
        },
    );
    let race = common::race_record("race-1", "task-1", &[("exp-b", 0), ("exp-a", 1)]);
    store.save_race(&race).unwrap();

    let reporter = Reporter::new(&store);
    let value = reporter.race_json("race-1").expect("race_json");

    let leaderboard = value["leaderboard"].as_array().expect("leaderboard array");
    assert_eq!(leaderboard.len(), 2);
    assert_eq!(leaderboard[0]["rank"].as_u64(), Some(1));
    assert_eq!(leaderboard[0]["id"].as_str(), Some("exp-a"));
    assert_eq!(leaderboard[1]["rank"].as_u64(), Some(2));
    assert_eq!(leaderboard[1]["id"].as_str(), Some("exp-b"));
}

#[test]
fn bisect_json_round_trips_the_persisted_record() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mut record = common::bisect_record("bisect-1", "task-1", "eval-1");
    record.culprit = Some("sha-bad".to_string());
    store.save_bisect(&record).unwrap();

    let reporter = Reporter::new(&store);
    let value = reporter.bisect_json("bisect-1").expect("bisect_json");

    assert_eq!(value["id"].as_str(), Some("bisect-1"));
    assert_eq!(value["culprit"].as_str(), Some("sha-bad"));
}

// ---- format_scorecard (shared by render_experiment and the `score` CLI command) ---------------

#[test]
fn format_scorecard_never_hides_raw_measurements_behind_the_total() {
    let evaluator = common::noop_evaluator_spec("eval-1");
    let baseline = good_verdict(10.0);
    let metrics = common::raw_metrics(
        good_verdict(31.0),
        DiffStats {
            files_changed: 3,
            lines_added: 100,
            lines_removed: 84,
        },
    );
    let weights = agentforge::scoring::default_weights();
    let score = agentforge::scoring::score(&metrics, &baseline, &evaluator, &weights);

    let text = agentforge::report::format_scorecard(
        &metrics,
        Some(&baseline),
        Some(&evaluator),
        &score,
        false,
    );

    assert!(text.contains("143/143"));
    assert!(text.contains("31s"));
    assert!(text.contains(&score.total.to_string()));
}
