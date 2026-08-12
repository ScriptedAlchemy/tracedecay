use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{ManifestDigest, RunId, UtcMicros, canonical_sha256};

use super::{LcmGrepSortV1, LcmRoleV1, LcmSearchScopeV1};
use crate::ApplicationContractError;

const MAX_AUTOMATION_REVIEW_LIMIT: u32 = 1_000;
const MAX_AUTOMATION_EVIDENCE_LIMIT: u32 = 50;
const MAX_AUTOMATION_RECENT_SESSION_LIMIT: u32 = 10;
const AUTOMATION_RUN_REQUEST_DIGEST_DOMAIN: &str = "tracedecay.automation-run.request-identity.v1";

/// Automation capability selected after one registered application admission.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTaskV1 {
    MemoryCurator,
    SessionReflector,
    SkillWriter,
    CombinedReview,
    UserJob,
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
#[serde(deny_unknown_fields)]
pub struct SkillWriterRunInputV1 {
    pub provider: String,
    pub query: String,
    pub evidence_limit: u32,
    pub include_recent_sessions: bool,
    pub recent_sessions_limit: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CombinedReviewRunInputV1 {
    pub session_reflector: SessionReflectorRunInputV1,
    pub skill_writer: SkillWriterRunInputV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UserJobRunInputV1 {
    pub job_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "options",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AutomationTaskRequestV1 {
    MemoryCurator(MemoryCuratorRunInputV1),
    SessionReflector(SessionReflectorRunInputV1),
    SkillWriter(SkillWriterRunInputV1),
    CombinedReview(CombinedReviewRunInputV1),
    UserJob(UserJobRunInputV1),
}

impl AutomationTaskRequestV1 {
    pub const fn task(&self) -> AutomationTaskV1 {
        match self {
            Self::MemoryCurator(_) => AutomationTaskV1::MemoryCurator,
            Self::SessionReflector(_) => AutomationTaskV1::SessionReflector,
            Self::SkillWriter(_) => AutomationTaskV1::SkillWriter,
            Self::CombinedReview(_) => AutomationTaskV1::CombinedReview,
            Self::UserJob(_) => AutomationTaskV1::UserJob,
        }
    }

    fn validate(&self) -> bool {
        match self {
            Self::MemoryCurator(options) => {
                (1..=MAX_AUTOMATION_REVIEW_LIMIT).contains(&options.fact_review_limit)
                    && options.min_confidence_millionths <= 1_000_000
            }
            Self::SessionReflector(options) => valid_reflector_options(options),
            Self::SkillWriter(options) => valid_skill_writer_options(options),
            Self::CombinedReview(options) => {
                valid_reflector_options(&options.session_reflector)
                    && valid_skill_writer_options(&options.skill_writer)
            }
            Self::UserJob(options) => valid_text(&options.job_id),
        }
    }

    pub fn expected_external_task_key(&self) -> Option<String> {
        match self {
            Self::SkillWriter(_) | Self::CombinedReview(_) => Some("skill_writer".to_owned()),
            Self::UserJob(options) => Some(format!("user_job:{}", options.job_id)),
            Self::MemoryCurator(_) | Self::SessionReflector(_) => None,
        }
    }
}

/// Canonical input to one durable automation run.
///
/// Trigger, actor, configuration and input digests are derived by the
/// registered application authority. The tagged task prevents a caller from
/// pairing one task identity with another task's options.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AutomationRunRequestV1 {
    pub run_id: RunId,
    pub task: AutomationTaskRequestV1,
}

impl AutomationRunRequestV1 {
    pub const fn task_kind(&self) -> AutomationTaskV1 {
        self.task.task()
    }

    pub fn validate(&self) -> bool {
        self.run_id.validate().is_ok() && self.task.validate()
    }

    pub fn input_digest(&self) -> Result<ManifestDigest, ApplicationContractError> {
        if !self.validate() {
            return Err(ApplicationContractError::Inconsistent {
                field: "automation run request",
            });
        }
        Ok(canonical_sha256(&(
            AUTOMATION_RUN_REQUEST_DIGEST_DOMAIN,
            &self.task,
        ))?)
    }
}

fn valid_skill_writer_options(options: &SkillWriterRunInputV1) -> bool {
    valid_text(&options.provider)
        && valid_text(&options.query)
        && (1..=MAX_AUTOMATION_EVIDENCE_LIMIT).contains(&options.evidence_limit)
        && (1..=MAX_AUTOMATION_RECENT_SESSION_LIMIT).contains(&options.recent_sessions_limit)
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

    use super::{AutomationRunRequestV1, AutomationTaskV1};

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
        let request = serde_json::from_value::<AutomationRunRequestV1>(reflector_request())
            .expect("canonical automation request");
        assert_eq!(request.task_kind(), AutomationTaskV1::SessionReflector);
        assert!(request.validate());

        for field in ["input", "input_digest", "approved", "proposal_id"] {
            let mut invalid = reflector_request();
            invalid[field] = json!("caller-controlled");
            assert!(serde_json::from_value::<AutomationRunRequestV1>(invalid).is_err());
        }
    }

    #[test]
    fn task_options_are_closed_and_bounded() {
        let mut wrong_task_options = reflector_request();
        wrong_task_options["task"]["options"] = json!({
            "fact_review_limit": 24,
            "min_confidence_millionths": 720000
        });
        assert!(serde_json::from_value::<AutomationRunRequestV1>(wrong_task_options).is_err());

        let mut proposal_nested = reflector_request();
        proposal_nested["task"]["options"]["proposal_id"] = json!("proposal.legacy");
        assert!(serde_json::from_value::<AutomationRunRequestV1>(proposal_nested).is_err());

        let mut unbounded = reflector_request();
        unbounded["task"]["options"]["evidence_limit"] = json!(51);
        let unbounded = serde_json::from_value::<AutomationRunRequestV1>(unbounded)
            .expect("typed but semantically unbounded request");
        assert!(!unbounded.validate());
    }

    #[test]
    fn cross_task_options_remain_closed() {
        let mut cross_authority = reflector_request();
        cross_authority["task"]["options"]["skill_writer"] = json!({
            "provider": "codex",
            "query": "skill evidence",
            "evidence_limit": 10,
            "include_recent_sessions": true,
            "recent_sessions_limit": 3
        });
        assert!(serde_json::from_value::<AutomationRunRequestV1>(cross_authority).is_err());

        let combined = json!({
            "run_id": "run.memory.combined",
            "task": {
                "kind": "combined_review",
                "options": {"session_reflector": reflector_request()["task"]["options"]}
            }
        });
        assert!(serde_json::from_value::<AutomationRunRequestV1>(combined).is_err());
    }

    #[test]
    fn every_registered_task_has_one_closed_request_shape() {
        let task = |kind, options| {
            json!({
                "run_id": format!("run.{kind}.test"),
                "task": { "kind": kind, "options": options }
            })
        };
        let reflector = reflector_request()["task"]["options"].clone();
        let skill = json!({
            "provider": "codex",
            "query": "bounded skill evidence",
            "evidence_limit": 10,
            "include_recent_sessions": true,
            "recent_sessions_limit": 3
        });
        for request in [
            task(
                "memory_curator",
                json!({
                    "fact_review_limit": 24,
                    "min_confidence_millionths": 720000
                }),
            ),
            task("session_reflector", reflector.clone()),
            task("skill_writer", skill.clone()),
            task(
                "combined_review",
                json!({ "session_reflector": reflector, "skill_writer": skill }),
            ),
            task("user_job", json!({ "job_id": "nightly-summary" })),
        ] {
            let request = serde_json::from_value::<AutomationRunRequestV1>(request)
                .expect("registered automation request shape");
            assert!(request.validate());
        }
    }

    #[test]
    fn request_digest_and_external_key_bind_the_full_typed_admission() {
        let first = serde_json::from_value::<AutomationRunRequestV1>(reflector_request())
            .expect("reflector request");
        let mut changed_wire = reflector_request();
        changed_wire["task"]["options"]["query"] = json!("different evidence");
        let changed = serde_json::from_value::<AutomationRunRequestV1>(changed_wire)
            .expect("changed reflector request");
        assert_ne!(
            first.input_digest().expect("first digest"),
            changed.input_digest().expect("changed digest")
        );

        let user_job = serde_json::from_value::<AutomationRunRequestV1>(json!({
            "run_id":"run.user-job.test",
            "task":{"kind":"user_job","options":{"job_id":"nightly"}}
        }))
        .expect("user-job request");
        assert_eq!(
            user_job.task.expected_external_task_key().as_deref(),
            Some("user_job:nightly")
        );
    }
}
