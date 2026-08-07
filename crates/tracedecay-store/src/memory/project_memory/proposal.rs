use tracedecay_domain::{
    ActorId, DomainError, FactId, FactOwnerV1, LocatorDigest, ProvenanceId, SourceStoreId,
};

use super::super::queries::MAX_CURRENT_LIMIT;
use super::super::{
    FactCommitOutcome, FactStoreError, FactStoreResult, FactWriteBatch,
    MAX_COMPATIBILITY_REASON_BYTES, validate_owned_fact_id,
};
use super::{ProjectMemoryFactAddCommandV1, ProjectMemoryFactMappingV1};

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
    ) -> FactStoreResult<Self> {
        proposal_id.validate()?;
        owner.validate()?;
        if let Some(reviewer) = &reviewer {
            reviewer.validate()?;
        }
        if batch.owner() != &owner {
            return Err(FactStoreError::OwnerMismatch);
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
pub enum ProjectMemoryFactProposalStateV1 {
    PendingApproval,
    Applying,
    Applied,
    Rejected,
    Quarantined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectMemoryFactProposalRevisionV1(u64);

impl ProjectMemoryFactProposalRevisionV1 {
    pub fn new(value: u64) -> FactStoreResult<Self> {
        if value == 0 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact proposal revision",
            }));
        }
        Ok(Self(value))
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactProposalPromotionV1 {
    owner: FactOwnerV1,
    proposal_id: ProvenanceId,
    expected_revision: ProjectMemoryFactProposalRevisionV1,
    reviewer: Option<ActorId>,
}

impl ProjectMemoryFactProposalPromotionV1 {
    pub fn new(
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_revision: ProjectMemoryFactProposalRevisionV1,
        reviewer: Option<ActorId>,
    ) -> FactStoreResult<Self> {
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
    pub fn expected_revision(&self) -> ProjectMemoryFactProposalRevisionV1 {
        self.expected_revision
    }
    pub fn reviewer(&self) -> Option<&ActorId> {
        self.reviewer.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactProposalRecordV1 {
    proposal_id: ProvenanceId,
    owner: FactOwnerV1,
    revision: ProjectMemoryFactProposalRevisionV1,
    state: ProjectMemoryFactProposalStateV1,
    request: ProjectMemoryFactAddCommandV1,
    applied_fact_id: Option<FactId>,
    applied_mapping: Option<ProjectMemoryFactMappingV1>,
    automation_run_id: Option<String>,
    reviewer: Option<ActorId>,
    reason: Option<String>,
}

impl ProjectMemoryFactProposalRecordV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        proposal_id: ProvenanceId,
        owner: FactOwnerV1,
        revision: ProjectMemoryFactProposalRevisionV1,
        state: ProjectMemoryFactProposalStateV1,
        request: ProjectMemoryFactAddCommandV1,
        applied_fact_id: Option<FactId>,
        applied_mapping: Option<ProjectMemoryFactMappingV1>,
        reviewer: Option<ActorId>,
        reason: Option<String>,
    ) -> FactStoreResult<Self> {
        proposal_id.validate()?;
        owner.validate()?;
        if request.owner() != &owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        if let Some(fact_id) = &applied_fact_id {
            validate_owned_fact_id(fact_id, &owner)?;
        }
        if let Some(mapping) = &applied_mapping {
            if mapping.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            if applied_fact_id.as_ref() != Some(mapping.fact_id()) {
                return Err(FactStoreError::FactMismatch);
            }
        }
        if let Some(reviewer) = &reviewer {
            reviewer.validate()?;
        }
        if reason.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact proposal reason",
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
            reviewer,
            reason,
        })
    }

    pub fn proposal_id(&self) -> &ProvenanceId {
        &self.proposal_id
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn revision(&self) -> ProjectMemoryFactProposalRevisionV1 {
        self.revision
    }
    pub fn state(&self) -> ProjectMemoryFactProposalStateV1 {
        self.state
    }
    pub fn request(&self) -> &ProjectMemoryFactAddCommandV1 {
        &self.request
    }
    pub fn applied_fact_id(&self) -> Option<&FactId> {
        self.applied_fact_id.as_ref()
    }
    pub fn legacy_fact_id(&self) -> Option<i64> {
        self.applied_mapping
            .as_ref()
            .and_then(ProjectMemoryFactMappingV1::legacy_fact_id)
    }
    /// Durable automation identity from typed canonical command metadata. It
    /// is never inferred from proposal IDs, payload metadata, or sidecars.
    pub fn automation_run_id(&self) -> Option<&str> {
        self.automation_run_id.as_deref()
    }
    pub fn reviewer(&self) -> Option<&ActorId> {
        self.reviewer.as_ref()
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// Atomic promotion disposition. `AlreadyPromoted` is an idempotent replay of
/// the same authority decision, not a caller-side pre-read or inferred state.
/// `Quarantined` is a durable privacy rejection and must not be retried as an
/// ordinary pending proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactProposalPromotionDispositionV1 {
    NewlyPromoted,
    AlreadyPromoted,
    Quarantined,
}

/// One authoritative proposal promotion result. The proposal is always the
/// durable terminal record; callers run downstream digest work only for
/// `NewlyPromoted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactProposalPromotionResultV1 {
    proposal: ProjectMemoryFactProposalRecordV1,
    disposition: ProjectMemoryFactProposalPromotionDispositionV1,
}

impl ProjectMemoryFactProposalPromotionResultV1 {
    pub fn new(
        proposal: ProjectMemoryFactProposalRecordV1,
        disposition: ProjectMemoryFactProposalPromotionDispositionV1,
    ) -> FactStoreResult<Self> {
        let state_matches_disposition = matches!(
            (proposal.state(), disposition),
            (
                ProjectMemoryFactProposalStateV1::Applied,
                ProjectMemoryFactProposalPromotionDispositionV1::NewlyPromoted
                    | ProjectMemoryFactProposalPromotionDispositionV1::AlreadyPromoted,
            ) | (
                ProjectMemoryFactProposalStateV1::Quarantined,
                ProjectMemoryFactProposalPromotionDispositionV1::Quarantined,
            )
        );
        if !state_matches_disposition {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact proposal promotion result state",
            }));
        }
        Ok(Self {
            proposal,
            disposition,
        })
    }

    pub fn proposal(&self) -> &ProjectMemoryFactProposalRecordV1 {
        &self.proposal
    }

    pub fn disposition(&self) -> ProjectMemoryFactProposalPromotionDispositionV1 {
        self.disposition
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactProposalPageV1 {
    owner: FactOwnerV1,
    proposals: Vec<ProjectMemoryFactProposalRecordV1>,
    next_after_proposal_id: Option<ProvenanceId>,
}

impl ProjectMemoryFactProposalPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        proposals: Vec<ProjectMemoryFactProposalRecordV1>,
        next_after_proposal_id: Option<ProvenanceId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if proposals.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: proposals.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        let mut previous: Option<&ProvenanceId> = None;
        for proposal in &proposals {
            if proposal.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            if previous.is_some_and(|value| value >= proposal.proposal_id()) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact proposal page order",
                }));
            }
            previous = Some(proposal.proposal_id());
        }
        if let Some(cursor) = &next_after_proposal_id {
            cursor.validate()?;
            if previous.is_some_and(|last| cursor <= last) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact proposal page cursor",
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
    pub fn proposals(&self) -> &[ProjectMemoryFactProposalRecordV1] {
        &self.proposals
    }
    pub fn next_after_proposal_id(&self) -> Option<&ProvenanceId> {
        self.next_after_proposal_id.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactProposalLegacyRecordV1 {
    legacy_proposal_id: i64,
    state: ProjectMemoryFactProposalStateV1,
    request: ProjectMemoryFactAddCommandV1,
}

impl ProjectMemoryFactProposalLegacyRecordV1 {
    pub fn new(
        legacy_proposal_id: i64,
        state: ProjectMemoryFactProposalStateV1,
        request: ProjectMemoryFactAddCommandV1,
    ) -> FactStoreResult<Self> {
        if legacy_proposal_id <= 0 {
            return Err(FactStoreError::InvalidLegacyFactId {
                legacy_fact_id: legacy_proposal_id,
            });
        }
        Ok(Self {
            legacy_proposal_id,
            state,
            request,
        })
    }

    pub fn legacy_proposal_id(&self) -> i64 {
        self.legacy_proposal_id
    }
    pub fn state(&self) -> ProjectMemoryFactProposalStateV1 {
        self.state
    }
    pub fn request(&self) -> &ProjectMemoryFactAddCommandV1 {
        &self.request
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactProposalImportV1 {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
    sidecar_digest: LocatorDigest,
    records: Vec<ProjectMemoryFactProposalLegacyRecordV1>,
}

impl ProjectMemoryFactProposalImportV1 {
    pub fn new(
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
        sidecar_digest: LocatorDigest,
        records: Vec<ProjectMemoryFactProposalLegacyRecordV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        source_store_id.validate()?;
        sidecar_digest.validate()?;
        if records.is_empty() || records.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: records.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        let mut previous = None;
        for record in &records {
            if record.request().owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            if previous.is_some_and(|value| value >= record.legacy_proposal_id()) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact proposal import order",
                }));
            }
            previous = Some(record.legacy_proposal_id());
        }
        Ok(Self {
            owner,
            source_store_id,
            sidecar_digest,
            records,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn source_store_id(&self) -> &SourceStoreId {
        &self.source_store_id
    }
    pub fn sidecar_digest(&self) -> &LocatorDigest {
        &self.sidecar_digest
    }
    pub fn records(&self) -> &[ProjectMemoryFactProposalLegacyRecordV1] {
        &self.records
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactProposalImportReceiptV1 {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
    sidecar_digest: LocatorDigest,
    imported_count: usize,
    quarantined_count: usize,
}

impl ProjectMemoryFactProposalImportReceiptV1 {
    pub fn new(
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
        sidecar_digest: LocatorDigest,
        imported_count: usize,
        quarantined_count: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        source_store_id.validate()?;
        sidecar_digest.validate()?;
        Ok(Self {
            owner,
            source_store_id,
            sidecar_digest,
            imported_count,
            quarantined_count,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn source_store_id(&self) -> &SourceStoreId {
        &self.source_store_id
    }
    pub fn sidecar_digest(&self) -> &LocatorDigest {
        &self.sidecar_digest
    }
    pub fn imported_count(&self) -> usize {
        self.imported_count
    }
    pub fn quarantined_count(&self) -> usize {
        self.quarantined_count
    }
}
