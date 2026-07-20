//! Pure, one-shot advisory feedback-cycle contracts.
//!
//! PR11 owns saved-content post-edit diagnostics and impact contracts here.
//! These values never schedule an agent, apply an edit, emit a transport
//! payload, or make dirty-overlay evidence durable.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::{
    AgentInstanceId, CommitId, DomainError, ManifestDigest, ProjectId, RepositoryId,
    RetrievalAnchorId, SessionId, WorktreeId, canonical_sha256,
};

pub mod evidence_packet;

pub use evidence_packet::FeedbackEvidencePacketV1;

const FEEDBACK_RESULT_ID_DOMAIN: &str = "tracedecay.feedback.result.v1";

macro_rules! feedback_id {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                validate_label(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), DomainError> {
                validate_label(&self.0, $field)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

feedback_id!(FeedbackCycleId, "feedback cycle id");
feedback_id!(FeedbackResultId, "feedback result id");
feedback_id!(FeedbackFindingId, "feedback finding id");

fn validate_label(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::Empty { field });
    }
    if value.trim() != value || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(DomainError::NonCanonical { field });
    }
    Ok(())
}

/// Exact repository scope used for a feedback evaluation. A path, current
/// working directory, repository display name, or mutable branch label is not
/// a substitute for this identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackScopeV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub branch_ref: String,
    pub head_commit_id: CommitId,
}

impl FeedbackScopeV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.project_id.validate()?;
        self.repository_id.validate()?;
        self.worktree_id.validate()?;
        self.head_commit_id.validate()?;
        validate_label(&self.branch_ref, "feedback branch ref")?;
        if !self.branch_ref.starts_with("refs/") {
            return Err(DomainError::NonCanonical {
                field: "feedback branch ref",
            });
        }
        Ok(())
    }
}

/// Content identity distinguishes durable saved content from an authorized
/// ephemeral document overlay. Overlay identity is deliberately local to its
/// owning session and cannot be made durable by converting it to a digest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FeedbackContentIdentityV1 {
    SavedContent {
        generation_digest: ManifestDigest,
        file_digest: ManifestDigest,
    },
    EphemeralOverlay {
        session_id: SessionId,
        agent_id: Option<AgentInstanceId>,
        document_version: u64,
        overlay_digest: ManifestDigest,
    },
}

impl FeedbackContentIdentityV1 {
    pub const fn durability(&self) -> FeedbackDurabilityV1 {
        match self {
            Self::SavedContent { .. } => FeedbackDurabilityV1::Durable,
            Self::EphemeralOverlay { .. } => FeedbackDurabilityV1::SessionOnly,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::SavedContent {
                generation_digest,
                file_digest,
            } => {
                generation_digest.validate()?;
                file_digest.validate()
            }
            Self::EphemeralOverlay {
                session_id,
                agent_id,
                document_version,
                overlay_digest,
            } => {
                session_id.validate()?;
                agent_id
                    .as_ref()
                    .map_or(Ok(()), AgentInstanceId::validate)?;
                if *document_version == 0 {
                    return Err(DomainError::NonCanonical {
                        field: "overlay document version",
                    });
                }
                overlay_digest.validate()
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackDurabilityV1 {
    Durable,
    SessionOnly,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackTriggerV1 {
    PostEditHook,
    DocumentSave,
    ExplicitDiagnostics,
    AgentStopGate,
}

/// Bounds for one deliberate evaluation. The model has no iteration field
/// because a feedback cycle never creates a fix/retry loop.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackBudgetV1 {
    pub deadline_millis: u64,
    pub maximum_latency_millis: u64,
    pub maximum_tokens: u64,
    pub maximum_cost_microunits: u64,
}

impl FeedbackBudgetV1 {
    pub fn bounded(
        deadline_millis: u64,
        maximum_latency_millis: u64,
        maximum_tokens: u64,
        maximum_cost_microunits: u64,
    ) -> Self {
        Self {
            deadline_millis,
            maximum_latency_millis,
            maximum_tokens,
            maximum_cost_microunits,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.deadline_millis == 0 || self.maximum_latency_millis == 0 || self.maximum_tokens == 0
        {
            return Err(DomainError::NonCanonical {
                field: "feedback cycle budget",
            });
        }
        Ok(())
    }
}

/// Concrete PR11 request for one post-edit advisory cycle. The request is
/// structurally advisory-only, preventing it from becoming an edit, task, or
/// workflow command through an adapter-local field.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackCycleRequestV1 {
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub content: FeedbackContentIdentityV1,
    pub trigger: FeedbackTriggerV1,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub budget: FeedbackBudgetV1,
    pub advisory_only: bool,
}

impl FeedbackCycleRequestV1 {
    pub fn new(
        cycle_id: FeedbackCycleId,
        scope: FeedbackScopeV1,
        content: FeedbackContentIdentityV1,
        trigger: FeedbackTriggerV1,
        policy_digest: ManifestDigest,
        configuration_digest: ManifestDigest,
        budget: FeedbackBudgetV1,
    ) -> Result<Self, DomainError> {
        let request = Self {
            cycle_id,
            scope,
            content,
            trigger,
            policy_digest,
            configuration_digest,
            budget,
            advisory_only: true,
        };
        request.validate()?;
        Ok(request)
    }

    pub const fn durability(&self) -> FeedbackDurabilityV1 {
        self.content.durability()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.cycle_id.validate()?;
        self.scope.validate()?;
        self.content.validate()?;
        self.policy_digest.validate()?;
        self.configuration_digest.validate()?;
        self.budget.validate()?;
        if !self.advisory_only {
            return Err(DomainError::NonCanonical {
                field: "feedback cycle advisory-only flag",
            });
        }
        Ok(())
    }
}

/// Complete provider states remain distinct. Empty findings are clean only
/// when every requested provider completed with complete supported coverage.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProviderEvaluationStateV1 {
    SupportedCompletedComplete,
    Unsupported,
    Absent,
    Indexing,
    Stale,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCycleTerminationV1 {
    Clean,
    DuplicateNoop,
    Blocked,
    IncompleteCoverage,
    StaleReplanRequired,
    BudgetExceeded,
    Cancelled,
    UserStop,
    DaemonUnavailable,
}

impl FeedbackCycleTerminationV1 {
    pub fn is_consistent_with_provider_states(self, states: &[ProviderEvaluationStateV1]) -> bool {
        match self {
            Self::Clean => {
                !states.is_empty()
                    && states.iter().all(|state| {
                        *state == ProviderEvaluationStateV1::SupportedCompletedComplete
                    })
            }
            Self::IncompleteCoverage => states.iter().any(|state| {
                matches!(
                    state,
                    ProviderEvaluationStateV1::Partial
                        | ProviderEvaluationStateV1::Indexing
                        | ProviderEvaluationStateV1::Unavailable
                )
            }),
            Self::StaleReplanRequired => states.contains(&ProviderEvaluationStateV1::Stale),
            Self::BudgetExceeded => states.contains(&ProviderEvaluationStateV1::TimedOut),
            Self::Cancelled => states.contains(&ProviderEvaluationStateV1::Cancelled),
            Self::DaemonUnavailable => states.contains(&ProviderEvaluationStateV1::Unavailable),
            Self::DuplicateNoop | Self::Blocked | Self::UserStop => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackFindingLifecycleV1 {
    Active,
    Superseded,
    Resolved,
    Cleared,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackDiagnosticClassificationV1 {
    New,
    PreExisting,
}

/// Reference-only PR11 finding. The safe preview is bounded display framing,
/// never a source-text copy or a second diagnostic store.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackFindingV1 {
    pub finding_id: FeedbackFindingId,
    pub classification: FeedbackDiagnosticClassificationV1,
    pub lifecycle: FeedbackFindingLifecycleV1,
    pub retrieval_anchor_id: Option<RetrievalAnchorId>,
    pub provider_state: ProviderEvaluationStateV1,
    pub safe_bounded_preview: Option<String>,
}

impl FeedbackFindingV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.finding_id.validate()?;
        self.retrieval_anchor_id
            .as_ref()
            .map_or(Ok(()), RetrievalAnchorId::validate)?;
        if let Some(preview) = &self.safe_bounded_preview {
            validate_label(preview, "feedback safe preview")?;
            if preview.len() > 512 {
                return Err(DomainError::UnsafeText {
                    field: "feedback safe preview",
                });
            }
        }
        Ok(())
    }
}

/// One deterministic result for one trigger. The result represents a
/// terminal advisory evaluation and contains no next-action execution hook.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackCycleResultV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub durability: FeedbackDurabilityV1,
    pub termination: FeedbackCycleTerminationV1,
    pub provider_states: Vec<ProviderEvaluationStateV1>,
    pub findings: Vec<FeedbackFindingV1>,
    pub total_findings: u64,
    pub returned_findings: u64,
    pub omitted_findings: u64,
    pub advisory_only: bool,
}

impl FeedbackCycleResultV1 {
    pub fn new(
        request: &FeedbackCycleRequestV1,
        termination: FeedbackCycleTerminationV1,
        provider_states: Vec<ProviderEvaluationStateV1>,
        findings: Vec<FeedbackFindingV1>,
        total_findings: u64,
        returned_findings: u64,
        omitted_findings: u64,
    ) -> Result<Self, DomainError> {
        request.validate()?;
        let result_id = derive_result_id(
            request,
            termination,
            &provider_states,
            &findings,
            total_findings,
            returned_findings,
            omitted_findings,
        )?;
        let result = Self {
            result_id,
            cycle_id: request.cycle_id.clone(),
            durability: request.durability(),
            termination,
            provider_states,
            findings,
            total_findings,
            returned_findings,
            omitted_findings,
            advisory_only: true,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.result_id.validate()?;
        self.cycle_id.validate()?;
        if !self.advisory_only
            || self.returned_findings > self.total_findings
            || self.omitted_findings != self.total_findings - self.returned_findings
            || self.returned_findings != self.findings.len() as u64
        {
            return Err(DomainError::NonCanonical {
                field: "feedback cycle result counts",
            });
        }
        if self.termination == FeedbackCycleTerminationV1::Clean
            && (!self.findings.is_empty()
                || !self
                    .termination
                    .is_consistent_with_provider_states(&self.provider_states))
        {
            return Err(DomainError::NonCanonical {
                field: "clean feedback cycle result",
            });
        }
        for finding in &self.findings {
            finding.validate()?;
        }
        Ok(())
    }
}

fn derive_result_id(
    request: &FeedbackCycleRequestV1,
    termination: FeedbackCycleTerminationV1,
    provider_states: &[ProviderEvaluationStateV1],
    findings: &[FeedbackFindingV1],
    total_findings: u64,
    returned_findings: u64,
    omitted_findings: u64,
) -> Result<FeedbackResultId, DomainError> {
    let digest = canonical_sha256(&(
        FEEDBACK_RESULT_ID_DOMAIN,
        request,
        termination,
        provider_states,
        findings,
        total_findings,
        returned_findings,
        omitted_findings,
    ))?;
    let encoded = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(DomainError::NonCanonical {
            field: "feedback result digest",
        })?;
    FeedbackResultId::new(format!("feedback.result.v1.{encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn request(content: FeedbackContentIdentityV1) -> FeedbackCycleRequestV1 {
        FeedbackCycleRequestV1::new(
            id("cycle.fixture"),
            FeedbackScopeV1 {
                project_id: id("project.fixture"),
                repository_id: id("repository.fixture"),
                worktree_id: id("worktree.fixture"),
                branch_ref: "refs/heads/main".to_owned(),
                head_commit_id: id("commit.fixture"),
            },
            content,
            FeedbackTriggerV1::PostEditHook,
            digest('a'),
            digest('b'),
            FeedbackBudgetV1::bounded(10, 10, 1, 0),
        )
        .unwrap()
    }

    #[test]
    fn overlay_requests_are_session_only() {
        let request = request(FeedbackContentIdentityV1::EphemeralOverlay {
            session_id: id("session.fixture"),
            agent_id: None,
            document_version: 1,
            overlay_digest: digest('c'),
        });
        assert_eq!(request.durability(), FeedbackDurabilityV1::SessionOnly);
    }

    #[test]
    fn clean_results_require_complete_provider_coverage() {
        let request = request(FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('c'),
            file_digest: digest('d'),
        });
        assert!(
            FeedbackCycleResultV1::new(
                &request,
                FeedbackCycleTerminationV1::Clean,
                vec![ProviderEvaluationStateV1::SupportedCompletedComplete],
                vec![],
                0,
                0,
                0,
            )
            .is_ok()
        );
        assert!(
            FeedbackCycleResultV1::new(
                &request,
                FeedbackCycleTerminationV1::Clean,
                vec![ProviderEvaluationStateV1::Partial],
                vec![],
                0,
                0,
                0,
            )
            .is_err()
        );
    }
}
