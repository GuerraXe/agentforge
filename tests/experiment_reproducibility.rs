//! Experiment seed reproducibility — SPEC.md §12, §20 (R2/T1), docs/ARCHITECTURE.md §10.
//!
//! Two angles: (1) a fixed task + a deterministic agent script must produce the same observable
//! result across repeated `run`s; (2) `race_index` assignment must be a pure function of the
//! declared `(agents, repeat)` inputs, fixed before any execution starts — never dependent on
//! wall-clock timing, which is what made v1's tie-break non-reproducible (SPEC.md §20, R2/T1).

mod common;

#[test]
fn experiment_run_produces_the_same_patch_for_a_fixed_deterministic_agent_script() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let task = common::task_spec("task-repro", &head, "noop", common::good_verdict());
    let (runner, store) = common::experiment_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let policy = common::valid_permission_policy("default");
    let agent_config = common::fake_agent_config("fake");
    let adapter = common::scripted_fake_adapter("hello-agentforge");

    let first = runner
        .run(&task, adapter.as_ref(), &agent_config, &policy)
        .expect("first run");
    let second = runner
        .run(&task, adapter.as_ref(), &agent_config, &policy)
        .expect("second run");

    let first_patch = std::fs::read_to_string(&first.patch_path).expect("read first patch");
    let second_patch = std::fs::read_to_string(&second.patch_path).expect("read second patch");
    assert_eq!(
        first_patch, second_patch,
        "the same task run against the same deterministic agent script must produce the same \
         captured patch across repeated runs"
    );

    let first_build = first
        .raw_metrics
        .as_ref()
        .map(|m| m.verdict.build_succeeded);
    let second_build = second
        .raw_metrics
        .as_ref()
        .map(|m| m.verdict.build_succeeded);
    assert_eq!(
        first_build,
        Some(true),
        "the noop evaluator's test_cmd trivially succeeds, so the verdict must be a real, \
         populated one, not a Failed/empty run: {:?}",
        first.status
    );
    assert_eq!(
        first_build, second_build,
        "the evaluator's verdict on an identical patch must not vary run to run"
    );
}

#[test]
fn each_run_gets_its_own_worktree_even_for_the_same_task_and_agent() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let task = common::task_spec("task-isolated", &head, "noop", common::good_verdict());
    let (runner, store) = common::experiment_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let policy = common::valid_permission_policy("default");
    let agent_config = common::fake_agent_config("fake");
    let adapter = common::scripted_fake_adapter("marker");

    let first = runner
        .run(&task, adapter.as_ref(), &agent_config, &policy)
        .expect("first run");
    let second = runner
        .run(&task, adapter.as_ref(), &agent_config, &policy)
        .expect("second run");

    assert_ne!(
        first.worktree_path, second.worktree_path,
        "repeated runs must never reuse or collide on a worktree path — SPEC.md §7"
    );
    assert_ne!(
        first.id, second.id,
        "repeated runs must produce distinct experiment ids"
    );
}

#[test]
fn race_index_assignment_is_deterministic_across_repeated_races_with_the_same_inputs() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let task = common::task_spec("task-race-repro", &head, "noop", common::good_verdict());
    let (race_runner, store) = common::race_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");

    let agents = vec![
        (
            common::fake_agent_config("a"),
            common::scripted_fake_adapter("a-script"),
        ),
        (
            common::fake_agent_config("b"),
            common::scripted_fake_adapter("b-script"),
        ),
    ];

    let race_1 = race_runner
        .run_race(&task, &agents, 2, 2)
        .expect("first race");
    let race_2 = race_runner
        .run_race(&task, &agents, 2, 2)
        .expect("second race");

    for race in [&race_1, &race_2] {
        assert_eq!(
            race.participants.len(),
            4,
            "2 agents x repeat 2 must produce exactly 4 participants"
        );
        let mut indices: Vec<u32> = race
            .participants
            .iter()
            .map(|(_, p)| p.race_index)
            .collect();
        indices.sort_unstable();
        assert_eq!(
            indices,
            vec![0, 1, 2, 3],
            "race_index must cover 0..participants.len() exactly once, every time, regardless \
             of execution timing"
        );
    }
}
