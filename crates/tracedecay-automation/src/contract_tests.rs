use crate::artifact_policy::artifact_policy;
use crate::backend::AgentTaskKind;
use crate::text::truncate_chars_for_prompt;
use crate::{AutomationError, AutomationRunRecord};
use serde_json::Value;

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
fn automation_error_preserves_standard_classifications() {
    let json = serde_json::from_str::<Value>("{").unwrap_err();
    let json: AutomationError = json.into();
    assert!(matches!(json, AutomationError::Json(_)));

    let config = AutomationError::config("invalid schedule");
    assert!(matches!(config, AutomationError::Config { .. }));
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
    assert!(!policy.handoff_test().is_empty());
    assert!(!policy.eval_replay_command().is_empty());
}

#[test]
fn prompt_truncation_counts_unicode_scalars() {
    assert_eq!(truncate_chars_for_prompt("a☺bc", 2), "a☺");
    assert_eq!(truncate_chars_for_prompt("a☺bc", 4), "a☺bc");
}
