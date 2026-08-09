use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{
    CodeGenerationId, ComponentVersion, ManifestDigest, RetrievalAnchorId, TemporalModeV1,
    UtcMicros,
};
use tracedecay_tool_catalog::{RetrieverId, SortContractId};

use crate::context::{CapabilityGrantId, DisclosureClass, RequestContext, ResolvedScope};
use crate::error::ApplicationContractError;
use crate::identity::application_identifier;

use super::{CancellationObservation, OperationBudgetUsage, OperationReceipt, ResultContractRef};

application_identifier!(
    @no_schema
    EvidenceIdentity => ("evidence identity", 512),
    ScoreId => ("score id", 512),
    // Existing authenticated query cursors bind typed scope, access, key,
    // participant, and watermark identity. Keep the application envelope
    // bounded without forcing a second compact cursor scheme.
    OpaqueCursor => ("opaque cursor", 4_096),
);

/// Application-level freshness. Missing or partial truth never becomes current.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessState {
    Current,
    Stale,
    Unknown,
}

/// Temporal provenance for a packet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TemporalState {
    pub requested_mode: TemporalModeV1,
    pub requested_at: UtcMicros,
    pub resolved_at: UtcMicros,
    pub source_generation: Option<CodeGenerationId>,
    pub watermark_digest: Option<ManifestDigest>,
    pub freshness: FreshnessState,
}

impl TemporalState {
    pub fn current(resolved_at: UtcMicros) -> Self {
        Self {
            requested_mode: TemporalModeV1::Current,
            requested_at: resolved_at,
            resolved_at,
            source_generation: None,
            watermark_digest: None,
            freshness: FreshnessState::Current,
        }
    }
}

/// A policy decision pinned into a receipt or provider identity.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyDecisionRef {
    pub decision_id: String,
    pub revision: u64,
    pub digest: ManifestDigest,
    pub evaluator_revision: ComponentVersion,
}

impl PolicyDecisionRef {
    pub fn new(
        decision_id: impl Into<String>,
        revision: u64,
        digest: ManifestDigest,
        evaluator_revision: ComponentVersion,
    ) -> Result<Self, ApplicationContractError> {
        let decision = Self {
            decision_id: decision_id.into(),
            revision,
            digest,
            evaluator_revision,
        };
        decision.validate()?;
        Ok(decision)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.decision_id.is_empty()
            || self.decision_id.trim() != self.decision_id
            || self.decision_id.len() > 512
            || self.decision_id.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "policy decision id",
            });
        }
        if self.revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "policy decision revision",
            });
        }
        self.digest.validate()?;
        self.evaluator_revision.validate()?;
        Ok(())
    }
}

/// Proof that this request crossed the current authorization boundary.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorityReceipt {
    pub grant_id: CapabilityGrantId,
    pub grant_revision: u64,
    pub grant_digest: ManifestDigest,
    pub authorized_scope_digest: ManifestDigest,
    pub disclosure: DisclosureClass,
    pub policy: PolicyDecisionRef,
    pub revalidated_at: UtcMicros,
}

impl AuthorityReceipt {
    pub fn from_context(
        context: &RequestContext,
        policy: PolicyDecisionRef,
        revalidated_at: UtcMicros,
    ) -> Result<Self, ApplicationContractError> {
        context.validate()?;
        policy.validate()?;
        let receipt = Self {
            grant_id: context.grant().grant_id.clone(),
            grant_revision: context.grant().revision,
            grant_digest: context.grant().digest.clone(),
            authorized_scope_digest: context.scope().scope_digest.clone(),
            disclosure: context.grant().disclosure,
            policy,
            revalidated_at,
        };
        receipt.validate_for(context.scope())?;
        Ok(receipt)
    }

    pub fn validate_for(&self, scope: &ResolvedScope) -> Result<(), ApplicationContractError> {
        if self.grant_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "authority receipt grant revision",
            });
        }
        self.grant_digest.validate()?;
        self.authorized_scope_digest.validate()?;
        if self.authorized_scope_digest != scope.scope_digest {
            return Err(ApplicationContractError::Inconsistent {
                field: "authority receipt scope",
            });
        }
        self.policy.validate()
    }
}

/// Requested evidence domain for bounded coverage and omissions.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceDomain {
    Symbol,
    Source,
    Graph,
    Test,
    Temporal,
    Anchor,
    Operational,
    Diagnostic,
}

/// Completeness is explicit; unknown never renders as clean.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CoverageCompleteness {
    Complete,
    Partial,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoverageDomainState {
    pub domain: EvidenceDomain,
    pub completeness: CoverageCompleteness,
}

/// Deterministic coverage fold input for an evidence packet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceCoverage {
    pub requested_domains: Vec<EvidenceDomain>,
    pub visited: Option<u64>,
    pub eligible: Option<u64>,
    pub returned: u64,
    pub completeness: CoverageCompleteness,
    pub domains: Vec<CoverageDomainState>,
}

impl EvidenceCoverage {
    pub fn complete(
        mut requested_domains: Vec<EvidenceDomain>,
        visited: u64,
        eligible: u64,
        returned: u64,
    ) -> Result<Self, ApplicationContractError> {
        requested_domains.sort_unstable();
        if requested_domains.is_empty()
            || requested_domains.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "coverage requested domains",
            });
        }
        if returned > eligible || visited < eligible {
            return Err(ApplicationContractError::InvalidRange {
                field: "complete coverage counts",
            });
        }
        let domains = requested_domains
            .iter()
            .copied()
            .map(|domain| CoverageDomainState {
                domain,
                completeness: CoverageCompleteness::Complete,
            })
            .collect();
        Ok(Self {
            requested_domains,
            visited: Some(visited),
            eligible: Some(eligible),
            returned,
            completeness: CoverageCompleteness::Complete,
            domains,
        })
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.requested_domains.is_empty()
            || self
                .requested_domains
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || self
                .domains
                .windows(2)
                .any(|pair| pair[0].domain >= pair[1].domain)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "coverage canonical order",
            });
        }
        if !self
            .requested_domains
            .iter()
            .copied()
            .eq(self.domains.iter().map(|state| state.domain))
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "coverage requested domain states",
            });
        }
        if self.completeness == CoverageCompleteness::Complete
            && (self.visited.is_none()
                || self.eligible.is_none()
                || self
                    .domains
                    .iter()
                    .any(|state| state.completeness != CoverageCompleteness::Complete))
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "complete coverage state",
            });
        }
        if let Some(eligible) = self.eligible
            && self.returned > eligible
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "coverage returned count",
            });
        }
        Ok(())
    }
}

/// Safe reason why authorized requested evidence was omitted.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OmissionReason {
    Budget,
    Redacted,
    Unavailable,
    Unsupported,
    Stale,
    Failed,
    Cancelled,
    TimedOut,
    Conflict,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Omission {
    pub domain: EvidenceDomain,
    pub count: u64,
    pub reason: OmissionReason,
}

/// Evidence score semantics. Scores are metadata, never authority.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceScoreKind {
    OrdinalRank,
    HeuristicScore,
    CalibratedProbability,
    CalibratedInterval,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EvidenceScoreValue {
    Ordinal {
        rank: u64,
    },
    FixedPoint {
        micros: u64,
    },
    Interval {
        lower_micros: u64,
        upper_micros: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceScore {
    pub score_id: ScoreId,
    pub kind: EvidenceScoreKind,
    pub value: EvidenceScoreValue,
    pub calibration_revision: Option<ComponentVersion>,
    pub calibration_valid: Option<bool>,
    pub deterministic_components: Vec<String>,
}

/// Evidence authority is separate from caller authority and cannot grant access.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceAuthority {
    pub evidence_id: EvidenceIdentity,
    pub source_kind: String,
    pub producer: String,
    pub scope: ResolvedScope,
    pub revision: ComponentVersion,
    pub horizon: Option<UtcMicros>,
}

/// Bounded elapsed-work classification supplied by a retriever.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BudgetClass {
    WithinBudget,
    ApproachingLimit,
    Exhausted,
}

/// Terminal state reported by one retriever contribution.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RetrieverContributionState {
    Completed,
    Partial,
    Unavailable,
    Unsupported,
    Stale,
    Failed,
    Cancelled,
    TimedOut,
}

/// One source-owned contribution to the packet fold.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrieverContribution {
    pub retriever_id: RetrieverId,
    pub contract: ResultContractRef,
    pub producer_revision: ComponentVersion,
    pub domain: EvidenceDomain,
    pub state: RetrieverContributionState,
    pub coverage: EvidenceCoverage,
    pub returned_count: u64,
    pub omitted_count: u64,
    pub score_ids: Vec<ScoreId>,
    pub provenance_anchors: Vec<RetrievalAnchorId>,
    pub evidence_authorities: Vec<EvidenceIdentity>,
    pub elapsed_budget_class: BudgetClass,
}

/// Stable page state. Cursor bytes stay opaque until authorization is
/// revalidated by the application service.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PageState {
    pub sort_contract_id: SortContractId,
    pub sort_revision: u32,
    pub total: Option<u64>,
    pub returned: u64,
    pub cursor: Option<OpaqueCursor>,
    pub expires_at: Option<UtcMicros>,
}

impl PageState {
    pub fn first_page(
        sort_contract_id: SortContractId,
        sort_revision: u32,
        total: Option<u64>,
        returned: u64,
    ) -> Result<Self, ApplicationContractError> {
        if sort_revision == 0 || total.is_some_and(|count| returned > count) {
            return Err(ApplicationContractError::InvalidRange {
                field: "page state",
            });
        }
        Ok(Self {
            sort_contract_id,
            sort_revision,
            total,
            returned,
            cursor: None,
            expires_at: None,
        })
    }
}

/// Port-produced read evidence before application authorization/receipt
/// assembly. It is a value, not a dispatcher or generic retrieval trait.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetrievalEvidence<T> {
    pub payload: Option<T>,
    pub temporal: TemporalState,
    pub evidence_authorities: Vec<EvidenceAuthority>,
    pub coverage: EvidenceCoverage,
    pub omissions: Vec<Omission>,
    pub scores: Vec<EvidenceScore>,
    pub contributions: Vec<RetrieverContribution>,
    pub page: PageState,
    pub finished_at: UtcMicros,
    pub budget: OperationBudgetUsage,
    pub cancellation: Option<CancellationObservation>,
}

/// Immutable evidence packet consumed by adapters and later planner work.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidencePacket<T> {
    pub temporal: TemporalState,
    pub authority: AuthorityReceipt,
    pub evidence_authorities: Vec<EvidenceAuthority>,
    pub coverage: EvidenceCoverage,
    pub omissions: Vec<Omission>,
    pub scores: Vec<EvidenceScore>,
    pub contributions: Vec<RetrieverContribution>,
    pub page: PageState,
    pub execution: OperationReceipt,
    pub payload: Option<T>,
}

impl<T> EvidencePacket<T> {
    pub fn from_retrieval(
        evidence: RetrievalEvidence<T>,
        authority: AuthorityReceipt,
        execution: OperationReceipt,
    ) -> Result<Self, ApplicationContractError> {
        evidence.coverage.validate()?;
        execution.validate()?;
        if execution.ended_at != evidence.finished_at
            || execution.budget != evidence.budget
            || execution.cancellation != evidence.cancellation
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "retrieval evidence execution receipt",
            });
        }
        if matches!(
            execution.termination,
            super::OperationTermination::Completed
        ) && evidence.payload.is_none()
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "completed evidence payload",
            });
        }
        Ok(Self {
            temporal: evidence.temporal,
            authority,
            evidence_authorities: evidence.evidence_authorities,
            coverage: evidence.coverage,
            omissions: evidence.omissions,
            scores: evidence.scores,
            contributions: evidence.contributions,
            page: evidence.page,
            execution,
            payload: evidence.payload,
        })
    }
}

impl<T> EvidencePacket<Vec<T>> {
    pub fn is_truthful_complete_empty(&self) -> bool {
        self.execution.termination == super::OperationTermination::Completed
            && self.coverage.completeness == CoverageCompleteness::Complete
            && self.omissions.is_empty()
            && self.payload.as_ref().is_some_and(Vec::is_empty)
    }
}
