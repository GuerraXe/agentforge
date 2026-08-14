//! `Store`'s experiment/race/bisect persistence — SPEC.md §5, §12, §13. Mirrors tests/store.rs's
//! structure for the result-record collections (no `--force`/collision semantics here, unlike
//! task/evaluator: these are always-overwrite, per docs/store/mod.rs's doc comments).

mod common;

use agentforge::domain::{DiffStats, EvaluatorVerdict, ExperimentStatus};

fn good_verdict() -> EvaluatorVerdict {
    EvaluatorVerdict {
        build_succeeded: true,
        tests_total: Some(10),
        tests_passed: Some(10),
        exit_code: 0,
        timed_out: false,
        wall_time_secs: 12.0,
    }
}

#[test]
fn save_then_load_experiment_round_trips_manifest_only() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let record = common::experiment_record("exp-1", "task-1", common::fake_agent_config("a"));

    store.save_experiment(&record).expect("save_experiment");
    let loaded = store.load_experiment("exp-1").expect("load_experiment");

    assert_eq!(loaded.id, record.id);
    assert_eq!(loaded.task_id, record.task_id);
    assert_eq!(loaded.status, ExperimentStatus::Running);
    assert!(
        loaded.raw_metrics.is_none(),
        "a still-Running experiment has no raw metrics yet"
    );
    assert!(loaded.score.is_none());
}

#[test]
fn save_then_load_experiment_round_trips_metrics_and_score() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mut record = common::experiment_record("exp-1", "task-1", common::fake_agent_config("a"));
    record.status = ExperimentStatus::Completed;
    let metrics = common::raw_metrics(
        good_verdict(),
        DiffStats {
            files_changed: 3,
            lines_added: 100,
            lines_removed: 84,
        },
    );
    let weights = agentforge::scoring::default_weights();
    let score =
        agentforge::scoring::score(&metrics, &good_verdict(), &weights_evaluator(), &weights);
    record.raw_metrics = Some(metrics.clone());
    record.score = Some(score.clone());

    store.save_experiment(&record).expect("save_experiment");
    let loaded = store.load_experiment("exp-1").expect("load_experiment");

    assert_eq!(loaded.raw_metrics, Some(metrics));
    let loaded_score = loaded.score.expect("score persisted");
    assert_eq!(loaded_score.total, score.total);
    assert_eq!(loaded_score.components.len(), score.components.len());
}

#[test]
fn load_experiment_reports_not_found_for_an_unknown_id() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    let result = store.load_experiment("does-not-exist");

    assert!(
        matches!(result, Err(agentforge::store::Error::NotFound(_))),
        "loading a never-saved experiment id must be a clean NotFound: {result:?}"
    );
}

#[test]
fn save_experiment_overwrites_without_a_force_flag() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mut record = common::experiment_record("exp-1", "task-1", common::fake_agent_config("a"));
    store.save_experiment(&record).expect("first save");

    record.status = ExperimentStatus::Failed;
    store
        .save_experiment(&record)
        .expect("re-save without --force must not collide");

    let loaded = store.load_experiment("exp-1").expect("load_experiment");
    assert_eq!(loaded.status, ExperimentStatus::Failed);
}

#[test]
fn save_then_load_race_round_trips_participants() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let race = common::race_record("race-1", "task-1", &[("exp-a", 0), ("exp-b", 1)]);

    store.save_race(&race).expect("save_race");
    let loaded = store.load_race("race-1").expect("load_race");

    assert_eq!(loaded.id, race.id);
    assert_eq!(loaded.participants.len(), 2);
    assert_eq!(loaded.participants[0].0, "exp-a");
    assert_eq!(loaded.participants[0].1.race_index, 0);
    assert_eq!(loaded.participants[1].0, "exp-b");
    assert_eq!(loaded.participants[1].1.race_index, 1);
}

#[test]
fn load_race_reports_not_found_for_an_unknown_id() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    let result = store.load_race("does-not-exist");

    assert!(matches!(result, Err(agentforge::store::Error::NotFound(_))));
}

#[test]
fn save_then_load_bisect_round_trips_steps_and_culprit() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let mut record = common::bisect_record("bisect-1", "task-1", "eval-1");
    record.steps = vec![
        common::bisect_step("sha1", good_verdict(), true),
        common::bisect_step(
            "sha2",
            EvaluatorVerdict {
                build_succeeded: false,
                ..good_verdict()
            },
            false,
        ),
    ];
    record.culprit = Some("sha2".to_string());

    store.save_bisect(&record).expect("save_bisect");
    let loaded = store.load_bisect("bisect-1").expect("load_bisect");

    assert_eq!(loaded.steps.len(), 2);
    assert_eq!(loaded.steps[0].commit, "sha1");
    assert!(loaded.steps[0].is_good);
    assert_eq!(loaded.steps[1].commit, "sha2");
    assert!(!loaded.steps[1].is_good);
    assert_eq!(loaded.culprit, Some("sha2".to_string()));
}

#[test]
fn save_then_load_bisect_with_no_culprit_round_trips_none() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let record = common::bisect_record("bisect-1", "task-1", "eval-1");

    store.save_bisect(&record).expect("save_bisect");
    let loaded = store.load_bisect("bisect-1").expect("load_bisect");

    assert_eq!(loaded.steps.len(), 0);
    assert_eq!(loaded.culprit, None);
}

#[test]
fn load_scoring_weights_falls_back_to_built_in_defaults_when_no_scoring_toml_exists() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());

    let weights = store.load_scoring_weights().expect("load_scoring_weights");

    assert_eq!(weights.source, "built-in-default");
    assert_eq!(weights.correctness, 80.0);
    assert_eq!(weights.rating_bands.excellent, 90);
}

#[test]
fn load_scoring_weights_reads_scoring_toml_when_present_and_stamps_its_own_path_as_source() {
    let repo = common::init_temp_repo();
    let store = common::store(repo.path());
    let toml = r#"
correctness = 70.0
efficiency = 20.0
parsimony = 10.0
formula_version = "v1"

[rating_bands]
excellent = 95
good = 80
fair = 50
poor = 25
"#;
    std::fs::create_dir_all(repo.path().join(".agentforge")).unwrap();
    std::fs::write(repo.path().join(".agentforge/scoring.toml"), toml).unwrap();

    let weights = store.load_scoring_weights().expect("load_scoring_weights");

    assert_eq!(weights.correctness, 70.0);
    assert_eq!(weights.rating_bands.excellent, 95);
    assert!(
        weights.source.ends_with("scoring.toml"),
        "source should be stamped with the resolved path, got {:?}",
        weights.source
    );
}

fn weights_evaluator() -> agentforge::domain::EvaluatorSpec {
    common::noop_evaluator_spec("eval-1")
}
