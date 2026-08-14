//! `RaceRunner::run_race` — SPEC.md §12, docs/ARCHITECTURE.md §10.
//!
//! Built entirely on `FakeAdapter` (SPEC.md §18: "no test may require the real `claude`
//! binary"). Covers what `tests/experiment_reproducibility.rs`'s race test doesn't: every
//! participant starting from the same base repository state in its own isolated worktree, that
//! ranking (computed by `report::Reporter`, not by `race` itself) is correct against real,
//! evaluator-distinguishable patches, that one participant's internal failure never aborts the
//! others, and that cleanup (worktrees, `RUNNING.lock`) happens for every participant regardless
//! of outcome.

mod common;

use agentforge::domain::{Cmd, EvaluatorSpec, ExperimentStatus};
use agentforge::report::Reporter;

fn detecting_evaluator(id: &str, marker: &str) -> EvaluatorSpec {
    let (program, args) = if cfg!(windows) {
        (
            "cmd".to_string(),
            vec![
                "/C".to_string(),
                format!("findstr /C:{marker} app.txt >nul && exit /b 0 || exit /b 1"),
            ],
        )
    } else {
        (
            "sh".to_string(),
            vec![
                "-c".to_string(),
                format!("grep -q {marker} app.txt && exit 0 || exit 1"),
            ],
        )
    };
    EvaluatorSpec {
        id: id.to_string(),
        setup_cmds: vec![],
        test_cmd: Cmd {
            program,
            args,
            cwd_relative: ".".into(),
        },
        timeout_secs: 30,
        budget_secs: 60,
        size_budget_lines: 200,
        metric_extractors: vec![],
    }
}

#[test]
fn every_participant_starts_from_the_same_base_ref_in_its_own_isolated_worktree() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let (race_runner, store) = common::race_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-equal-start", &head, "noop", common::good_verdict());

    let agents = vec![
        (
            common::fake_agent_config("a"),
            common::scripted_fake_adapter("a"),
        ),
        (
            common::fake_agent_config("b"),
            common::scripted_fake_adapter("b"),
        ),
    ];

    let before_status = common::run_git(repo.path(), &["status", "--porcelain"]);
    let race = race_runner
        .run_race(&task, &agents, 1, 2)
        .expect("run_race");
    let after_status = common::run_git(repo.path(), &["status", "--porcelain"]);

    assert_eq!(race.participants.len(), 2);
    let mut worktree_paths = std::collections::HashSet::new();
    for (experiment_id, _) in &race.participants {
        let record = store
            .load_experiment(experiment_id)
            .expect("load_experiment");
        assert_eq!(
            record.base_ref, head,
            "every participant must start from the task's resolved base_ref"
        );
        assert!(
            worktree_paths.insert(record.worktree_path.clone()),
            "each participant must get its own isolated worktree, never a shared/reused path"
        );
    }

    assert_eq!(
        before_status.stdout, after_status.stdout,
        "the primary checkout must be untouched by a race — SPEC.md §7"
    );
}

#[test]
fn race_index_maps_to_agents_in_listed_order_then_repeat() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let (race_runner, store) = common::race_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-index-map", &head, "noop", common::good_verdict());

    let agents = vec![
        (
            common::fake_agent_config("first-agent"),
            common::scripted_fake_adapter("first"),
        ),
        (
            common::fake_agent_config("second-agent"),
            common::scripted_fake_adapter("second"),
        ),
    ];

    let race = race_runner
        .run_race(&task, &agents, 2, 4)
        .expect("run_race");
    assert_eq!(race.participants.len(), 4);

    // agents (listed order) x repeat: race_index 0,1 => first-agent; 2,3 => second-agent —
    // SPEC.md §12.
    let mut by_index: Vec<(u32, String)> = Vec::new();
    for (experiment_id, participant) in &race.participants {
        let record = store
            .load_experiment(experiment_id)
            .expect("load_experiment");
        by_index.push((participant.race_index, record.agent_config.adapter));
    }
    by_index.sort_by_key(|(index, _)| *index);
    let adapters: Vec<&str> = by_index.iter().map(|(_, a)| a.as_str()).collect();
    assert_eq!(
        adapters,
        vec!["first-agent", "first-agent", "second-agent", "second-agent"]
    );
}

#[test]
fn ranking_is_correct_against_real_evaluator_distinguishable_patches() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "app.txt", "BROKEN\n", "seed");
    let head = common::head_sha(repo.path());
    let (race_runner, store) = common::race_runner_with_store(repo.path());
    store
        .save_evaluator(&detecting_evaluator("detect", "FIXED"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-ranking", &head, "detect", common::good_verdict());

    let agents = vec![
        (
            common::fake_agent_config("winner"),
            common::file_writing_fake_adapter("app.txt", "FIXED"),
        ),
        (
            common::fake_agent_config("loser"),
            common::scripted_fake_adapter("does-not-touch-app-txt"),
        ),
    ];

    let race = race_runner
        .run_race(&task, &agents, 1, 2)
        .expect("run_race");
    assert_eq!(race.participants.len(), 2);

    // No race-specific scoring logic exists — this is `scoring::score` (called once per
    // participant inside `ExperimentRunner::run`) plus `report::Reporter`'s live-computed
    // leaderboard (SPEC.md §20, D3), exercised end to end.
    let reporter = Reporter::new(&store);
    let leaderboard = reporter.race_json(&race.id).expect("race_json");
    let entries = leaderboard["leaderboard"]
        .as_array()
        .expect("leaderboard array");
    assert_eq!(entries.len(), 2);

    let first_agent_config = &entries[0]["agent_config"];
    assert_eq!(
        first_agent_config["adapter"], "winner",
        "the participant that actually fixed the file must rank first: {leaderboard:#}"
    );
    assert_eq!(entries[0]["status"], "Completed");
    let first_score = entries[0]["score"]["total"].as_u64().unwrap();
    let second_score = entries[1]["score"]["total"].as_u64().unwrap();
    assert!(
        first_score >= second_score,
        "leaderboard must be sorted by score.total descending: {first_score} vs {second_score}"
    );
    assert!(
        entries[1]["score"]["gated"].as_bool().unwrap_or(false),
        "the participant that never fixed app.txt must trip the correctness gate: {leaderboard:#}"
    );
}

/// SPEC.md §17/§18 (row 19, R2/T1 resolution): "on a `FakeAdapter` fixture scripted so two
/// participants produce identical `score.total`, the leaderboard orders them by ascending
/// `race_index` in 20/20 repeated invocations" — a repetition-based flake check for concurrent
/// completion order, not a timing-based one. `report::rank_participants`'s comparator
/// (`b_total.cmp(&a_total).then(a.race_index.cmp(&b.race_index))`) makes this deterministic by
/// construction once every participant's `ExperimentRecord` is persisted, but participants finish
/// in a genuinely nondeterministic order under `max_parallel` fan-out — this proves race_index
/// (stamped before any participant runs, SPEC.md §12) survives that, not just the sort itself.
#[test]
fn leaderboard_tie_break_by_race_index_is_stable_across_20_repeated_runs() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "app.txt", "BROKEN\n", "seed");
    let head = common::head_sha(repo.path());
    let (race_runner, store) = common::race_runner_with_store(repo.path());
    store
        .save_evaluator(&detecting_evaluator("detect", "FIXED"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-tie", &head, "detect", common::good_verdict());
    let reporter = Reporter::new(&store);

    for i in 0..20 {
        let agents = vec![
            (
                common::fake_agent_config("agent-a"),
                common::file_writing_fake_adapter("app.txt", "FIXED"),
            ),
            (
                common::fake_agent_config("agent-b"),
                common::file_writing_fake_adapter("app.txt", "FIXED"),
            ),
        ];
        let race = race_runner
            .run_race(&task, &agents, 1, 2)
            .expect("run_race");
        let leaderboard = reporter.race_json(&race.id).expect("race_json");
        let entries = leaderboard["leaderboard"]
            .as_array()
            .expect("leaderboard array");
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0]["score"]["total"], entries[1]["score"]["total"],
            "both participants must produce an identical (tied) score.total for this to be a \
             real tie-break test — iteration {i}: {leaderboard:#}"
        );
        let first_index = entries[0]["race_index"].as_u64().unwrap();
        let second_index = entries[1]["race_index"].as_u64().unwrap();
        assert!(
            first_index < second_index,
            "iteration {i}: a tied score must order by ascending race_index ({first_index} vs \
             {second_index}): {leaderboard:#}"
        );
    }
}

#[test]
fn one_participants_internal_failure_does_not_abort_the_others() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let (race_runner, store) = common::race_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let task = common::task_spec(
        "task-partial-failure",
        &head,
        "noop",
        common::good_verdict(),
    );

    let agents = vec![
        (
            common::fake_agent_config("broken"),
            common::broken_fake_adapter(),
        ),
        (
            common::fake_agent_config("healthy"),
            common::scripted_fake_adapter("fine"),
        ),
    ];

    let race = race_runner
        .run_race(&task, &agents, 1, 2)
        .expect("run_race must succeed even though one participant fails internally");

    assert_eq!(
        race.participants.len(),
        2,
        "both participants must still produce a persisted ExperimentRecord — SPEC.md §12 (F3)"
    );

    let mut statuses = std::collections::HashMap::new();
    for (experiment_id, _) in &race.participants {
        let record = store
            .load_experiment(experiment_id)
            .expect("load_experiment");
        statuses.insert(record.agent_config.adapter.clone(), record.status);
    }
    assert_eq!(statuses.get("broken"), Some(&ExperimentStatus::Failed));
    assert_eq!(
        statuses.get("healthy"),
        Some(&ExperimentStatus::Completed),
        "the healthy participant must still run to completion despite the other's failure"
    );
}

/// Adversarial-review regression (docs/ADVERSARIAL_REVIEW.md): before `RaceRunner::run_race`
/// wrapped each participant thread in `std::panic::catch_unwind`, a single panicking participant
/// would unwind straight past `std::thread::scope`, losing every already-collected result for the
/// *whole* race — not just failing that one participant, the way an ordinary internal failure
/// does (see `one_participants_internal_failure_does_not_abort_the_others` above). This proves a
/// panic is now isolated exactly the same way.
#[test]
fn one_participants_panic_does_not_abort_the_others() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let (race_runner, store) = common::race_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let task = common::task_spec(
        "task-panic-isolation",
        &head,
        "noop",
        common::good_verdict(),
    );

    let agents = vec![
        (
            common::fake_agent_config("panicking"),
            common::panicking_fake_adapter(),
        ),
        (
            common::fake_agent_config("healthy"),
            common::scripted_fake_adapter("fine"),
        ),
    ];

    let race = race_runner
        .run_race(&task, &agents, 1, 2)
        .expect("run_race must return Ok even though one participant's thread panicked");

    assert_eq!(
        race.participants.len(),
        1,
        "the panicking participant produced no ExperimentRecord to stamp (same as a pre-record \
         failure), but the healthy one must still be persisted and listed"
    );
    let (experiment_id, _) = &race.participants[0];
    let record = store
        .load_experiment(experiment_id)
        .expect("load_experiment");
    assert_eq!(
        record.agent_config.adapter, "healthy",
        "the healthy participant must still run to completion despite the other's panic"
    );
    assert_eq!(record.status, ExperimentStatus::Completed);
}

#[test]
fn every_participants_worktree_and_lock_are_cleaned_up_after_the_race() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let (race_runner, store) = common::race_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-cleanup", &head, "noop", common::good_verdict());

    let agents = vec![
        (
            common::fake_agent_config("broken"),
            common::broken_fake_adapter(),
        ),
        (
            common::fake_agent_config("healthy"),
            common::scripted_fake_adapter("fine"),
        ),
    ];

    let race = race_runner
        .run_race(&task, &agents, 1, 2)
        .expect("run_race");

    for (experiment_id, _) in &race.participants {
        let record = store
            .load_experiment(experiment_id)
            .expect("load_experiment");
        assert!(
            !record.worktree_path.exists(),
            "every participant's worktree must be removed once its experiment finalizes, \
             regardless of outcome: {}",
            record.worktree_path.display()
        );
        let lock_path = record
            .audit_log_path
            .parent()
            .expect("audit log has a parent dir")
            .join("RUNNING.lock");
        assert!(
            !lock_path.exists(),
            "RUNNING.lock must be cleared for every participant, regardless of outcome"
        );
    }
}

#[test]
fn max_parallel_one_still_produces_a_correct_race() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let (race_runner, store) = common::race_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-serial", &head, "noop", common::good_verdict());

    let agents = vec![
        (
            common::fake_agent_config("a"),
            common::scripted_fake_adapter("a"),
        ),
        (
            common::fake_agent_config("b"),
            common::scripted_fake_adapter("b"),
        ),
    ];

    let race = race_runner
        .run_race(&task, &agents, 1, 1)
        .expect("run_race with max_parallel=1 (fully serial)");

    assert_eq!(race.max_parallel, 1);
    assert_eq!(race.participants.len(), 2);
    let mut indices: Vec<u32> = race
        .participants
        .iter()
        .map(|(_, p)| p.race_index)
        .collect();
    indices.sort_unstable();
    assert_eq!(indices, vec![0, 1]);
}
