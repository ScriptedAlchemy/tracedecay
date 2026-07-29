use std::error::Error as _;

use serde_json::{Value, json};

use crate::apply_policy::{
    MemoryApplyDecision, MemoryApplyPolicy, record_has_auto_applied_memory_ops,
};
use crate::artifact_policy::artifact_policy;
use crate::backend::AgentTaskKind;
use crate::config::AutomationConfig;
use crate::text::truncate_chars_for_prompt;
use crate::{AutomationError, AutomationRunRecord};

#[derive(Default)]
struct TestRunRecord {
    accepted_count: usize,
    validation_report: Option<Value>,
    applied_ops: Option<Value>,
}

impl AutomationRunRecord for TestRunRecord {
    fn accepted_count(&self) -> usize {
        self.accepted_count
    }

    fn validation_report(&self) -> Option<&Value> {
        self.validation_report.as_ref()
    }

    fn applied_ops(&self) -> Option<&Value> {
        self.applied_ops.as_ref()
    }
}

#[test]
fn automation_error_preserves_port_source() {
    let error = AutomationError::port(
        "agent_task_backend",
        std::io::Error::other("backend disconnected"),
    );

    assert!(matches!(
        error,
        AutomationError::Port {
            port: "agent_task_backend",
            ..
        }
    ));
    assert_eq!(
        error.source().map(ToString::to_string).as_deref(),
        Some("backend disconnected")
    );
}

#[test]
fn automation_error_preserves_standard_classifications() {
    let io: AutomationError = std::io::Error::other("disk unavailable").into();
    assert!(matches!(io, AutomationError::Io(_)));

    let json = serde_json::from_str::<Value>("{").unwrap_err();
    let json: AutomationError = json.into();
    assert!(matches!(json, AutomationError::Json(_)));

    let config = AutomationError::config("invalid schedule");
    assert!(matches!(config, AutomationError::Config { .. }));
}

#[test]
fn automation_run_record_exposes_only_policy_inputs() {
    let record = TestRunRecord {
        accepted_count: 2,
        validation_report: Some(json!({"applied": 2})),
        applied_ops: Some(json!(["proposal-1", "proposal-2"])),
    };

    assert_eq!(record.accepted_count(), 2);
    assert_eq!(
        record
            .validation_report()
            .and_then(|value| value["applied"].as_u64()),
        Some(2)
    );
    assert_eq!(
        record.applied_ops().and_then(Value::as_array).map(Vec::len),
        Some(2)
    );
}

#[test]
fn apply_policy_preserves_complete_and_partial_outcomes() {
    let config = AutomationConfig::default();
    assert_eq!(
        MemoryApplyPolicy::applied_curation_ops(&config, 2, 2).decision(),
        MemoryApplyDecision::AutoApplyAllowed
    );
    assert_eq!(
        MemoryApplyPolicy::applied_curation_ops(&config, 2, 1).decision(),
        MemoryApplyDecision::ApplyIncomplete
    );

    let applied = TestRunRecord {
        accepted_count: 2,
        validation_report: Some(json!({"applied": 2})),
        applied_ops: None,
    };
    assert!(record_has_auto_applied_memory_ops(
        AgentTaskKind::MemoryCurator,
        &applied
    ));
}

#[test]
fn artifact_policy_changes_handoff_by_acceptance() {
    let policy = artifact_policy(AgentTaskKind::SkillWriter);
    let accepted = TestRunRecord {
        accepted_count: 1,
        ..TestRunRecord::default()
    };
    let rejected = TestRunRecord::default();

    assert!(policy.next_actions(&accepted)[0].contains("managed skill"));
    assert!(policy.next_actions(&rejected)[0].contains("rejected"));
    assert_eq!(policy.handoff_tests().len(), 1);
    assert_eq!(policy.eval_replay_commands().len(), 1);
}

#[test]
fn prompt_truncation_counts_unicode_scalars() {
    assert_eq!(truncate_chars_for_prompt("a☺bc", 2), "a☺");
    assert_eq!(truncate_chars_for_prompt("a☺bc", 4), "a☺bc");
}
