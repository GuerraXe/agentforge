//! `ExperimentRunner::run` — the one `run` primitive, SPEC.md §8, docs/ARCHITECTURE.md §10.
//!
//! Covers what `tests/experiment_reproducibility.rs` doesn't: status transitions (`Completed`
//! with a good verdict, `Completed` with a bad verdict, `Failed` on an AgentForge-internal
//! error, `TimedOut` on an agent process killed for exceeding its budget), worktree/lock
//! cleanup, patch capture against a real tracked-file edit, persistence through `Store`, and
//! that the primary checkout is never touched — SPEC.md §7, §17.

mod common;

use agentforge::domain::ExperimentStatus;

fn evaluator_detecting(marker: &str) -> agentforge::domain::EvaluatorSpec {
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
    agentforge::domain::EvaluatorSpec {
        id: "detect".to_string(),
        setup_cmds: vec![],
        test_cmd: agentforge::domain::Cmd {
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
fn a_completed_run_with_a_good_verdict_is_persisted_worktree_removed_lock_cleared() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "app.txt", "BROKEN\n", "seed");
    let head = common::head_sha(repo.path());
    let (runner, store) = common::experiment_runner_with_store(repo.path());
    store
        .save_evaluator(&evaluator_detecting("FIXED"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-good", &head, "detect", common::good_verdict());
    let policy = common::valid_permission_policy("default");
    let agent_config = common::fake_agent_config("fixer");
    let adapter = common::file_writing_fake_adapter("app.txt", "FIXED");

    let before_status = common::run_git(repo.path(), &["status", "--porcelain"]);
    let record = runner
        .run(&task, adapter.as_ref(), &agent_config, &policy)
        .expect("run");
    let after_status = common::run_git(repo.path(), &["status", "--porcelain"]);

    assert_eq!(
        record.status,
        ExperimentStatus::Completed,
        "an agent that fixes the file must complete with a good verdict"
    );
    let metrics = record.raw_metrics.as_ref().expect("raw_metrics populated");
    assert!(metrics.verdict.is_good());
    assert!(record.score.is_some(), "a Completed run must carry a score");

    assert!(
        !record.worktree_path.exists(),
        "the worktree must be removed once the experiment finalizes — SPEC.md §7/§17"
    );
    assert!(
        record.audit_log_path.exists(),
        "the audit log is written under the external state root, independent of the worktree"
    );
    let patch = std::fs::read_to_string(&record.patch_path).expect("patch captured");
    assert!(
        patch.contains("app.txt"),
        "the captured patch must reflect the real tracked-file edit: {patch}"
    );

    let loaded = store.load_experiment(&record.id).expect("load_experiment");
    assert_eq!(loaded.status, ExperimentStatus::Completed);
    assert_eq!(loaded.raw_metrics, record.raw_metrics);

    assert_eq!(
        before_status.stdout, after_status.stdout,
        "the primary checkout's working tree/index must never be touched by run — SPEC.md §7"
    );
}

#[test]
fn a_completed_run_with_a_bad_verdict_is_not_gated_into_failed_status() {
    let repo = common::init_temp_repo();
    common::commit_file(repo.path(), "app.txt", "BROKEN\n", "seed");
    let head = common::head_sha(repo.path());
    let (runner, store) = common::experiment_runner_with_store(repo.path());
    store
        .save_evaluator(&evaluator_detecting("FIXED"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-bad", &head, "detect", common::good_verdict());
    let policy = common::valid_permission_policy("default");
    let agent_config = common::fake_agent_config("do-nothing");
    // Writes an untracked file — `git diff` never shows it, so the tracked `app.txt` stays
    // "BROKEN" and the evaluator's verdict must be bad, not because anything crashed.
    let adapter = common::scripted_fake_adapter("does-not-fix-anything");

    let record = runner
        .run(&task, adapter.as_ref(), &agent_config, &policy)
        .expect("run");

    assert_eq!(
        record.status,
        ExperimentStatus::Completed,
        "a judged-bad patch is still a Completed experiment, not Failed — SPEC.md §6 (C2): \
         Failed means an AgentForge-internal error, not a bad agent patch"
    );
    let metrics = record.raw_metrics.as_ref().expect("raw_metrics populated");
    assert!(!metrics.verdict.is_good());
    let score = record.score.as_ref().expect("score always computed");
    assert!(
        score.gated,
        "an unfixed patch must trip the correctness gate"
    );
}

#[test]
fn a_spawn_refused_by_policy_ends_failed_not_a_panic_or_err() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let (runner, store) = common::experiment_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-denied", &head, "noop", common::good_verdict());
    let agent_config = common::fake_agent_config("fake");
    let adapter = common::scripted_fake_adapter("irrelevant");

    // A policy that denies every program — the agent's own spawn is refused before anything
    // runs, an AgentForge-internal condition distinct from a bad patch judgment.
    let mut policy = common::valid_permission_policy("deny-all");
    policy.denied_programs = vec!["cmd".to_string(), "sh".to_string()];

    let record = runner
        .run(&task, adapter.as_ref(), &agent_config, &policy)
        .expect("run must return Ok(record) even when the internal spawn is refused");

    assert_eq!(record.status, ExperimentStatus::Failed);
    assert!(record.raw_metrics.is_none());
    assert!(record.score.is_none());
    assert!(
        !record.worktree_path.exists(),
        "the worktree is still cleaned up even for a Failed run"
    );

    let loaded = store.load_experiment(&record.id).expect("load_experiment");
    assert_eq!(loaded.status, ExperimentStatus::Failed);
}

#[test]
fn an_agent_process_exceeding_its_timeout_ends_timedout_not_completed() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let (runner, store) = common::experiment_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let mut task = common::task_spec("task-timeout", &head, "noop", common::good_verdict());
    task.agent_timeout_secs = 2;
    let policy = common::valid_permission_policy("default");
    let agent_config = common::fake_agent_config("slow");
    let adapter = common::sleeping_fake_adapter(30);

    let start = std::time::Instant::now();
    let record = runner
        .run(&task, adapter.as_ref(), &agent_config, &policy)
        .expect("run");
    let elapsed = start.elapsed();

    assert_eq!(
        record.status,
        ExperimentStatus::TimedOut,
        "an agent process killed for exceeding its budget must end TimedOut, not Completed"
    );
    assert!(
        elapsed <= std::time::Duration::from_secs(10),
        "a 2s-budget agent sleeping 30s must be killed within a generous bound, not run to \
         completion — SPEC.md §18 (test 5's margin): got {elapsed:?}"
    );
    let metrics = record
        .raw_metrics
        .as_ref()
        .expect("raw_metrics still populated");
    assert!(metrics.agent_timed_out);

    let loaded = store.load_experiment(&record.id).expect("load_experiment");
    assert_eq!(loaded.status, ExperimentStatus::TimedOut);
}

#[test]
fn the_running_lock_is_cleared_once_run_returns() {
    let repo = common::init_temp_repo();
    let head = common::head_sha(repo.path());
    let (runner, store) = common::experiment_runner_with_store(repo.path());
    store
        .save_evaluator(&common::noop_evaluator_spec("noop"), false)
        .expect("save_evaluator");
    let task = common::task_spec("task-lock", &head, "noop", common::good_verdict());
    let policy = common::valid_permission_policy("default");
    let agent_config = common::fake_agent_config("fake");
    let adapter = common::scripted_fake_adapter("marker");

    let record = runner
        .run(&task, adapter.as_ref(), &agent_config, &policy)
        .expect("run");

    // `RUNNING.lock` lives at `<state_root>/experiments/<id>/RUNNING.lock` — its absence (not
    // the `status` field) is what `clean`'s reconciliation keys off, SPEC.md §7.
    let lock_path = record
        .audit_log_path
        .parent()
        .expect("audit log has a parent dir")
        .join("RUNNING.lock");
    assert!(
        !lock_path.exists(),
        "RUNNING.lock must be cleared before run() returns"
    );
}
