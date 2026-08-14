//! Malformed configuration rejection — SPEC.md §17, §20 (S4).
//!
//! Two layers: (1) field-level validation on `EvaluatorSpec`/`PermissionPolicy` — still
//! `todo!()`, expected to fail for now; (2) TOML deserialization of a spec missing mandatory
//! fields — real `serde` behavior, these pass today and guard the wire format itself.

mod common;

#[test]
fn evaluator_spec_rejects_zero_timeout_secs() {
    let mut spec = common::noop_evaluator_spec("bad-timeout");
    spec.timeout_secs = 0;
    assert!(
        spec.validate_fields().is_err(),
        "timeout_secs = 0 must be rejected — SPEC.md §20 (S4)"
    );
}

#[test]
fn evaluator_spec_rejects_zero_budget_secs() {
    let mut spec = common::noop_evaluator_spec("bad-budget");
    spec.budget_secs = 0;
    assert!(
        spec.validate_fields().is_err(),
        "budget_secs = 0 must be rejected — it divides the efficiency score component"
    );
}

#[test]
fn evaluator_spec_rejects_zero_size_budget_lines() {
    let mut spec = common::noop_evaluator_spec("bad-size-budget");
    spec.size_budget_lines = 0;
    assert!(
        spec.validate_fields().is_err(),
        "size_budget_lines = 0 must be rejected — it divides the parsimony score component"
    );
}

#[test]
fn evaluator_spec_accepts_well_formed_fields() {
    let spec = common::noop_evaluator_spec("valid");
    assert!(
        spec.validate_fields().is_ok(),
        "a well-formed EvaluatorSpec must validate cleanly, not be rejected by mistake"
    );
}

#[test]
fn permission_policy_rejects_zero_max_wall_time_secs() {
    let mut policy = common::valid_permission_policy("bad-wall-time");
    policy.max_wall_time_secs = 0;
    assert!(
        policy.validate_fields().is_err(),
        "max_wall_time_secs = 0 must be rejected — SPEC.md §17"
    );
}

#[test]
fn permission_policy_rejects_zero_max_output_bytes() {
    let mut policy = common::valid_permission_policy("bad-output-bytes");
    policy.max_output_bytes = 0;
    assert!(
        policy.validate_fields().is_err(),
        "max_output_bytes = 0 must be rejected — SPEC.md §17"
    );
}

#[test]
fn permission_policy_accepts_well_formed_fields() {
    let policy = common::valid_permission_policy("valid");
    assert!(
        policy.validate_fields().is_ok(),
        "a well-formed PermissionPolicy must validate cleanly"
    );
}

#[test]
fn enforcement_report_tags_executor_enforced_command_and_execution_fields_as_enforced() {
    use agentforge::domain::EnforcementLevel;

    let policy = common::valid_permission_policy("valid");
    let report = policy.enforcement_report();

    for field in [
        "env_passthrough",
        "max_wall_time_secs",
        "max_output_bytes",
        "allowed_programs",
        "denied_programs",
        "allowed_roots",
    ] {
        let entry = report
            .iter()
            .find(|e| e.field == field)
            .unwrap_or_else(|| panic!("enforcement_report must include a {field} entry"));
        assert_eq!(
            entry.level,
            EnforcementLevel::Enforced,
            "{field} is checked/applied by the Executor itself and must be tagged Enforced"
        );
    }
}

#[test]
fn enforcement_report_tags_adapter_relayed_fields_as_requested_only() {
    use agentforge::domain::EnforcementLevel;

    let policy = common::valid_permission_policy("valid");
    let report = policy.enforcement_report();

    for field in ["deny_network", "extra_readonly_paths"] {
        let entry = report
            .iter()
            .find(|e| e.field == field)
            .unwrap_or_else(|| panic!("enforcement_report must include a {field} entry"));
        assert_eq!(
            entry.level,
            EnforcementLevel::RequestedOnly,
            "{field} is adapter-relayed, best-effort only, and must never be tagged Enforced"
        );
    }
}

#[test]
fn enforcement_report_tags_resource_limits_as_unsupported() {
    use agentforge::domain::EnforcementLevel;

    let policy = common::valid_permission_policy("valid");
    let report = policy.enforcement_report();

    let entry = report
        .iter()
        .find(|e| e.field == "max_memory_bytes")
        .expect("enforcement_report must include a max_memory_bytes entry");
    assert_eq!(
        entry.level,
        EnforcementLevel::Unsupported,
        "no MVP platform enforces a memory cap — this must be tagged Unsupported, never claimed \
         as enforced (SPEC.md §3.1)"
    );
}

#[test]
fn malformed_task_toml_missing_mandatory_fields_fails_to_deserialize() {
    // Real serde behavior — no implementation needed for this one, it should pass today.
    let malformed = r#"
        id = "task-1"
        name = "broken"
        prompt = "do the thing"
        repo_path = "."
        base_ref = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
        agent_timeout_secs = 30
        created_at = "2026-01-01T00:00:00Z"
    "#; // missing the mandatory `evaluator` and `baseline` fields
    let result: Result<agentforge::domain::TaskSpec, _> = toml::from_str(malformed);
    assert!(
        result.is_err(),
        "a TaskSpec TOML document missing mandatory fields must fail to parse, not silently \
         default them — a task with no evaluator id must never become usable"
    );
}

#[test]
fn well_formed_task_toml_round_trips_through_serialize_and_deserialize() {
    let task = common::task_spec("task-1", &"a".repeat(40), "eval-1", common::good_verdict());
    let text = toml::to_string(&task).expect("a well-formed TaskSpec must serialize to TOML");
    let parsed: agentforge::domain::TaskSpec =
        toml::from_str(&text).expect("the serialized TOML must deserialize back cleanly");
    assert_eq!(parsed.id, task.id);
    assert_eq!(parsed.base_ref, task.base_ref);
    assert_eq!(parsed.evaluator, task.evaluator);
    assert_eq!(parsed.baseline, task.baseline);
}

#[test]
fn malformed_evaluator_toml_with_wrong_field_type_fails_to_deserialize() {
    let malformed = r#"
        id = "eval-1"
        setup_cmds = []
        timeout_secs = "thirty"
        budget_secs = 60
        size_budget_lines = 200
        metric_extractors = []

        [test_cmd]
        program = "cargo"
        args = ["test"]
        cwd_relative = "."
    "#; // timeout_secs must be an integer, not a string
    let result: Result<agentforge::domain::EvaluatorSpec, _> = toml::from_str(malformed);
    assert!(
        result.is_err(),
        "a field with the wrong type must fail to deserialize rather than being coerced"
    );
}
