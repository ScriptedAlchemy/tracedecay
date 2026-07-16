use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::future::Future;

use tracedecay_domain::{
    ActorId, Confidence, DomainError, EntityRef, FactAssertionId, FactAssertionV1, FactEventId,
    FactEvidenceRelationV1, FactId, FactIdentityMaterialV1, FactIdentitySourceV1,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, LegacyFactMappingV1,
    LegacyHistoryCoverageV1, PayloadAccessState, ProvenanceId, RetrievalAnchorId,
    RetrievalAnchorRecordV2, SourceStoreId, UtcMicros, VectorWatermark,
};

const MAX_CURRENT_LIMIT: usize = 1_000;
const MAX_LINEAGE_LIMIT: usize = 1_000;
const MAX_COMPATIBILITY_SEARCH_BYTES: usize = 4 * 1024;
const MAX_COMPATIBILITY_REASON_BYTES: usize = 4 * 1024;
const MAX_COMPATIBILITY_VECTOR_DIMENSIONS: u32 = 16_384;
const MAX_COMPATIBILITY_VECTOR_BYTES: usize = 64 * 1024;

/// One validated, atomic append to a fact's authoritative lineage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactWriteBatch {
    fact_id: FactId,
    owner: FactOwnerV1,
    identity_material: Option<FactIdentityMaterialV1>,
    assertion: Option<FactAssertionV1>,
    events: Vec<FactLineageEventV1>,
    new_anchors: Vec<RetrievalAnchorRecordV2>,
    referenced_anchor_ids: Vec<RetrievalAnchorId>,
    legacy_mapping: Option<LegacyFactMappingV1>,
    expected_last_event_id: Option<FactEventId>,
}

impl FactWriteBatch {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fact_id: FactId,
        owner: FactOwnerV1,
        assertion: Option<FactAssertionV1>,
        events: Vec<FactLineageEventV1>,
        new_anchors: Vec<RetrievalAnchorRecordV2>,
        referenced_anchor_ids: Vec<RetrievalAnchorId>,
        legacy_mapping: Option<LegacyFactMappingV1>,
        expected_last_event_id: Option<FactEventId>,
    ) -> FactStoreResult<Self> {
        fact_id.validate()?;
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if events.is_empty() {
            return Err(FactStoreError::EmptyBatch);
        }

        if let Some(assertion) = &assertion {
            if assertion.fact_id() != &fact_id {
                return Err(FactStoreError::FactMismatch);
            }
            if assertion.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            let has_recording_event = events.iter().any(|event| {
                matches!(
                    event.kind(),
                    FactLineageEventKindV1::AssertionRecorded { assertion_id }
                        if assertion_id == assertion.assertion_id()
                )
            });
            if !has_recording_event {
                return Err(FactStoreError::MissingAssertionEvent {
                    assertion_id: assertion.assertion_id().clone(),
                });
            }
        }

        let mut event_ids = BTreeSet::new();
        let mut previous_event: Option<&FactLineageEventV1> = None;
        for event in &events {
            if event.fact_id() != &fact_id {
                return Err(FactStoreError::FactMismatch);
            }
            if event.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            if !event_ids.insert(event.event_id()) {
                return Err(FactStoreError::DuplicateEventId {
                    event_id: event.event_id().clone(),
                });
            }
            if previous_event.is_some_and(|previous| {
                (previous.occurred_at(), previous.event_id())
                    > (event.occurred_at(), event.event_id())
            }) {
                return Err(FactStoreError::EventsOutOfOrder);
            }
            previous_event = Some(event);
        }

        if let Some(mapping) = &legacy_mapping {
            if mapping.fact_id() != &fact_id {
                return Err(FactStoreError::FactMismatch);
            }
            if mapping.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
        }

        let mut available_anchor_ids = BTreeSet::new();
        for anchor_id in &referenced_anchor_ids {
            anchor_id.validate()?;
            if !available_anchor_ids.insert(anchor_id) {
                return Err(FactStoreError::DuplicateAnchorId {
                    anchor_id: anchor_id.clone(),
                });
            }
        }
        for anchor in &new_anchors {
            anchor.validate()?;
            if FactOwnerV1::from(anchor.owner().clone()) != owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            if !available_anchor_ids.insert(anchor.anchor_id()) {
                return Err(FactStoreError::DuplicateAnchorId {
                    anchor_id: anchor.anchor_id().clone(),
                });
            }
        }
        validate_anchor_lineage(&new_anchors, &referenced_anchor_ids)?;
        if let Some(assertion) = &assertion {
            for evidence in assertion.evidence() {
                if !available_anchor_ids.contains(evidence.anchor_id()) {
                    return Err(FactStoreError::MissingEvidenceAnchor {
                        anchor_id: evidence.anchor_id().clone(),
                    });
                }
            }
        }

        Ok(Self {
            fact_id,
            owner,
            identity_material: None,
            assertion,
            events,
            new_anchors,
            referenced_anchor_ids,
            legacy_mapping,
            expected_last_event_id,
        })
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    /// Supplies the deterministic identity source when this batch may create
    /// the fact. Later batches may omit it because the authority already owns
    /// the immutable identity material.
    pub fn with_identity_material(
        mut self,
        identity_material: FactIdentityMaterialV1,
    ) -> FactStoreResult<Self> {
        if identity_material.owner() != &self.owner
            || FactId::derive(&identity_material)? != self.fact_id
        {
            return Err(FactStoreError::FactMismatch);
        }
        self.identity_material = Some(identity_material);
        Ok(self)
    }

    pub fn identity_material(&self) -> Option<&FactIdentityMaterialV1> {
        self.identity_material.as_ref()
    }

    pub fn assertion(&self) -> Option<&FactAssertionV1> {
        self.assertion.as_ref()
    }

    pub fn events(&self) -> &[FactLineageEventV1] {
        &self.events
    }

    pub fn new_anchors(&self) -> &[RetrievalAnchorRecordV2] {
        &self.new_anchors
    }

    pub fn referenced_anchor_ids(&self) -> &[RetrievalAnchorId] {
        &self.referenced_anchor_ids
    }

    pub fn legacy_mapping(&self) -> Option<&LegacyFactMappingV1> {
        self.legacy_mapping.as_ref()
    }

    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }

    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        self,
    ) -> (
        FactId,
        FactOwnerV1,
        Option<FactIdentityMaterialV1>,
        Option<FactAssertionV1>,
        Vec<FactLineageEventV1>,
        Vec<RetrievalAnchorRecordV2>,
        Vec<RetrievalAnchorId>,
        Option<LegacyFactMappingV1>,
        Option<FactEventId>,
    ) {
        (
            self.fact_id,
            self.owner,
            self.identity_material,
            self.assertion,
            self.events,
            self.new_anchors,
            self.referenced_anchor_ids,
            self.legacy_mapping,
            self.expected_last_event_id,
        )
    }
}

fn validate_anchor_lineage(
    new_anchors: &[RetrievalAnchorRecordV2],
    referenced_anchor_ids: &[RetrievalAnchorId],
) -> FactStoreResult<()> {
    let referenced = referenced_anchor_ids.iter().collect::<BTreeSet<_>>();
    let anchors = new_anchors
        .iter()
        .map(|anchor| (anchor.anchor_id().clone(), anchor))
        .collect::<BTreeMap<_, _>>();

    for anchor in new_anchors {
        for source in anchor.source_anchors() {
            if !referenced.contains(source.anchor_id()) && !anchors.contains_key(source.anchor_id())
            {
                return Err(FactStoreError::MissingAnchorLineageSource {
                    anchor_id: source.anchor_id().clone(),
                });
            }
        }
    }

    let mut remaining = anchors.keys().cloned().collect::<BTreeSet<_>>();
    while !remaining.is_empty() {
        let removable = remaining
            .iter()
            .find(|anchor_id| {
                anchors[*anchor_id]
                    .source_anchors()
                    .iter()
                    .all(|source| !remaining.contains(source.anchor_id()))
            })
            .cloned();
        let Some(anchor_id) = removable else {
            let Some(anchor_id) = remaining.first().cloned() else {
                break;
            };
            return Err(FactStoreError::CyclicAnchorLineage { anchor_id });
        };
        remaining.remove(&anchor_id);
    }
    Ok(())
}

/// Deterministic current or as-of projection of one fact's lineage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFactV1 {
    fact_id: FactId,
    owner: FactOwnerV1,
    payload: Option<FactPayloadV1>,
    payload_access: PayloadAccessState,
    trust: Confidence,
    active_assertion_id: FactAssertionId,
    last_event_id: FactEventId,
    legacy_mapping: Option<LegacyFactMappingV1>,
    projected_as_of: UtcMicros,
}

impl StoredFactV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fact_id: FactId,
        owner: FactOwnerV1,
        payload: Option<FactPayloadV1>,
        payload_access: PayloadAccessState,
        trust: Confidence,
        active_assertion_id: FactAssertionId,
        last_event_id: FactEventId,
        legacy_mapping: Option<LegacyFactMappingV1>,
        projected_as_of: UtcMicros,
    ) -> FactStoreResult<Self> {
        fact_id.validate()?;
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        active_assertion_id.validate()?;
        last_event_id.validate()?;
        if payload.is_some() != (payload_access == PayloadAccessState::Eligible) {
            return Err(FactStoreError::PayloadAccessMismatch);
        }
        if let Some(mapping) = &legacy_mapping {
            if mapping.fact_id() != &fact_id {
                return Err(FactStoreError::FactMismatch);
            }
            if mapping.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
        }
        Ok(Self {
            fact_id,
            owner,
            payload,
            payload_access,
            trust,
            active_assertion_id,
            last_event_id,
            legacy_mapping,
            projected_as_of,
        })
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn payload(&self) -> Option<&FactPayloadV1> {
        self.payload.as_ref()
    }

    pub fn payload_access(&self) -> PayloadAccessState {
        self.payload_access
    }

    pub fn trust(&self) -> Confidence {
        self.trust
    }

    pub fn active_assertion_id(&self) -> &FactAssertionId {
        &self.active_assertion_id
    }

    pub fn last_event_id(&self) -> &FactEventId {
        &self.last_event_id
    }

    pub fn legacy_mapping(&self) -> Option<&LegacyFactMappingV1> {
        self.legacy_mapping.as_ref()
    }

    pub fn projected_as_of(&self) -> UtcMicros {
        self.projected_as_of
    }
}

/// Page of current facts ordered by `(FactId)` after the exclusive cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentFactsQuery {
    owner: FactOwnerV1,
    after_fact_id: Option<FactId>,
    limit: usize,
}

/// One current fact, authorized by its canonical owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactCurrentQuery {
    owner: FactOwnerV1,
    fact_id: FactId,
}

impl FactCurrentQuery {
    pub fn new(owner: FactOwnerV1, fact_id: FactId) -> FactStoreResult<Self> {
        owner.validate()?;
        fact_id.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        Ok(Self { owner, fact_id })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }
}

impl CurrentFactsQuery {
    pub fn new(
        owner: FactOwnerV1,
        after_fact_id: Option<FactId>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if let Some(fact_id) = &after_fact_id {
            fact_id.validate()?;
            validate_owned_fact_id(fact_id, &owner)?;
        }
        validate_limit(limit, MAX_CURRENT_LIMIT)?;
        Ok(Self {
            owner,
            after_fact_id,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn after_fact_id(&self) -> Option<&FactId> {
        self.after_fact_id.as_ref()
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// One fact projected through an inclusive UTC timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactAsOfQuery {
    owner: FactOwnerV1,
    fact_id: FactId,
    as_of: UtcMicros,
}

impl FactAsOfQuery {
    pub fn new(owner: FactOwnerV1, fact_id: FactId, as_of: UtcMicros) -> FactStoreResult<Self> {
        owner.validate()?;
        fact_id.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        Ok(Self {
            owner,
            fact_id,
            as_of,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn as_of(&self) -> UtcMicros {
        self.as_of
    }
}

/// Exclusive cursor for lineage ordered by `(occurred_at, FactEventId)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactLineageCursor {
    occurred_at: UtcMicros,
    event_id: FactEventId,
}

impl FactLineageCursor {
    pub fn new(occurred_at: UtcMicros, event_id: FactEventId) -> FactStoreResult<Self> {
        event_id.validate()?;
        Ok(Self {
            occurred_at,
            event_id,
        })
    }

    pub fn occurred_at(&self) -> UtcMicros {
        self.occurred_at
    }

    pub fn event_id(&self) -> &FactEventId {
        &self.event_id
    }
}

/// Page of lineage events ordered by `(occurred_at, FactEventId)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactLineageQuery {
    owner: FactOwnerV1,
    fact_id: FactId,
    after: Option<FactLineageCursor>,
    limit: usize,
}

/// Compatibility lookup for one V1 integer identity in its original store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyFactQuery {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
    legacy_fact_id: i64,
}

impl LegacyFactQuery {
    pub fn new(
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
        legacy_fact_id: i64,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        source_store_id.validate()?;
        if legacy_fact_id <= 0 {
            return Err(FactStoreError::InvalidLegacyFactId { legacy_fact_id });
        }
        Ok(Self {
            owner,
            source_store_id,
            legacy_fact_id,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn source_store_id(&self) -> &SourceStoreId {
        &self.source_store_id
    }

    pub fn legacy_fact_id(&self) -> i64 {
        self.legacy_fact_id
    }

    /// Validate the canonical result returned for this legacy lookup.
    pub fn validate_resolved_fact_id(&self, fact_id: &FactId) -> FactStoreResult<()> {
        validate_owned_fact_id(fact_id, &self.owner)
    }
}

impl FactLineageQuery {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: FactId,
        after: Option<FactLineageCursor>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        fact_id.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        validate_limit(limit, MAX_LINEAGE_LIMIT)?;
        Ok(Self {
            owner,
            fact_id,
            after,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn after(&self) -> Option<&FactLineageCursor> {
        self.after.as_ref()
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// Owner-authorized lookup for a stable retrieval anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetrievalAnchorQuery {
    owner: FactOwnerV1,
    anchor_id: RetrievalAnchorId,
}

impl RetrievalAnchorQuery {
    pub fn new(owner: FactOwnerV1, anchor_id: RetrievalAnchorId) -> FactStoreResult<Self> {
        owner.validate()?;
        anchor_id.validate()?;
        Ok(Self { owner, anchor_id })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }
}

fn validate_limit(limit: usize, max: usize) -> FactStoreResult<()> {
    if !(1..=max).contains(&limit) {
        return Err(FactStoreError::InvalidQueryLimit { limit, max });
    }
    Ok(())
}

fn validate_owned_fact_id(fact_id: &FactId, owner: &FactOwnerV1) -> FactStoreResult<()> {
    fact_id
        .validate_owner(owner)
        .map_err(|_| FactStoreError::OwnerMismatch)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactCommitReceipt {
    fact_id: FactId,
    owner: FactOwnerV1,
    committed_event_ids: Vec<FactEventId>,
    last_event_id: FactEventId,
    active_assertion_id: Option<FactAssertionId>,
}

impl FactCommitReceipt {
    pub fn new(
        fact_id: FactId,
        owner: FactOwnerV1,
        committed_event_ids: Vec<FactEventId>,
        last_event_id: FactEventId,
        active_assertion_id: Option<FactAssertionId>,
    ) -> FactStoreResult<Self> {
        fact_id.validate()?;
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        last_event_id.validate()?;
        if committed_event_ids.is_empty() || committed_event_ids.last() != Some(&last_event_id) {
            return Err(FactStoreError::InvalidCommitReceipt);
        }
        let mut seen = BTreeSet::new();
        for event_id in &committed_event_ids {
            event_id.validate()?;
            if !seen.insert(event_id) {
                return Err(FactStoreError::DuplicateEventId {
                    event_id: event_id.clone(),
                });
            }
        }
        if let Some(assertion_id) = &active_assertion_id {
            assertion_id.validate()?;
        }
        Ok(Self {
            fact_id,
            owner,
            committed_event_ids,
            last_event_id,
            active_assertion_id,
        })
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn committed_event_ids(&self) -> &[FactEventId] {
        &self.committed_event_ids
    }

    pub fn last_event_id(&self) -> &FactEventId {
        &self.last_event_id
    }

    pub fn active_assertion_id(&self) -> Option<&FactAssertionId> {
        self.active_assertion_id.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FactCommitConflict {
    LastEventMismatch {
        expected: Option<FactEventId>,
        actual: Option<FactEventId>,
    },
    IdentityCollision {
        kind: &'static str,
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FactCommitOutcome {
    Committed(FactCommitReceipt),
    IdempotentReplay(FactCommitReceipt),
    Conflict(FactCommitConflict),
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FactStoreError {
    #[error("fact write batch must append at least one lineage event")]
    EmptyBatch,
    #[error("fact write contains an item for another fact")]
    FactMismatch,
    #[error("fact write contains an item for another owner")]
    OwnerMismatch,
    #[error("fact assertion {assertion_id} has no matching lineage event")]
    MissingAssertionEvent { assertion_id: FactAssertionId },
    #[error("fact lineage event {event_id} is duplicated")]
    DuplicateEventId { event_id: FactEventId },
    #[error("fact lineage events are not in canonical order")]
    EventsOutOfOrder,
    #[error("retrieval anchor {anchor_id} is declared more than once")]
    DuplicateAnchorId { anchor_id: RetrievalAnchorId },
    #[error("fact evidence references unavailable retrieval anchor {anchor_id}")]
    MissingEvidenceAnchor { anchor_id: RetrievalAnchorId },
    #[error("retrieval anchor lineage references unavailable anchor {anchor_id}")]
    MissingAnchorLineageSource { anchor_id: RetrievalAnchorId },
    #[error("retrieval anchor lineage contains a cycle at {anchor_id}")]
    CyclicAnchorLineage { anchor_id: RetrievalAnchorId },
    #[error("fact projection payload presence disagrees with its access state")]
    PayloadAccessMismatch,
    #[error("legacy fact id {legacy_fact_id} must be positive")]
    InvalidLegacyFactId { legacy_fact_id: i64 },
    #[error("fact query limit {limit} must be between 1 and {max}")]
    InvalidQueryLimit { limit: usize, max: usize },
    #[error("fact commit receipt is inconsistent with its event list")]
    InvalidCommitReceipt,
    #[error("fact contract validation failed")]
    Contract(#[from] DomainError),
    #[error("fact storage operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

pub type FactStoreResult<T> = Result<T, FactStoreError>;

/// Authoritative persistence boundary for append-only facts and evidence.
pub trait FactStore: Send + Sync {
    fn commit_fact(
        &self,
        batch: FactWriteBatch,
    ) -> impl Future<Output = FactStoreResult<FactCommitOutcome>> + Send;

    fn query_current_facts(
        &self,
        query: CurrentFactsQuery,
    ) -> impl Future<Output = FactStoreResult<Vec<StoredFactV1>>> + Send;

    fn query_fact_current(
        &self,
        query: FactCurrentQuery,
    ) -> impl Future<Output = FactStoreResult<Option<StoredFactV1>>> + Send;

    fn query_fact_as_of(
        &self,
        query: FactAsOfQuery,
    ) -> impl Future<Output = FactStoreResult<Option<StoredFactV1>>> + Send;

    fn query_fact_lineage(
        &self,
        query: FactLineageQuery,
    ) -> impl Future<Output = FactStoreResult<Vec<FactLineageEventV1>>> + Send;

    fn resolve_legacy_fact(
        &self,
        query: LegacyFactQuery,
    ) -> impl Future<Output = FactStoreResult<Option<FactId>>> + Send;

    fn get_retrieval_anchor(
        &self,
        query: RetrievalAnchorQuery,
    ) -> impl Future<Output = FactStoreResult<Option<RetrievalAnchorRecordV2>>> + Send;
}

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

    pub fn into_batch(self) -> FactWriteBatch {
        self.batch
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

#[derive(Debug, thiserror::Error)]
pub enum FactProposalStoreError {
    #[error("fact authority operation failed")]
    Store(#[from] FactStoreError),
    #[error("fact proposal {proposal_id} state changed before promotion")]
    ProposalStateConflict {
        proposal_id: ProvenanceId,
        expected: FactProposalPromotionStateV1,
        actual: Option<FactProposalPromotionStateV1>,
    },
    #[error("fact proposal storage operation {operation} failed")]
    Storage {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
}

/// Owner-bound compound authority for atomically promoting one proposal.
pub trait FactProposalStore: FactStore {
    fn promote_fact_proposal(
        &self,
        promotion: PromoteFactProposal,
    ) -> impl Future<Output = Result<PromoteFactProposalOutcome, FactProposalStoreError>> + Send;
}

/// Stable, owner-bound identifier used by V1-compatible fact surfaces.  It is
/// deliberately the canonical fact identity rather than a process-local row
/// number; an optional [`LegacyFactMappingV1`] carries a historical `i64` only
/// where the authoritative migration reconstructed one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompatibilityFactIdV1 {
    owner: FactOwnerV1,
    fact_id: FactId,
}

impl CompatibilityFactIdV1 {
    pub fn new(owner: FactOwnerV1, fact_id: FactId) -> FactStoreResult<Self> {
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        Ok(Self { owner, fact_id })
    }

    pub fn from_legacy_mapping(mapping: &LegacyFactMappingV1) -> FactStoreResult<Self> {
        Self::new(mapping.owner().clone(), mapping.fact_id().clone())
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }
}

/// Owner-bound forward/reverse compatibility mapping.  The optional legacy
/// mapping is the sole source of a legacy integer identifier; callers must not
/// coerce or hash canonical identifiers into one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactMappingV1 {
    compatibility_id: CompatibilityFactIdV1,
    legacy_mapping: Option<LegacyFactMappingV1>,
}

impl CompatibilityFactMappingV1 {
    pub fn new(
        compatibility_id: CompatibilityFactIdV1,
        legacy_mapping: Option<LegacyFactMappingV1>,
    ) -> FactStoreResult<Self> {
        if let Some(mapping) = &legacy_mapping {
            if mapping.owner() != compatibility_id.owner() {
                return Err(FactStoreError::OwnerMismatch);
            }
            if mapping.fact_id() != compatibility_id.fact_id() {
                return Err(FactStoreError::FactMismatch);
            }
        }
        Ok(Self {
            compatibility_id,
            legacy_mapping,
        })
    }

    pub fn compatibility_id(&self) -> &CompatibilityFactIdV1 {
        &self.compatibility_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        self.compatibility_id.owner()
    }

    pub fn fact_id(&self) -> &FactId {
        self.compatibility_id.fact_id()
    }

    pub fn legacy_mapping(&self) -> Option<&LegacyFactMappingV1> {
        self.legacy_mapping.as_ref()
    }

    pub fn legacy_fact_id(&self) -> Option<i64> {
        self.legacy_mapping
            .as_ref()
            .map(LegacyFactMappingV1::legacy_fact_id)
    }

    pub fn history_coverage(&self) -> Option<LegacyHistoryCoverageV1> {
        self.legacy_mapping
            .as_ref()
            .map(LegacyFactMappingV1::history_coverage)
    }
}

/// Typed source provenance for a compatibility projection.  Canonical sources
/// contain only sanitized domain identifiers; `Unknown` is explicit for legacy
/// history that cannot be reconstructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompatibilityFactSourceV1 {
    Canonical(FactIdentitySourceV1),
    Unknown,
}

impl CompatibilityFactSourceV1 {
    fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if let Self::Canonical(source) = self {
            FactIdentityMaterialV1::new(owner.clone(), source.clone())?;
        }
        Ok(())
    }
}

/// Counters and timestamps V1 clients expose.  They are non-negative by type
/// and stay separate from the immutable fact payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactTelemetryV1 {
    retrieval_count: u64,
    access_count: u64,
    helpful_count: u64,
    unhelpful_count: u64,
    created_at: UtcMicros,
    updated_at: UtcMicros,
    last_retrieved_at: Option<UtcMicros>,
    last_recalled_at: Option<UtcMicros>,
    last_feedback_at: Option<UtcMicros>,
}

impl CompatibilityFactTelemetryV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        retrieval_count: u64,
        access_count: u64,
        helpful_count: u64,
        unhelpful_count: u64,
        created_at: UtcMicros,
        updated_at: UtcMicros,
        last_retrieved_at: Option<UtcMicros>,
        last_recalled_at: Option<UtcMicros>,
        last_feedback_at: Option<UtcMicros>,
    ) -> FactStoreResult<Self> {
        if updated_at < created_at
            || last_retrieved_at.is_some_and(|value| value < created_at)
            || last_recalled_at.is_some_and(|value| value < created_at)
            || last_feedback_at.is_some_and(|value| value < created_at)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact telemetry timestamps",
            }));
        }
        Ok(Self {
            retrieval_count,
            access_count,
            helpful_count,
            unhelpful_count,
            created_at,
            updated_at,
            last_retrieved_at,
            last_recalled_at,
            last_feedback_at,
        })
    }

    pub fn retrieval_count(&self) -> u64 {
        self.retrieval_count
    }
    pub fn access_count(&self) -> u64 {
        self.access_count
    }
    pub fn helpful_count(&self) -> u64 {
        self.helpful_count
    }
    pub fn unhelpful_count(&self) -> u64 {
        self.unhelpful_count
    }
    pub fn created_at(&self) -> UtcMicros {
        self.created_at
    }
    pub fn updated_at(&self) -> UtcMicros {
        self.updated_at
    }
    pub fn last_retrieved_at(&self) -> Option<UtcMicros> {
        self.last_retrieved_at
    }
    pub fn last_recalled_at(&self) -> Option<UtcMicros> {
        self.last_recalled_at
    }
    pub fn last_feedback_at(&self) -> Option<UtcMicros> {
        self.last_feedback_at
    }
}

/// V1-shaped projection of one canonical fact.  `StoredFactV1` keeps access
/// state and the sanitized [`FactPayloadV1`] together so adapters cannot expose
/// deleted or un-sanitized payload fields accidentally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactV1 {
    fact: StoredFactV1,
    mapping: CompatibilityFactMappingV1,
    source: CompatibilityFactSourceV1,
    telemetry: CompatibilityFactTelemetryV1,
}

impl CompatibilityFactV1 {
    pub fn new(
        fact: StoredFactV1,
        mapping: CompatibilityFactMappingV1,
        source: CompatibilityFactSourceV1,
        telemetry: CompatibilityFactTelemetryV1,
    ) -> FactStoreResult<Self> {
        if fact.owner() != mapping.owner() {
            return Err(FactStoreError::OwnerMismatch);
        }
        if fact.fact_id() != mapping.fact_id() {
            return Err(FactStoreError::FactMismatch);
        }
        if let Some(legacy) = fact.legacy_mapping() {
            if mapping.legacy_mapping() != Some(legacy) {
                return Err(FactStoreError::FactMismatch);
            }
        }
        source.validate_for_owner(fact.owner())?;
        if let CompatibilityFactSourceV1::Canonical(identity_source) = &source {
            let material =
                FactIdentityMaterialV1::new(fact.owner().clone(), identity_source.clone())?;
            if FactId::derive(&material)? != *fact.fact_id() {
                return Err(FactStoreError::FactMismatch);
            }
        }
        Ok(Self {
            fact,
            mapping,
            source,
            telemetry,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if self.owner() != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(())
    }

    pub fn fact(&self) -> &StoredFactV1 {
        &self.fact
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        self.fact.owner()
    }
    pub fn fact_id(&self) -> &FactId {
        self.fact.fact_id()
    }
    pub fn mapping(&self) -> &CompatibilityFactMappingV1 {
        &self.mapping
    }
    pub fn legacy_fact_id(&self) -> Option<i64> {
        self.mapping.legacy_fact_id()
    }
    pub fn source(&self) -> &CompatibilityFactSourceV1 {
        &self.source
    }
    pub fn telemetry(&self) -> &CompatibilityFactTelemetryV1 {
        &self.telemetry
    }
    pub fn payload(&self) -> Option<&FactPayloadV1> {
        self.fact.payload()
    }
}

/// A bounded, deterministic compatibility list page.  Facts are sorted by
/// canonical `FactId` ascending, which makes the cursor stable across rebuilds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactPageV1 {
    owner: FactOwnerV1,
    facts: Vec<CompatibilityFactV1>,
    next_after_fact_id: Option<FactId>,
}

impl CompatibilityFactPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        facts: Vec<CompatibilityFactV1>,
        next_after_fact_id: Option<FactId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if facts.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: facts.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        let mut previous: Option<&FactId> = None;
        for fact in &facts {
            fact.validate_for_owner(&owner)?;
            if previous.is_some_and(|value| value >= fact.fact_id()) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact page order",
                }));
            }
            previous = Some(fact.fact_id());
        }
        if let Some(cursor) = &next_after_fact_id {
            validate_owned_fact_id(cursor, &owner)?;
            if previous.is_some_and(|last| cursor <= last) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact page cursor",
                }));
            }
        }
        Ok(Self {
            owner,
            facts,
            next_after_fact_id,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if &self.owner != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(())
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn facts(&self) -> &[CompatibilityFactV1] {
        &self.facts
    }
    pub fn next_after_fact_id(&self) -> Option<&FactId> {
        self.next_after_fact_id.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompatibilityFactSearchKindV1 {
    Search,
    Probe,
    Related { fact_id: FactId },
    Reason { fact_id: FactId },
}

impl CompatibilityFactSearchKindV1 {
    fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if let Self::Related { fact_id } | Self::Reason { fact_id } = self {
            validate_owned_fact_id(fact_id, owner)?;
        }
        Ok(())
    }
}

/// Bounded request for search, probe, related, or reason retrieval.  Search
/// results must use deterministic score/fact-ID ordering in the response DTO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactSearchQuery {
    owner: FactOwnerV1,
    kind: CompatibilityFactSearchKindV1,
    query: Option<String>,
    after_fact_id: Option<FactId>,
    limit: usize,
}

impl CompatibilityFactSearchQuery {
    pub fn new(
        owner: FactOwnerV1,
        kind: CompatibilityFactSearchKindV1,
        query: Option<String>,
        after_fact_id: Option<FactId>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        kind.validate_for_owner(&owner)?;
        if let Some(query) = &query {
            if query.trim().is_empty() || query.len() > MAX_COMPATIBILITY_SEARCH_BYTES {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search query",
                }));
            }
        } else if matches!(
            kind,
            CompatibilityFactSearchKindV1::Search | CompatibilityFactSearchKindV1::Probe
        ) {
            return Err(FactStoreError::Contract(DomainError::Empty {
                field: "compatibility fact search query",
            }));
        }
        if let Some(cursor) = &after_fact_id {
            validate_owned_fact_id(cursor, &owner)?;
        }
        validate_limit(limit, MAX_CURRENT_LIMIT)?;
        Ok(Self {
            owner,
            kind,
            query,
            after_fact_id,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn kind(&self) -> CompatibilityFactSearchKindV1 {
        self.kind.clone()
    }
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }
    pub fn after_fact_id(&self) -> Option<&FactId> {
        self.after_fact_id.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// One scored compatibility search result.  Scores are fixed-point millionths,
/// avoiding non-deterministic floating point ordering at the transport edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactSearchHitV1 {
    fact: CompatibilityFactV1,
    score_millionths: u32,
}

impl CompatibilityFactSearchHitV1 {
    pub fn new(fact: CompatibilityFactV1, score_millionths: u32) -> FactStoreResult<Self> {
        if score_millionths > 1_000_000 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact search score",
            }));
        }
        Ok(Self {
            fact,
            score_millionths,
        })
    }

    pub fn fact(&self) -> &CompatibilityFactV1 {
        &self.fact
    }
    pub fn score_millionths(&self) -> u32 {
        self.score_millionths
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactSearchPageV1 {
    owner: FactOwnerV1,
    hits: Vec<CompatibilityFactSearchHitV1>,
    next_after_fact_id: Option<FactId>,
}

impl CompatibilityFactSearchPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        hits: Vec<CompatibilityFactSearchHitV1>,
        next_after_fact_id: Option<FactId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if hits.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: hits.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        let mut previous: Option<&CompatibilityFactSearchHitV1> = None;
        for hit in &hits {
            hit.fact().validate_for_owner(&owner)?;
            if previous.is_some_and(|value| {
                value.score_millionths() < hit.score_millionths()
                    || (value.score_millionths() == hit.score_millionths()
                        && value.fact().fact_id() >= hit.fact().fact_id())
            }) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search order",
                }));
            }
            previous = Some(hit);
        }
        if let Some(cursor) = &next_after_fact_id {
            validate_owned_fact_id(cursor, &owner)?;
        }
        Ok(Self {
            owner,
            hits,
            next_after_fact_id,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if &self.owner != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(())
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn hits(&self) -> &[CompatibilityFactSearchHitV1] {
        &self.hits
    }
    pub fn next_after_fact_id(&self) -> Option<&FactId> {
        self.next_after_fact_id.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactHistoryV1 {
    owner: FactOwnerV1,
    fact_id: FactId,
    events: Vec<FactLineageEventV1>,
    next_after: Option<FactLineageCursor>,
}

impl CompatibilityFactHistoryV1 {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: FactId,
        events: Vec<FactLineageEventV1>,
        next_after: Option<FactLineageCursor>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        validate_owned_fact_id(&fact_id, &owner)?;
        if events.len() > MAX_LINEAGE_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: events.len(),
                max: MAX_LINEAGE_LIMIT,
            });
        }
        let mut previous: Option<&FactLineageEventV1> = None;
        for event in &events {
            if event.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            if event.fact_id() != &fact_id {
                return Err(FactStoreError::FactMismatch);
            }
            if previous.is_some_and(|value| {
                (value.occurred_at(), value.event_id()) >= (event.occurred_at(), event.event_id())
            }) {
                return Err(FactStoreError::EventsOutOfOrder);
            }
            previous = Some(event);
        }
        Ok(Self {
            owner,
            fact_id,
            events,
            next_after,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if &self.owner != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(())
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }
    pub fn events(&self) -> &[FactLineageEventV1] {
        &self.events
    }
    pub fn next_after(&self) -> Option<&FactLineageCursor> {
        self.next_after.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibilityProjectionStateV1 {
    Ready,
    Rebuilding,
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactStatusV1 {
    owner: FactOwnerV1,
    fact_id: Option<FactId>,
    payload_access: Option<PayloadAccessState>,
    projection_state: CompatibilityProjectionStateV1,
    projected_as_of: Option<UtcMicros>,
    vector_watermark: Option<VectorWatermark>,
}

impl CompatibilityFactStatusV1 {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: Option<FactId>,
        payload_access: Option<PayloadAccessState>,
        projection_state: CompatibilityProjectionStateV1,
        projected_as_of: Option<UtcMicros>,
        vector_watermark: Option<VectorWatermark>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if let Some(fact_id) = &fact_id {
            validate_owned_fact_id(fact_id, &owner)?;
        }
        Ok(Self {
            owner,
            fact_id,
            payload_access,
            projection_state,
            projected_as_of,
            vector_watermark,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        if &self.owner != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(())
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn fact_id(&self) -> Option<&FactId> {
        self.fact_id.as_ref()
    }
    pub fn payload_access(&self) -> Option<PayloadAccessState> {
        self.payload_access
    }
    pub fn projection_state(&self) -> CompatibilityProjectionStateV1 {
        self.projection_state
    }
    pub fn projected_as_of(&self) -> Option<UtcMicros> {
        self.projected_as_of
    }
    pub fn vector_watermark(&self) -> Option<&VectorWatermark> {
        self.vector_watermark.as_ref()
    }
}

/// Bounded detail projection used for V1 `get`, history, status, and dashboard
/// inspection without exposing a database row or arbitrary JSON transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactInspectionV1 {
    fact: CompatibilityFactV1,
    history: CompatibilityFactHistoryV1,
    anchors: Vec<RetrievalAnchorRecordV2>,
    status: CompatibilityFactStatusV1,
}

impl CompatibilityFactInspectionV1 {
    pub fn new(
        fact: CompatibilityFactV1,
        history: CompatibilityFactHistoryV1,
        anchors: Vec<RetrievalAnchorRecordV2>,
        status: CompatibilityFactStatusV1,
    ) -> FactStoreResult<Self> {
        history.validate_for_owner(fact.owner())?;
        status.validate_for_owner(fact.owner())?;
        if history.fact_id() != fact.fact_id()
            || status.fact_id().is_some_and(|id| id != fact.fact_id())
        {
            return Err(FactStoreError::FactMismatch);
        }
        if anchors.len() > MAX_LINEAGE_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: anchors.len(),
                max: MAX_LINEAGE_LIMIT,
            });
        }
        let mut previous: Option<&RetrievalAnchorId> = None;
        for anchor in &anchors {
            anchor.validate()?;
            if FactOwnerV1::from(anchor.owner().clone()) != *fact.owner() {
                return Err(FactStoreError::OwnerMismatch);
            }
            if previous.is_some_and(|id| id >= anchor.anchor_id()) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact inspection anchors",
                }));
            }
            previous = Some(anchor.anchor_id());
        }
        Ok(Self {
            fact,
            history,
            anchors,
            status,
        })
    }

    pub fn validate_for_owner(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        self.fact.validate_for_owner(owner)
    }
    pub fn owner(&self) -> &FactOwnerV1 {
        self.fact.owner()
    }
    pub fn fact(&self) -> &CompatibilityFactV1 {
        &self.fact
    }
    pub fn history(&self) -> &CompatibilityFactHistoryV1 {
        &self.history
    }
    pub fn anchors(&self) -> &[RetrievalAnchorRecordV2] {
        &self.anchors
    }
    pub fn status(&self) -> &CompatibilityFactStatusV1 {
        &self.status
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::{
        AccessPolicyDigest, AnchorDurabilityClass, AnchorLineageRefV2, AnchorProvenanceRelationV2,
        AnchorSourceGenerationV2, CapabilityId, ComponentVersion, CoverageReportV1, EntityId,
        EntityKind, EntityRef, EvidenceClass, FactAssertionKindV1, FactCategoryV1,
        FactEvidenceRefV1, FactEvidenceRelationV1, FactIdentityMaterialV1, FactIdentitySourceV1,
        ObservationScopeV1, PayloadReferenceV1, PrivacyDomainBoundLocatorDigest, PrivacyDomainId,
        ProjectionGenerationId, ProvenanceId, ResolutionAuthorizationV1, RetentionClass,
        RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2, SanitizationReceiptId,
        SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, ScopeResolutionId,
        SensitivityV1, VectorWatermark,
    };

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn fact_id(owner: FactOwnerV1, operation: &str) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                owner,
                FactIdentitySourceV1::Application {
                    operation_id: id::<ProvenanceId>(operation),
                },
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn payload() -> FactPayloadV1 {
        let material = json!({
            "content": "The daemon is the only writer.",
            "category": "project",
            "tags": ["database"],
            "entities": ["TraceDecay"],
            "metadata": {},
        });
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                id::<SanitizationReceiptId>("receipt.fact.store.fixture"),
                id::<ComponentVersion>("sanitizer.fixture.v1"),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&material).unwrap()),
        )
        .unwrap();
        FactPayloadV1::new(
            "The daemon is the only writer.".to_owned(),
            FactCategoryV1::Project,
            vec!["database".to_owned()],
            vec!["TraceDecay".to_owned()],
            json!({}),
            receipt,
            RetentionClass::new("durable.fact").unwrap(),
        )
        .unwrap()
    }

    fn payload_event(fact_id: FactId, owner: FactOwnerV1, occurred_at: i64) -> FactLineageEventV1 {
        FactLineageEventV1::new(
            fact_id,
            owner,
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(occurred_at),
            None,
        )
        .unwrap()
    }

    fn anchor(entity_id: &str, source_anchors: Vec<AnchorLineageRefV2>) -> RetrievalAnchorRecordV2 {
        const DIGEST_A: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const DIGEST_B: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
            target: RetrievalAnchorTargetV2::Entity(EntityRef {
                id: EntityId::new(entity_id).unwrap(),
                kind: EntityKind::Document,
            }),
            owner: ObservationScopeV1::Profile,
            aliases: vec![],
            occurred_at: None,
            ingested_at: UtcMicros(1),
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV2::Unknown,
            projection_generation: ProjectionGenerationId::new("projection.fixture").unwrap(),
            projection_watermark: VectorWatermark::default(),
            coverage: CoverageReportV1::default(),
            source_observations: vec![],
            source_anchors,
            authorization: ResolutionAuthorizationV1 {
                resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
                privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
                access_policy_digest: AccessPolicyDigest::new(DIGEST_A).unwrap(),
                capability_id: CapabilityId::new("capability.fixture").unwrap(),
                canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(DIGEST_B).unwrap(),
            },
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .unwrap()
    }

    fn anchor_source(anchor_id: RetrievalAnchorId) -> AnchorLineageRefV2 {
        AnchorLineageRefV2::new(
            AnchorProvenanceRelationV2::DerivedFrom,
            anchor_id,
            ObservationScopeV1::Profile,
        )
        .unwrap()
    }

    #[test]
    fn batch_rejects_owner_mismatch() {
        let fact_id = fact_id(FactOwnerV1::Profile, "operation.owner");
        let event = payload_event(fact_id.clone(), FactOwnerV1::Profile, 1);
        let error = FactWriteBatch::new(
            fact_id,
            FactOwnerV1::Project {
                project_id: id("project.other"),
            },
            None,
            vec![event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, FactStoreError::OwnerMismatch));
    }

    #[test]
    fn batch_rejects_missing_and_cyclic_anchor_lineage() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id(owner.clone(), "operation.anchor-lineage");
        let event = payload_event(fact_id.clone(), owner.clone(), 1);
        let missing_id: RetrievalAnchorId = id("retrieval.missing-source");
        let missing = anchor("entity.missing", vec![anchor_source(missing_id.clone())]);
        let error = FactWriteBatch::new(
            fact_id.clone(),
            owner.clone(),
            None,
            vec![event.clone()],
            vec![missing],
            vec![],
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            FactStoreError::MissingAnchorLineageSource { anchor_id }
                if anchor_id == missing_id
        ));

        let base_a = anchor("entity.cycle.a", vec![]);
        let base_b = anchor("entity.cycle.b", vec![]);
        let cycle_a = anchor(
            "entity.cycle.a",
            vec![anchor_source(base_b.anchor_id().clone())],
        );
        let cycle_b = anchor(
            "entity.cycle.b",
            vec![anchor_source(base_a.anchor_id().clone())],
        );
        let error = FactWriteBatch::new(
            fact_id,
            owner,
            None,
            vec![event],
            vec![cycle_a, cycle_b],
            vec![],
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, FactStoreError::CyclicAnchorLineage { .. }));
    }

    #[test]
    fn batch_accepts_order_independent_acyclic_anchor_lineage() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id(owner.clone(), "operation.anchor-dag");
        let root = anchor("entity.dag.root", vec![]);
        let child = anchor(
            "entity.dag.child",
            vec![anchor_source(root.anchor_id().clone())],
        );

        FactWriteBatch::new(
            fact_id.clone(),
            owner.clone(),
            None,
            vec![payload_event(fact_id, owner, 1)],
            vec![child, root],
            vec![],
            None,
            None,
        )
        .unwrap();
    }

    #[test]
    fn batch_rejects_missing_evidence_anchor() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id(owner.clone(), "operation.anchor");
        let evidence = FactEvidenceRefV1::new(
            fact_id.clone(),
            id("retrieval.missing"),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Observed,
            Confidence::new(1.0).unwrap(),
        )
        .unwrap();
        let assertion = FactAssertionV1::new(
            fact_id.clone(),
            owner.clone(),
            FactAssertionKindV1::Initial,
            payload(),
            vec![evidence],
            UtcMicros(1),
            None,
        )
        .unwrap();
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::AssertionRecorded {
                assertion_id: assertion.assertion_id().clone(),
            },
            UtcMicros(1),
            None,
        )
        .unwrap();

        let error = FactWriteBatch::new(
            fact_id,
            owner,
            Some(assertion),
            vec![event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            FactStoreError::MissingEvidenceAnchor { .. }
        ));
    }

    #[test]
    fn batch_rejects_duplicate_replay_shape() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id(owner.clone(), "operation.replay");
        let event = payload_event(fact_id.clone(), owner.clone(), 1);
        let error = FactWriteBatch::new(
            fact_id,
            owner,
            None,
            vec![event.clone(), event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, FactStoreError::DuplicateEventId { .. }));
    }

    #[test]
    fn creation_identity_material_must_derive_the_batch_fact() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id(owner.clone(), "operation.identity.expected");
        let event = payload_event(fact_id.clone(), owner.clone(), 1);
        let batch = FactWriteBatch::new(
            fact_id,
            owner.clone(),
            None,
            vec![event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap();
        let unrelated = FactIdentityMaterialV1::new(
            owner,
            FactIdentitySourceV1::Application {
                operation_id: id("operation.identity.unrelated"),
            },
        )
        .unwrap();

        assert!(matches!(
            batch.with_identity_material(unrelated),
            Err(FactStoreError::FactMismatch)
        ));
    }

    #[test]
    fn tombstone_rejects_payload() {
        let owner = FactOwnerV1::Profile;
        let tombstone_fact_id = fact_id(owner.clone(), "operation.tombstone");
        let error = StoredFactV1::new(
            tombstone_fact_id,
            owner,
            Some(payload()),
            PayloadAccessState::Deleted,
            Confidence::new(1.0).unwrap(),
            id("assertion.fixture"),
            id("event.fixture"),
            None,
            UtcMicros(2),
        )
        .unwrap_err();
        assert!(matches!(error, FactStoreError::PayloadAccessMismatch));

        let fact_id = fact_id(FactOwnerV1::Profile, "operation.missing-payload");
        let error = StoredFactV1::new(
            fact_id,
            FactOwnerV1::Profile,
            None,
            PayloadAccessState::Eligible,
            Confidence::new(1.0).unwrap(),
            id("assertion.fixture"),
            id("event.fixture"),
            None,
            UtcMicros(2),
        )
        .unwrap_err();
        assert!(matches!(error, FactStoreError::PayloadAccessMismatch));
    }

    #[test]
    fn queries_enforce_bounds() {
        assert!(matches!(
            CurrentFactsQuery::new(FactOwnerV1::Profile, None, 0),
            Err(FactStoreError::InvalidQueryLimit { .. })
        ));
        let fact_id = fact_id(FactOwnerV1::Profile, "operation.query");
        assert!(matches!(
            FactLineageQuery::new(FactOwnerV1::Profile, fact_id, None, MAX_LINEAGE_LIMIT + 1,),
            Err(FactStoreError::InvalidQueryLimit { .. })
        ));
        assert!(matches!(
            LegacyFactQuery::new(FactOwnerV1::Profile, id("store.v1"), 0),
            Err(FactStoreError::InvalidLegacyFactId { .. })
        ));
    }

    #[test]
    fn projections_queries_and_receipts_reject_cross_owner_fact_ids() {
        let profile_fact_id = fact_id(FactOwnerV1::Profile, "operation.cross-owner");
        let project_owner = FactOwnerV1::Project {
            project_id: id("project.other"),
        };

        assert!(matches!(
            StoredFactV1::new(
                profile_fact_id.clone(),
                project_owner.clone(),
                None,
                PayloadAccessState::Deleted,
                Confidence::new(1.0).unwrap(),
                id("assertion.fixture"),
                id("event.fixture"),
                None,
                UtcMicros(2),
            ),
            Err(FactStoreError::OwnerMismatch)
        ));
        assert!(matches!(
            CurrentFactsQuery::new(project_owner.clone(), Some(profile_fact_id.clone()), 10,),
            Err(FactStoreError::OwnerMismatch)
        ));
        assert!(matches!(
            FactCurrentQuery::new(project_owner.clone(), profile_fact_id.clone()),
            Err(FactStoreError::OwnerMismatch)
        ));
        assert!(matches!(
            FactAsOfQuery::new(project_owner.clone(), profile_fact_id.clone(), UtcMicros(2),),
            Err(FactStoreError::OwnerMismatch)
        ));
        assert!(matches!(
            FactLineageQuery::new(project_owner.clone(), profile_fact_id.clone(), None, 10,),
            Err(FactStoreError::OwnerMismatch)
        ));

        let legacy = LegacyFactQuery::new(project_owner.clone(), id("store.v1"), 7).unwrap();
        assert!(matches!(
            legacy.validate_resolved_fact_id(&profile_fact_id),
            Err(FactStoreError::OwnerMismatch)
        ));

        let event_id: FactEventId = id("event.fixture");
        assert!(matches!(
            FactCommitReceipt::new(
                profile_fact_id,
                project_owner,
                vec![event_id.clone()],
                event_id,
                None,
            ),
            Err(FactStoreError::OwnerMismatch)
        ));
    }
}
