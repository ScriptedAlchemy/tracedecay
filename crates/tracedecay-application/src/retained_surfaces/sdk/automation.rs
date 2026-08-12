use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{RunId, UtcMicros};

use super::{LcmGrepSortV1, LcmRoleV1, LcmSearchScopeV1};

const MAX_AUTOMATION_REVIEW_LIMIT: u32 = 1_000;
const MAX_AUTOMATION_EVIDENCE_LIMIT: u32 = 50;
const MAX_AUTOMATION_RECENT_SESSION_LIMIT: u32 = 10;

/// Automatic memory capability selected after one registered application
/// admission. Skill-only automation remains outside this effect family.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAutomationTaskV1 {
    MemoryCurator,
    SessionReflector,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryCuratorRunInputV1 {
    pub fact_review_limit: u32,
    pub min_confidence_millionths: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionReflectorRunInputV1 {
    pub provider: String,
    pub query: String,
    pub scope: LcmSearchScopeV1,
    pub session_id: Option<String>,
    pub include_summaries: bool,
    pub evidence_limit: u32,
    pub include_recent_sessions: bool,
    pub recent_sessions_limit: u32,
    pub sort: LcmGrepSortV1,
    pub source: Option<String>,
    pub role: Option<LcmRoleV1>,
    pub start_time: Option<UtcMicros>,
    pub end_time: Option<UtcMicros>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "options",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum MemoryAutomationTaskRequestV1 {
    MemoryCurator(MemoryCuratorRunInputV1),
    SessionReflector(SessionReflectorRunInputV1),
}

impl MemoryAutomationTaskRequestV1 {
    pub const fn task(&self) -> MemoryAutomationTaskV1 {
        match self {
            Self::MemoryCurator(_) => MemoryAutomationTaskV1::MemoryCurator,
            Self::SessionReflector(_) => MemoryAutomationTaskV1::SessionReflector,
        }
    }

    fn validate(&self) -> bool {
        match self {
            Self::MemoryCurator(options) => {
                (1..=MAX_AUTOMATION_REVIEW_LIMIT).contains(&options.fact_review_limit)
                    && options.min_confidence_millionths <= 1_000_000
            }
            Self::SessionReflector(options) => valid_reflector_options(options),
        }
    }
}

/// Canonical input to one durable automatic-memory run.
///
/// Trigger, actor, configuration and input digests are derived by the
/// registered application authority. The tagged task prevents a caller from
/// pairing one task identity with another task's options.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAutomationRunRequestV1 {
    pub run_id: RunId,
    pub task: MemoryAutomationTaskRequestV1,
}

impl MemoryAutomationRunRequestV1 {
    pub const fn task_kind(&self) -> MemoryAutomationTaskV1 {
        self.task.task()
    }

    pub fn validate(&self) -> bool {
        self.run_id.validate().is_ok() && self.task.validate()
    }
}

fn valid_reflector_options(options: &SessionReflectorRunInputV1) -> bool {
    valid_text(&options.provider)
        && valid_text(&options.query)
        && (1..=MAX_AUTOMATION_EVIDENCE_LIMIT).contains(&options.evidence_limit)
        && (1..=MAX_AUTOMATION_RECENT_SESSION_LIMIT).contains(&options.recent_sessions_limit)
        && options.session_id.as_deref().is_none_or(valid_text)
        && options.source.as_deref().is_none_or(valid_text)
        && options
            .start_time
            .zip(options.end_time)
            .is_none_or(|(start, end)| start.0 <= end.0)
}

fn valid_text(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= 4_096 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MemoryAutomationRunRequestV1, MemoryAutomationTaskV1};

    fn reflector_request() -> serde_json::Value {
        json!({
            "run_id": "run.memory.test",
            "task": {
                "kind": "session_reflector",
                "options": {
                "provider": "codex",
                "query": "canonical memory evidence",
                "scope": "all",
                "session_id": null,
                "include_summaries": true,
                "evidence_limit": 10,
                "include_recent_sessions": true,
                "recent_sessions_limit": 3,
                "sort": "recency",
                "source": null,
                "role": null,
                "start_time": null,
                    "end_time": null
                }
            }
        })
    }

    #[test]
    fn request_is_task_tagged_and_rejects_approval_or_proposal_fields() {
        let request = serde_json::from_value::<MemoryAutomationRunRequestV1>(reflector_request())
            .expect("canonical automatic memory request");
        assert_eq!(
            request.task_kind(),
            MemoryAutomationTaskV1::SessionReflector
        );
        assert!(request.validate());

        for field in ["input", "input_digest", "approved", "proposal_id"] {
            let mut invalid = reflector_request();
            invalid[field] = json!("caller-controlled");
            assert!(serde_json::from_value::<MemoryAutomationRunRequestV1>(invalid).is_err());
        }
    }

    #[test]
    fn task_options_are_closed_and_bounded() {
        let mut wrong_task_options = reflector_request();
        wrong_task_options["task"]["options"] = json!({
            "fact_review_limit": 24,
            "min_confidence_millionths": 720000
        });
        assert!(
            serde_json::from_value::<MemoryAutomationRunRequestV1>(wrong_task_options).is_err()
        );

        let mut proposal_nested = reflector_request();
        proposal_nested["task"]["options"]["proposal_id"] = json!("proposal.legacy");
        assert!(serde_json::from_value::<MemoryAutomationRunRequestV1>(proposal_nested).is_err());

        let mut unbounded = reflector_request();
        unbounded["task"]["options"]["evidence_limit"] = json!(51);
        let unbounded = serde_json::from_value::<MemoryAutomationRunRequestV1>(unbounded)
            .expect("typed but semantically unbounded request");
        assert!(!unbounded.validate());
    }

    #[test]
    fn combined_and_skill_authority_fields_cannot_enter_a_memory_request() {
        let mut cross_authority = reflector_request();
        cross_authority["task"]["options"]["skill_writer"] = json!({
            "provider": "codex",
            "query": "skill evidence",
            "evidence_limit": 10,
            "include_recent_sessions": true,
            "recent_sessions_limit": 3
        });
        assert!(serde_json::from_value::<MemoryAutomationRunRequestV1>(cross_authority).is_err());

        let combined = json!({
            "run_id": "run.memory.combined",
            "task": {
                "kind": "combined_review",
                "options": {"session_reflector": reflector_request()["task"]["options"]}
            }
        });
        assert!(serde_json::from_value::<MemoryAutomationRunRequestV1>(combined).is_err());
    }
}
