//! Pure, one-shot advisory feedback-cycle contracts.
//!
//! PR11 owns saved-content post-edit diagnostics and impact contracts here.
//! These values never schedule an agent, apply an edit, emit a transport
//! payload, or make dirty-overlay evidence durable.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::code_intelligence::{
    CodeGenerationId, FileOccurrenceId, SourceSpan, SymbolOccurrenceId,
};
use crate::diagnostics::{DiagnosticSeverityV1, GenerationDiagnosticV1};
use crate::research::{
    AgentInstanceId, CommitId, DomainError, HostInstanceId, ManifestDigest, ProjectId,
    RepositoryId, RetrievalAnchorId, SessionId, TurnId, UtcMicros, WorktreeId, canonical_sha256,
};

pub mod ci_localization;
pub mod evidence_packet;
pub mod github_review;
pub mod proximity;

pub use ci_localization::*;
pub use evidence_packet::*;
pub use github_review::*;
pub use proximity::*;

const FEEDBACK_DEDUPE_KEY_DOMAIN: &str = "tracedecay.feedback.dedupe.v1";
const FEEDBACK_FINDING_ID_DOMAIN: &str = "tracedecay.feedback.finding.v1";
const FEEDBACK_RESULT_ID_DOMAIN: &str = "tracedecay.feedback.result.v1";

pub(crate) use crate::canonical_text::validate_canonical_string as validate_label;
use crate::canonical_text::validated_string_newtype;

validated_string_newtype!(
    plain,
    DomainError,
    validate_label;
    FeedbackCycleId => "feedback cycle id",
    FeedbackResultId => "feedback result id",
    FeedbackFindingId => "feedback finding id",
    FeedbackDedupeKeyV1 => "feedback dedupe key",
    FeedbackSavedDedupeKeyV1 => "saved feedback dedupe key",
    FeedbackDedupeClaimId => "feedback dedupe claim id",
);

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
        owner_client_id: HostInstanceId,
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
                owner_client_id,
                agent_id,
                document_version,
                overlay_digest,
            } => {
                session_id.validate()?;
                owner_client_id.validate()?;
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

/// Current immutable facts observed immediately before one feedback
/// evaluation. The application compares this snapshot with the request before
/// invoking providers so branch/head/content/policy/configuration drift is
/// typed as stale rather than silently evaluated as current.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackCycleRuntimeSnapshotV1 {
    pub scope: FeedbackScopeV1,
    pub content: FeedbackContentIdentityV1,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
}

impl FeedbackCycleRuntimeSnapshotV1 {
    pub fn from_request(request: &FeedbackCycleRequestV1) -> Self {
        Self {
            scope: request.scope.clone(),
            content: request.content.clone(),
            policy_digest: request.policy_digest.clone(),
            configuration_digest: request.configuration_digest.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.scope.validate()?;
        self.content.validate()?;
        self.policy_digest.validate()?;
        self.configuration_digest.validate()
    }

    pub fn has_same_root(&self, request: &FeedbackCycleRequestV1) -> bool {
        self.scope.project_id == request.scope.project_id
            && self.scope.repository_id == request.scope.repository_id
            && self.scope.worktree_id == request.scope.worktree_id
    }

    pub fn is_current_for(&self, request: &FeedbackCycleRequestV1) -> bool {
        self.has_same_root(request)
            && self.scope == request.scope
            && self.content == request.content
            && self.policy_digest == request.policy_digest
            && self.configuration_digest == request.configuration_digest
    }
}

/// Exact changed-code address for one single-root feedback evaluation. The
/// address carries canonical file/range/symbol identities, never a path or
/// mutable line number.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackTargetV1 {
    pub file: FileOccurrenceId,
    pub span: Option<SourceSpan>,
    pub symbol: Option<SymbolOccurrenceId>,
    pub generation_id: Option<CodeGenerationId>,
}

impl FeedbackTargetV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.file.validate()?;
        self.span.as_ref().map_or(Ok(()), SourceSpan::validate)?;
        self.symbol
            .as_ref()
            .map_or(Ok(()), SymbolOccurrenceId::validate)?;
        self.generation_id
            .as_ref()
            .map_or(Ok(()), CodeGenerationId::validate)
    }
}

/// Agent/session identity is evidence about who owned an overlay trigger; it
/// is not a workflow assignment, lease, or continuation authority.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackActorContextV1 {
    pub session_id: Option<SessionId>,
    pub client_id: Option<HostInstanceId>,
    pub agent_id: Option<AgentInstanceId>,
    pub turn_id: Option<TurnId>,
}

impl FeedbackActorContextV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.session_id
            .as_ref()
            .map_or(Ok(()), SessionId::validate)?;
        self.client_id
            .as_ref()
            .map_or(Ok(()), HostInstanceId::validate)?;
        self.agent_id
            .as_ref()
            .map_or(Ok(()), AgentInstanceId::validate)?;
        self.turn_id.as_ref().map_or(Ok(()), TurnId::validate)?;
        if self.turn_id.is_some() && self.session_id.is_none() {
            return Err(DomainError::NonCanonical {
                field: "feedback turn session binding",
            });
        }
        Ok(())
    }
}

/// Inputs required to turn a durable cycle request into one post-edit
/// evaluation. The durable request remains the only source of policy and
/// configuration truth; this value adds the exact code address and optional
/// local actor context needed for one trigger.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackEvaluationInputV1 {
    pub request: FeedbackCycleRequestV1,
    pub target: FeedbackTargetV1,
    pub actor: FeedbackActorContextV1,
    pub observed_at: UtcMicros,
}

impl FeedbackEvaluationInputV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.request.validate()?;
        self.target.validate()?;
        self.actor.validate()?;
        match &self.request.content {
            FeedbackContentIdentityV1::SavedContent { .. }
                if self.target.generation_id.is_none() =>
            {
                Err(DomainError::NonCanonical {
                    field: "saved feedback target generation",
                })
            }
            FeedbackContentIdentityV1::EphemeralOverlay {
                session_id,
                owner_client_id,
                agent_id,
                ..
            } => {
                if self.actor.session_id.as_ref() != Some(session_id)
                    || self.actor.client_id.as_ref() != Some(owner_client_id)
                    || self.actor.agent_id.as_ref() != agent_id.as_ref()
                {
                    return Err(DomainError::NonCanonical {
                        field: "overlay feedback actor binding",
                    });
                }
                Ok(())
            }
            FeedbackContentIdentityV1::SavedContent { .. } => Ok(()),
        }
    }

    /// Converts only saved content into the input accepted by durable sinks.
    /// Overlay ownership and content cannot be represented by this type.
    pub fn saved(&self) -> Result<FeedbackSavedEvaluationV1, DomainError> {
        self.validate()?;
        let FeedbackContentIdentityV1::SavedContent {
            generation_digest,
            file_digest,
        } = &self.request.content
        else {
            return Err(DomainError::NonCanonical {
                field: "durable feedback saved content",
            });
        };
        Ok(FeedbackSavedEvaluationV1 {
            cycle_id: self.request.cycle_id.clone(),
            scope: self.request.scope.clone(),
            generation_digest: generation_digest.clone(),
            file_digest: file_digest.clone(),
            trigger: self.request.trigger,
            policy_digest: self.request.policy_digest.clone(),
            configuration_digest: self.request.configuration_digest.clone(),
            target: self.target.clone(),
            observed_at: self.observed_at,
        })
    }

    pub fn dedupe_key(
        &self,
        evidence_identity: &ManifestDigest,
    ) -> Result<FeedbackDedupeKeyV1, DomainError> {
        let saved_key = self.saved()?.dedupe_key(evidence_identity)?;
        FeedbackDedupeKeyV1::new(saved_key.as_str())
    }
}

/// Saved-content-only input for durable observations and dedupe. Semantic
/// dedupe deliberately excludes `cycle_id` and `observed_at`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackSavedEvaluationV1 {
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub generation_digest: ManifestDigest,
    pub file_digest: ManifestDigest,
    pub trigger: FeedbackTriggerV1,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub target: FeedbackTargetV1,
    pub observed_at: UtcMicros,
}

impl FeedbackSavedEvaluationV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.cycle_id.validate()?;
        self.scope.validate()?;
        self.generation_digest.validate()?;
        self.file_digest.validate()?;
        self.policy_digest.validate()?;
        self.configuration_digest.validate()?;
        self.target.validate()?;
        if self.target.generation_id.is_none() {
            return Err(DomainError::NonCanonical {
                field: "saved feedback target generation",
            });
        }
        Ok(())
    }

    pub fn dedupe_key(
        &self,
        evidence_identity: &ManifestDigest,
    ) -> Result<FeedbackSavedDedupeKeyV1, DomainError> {
        self.validate()?;
        evidence_identity.validate()?;
        let digest = canonical_sha256(&(
            FEEDBACK_DEDUPE_KEY_DOMAIN,
            &self.scope,
            &self.generation_digest,
            &self.file_digest,
            self.trigger,
            &self.policy_digest,
            &self.configuration_digest,
            &self.target,
            evidence_identity,
        ))?;
        let encoded = digest
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(DomainError::NonCanonical {
                field: "feedback dedupe digest",
            })?;
        FeedbackSavedDedupeKeyV1::new(format!("feedback.dedupe.v1.{encoded}"))
    }
}

/// Coverage state for graph impact and affected-test evidence. An empty impact
/// set is clean only when its state is complete.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackImpactStateV1 {
    Complete,
    Partial,
    Stale,
    Unavailable,
}

/// Reference-only graph and test impact for one feedback target. The owning
/// graph/query layer supplies these identities; this contract does not create
/// another graph, test map, or evidence store.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackImpactV1 {
    pub target: FeedbackTargetV1,
    pub affected_files: Vec<FileOccurrenceId>,
    pub affected_callers: Vec<SymbolOccurrenceId>,
    pub affected_tests: Vec<SymbolOccurrenceId>,
    pub evidence_anchors: Vec<RetrievalAnchorId>,
    pub state: FeedbackImpactStateV1,
    pub affected_tests_state: FeedbackImpactStateV1,
}

impl FeedbackImpactV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.target.validate()?;
        for file in &self.affected_files {
            file.validate()?;
        }
        for symbol in self
            .affected_callers
            .iter()
            .chain(self.affected_tests.iter())
        {
            symbol.validate()?;
        }
        for anchor in &self.evidence_anchors {
            anchor.validate()?;
        }
        if has_duplicates(&self.affected_files)
            || has_duplicates(&self.affected_callers)
            || has_duplicates(&self.affected_tests)
            || has_duplicates(&self.evidence_anchors)
        {
            return Err(DomainError::NonCanonical {
                field: "feedback impact duplicate identities",
            });
        }
        Ok(())
    }
}

fn has_duplicates<T: PartialEq>(values: &[T]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[index.saturating_add(1)..].contains(value))
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
                    ProviderEvaluationStateV1::Unsupported
                        | ProviderEvaluationStateV1::Absent
                        | ProviderEvaluationStateV1::Partial
                        | ProviderEvaluationStateV1::Indexing
                        | ProviderEvaluationStateV1::Failed
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
    Unknown,
}

/// Availability of the canonical baseline used to classify a current
/// diagnostic. An unavailable or partial baseline never upgrades a finding
/// to `New`; only an authoritative `NoPriorBaseline` state may do so without
/// a baseline record.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackBaselineStateV1 {
    Complete,
    Partial,
    Stale,
    /// The authoritative runtime confirmed that there is no prior saved
    /// generation to compare. This is authoritative empty history, not an
    /// invented horizon and not unavailable/partial coverage.
    NoPriorBaseline,
    Unavailable,
}

impl FeedbackBaselineStateV1 {
    pub const fn supports_complete_comparison(self) -> bool {
        matches!(self, Self::Complete | Self::NoPriorBaseline)
    }
}

/// Exact address of one authoritative diagnostics-history baseline. The
/// provider digest is over the complete canonical provider identity, not a
/// mutable provider label.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackBaselineHorizonV1 {
    pub comparison_generation_id: CodeGenerationId,
    pub comparison_generation_digest: ManifestDigest,
    pub comparison_head_commit_id: CommitId,
    pub comparison_content_digest: ManifestDigest,
    pub watermark: ManifestDigest,
}

impl FeedbackBaselineHorizonV1 {
    pub fn validate_for(
        &self,
        current_generation_id: &CodeGenerationId,
        current_generation_digest: &ManifestDigest,
        current_head_commit_id: &CommitId,
        current_content_digest: &ManifestDigest,
    ) -> Result<(), DomainError> {
        self.comparison_generation_id.validate()?;
        self.comparison_generation_digest.validate()?;
        self.comparison_head_commit_id.validate()?;
        self.comparison_content_digest.validate()?;
        self.watermark.validate()?;
        if self.comparison_generation_id == *current_generation_id
            && self.comparison_generation_digest == *current_generation_digest
            && self.comparison_head_commit_id == *current_head_commit_id
            && self.comparison_content_digest == *current_content_digest
        {
            return Err(DomainError::NonCanonical {
                field: "feedback baseline comparison horizon",
            });
        }
        Ok(())
    }
}

/// Authoritative runtime resolution returned by the runtime-state port. The
/// watermark makes concurrent changes observable across the two resolutions.
/// `baseline_horizon: None` means either that an overlay has no durable
/// baseline or that the authoritative saved-content history has no prior
/// generation; callers must never manufacture a comparison horizon.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackAuthoritativeRuntimeStateV1 {
    pub snapshot: FeedbackCycleRuntimeSnapshotV1,
    pub baseline_horizon: Option<FeedbackBaselineHorizonV1>,
    pub runtime_watermark: ManifestDigest,
}

impl FeedbackAuthoritativeRuntimeStateV1 {
    pub fn validate_for(&self, input: &FeedbackEvaluationInputV1) -> Result<(), DomainError> {
        self.snapshot.validate()?;
        self.runtime_watermark.validate()?;
        match (&input.request.content, &self.baseline_horizon) {
            (FeedbackContentIdentityV1::SavedContent { .. }, None) => Ok(()),
            (
                FeedbackContentIdentityV1::SavedContent {
                    generation_digest,
                    file_digest,
                },
                Some(horizon),
            ) => horizon.validate_for(
                input
                    .target
                    .generation_id
                    .as_ref()
                    .ok_or(DomainError::NonCanonical {
                        field: "feedback runtime generation",
                    })?,
                generation_digest,
                &input.request.scope.head_commit_id,
                file_digest,
            ),
            (FeedbackContentIdentityV1::EphemeralOverlay { .. }, None) => Ok(()),
            _ => Err(DomainError::NonCanonical {
                field: "feedback runtime baseline horizon",
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackDiagnosticBaselineIdentityV1 {
    pub current_generation_id: CodeGenerationId,
    pub current_generation_digest: ManifestDigest,
    pub current_head_commit_id: CommitId,
    pub current_content_digest: ManifestDigest,
    pub provider_identity_digest: ManifestDigest,
    pub horizon: FeedbackBaselineHorizonV1,
}

impl FeedbackDiagnosticBaselineIdentityV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.current_generation_id.validate()?;
        self.current_generation_digest.validate()?;
        self.current_head_commit_id.validate()?;
        self.current_content_digest.validate()?;
        self.provider_identity_digest.validate()?;
        self.horizon.validate_for(
            &self.current_generation_id,
            &self.current_generation_digest,
            &self.current_head_commit_id,
            &self.current_content_digest,
        )
    }
}

/// Reference-only prior diagnostic identity set. It is supplied by the
/// authoritative diagnostic store/query port and is not a feedback-local
/// finding store.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackDiagnosticBaselineV1 {
    pub identity: FeedbackDiagnosticBaselineIdentityV1,
    pub diagnostic_anchors: Vec<RetrievalAnchorId>,
    pub state: FeedbackBaselineStateV1,
}

impl FeedbackDiagnosticBaselineV1 {
    pub fn classify(
        &self,
        expected_identity: &FeedbackDiagnosticBaselineIdentityV1,
        diagnostic_anchor: &RetrievalAnchorId,
    ) -> FeedbackDiagnosticClassificationV1 {
        if self.identity != *expected_identity {
            FeedbackDiagnosticClassificationV1::Unknown
        } else if self.diagnostic_anchors.contains(diagnostic_anchor) {
            FeedbackDiagnosticClassificationV1::PreExisting
        } else if self.state == FeedbackBaselineStateV1::Complete {
            FeedbackDiagnosticClassificationV1::New
        } else {
            FeedbackDiagnosticClassificationV1::Unknown
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.identity.validate()?;
        if self.state == FeedbackBaselineStateV1::NoPriorBaseline {
            return Err(DomainError::NonCanonical {
                field: "feedback baseline no-prior state",
            });
        }
        for anchor in &self.diagnostic_anchors {
            anchor.validate()?;
        }
        if has_duplicates(&self.diagnostic_anchors) {
            return Err(DomainError::NonCanonical {
                field: "feedback baseline duplicate anchors",
            });
        }
        Ok(())
    }
}

/// Immediate diagnostic returned for the authorized owner of a dirty
/// document. It deliberately has no generation, durable anchor, evidence
/// packet, observation, receipt, history, or cache identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackSessionDiagnosticV1 {
    pub span: SourceSpan,
    pub symbol: Option<SymbolOccurrenceId>,
    pub code: String,
    pub severity: DiagnosticSeverityV1,
    pub safe_bounded_message: String,
}

impl FeedbackSessionDiagnosticV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.span.validate()?;
        self.symbol
            .as_ref()
            .map_or(Ok(()), SymbolOccurrenceId::validate)?;
        validate_label(&self.code, "overlay diagnostic code")?;
        validate_label(&self.safe_bounded_message, "overlay diagnostic message")?;
        if self.safe_bounded_message.len() > 512 {
            return Err(DomainError::UnsafeText {
                field: "overlay diagnostic message",
            });
        }
        Ok(())
    }
}

/// Provider payload accepted by a feedback cycle. Saved diagnostics reuse the
/// canonical durable generation record; overlays use the structurally
/// non-durable session shape above.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "diagnostic")]
pub enum FeedbackDiagnosticV1 {
    Saved(Box<GenerationDiagnosticV1>),
    SessionOverlay(FeedbackSessionDiagnosticV1),
}

impl FeedbackDiagnosticV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Saved(diagnostic) => diagnostic.validate(),
            Self::SessionOverlay(diagnostic) => diagnostic.validate(),
        }
    }
}

/// Bounded code location used only to project an anchored advisory finding
/// into an editor. The finding's `retrieval_anchor_id` remains the evidence
/// expansion authority; this value carries no source body or provider payload.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackDiagnosticProjectionV1 {
    pub file: FileOccurrenceId,
    pub span: SourceSpan,
    pub symbol: Option<SymbolOccurrenceId>,
    pub code: String,
    pub severity: DiagnosticSeverityV1,
    pub safe_bounded_message: String,
    pub producer: FeedbackDiagnosticProducerV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_description_uri: Option<String>,
}

impl FeedbackDiagnosticProjectionV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.file.validate()?;
        self.span.validate()?;
        self.symbol
            .as_ref()
            .map_or(Ok(()), SymbolOccurrenceId::validate)?;
        validate_label(&self.code, "feedback diagnostic projection code")?;
        validate_label(
            &self.safe_bounded_message,
            "feedback diagnostic projection message",
        )?;
        if self.safe_bounded_message.len() > 512 {
            return Err(DomainError::UnsafeText {
                field: "feedback diagnostic projection message",
            });
        }
        if !safe_diagnostic_code_description_uri(
            self.producer,
            self.code_description_uri.as_deref(),
        ) {
            return Err(DomainError::UnsafeText {
                field: "feedback diagnostic code description URI",
            });
        }
        Ok(())
    }
}

fn safe_diagnostic_code_description_uri(
    producer: FeedbackDiagnosticProducerV1,
    value: Option<&str>,
) -> bool {
    let Some(value) = value else {
        return true;
    };
    if producer != FeedbackDiagnosticProducerV1::GitHubReview {
        return false;
    }
    if value.len() > 2_048 {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
}

/// Closed producer vocabulary for standard diagnostic projection.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackDiagnosticProducerV1 {
    GitHubReview,
    CiLocalization,
    Proximity,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_projection: Option<FeedbackDiagnosticProjectionV1>,
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
        self.diagnostic_projection
            .as_ref()
            .map_or(Ok(()), FeedbackDiagnosticProjectionV1::validate)?;
        if self.diagnostic_projection.is_some()
            && (self.lifecycle != FeedbackFindingLifecycleV1::Active
                || self.retrieval_anchor_id.is_none())
        {
            return Err(DomainError::NonCanonical {
                field: "feedback diagnostic projection authority",
            });
        }
        Ok(())
    }
}

/// Stable finding identity derived from the canonical diagnostic anchor and
/// the exact provider-result identity. Distinct producers remain distinct;
/// identical producer/anchor pairs converge without a feedback-local store.
pub fn derive_feedback_finding_id(
    diagnostic_anchor: &RetrievalAnchorId,
    provider_identity_digest: &ManifestDigest,
) -> Result<FeedbackFindingId, DomainError> {
    diagnostic_anchor.validate()?;
    provider_identity_digest.validate()?;
    let digest = canonical_sha256(&(
        FEEDBACK_FINDING_ID_DOMAIN,
        diagnostic_anchor,
        provider_identity_digest,
    ))?;
    let encoded = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(DomainError::NonCanonical {
            field: "feedback finding digest",
        })?;
    FeedbackFindingId::new(format!("feedback.finding.v1.{encoded}"))
}

/// Session-local finding identity for a non-durable overlay projection. The
/// caller must never use this identity as an anchor or persistence key.
pub fn derive_overlay_feedback_finding_id(
    diagnostic: &FeedbackSessionDiagnosticV1,
    provider_identity_digest: &ManifestDigest,
) -> Result<FeedbackFindingId, DomainError> {
    diagnostic.validate()?;
    provider_identity_digest.validate()?;
    let digest = canonical_sha256(&(
        FEEDBACK_FINDING_ID_DOMAIN,
        "session_overlay",
        diagnostic,
        provider_identity_digest,
    ))?;
    let encoded = digest
        .as_str()
        .strip_prefix("sha256:")
        .ok_or(DomainError::NonCanonical {
            field: "overlay feedback finding digest",
        })?;
    FeedbackFindingId::new(format!("feedback.finding.v1.{encoded}"))
}

/// One deterministic result for one trigger. The result represents a
/// terminal advisory evaluation and contains no next-action execution hook.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackCycleResultV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_identity: Option<FeedbackContentIdentityV1>,
    pub durability: FeedbackDurabilityV1,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub termination: FeedbackCycleTerminationV1,
    pub provider_states: Vec<ProviderEvaluationStateV1>,
    pub baseline_states: Vec<FeedbackBaselineStateV1>,
    pub impact: Option<FeedbackImpactV1>,
    pub impact_state: Option<FeedbackImpactStateV1>,
    pub affected_tests_state: Option<FeedbackImpactStateV1>,
    pub findings: Vec<FeedbackFindingV1>,
    pub total_findings: u64,
    pub returned_findings: u64,
    pub omitted_findings: u64,
    pub advisory_only: bool,
}

impl FeedbackCycleResultV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request: &FeedbackCycleRequestV1,
        termination: FeedbackCycleTerminationV1,
        provider_states: Vec<ProviderEvaluationStateV1>,
        baseline_states: Vec<FeedbackBaselineStateV1>,
        impact: Option<FeedbackImpactV1>,
        impact_state: Option<FeedbackImpactStateV1>,
        affected_tests_state: Option<FeedbackImpactStateV1>,
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
            &baseline_states,
            &impact,
            impact_state,
            affected_tests_state,
            &findings,
            total_findings,
            returned_findings,
            omitted_findings,
        )?;
        let result = Self {
            result_id,
            cycle_id: request.cycle_id.clone(),
            scope: request.scope.clone(),
            content_identity: Some(request.content.clone()),
            durability: request.durability(),
            policy_digest: request.policy_digest.clone(),
            configuration_digest: request.configuration_digest.clone(),
            termination,
            provider_states,
            baseline_states,
            impact,
            impact_state,
            affected_tests_state,
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
        self.scope.validate()?;
        if let Some(content_identity) = &self.content_identity {
            content_identity.validate()?;
            if content_identity.durability() != self.durability {
                return Err(DomainError::NonCanonical {
                    field: "feedback result content durability",
                });
            }
        }
        self.policy_digest.validate()?;
        self.configuration_digest.validate()?;
        if let Some(impact) = &self.impact {
            impact.validate()?;
            if self.impact_state != Some(impact.state) {
                return Err(DomainError::NonCanonical {
                    field: "feedback impact state",
                });
            }
        } else if matches!(
            self.impact_state,
            Some(FeedbackImpactStateV1::Complete | FeedbackImpactStateV1::Partial)
        ) {
            return Err(DomainError::NonCanonical {
                field: "feedback impact payload",
            });
        }
        if let Some(impact) = &self.impact {
            if self.affected_tests_state != Some(impact.affected_tests_state) {
                return Err(DomainError::NonCanonical {
                    field: "feedback affected-test state",
                });
            }
        } else if self.affected_tests_state != self.impact_state {
            return Err(DomainError::NonCanonical {
                field: "feedback affected-test state without impact",
            });
        }
        if self.durability == FeedbackDurabilityV1::SessionOnly
            && (!self.baseline_states.is_empty()
                || self
                    .impact
                    .as_ref()
                    .is_some_and(|impact| !impact.evidence_anchors.is_empty())
                || self
                    .findings
                    .iter()
                    .any(|finding| finding.retrieval_anchor_id.is_some()))
        {
            return Err(DomainError::NonCanonical {
                field: "overlay feedback durable evidence",
            });
        }
        if !self.advisory_only
            || self.returned_findings > self.total_findings
            || self.omitted_findings != self.total_findings - self.returned_findings
            || self.returned_findings != self.findings.len() as u64
        {
            return Err(DomainError::NonCanonical {
                field: "feedback cycle result counts",
            });
        }
        match self.termination {
            FeedbackCycleTerminationV1::Clean
                if self.total_findings != 0
                    || !self.findings.is_empty()
                    || (self.durability == FeedbackDurabilityV1::Durable
                        && (self.baseline_states.is_empty()
                            || self
                                .baseline_states
                                .iter()
                                .any(|state| !state.supports_complete_comparison())))
                    || self.impact_state != Some(FeedbackImpactStateV1::Complete)
                    || self.affected_tests_state != Some(FeedbackImpactStateV1::Complete)
                    || self
                        .impact
                        .as_ref()
                        .is_none_or(|impact| impact.state != FeedbackImpactStateV1::Complete)
                    || !self
                        .termination
                        .is_consistent_with_provider_states(&self.provider_states) =>
            {
                return Err(DomainError::NonCanonical {
                    field: "clean feedback cycle result",
                });
            }
            FeedbackCycleTerminationV1::DuplicateNoop
                if self.total_findings != 0
                    || !self.findings.is_empty()
                    || !self.provider_states.is_empty()
                    || !self.baseline_states.is_empty()
                    || self.impact_state.is_some() =>
            {
                return Err(DomainError::NonCanonical {
                    field: "duplicate feedback cycle result",
                });
            }
            FeedbackCycleTerminationV1::UserStop
                if self.total_findings != 0
                    || !self.findings.is_empty()
                    || !self.provider_states.is_empty()
                    || !self.baseline_states.is_empty()
                    || self.impact_state.is_some() =>
            {
                return Err(DomainError::NonCanonical {
                    field: "user-stopped feedback cycle result",
                });
            }
            FeedbackCycleTerminationV1::StaleReplanRequired
                if !self
                    .provider_states
                    .contains(&ProviderEvaluationStateV1::Stale)
                    && !self
                        .baseline_states
                        .contains(&FeedbackBaselineStateV1::Stale)
                    && self.impact_state != Some(FeedbackImpactStateV1::Stale) =>
            {
                return Err(DomainError::NonCanonical {
                    field: "feedback cycle stale state",
                });
            }
            FeedbackCycleTerminationV1::BudgetExceeded
            | FeedbackCycleTerminationV1::Cancelled
            | FeedbackCycleTerminationV1::DaemonUnavailable
                if !self
                    .termination
                    .is_consistent_with_provider_states(&self.provider_states) =>
            {
                return Err(DomainError::NonCanonical {
                    field: "feedback cycle terminal provider state",
                });
            }
            _ => {}
        }
        for finding in &self.findings {
            finding.validate()?;
        }
        if self.findings.iter().enumerate().any(|(index, finding)| {
            self.findings[index.saturating_add(1)..]
                .iter()
                .any(|other| other.finding_id == finding.finding_id)
        }) {
            return Err(DomainError::NonCanonical {
                field: "feedback cycle duplicate finding id",
            });
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn derive_result_id(
    request: &FeedbackCycleRequestV1,
    termination: FeedbackCycleTerminationV1,
    provider_states: &[ProviderEvaluationStateV1],
    baseline_states: &[FeedbackBaselineStateV1],
    impact: &Option<FeedbackImpactV1>,
    impact_state: Option<FeedbackImpactStateV1>,
    affected_tests_state: Option<FeedbackImpactStateV1>,
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
        baseline_states,
        impact,
        impact_state,
        affected_tests_state,
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

/// Privacy-safe PR11 feedback-cycle observation categories. They are separate
/// from the feedback result because telemetry must never copy paths, source,
/// diagnostic messages, or overlay content.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackObservationKindV1 {
    Trigger,
    EvaluationStage,
    Terminal,
    DedupeSuppressed,
    Latency,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackEvaluationStageV1 {
    Admission,
    Diagnostics,
    BaselineClassification,
    Impact,
    AffectedTests,
    ResultAssembly,
    Total,
}

/// One durable Plan-26 PR11 observation. Session-only overlay cycles cannot
/// construct this value and therefore cannot enter telemetry or any other
/// durable observation path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackCycleObservationV1 {
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub policy_digest: ManifestDigest,
    pub configuration_digest: ManifestDigest,
    pub kind: FeedbackObservationKindV1,
    pub stage: Option<FeedbackEvaluationStageV1>,
    pub termination: Option<FeedbackCycleTerminationV1>,
    pub dedupe_key: Option<FeedbackDedupeKeyV1>,
    pub observed_at: UtcMicros,
    pub latency_micros: Option<u64>,
    pub advisory_only: bool,
}

impl FeedbackCycleObservationV1 {
    pub fn trigger(input: &FeedbackEvaluationInputV1) -> Result<Self, DomainError> {
        Self::new(
            input,
            FeedbackObservationKindV1::Trigger,
            None,
            None,
            None,
            None,
        )
    }

    pub fn stage(
        input: &FeedbackEvaluationInputV1,
        stage: FeedbackEvaluationStageV1,
    ) -> Result<Self, DomainError> {
        Self::new(
            input,
            FeedbackObservationKindV1::EvaluationStage,
            Some(stage),
            None,
            None,
            None,
        )
    }

    pub fn terminal(
        input: &FeedbackEvaluationInputV1,
        termination: FeedbackCycleTerminationV1,
    ) -> Result<Self, DomainError> {
        Self::new(
            input,
            FeedbackObservationKindV1::Terminal,
            None,
            Some(termination),
            None,
            None,
        )
    }

    pub fn dedupe_suppressed(
        input: &FeedbackEvaluationInputV1,
        dedupe_key: FeedbackDedupeKeyV1,
    ) -> Result<Self, DomainError> {
        Self::new(
            input,
            FeedbackObservationKindV1::DedupeSuppressed,
            None,
            Some(FeedbackCycleTerminationV1::DuplicateNoop),
            Some(dedupe_key),
            None,
        )
    }

    pub fn latency(
        input: &FeedbackEvaluationInputV1,
        stage: FeedbackEvaluationStageV1,
        latency_micros: u64,
    ) -> Result<Self, DomainError> {
        Self::new(
            input,
            FeedbackObservationKindV1::Latency,
            Some(stage),
            None,
            None,
            Some(latency_micros),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        input: &FeedbackEvaluationInputV1,
        kind: FeedbackObservationKindV1,
        stage: Option<FeedbackEvaluationStageV1>,
        termination: Option<FeedbackCycleTerminationV1>,
        dedupe_key: Option<FeedbackDedupeKeyV1>,
        latency_micros: Option<u64>,
    ) -> Result<Self, DomainError> {
        input.validate()?;
        if input.request.durability() != FeedbackDurabilityV1::Durable {
            return Err(DomainError::NonCanonical {
                field: "overlay feedback observation durability",
            });
        }
        let observation = Self {
            cycle_id: input.request.cycle_id.clone(),
            scope: input.request.scope.clone(),
            policy_digest: input.request.policy_digest.clone(),
            configuration_digest: input.request.configuration_digest.clone(),
            kind,
            stage,
            termination,
            dedupe_key,
            observed_at: input.observed_at,
            latency_micros,
            advisory_only: true,
        };
        observation.validate()?;
        Ok(observation)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.cycle_id.validate()?;
        self.scope.validate()?;
        self.policy_digest.validate()?;
        self.configuration_digest.validate()?;
        self.dedupe_key
            .as_ref()
            .map_or(Ok(()), FeedbackDedupeKeyV1::validate)?;
        if !self.advisory_only {
            return Err(DomainError::NonCanonical {
                field: "feedback observation advisory-only flag",
            });
        }
        let valid_shape = match self.kind {
            FeedbackObservationKindV1::Trigger => {
                self.stage.is_none()
                    && self.termination.is_none()
                    && self.dedupe_key.is_none()
                    && self.latency_micros.is_none()
            }
            FeedbackObservationKindV1::EvaluationStage => {
                self.stage.is_some()
                    && self.termination.is_none()
                    && self.dedupe_key.is_none()
                    && self.latency_micros.is_none()
            }
            FeedbackObservationKindV1::Terminal => {
                self.stage.is_none()
                    && self.termination.is_some()
                    && self.dedupe_key.is_none()
                    && self.latency_micros.is_none()
            }
            FeedbackObservationKindV1::DedupeSuppressed => {
                self.stage.is_none()
                    && self.termination == Some(FeedbackCycleTerminationV1::DuplicateNoop)
                    && self.dedupe_key.is_some()
                    && self.latency_micros.is_none()
            }
            FeedbackObservationKindV1::Latency => {
                self.stage.is_some()
                    && self.termination.is_none()
                    && self.dedupe_key.is_none()
                    && self.latency_micros.is_some()
            }
        };
        if valid_shape {
            Ok(())
        } else {
            Err(DomainError::NonCanonical {
                field: "feedback observation shape",
            })
        }
    }
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

    fn complete_impact() -> FeedbackImpactV1 {
        FeedbackImpactV1 {
            target: FeedbackTargetV1 {
                file: id("file.fixture"),
                span: None,
                symbol: None,
                generation_id: Some(id("generation.fixture")),
            },
            affected_files: Vec::new(),
            affected_callers: Vec::new(),
            affected_tests: Vec::new(),
            evidence_anchors: Vec::new(),
            state: FeedbackImpactStateV1::Complete,
            affected_tests_state: FeedbackImpactStateV1::Complete,
        }
    }

    #[test]
    fn diagnostic_links_are_github_review_only() {
        let github = Some("https://github.com/owner/repository/pull/13#discussion_r1");
        assert!(safe_diagnostic_code_description_uri(
            FeedbackDiagnosticProducerV1::GitHubReview,
            github,
        ));
        assert!(!safe_diagnostic_code_description_uri(
            FeedbackDiagnosticProducerV1::CiLocalization,
            github,
        ));
        assert!(!safe_diagnostic_code_description_uri(
            FeedbackDiagnosticProducerV1::GitHubReview,
            Some("https://example.com/owner/repository/pull/13#discussion_r1"),
        ));
        assert!(safe_diagnostic_code_description_uri(
            FeedbackDiagnosticProducerV1::Proximity,
            None,
        ));
    }

    #[test]
    fn overlay_requests_are_session_only() {
        let request = request(FeedbackContentIdentityV1::EphemeralOverlay {
            session_id: id("session.fixture"),
            owner_client_id: id("client.fixture"),
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
                vec![FeedbackBaselineStateV1::Complete],
                Some(complete_impact()),
                Some(FeedbackImpactStateV1::Complete),
                Some(FeedbackImpactStateV1::Complete),
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
                vec![FeedbackBaselineStateV1::Complete],
                Some(complete_impact()),
                Some(FeedbackImpactStateV1::Complete),
                Some(FeedbackImpactStateV1::Complete),
                vec![],
                0,
                0,
                0,
            )
            .is_err()
        );
    }
}
