use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::{ActorId, DomainError, FactId, FactOwnerV1, ProvenanceId, UtcMicros};

use super::super::queries::MAX_CURRENT_LIMIT;
use super::super::{
    FactCommitOutcome, FactLineageError, FactLineageResult, FactWriteBatch, MAX_FACT_REASON_BYTES,
    validate_owned_fact_id,
};
use super::{FactAddCommand, FactMapping};

/// Authoritative proposal states from which an interrupted promotion may resume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactProposalPromotionStateV1 {
    PendingApproval,
    Applying,
}

/// One compare-and-swap request whose proposal transition and fact batch must
/// commit in the same authority transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromoteFactProposal {
    proposal_id: ProvenanceId,
    owner: FactOwnerV1,
    expected_state: FactProposalPromotionStateV1,
    reviewer: Option<ActorId>,
    batch: FactWriteBatch,
}

impl PromoteFactProposal {
    pub fn new(
        proposal_id: ProvenanceId,
        owner: FactOwnerV1,
        expected_state: FactProposalPromotionStateV1,
        reviewer: Option<ActorId>,
        batch: FactWriteBatch,
    ) -> FactLineageResult<Self> {
        proposal_id.validate()?;
        owner.validate()?;
        if let Some(reviewer) = &reviewer {
            reviewer.validate()?;
        }
        if batch.owner() != &owner {
            return Err(FactLineageError::OwnerMismatch);
        }
        Ok(Self {
            proposal_id,
            owner,
            expected_state,
            reviewer,
            batch,
        })
    }

    pub fn proposal_id(&self) -> &ProvenanceId {
        &self.proposal_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn expected_state(&self) -> FactProposalPromotionStateV1 {
        self.expected_state
    }

    pub fn reviewer(&self) -> Option<&ActorId> {
        self.reviewer.as_ref()
    }

    pub fn batch(&self) -> &FactWriteBatch {
        &self.batch
    }
}

/// Result of one atomic proposal CAS and fact append.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromoteFactProposalOutcome {
    proposal_id: ProvenanceId,
    previous_state: FactProposalPromotionStateV1,
    commit: FactCommitOutcome,
}

impl PromoteFactProposalOutcome {
    pub fn new(
        proposal_id: ProvenanceId,
        previous_state: FactProposalPromotionStateV1,
        commit: FactCommitOutcome,
    ) -> Result<Self, DomainError> {
        proposal_id.validate()?;
        Ok(Self {
            proposal_id,
            previous_state,
            commit,
        })
    }

    pub fn proposal_id(&self) -> &ProvenanceId {
        &self.proposal_id
    }

    pub fn previous_state(&self) -> FactProposalPromotionStateV1 {
        self.previous_state
    }

    pub fn commit(&self) -> &FactCommitOutcome {
        &self.commit
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactProposalState {
    PendingApproval,
    Applying,
    Applied,
    Rejected,
    Quarantined,
}

/// Presentation evidence committed with a proposal in the canonical fact
/// authority. This is the only source for proposal dashboard metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactProposalEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proposal: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    validation: Option<Value>,
}

impl FactProposalEvidence {
    pub fn new(
        evidence_hash: Option<String>,
        proposal: Option<Value>,
        validation: Option<Value>,
    ) -> FactLineageResult<Self> {
        let evidence = Self {
            evidence_hash,
            proposal,
            validation,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    fn validate(&self) -> FactLineageResult<()> {
        if self.evidence_hash.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > 160 || value.chars().any(char::is_control)
        }) {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "fact proposal evidence hash",
            }));
        }
        Ok(())
    }

    pub fn evidence_hash(&self) -> Option<&str> {
        self.evidence_hash.as_deref()
    }

    pub fn proposal(&self) -> Option<&Value> {
        self.proposal.as_ref()
    }

    pub fn validation(&self) -> Option<&Value> {
        self.validation.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactProposalRevision(u64);

impl FactProposalRevision {
    pub fn new(value: u64) -> FactLineageResult<Self> {
        if value == 0 {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "fact proposal revision",
            }));
        }
        Ok(Self(value))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactProposalPromotion {
    owner: FactOwnerV1,
    proposal_id: ProvenanceId,
    expected_revision: FactProposalRevision,
    reviewer: Option<ActorId>,
}

impl FactProposalPromotion {
    pub fn new(
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_revision: FactProposalRevision,
        reviewer: Option<ActorId>,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        proposal_id.validate()?;
        if let Some(reviewer) = &reviewer {
            reviewer.validate()?;
        }
        Ok(Self {
            owner,
            proposal_id,
            expected_revision,
            reviewer,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn proposal_id(&self) -> &ProvenanceId {
        &self.proposal_id
    }
    pub fn expected_revision(&self) -> FactProposalRevision {
        self.expected_revision
    }
    pub fn reviewer(&self) -> Option<&ActorId> {
        self.reviewer.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactProposalRecord {
    proposal_id: ProvenanceId,
    owner: FactOwnerV1,
    revision: FactProposalRevision,
    state: FactProposalState,
    request: FactAddCommand,
    applied_fact_id: Option<FactId>,
    applied_mapping: Option<FactMapping>,
    automation_run_id: Option<String>,
    evidence: FactProposalEvidence,
    reviewer: Option<ActorId>,
    reason: Option<String>,
    submitted_at: UtcMicros,
    updated_at: UtcMicros,
}

impl FactProposalRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        proposal_id: ProvenanceId,
        owner: FactOwnerV1,
        revision: FactProposalRevision,
        state: FactProposalState,
        request: FactAddCommand,
        applied_fact_id: Option<FactId>,
        applied_mapping: Option<FactMapping>,
        evidence: FactProposalEvidence,
        reviewer: Option<ActorId>,
        reason: Option<String>,
        submitted_at: UtcMicros,
        updated_at: UtcMicros,
    ) -> FactLineageResult<Self> {
        proposal_id.validate()?;
        owner.validate()?;
        if request.owner() != &owner {
            return Err(FactLineageError::OwnerMismatch);
        }
        if let Some(fact_id) = &applied_fact_id {
            validate_owned_fact_id(fact_id, &owner)?;
        }
        if let Some(mapping) = &applied_mapping {
            if mapping.owner() != &owner {
                return Err(FactLineageError::OwnerMismatch);
            }
            if applied_fact_id.as_ref() != Some(mapping.fact_id()) {
                return Err(FactLineageError::FactMismatch);
            }
        }
        if let Some(reviewer) = &reviewer {
            reviewer.validate()?;
        }
        evidence.validate()?;
        if reason
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > MAX_FACT_REASON_BYTES)
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "fact proposal reason",
            }));
        }
        if updated_at < submitted_at {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "fact proposal timestamps",
            }));
        }
        let automation_run_id = request.automation_run_id().map(ToOwned::to_owned);
        Ok(Self {
            proposal_id,
            owner,
            revision,
            state,
            request,
            applied_fact_id,
            applied_mapping,
            automation_run_id,
            evidence,
            reviewer,
            reason,
            submitted_at,
            updated_at,
        })
    }

    pub fn proposal_id(&self) -> &ProvenanceId {
        &self.proposal_id
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn revision(&self) -> FactProposalRevision {
        self.revision
    }
    pub fn state(&self) -> FactProposalState {
        self.state
    }
    pub fn request(&self) -> &FactAddCommand {
        &self.request
    }
    pub fn applied_fact_id(&self) -> Option<&FactId> {
        self.applied_fact_id.as_ref()
    }
    pub fn legacy_fact_id(&self) -> Option<i64> {
        self.applied_mapping
            .as_ref()
            .and_then(FactMapping::legacy_fact_id)
    }
    /// Durable automation identity from typed canonical command metadata. It
    /// is never inferred from proposal IDs, payload metadata, or sidecars.
    pub fn automation_run_id(&self) -> Option<&str> {
        self.automation_run_id.as_deref()
    }
    pub fn evidence(&self) -> &FactProposalEvidence {
        &self.evidence
    }
    pub fn reviewer(&self) -> Option<&ActorId> {
        self.reviewer.as_ref()
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    pub fn submitted_at(&self) -> UtcMicros {
        self.submitted_at
    }
    pub fn updated_at(&self) -> UtcMicros {
        self.updated_at
    }
}

/// Atomic promotion disposition. `AlreadyPromoted` is an idempotent replay of
/// the same authority decision, not a caller-side pre-read or inferred state.
/// `Quarantined` is a durable privacy rejection and must not be retried as an
/// ordinary pending proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactProposalPromotionDisposition {
    NewlyPromoted,
    AlreadyPromoted,
    Quarantined,
}

/// One authoritative proposal promotion result. The proposal is always the
/// durable terminal record; callers run downstream digest work only for
/// `NewlyPromoted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactProposalPromotionResult {
    proposal: FactProposalRecord,
    disposition: FactProposalPromotionDisposition,
}

impl FactProposalPromotionResult {
    pub fn new(
        proposal: FactProposalRecord,
        disposition: FactProposalPromotionDisposition,
    ) -> FactLineageResult<Self> {
        let state_matches_disposition = matches!(
            (proposal.state(), disposition),
            (
                FactProposalState::Applied,
                FactProposalPromotionDisposition::NewlyPromoted
                    | FactProposalPromotionDisposition::AlreadyPromoted,
            ) | (
                FactProposalState::Quarantined,
                FactProposalPromotionDisposition::Quarantined,
            )
        );
        if !state_matches_disposition {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "fact proposal promotion result state",
            }));
        }
        Ok(Self {
            proposal,
            disposition,
        })
    }

    pub fn proposal(&self) -> &FactProposalRecord {
        &self.proposal
    }

    pub fn disposition(&self) -> FactProposalPromotionDisposition {
        self.disposition
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactProposalPage {
    owner: FactOwnerV1,
    proposals: Vec<FactProposalRecord>,
    next_after_proposal_id: Option<ProvenanceId>,
}

impl FactProposalPage {
    pub fn new(
        owner: FactOwnerV1,
        proposals: Vec<FactProposalRecord>,
        next_after_proposal_id: Option<ProvenanceId>,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        if proposals.len() > MAX_CURRENT_LIMIT {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: proposals.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        let mut previous: Option<&ProvenanceId> = None;
        for proposal in &proposals {
            if proposal.owner() != &owner {
                return Err(FactLineageError::OwnerMismatch);
            }
            if previous.is_some_and(|value| value >= proposal.proposal_id()) {
                return Err(FactLineageError::Contract(DomainError::NonCanonical {
                    field: "fact proposal page order",
                }));
            }
            previous = Some(proposal.proposal_id());
        }
        if let Some(cursor) = &next_after_proposal_id {
            cursor.validate()?;
            if previous.is_some_and(|last| cursor <= last) {
                return Err(FactLineageError::Contract(DomainError::NonCanonical {
                    field: "fact proposal page cursor",
                }));
            }
        }
        Ok(Self {
            owner,
            proposals,
            next_after_proposal_id,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn proposals(&self) -> &[FactProposalRecord] {
        &self.proposals
    }
    pub fn next_after_proposal_id(&self) -> Option<&ProvenanceId> {
        self.next_after_proposal_id.as_ref()
    }
}
