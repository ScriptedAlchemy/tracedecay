use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::future::Future;

use serde_json::Value;

use tracedecay_domain::{
    ActorId, Confidence, DomainError, FactAssertionId, FactAssertionV1, FactCategoryV1,
    FactEventId, FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1,
    FactLineageEventV1, FactOwnerV1, FactPayloadV1, LegacyFactMappingV1, LegacyHistoryCoverageV1,
    LocatorDigest, PayloadAccessState, ProvenanceId, RetrievalAnchorId, RetrievalAnchorRecordV2,
    SourceStoreId, UtcMicros, VectorWatermark,
};

const MAX_CURRENT_LIMIT: usize = 1_000;
const MAX_LINEAGE_LIMIT: usize = 1_000;
const MAX_COMPATIBILITY_SEARCH_BYTES: usize = 4 * 1024;
const MAX_COMPATIBILITY_REASON_BYTES: usize = 4 * 1024;

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

fn validate_compatibility_entity(value: &str) -> FactStoreResult<()> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > MAX_COMPATIBILITY_SEARCH_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(FactStoreError::Contract(DomainError::NonCanonical {
            field: "compatibility fact entity",
        }));
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
    source_label: Option<String>,
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
            source_label: None,
            telemetry,
        })
    }

    pub fn with_source_label(mut self, source_label: Option<String>) -> FactStoreResult<Self> {
        if source_label.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact source label",
            }));
        }
        self.source_label = source_label;
        Ok(self)
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
    pub fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
    }
    pub fn telemetry(&self) -> &CompatibilityFactTelemetryV1 {
        &self.telemetry
    }
    pub fn payload(&self) -> Option<&FactPayloadV1> {
        self.fact.payload()
    }
    pub fn content(&self) -> Option<&str> {
        self.payload().map(FactPayloadV1::content)
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.payload().map(FactPayloadV1::category)
    }
    pub fn tags(&self) -> Option<&[String]> {
        self.payload().map(FactPayloadV1::tags)
    }
    pub fn entities(&self) -> Option<&[String]> {
        self.payload().map(FactPayloadV1::entities)
    }
    pub fn metadata(&self) -> Option<&Value> {
        self.payload().map(FactPayloadV1::metadata)
    }
}

/// A bounded, deterministic compatibility list page.  Facts are sorted by
/// canonical `FactId` ascending, which makes the cursor stable across rebuilds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactPageV1 {
    owner: FactOwnerV1,
    facts: Vec<CompatibilityFactProjectionV1>,
    next_after_fact_id: Option<FactId>,
}

impl CompatibilityFactPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        facts: Vec<CompatibilityFactProjectionV1>,
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
            if fact.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
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
    pub fn facts(&self) -> &[CompatibilityFactProjectionV1] {
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
    Related { entity: String },
    Reason { entities: Vec<String> },
}

impl CompatibilityFactSearchKindV1 {
    fn validate(&self) -> FactStoreResult<()> {
        match self {
            Self::Search | Self::Probe => {}
            Self::Related { entity } => validate_compatibility_entity(entity)?,
            Self::Reason { entities } => {
                if entities.is_empty() || entities.len() > MAX_CURRENT_LIMIT {
                    return Err(FactStoreError::Contract(DomainError::NonCanonical {
                        field: "compatibility fact reason entities",
                    }));
                }
                let mut previous: Option<&String> = None;
                for entity in entities {
                    validate_compatibility_entity(entity)?;
                    if previous.is_some_and(|value| value >= entity) {
                        return Err(FactStoreError::Contract(DomainError::NonCanonical {
                            field: "compatibility fact reason entities",
                        }));
                    }
                    previous = Some(entity);
                }
            }
        }
        Ok(())
    }
}

/// Optional deterministic constraints applied before compatibility ranking.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompatibilityFactSearchFilterV1 {
    category: Option<FactCategoryV1>,
    min_trust: Option<Confidence>,
    threshold_millionths: Option<u32>,
}

impl CompatibilityFactSearchFilterV1 {
    pub fn new(
        category: Option<FactCategoryV1>,
        min_trust: Option<Confidence>,
        threshold_millionths: Option<u32>,
    ) -> FactStoreResult<Self> {
        if threshold_millionths.is_some_and(|value| value > 1_000_000) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact search threshold",
            }));
        }
        Ok(Self {
            category,
            min_trust,
            threshold_millionths,
        })
    }

    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }

    pub fn min_trust(&self) -> Option<Confidence> {
        self.min_trust
    }

    pub fn threshold_millionths(&self) -> Option<u32> {
        self.threshold_millionths
    }
}

/// Exclusive continuation token for score-descending compatibility retrieval.
/// The fact ID breaks equal-score ties, so a page can resume deterministically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactSearchCursorV1 {
    score_millionths: u32,
    updated_at: UtcMicros,
    fact_id: FactId,
}

impl CompatibilityFactSearchCursorV1 {
    pub fn new(
        score_millionths: u32,
        updated_at: UtcMicros,
        fact_id: FactId,
    ) -> FactStoreResult<Self> {
        if score_millionths > 1_000_000 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact search cursor score",
            }));
        }
        fact_id.validate()?;
        Ok(Self {
            score_millionths,
            updated_at,
            fact_id,
        })
    }

    pub fn score_millionths(&self) -> u32 {
        self.score_millionths
    }

    pub fn updated_at(&self) -> UtcMicros {
        self.updated_at
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }
}

/// Bounded request for search, probe, related, or reason retrieval.  Search
/// results must use deterministic score/fact-ID ordering in the response DTO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactSearchQuery {
    owner: FactOwnerV1,
    kind: CompatibilityFactSearchKindV1,
    query: Option<String>,
    filter: CompatibilityFactSearchFilterV1,
    after: Option<CompatibilityFactSearchCursorV1>,
    limit: usize,
}

impl CompatibilityFactSearchQuery {
    pub fn new(
        owner: FactOwnerV1,
        kind: CompatibilityFactSearchKindV1,
        query: Option<String>,
        after: Option<CompatibilityFactSearchCursorV1>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        Self::with_filter(
            owner,
            kind,
            query,
            CompatibilityFactSearchFilterV1::default(),
            after,
            limit,
        )
    }

    pub fn with_filter(
        owner: FactOwnerV1,
        kind: CompatibilityFactSearchKindV1,
        query: Option<String>,
        filter: CompatibilityFactSearchFilterV1,
        after: Option<CompatibilityFactSearchCursorV1>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        kind.validate()?;
        if let Some(query) = &query {
            if query.trim().is_empty() || query.len() > MAX_COMPATIBILITY_SEARCH_BYTES {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search query",
                }));
            }
        } else if matches!(
            &kind,
            CompatibilityFactSearchKindV1::Search | CompatibilityFactSearchKindV1::Probe
        ) {
            return Err(FactStoreError::Contract(DomainError::Empty {
                field: "compatibility fact search query",
            }));
        }
        if let Some(cursor) = &after {
            validate_owned_fact_id(cursor.fact_id(), &owner)?;
        }
        validate_limit(limit, MAX_CURRENT_LIMIT)?;
        Ok(Self {
            owner,
            kind,
            query,
            filter,
            after,
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
    pub fn filter(&self) -> &CompatibilityFactSearchFilterV1 {
        &self.filter
    }
    pub fn after(&self) -> Option<&CompatibilityFactSearchCursorV1> {
        self.after.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// One scored compatibility search result.  Scores are fixed-point millionths,
/// avoiding non-deterministic floating point ordering at the transport edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompatibilityFactSearchScoresV1 {
    score_millionths: u32,
    fts_score_millionths: u32,
    jaccard_score_millionths: u32,
    holographic_score_millionths: u32,
    trust_score_millionths: u32,
}

impl CompatibilityFactSearchScoresV1 {
    pub fn new(
        score_millionths: u32,
        fts_score_millionths: u32,
        jaccard_score_millionths: u32,
        holographic_score_millionths: u32,
        trust_score_millionths: u32,
    ) -> FactStoreResult<Self> {
        if [
            score_millionths,
            fts_score_millionths,
            jaccard_score_millionths,
            holographic_score_millionths,
            trust_score_millionths,
        ]
        .into_iter()
        .any(|value| value > 1_000_000)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact search score",
            }));
        }
        Ok(Self {
            score_millionths,
            fts_score_millionths,
            jaccard_score_millionths,
            holographic_score_millionths,
            trust_score_millionths,
        })
    }

    pub fn score_millionths(self) -> u32 {
        self.score_millionths
    }
    pub fn fts_score_millionths(self) -> u32 {
        self.fts_score_millionths
    }
    pub fn jaccard_score_millionths(self) -> u32 {
        self.jaccard_score_millionths
    }
    pub fn holographic_score_millionths(self) -> u32 {
        self.holographic_score_millionths
    }
    pub fn trust_score_millionths(self) -> u32 {
        self.trust_score_millionths
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactSearchHitV1 {
    fact: CompatibilityFactV1,
    scores: CompatibilityFactSearchScoresV1,
    why: Option<String>,
}

impl CompatibilityFactSearchHitV1 {
    pub fn new(
        fact: CompatibilityFactV1,
        scores: CompatibilityFactSearchScoresV1,
        why: Option<String>,
    ) -> FactStoreResult<Self> {
        if why.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact search why",
            }));
        }
        Ok(Self { fact, scores, why })
    }

    pub fn fact(&self) -> &CompatibilityFactV1 {
        &self.fact
    }
    pub fn score_millionths(&self) -> u32 {
        self.scores.score_millionths()
    }
    pub fn scores(&self) -> CompatibilityFactSearchScoresV1 {
        self.scores
    }
    pub fn why(&self) -> Option<&str> {
        self.why.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactSearchPageV1 {
    owner: FactOwnerV1,
    hits: Vec<CompatibilityFactSearchHitV1>,
    next_after: Option<CompatibilityFactSearchCursorV1>,
}

impl CompatibilityFactSearchPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        hits: Vec<CompatibilityFactSearchHitV1>,
        next_after: Option<CompatibilityFactSearchCursorV1>,
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
                        && (value.fact().telemetry().updated_at()
                            < hit.fact().telemetry().updated_at()
                            || (value.fact().telemetry().updated_at()
                                == hit.fact().telemetry().updated_at()
                                && value.fact().fact_id() >= hit.fact().fact_id())))
            }) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search order",
                }));
            }
            previous = Some(hit);
        }
        if let Some(cursor) = &next_after {
            validate_owned_fact_id(cursor.fact_id(), &owner)?;
            let Some(last) = hits.last() else {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search cursor without hits",
                }));
            };
            if cursor.score_millionths() != last.score_millionths()
                || cursor.updated_at() != last.fact().telemetry().updated_at()
                || cursor.fact_id() != last.fact().fact_id()
            {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility fact search cursor",
                }));
            }
        }
        Ok(Self {
            owner,
            hits,
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
    pub fn hits(&self) -> &[CompatibilityFactSearchHitV1] {
        &self.hits
    }
    pub fn next_after(&self) -> Option<&CompatibilityFactSearchCursorV1> {
        self.next_after.as_ref()
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

/// Owner aggregate for the legacy memory-status response.  Counts originate
/// from one authority snapshot rather than handler-side joins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityMemoryFeedbackFunnelV1 {
    retrieval_count_total: u64,
    access_count_total: u64,
    retrieved_fact_count: u64,
    rated_fact_count: u64,
    feedback_total: u64,
    seen_to_feedback_ratio: Option<u64>,
}

impl CompatibilityMemoryFeedbackFunnelV1 {
    pub fn new(
        retrieval_count_total: u64,
        access_count_total: u64,
        retrieved_fact_count: u64,
        rated_fact_count: u64,
        feedback_total: u64,
    ) -> Self {
        Self {
            retrieval_count_total,
            access_count_total,
            retrieved_fact_count,
            rated_fact_count,
            feedback_total,
            seen_to_feedback_ratio: (feedback_total != 0)
                .then_some((retrieval_count_total + access_count_total) / feedback_total),
        }
    }

    pub fn retrieval_count_total(&self) -> u64 {
        self.retrieval_count_total
    }
    pub fn access_count_total(&self) -> u64 {
        self.access_count_total
    }
    pub fn retrieved_fact_count(&self) -> u64 {
        self.retrieved_fact_count
    }
    pub fn rated_fact_count(&self) -> u64 {
        self.rated_fact_count
    }
    pub fn feedback_total(&self) -> u64 {
        self.feedback_total
    }
    pub fn seen_to_feedback_ratio(&self) -> Option<u64> {
        self.seen_to_feedback_ratio
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityMemoryStatusV1 {
    owner: FactOwnerV1,
    fact_count: u64,
    entity_count: u64,
    bank_count: u64,
    algebra: CompatibilityMemoryAlgebraV1,
    trust_0_025_count: u64,
    trust_025_050_count: u64,
    trust_050_075_count: u64,
    trust_075_100_count: u64,
    below_default_recall_threshold_count: u64,
    helpful_count: u64,
    unhelpful_count: u64,
    missing_vector_count: u64,
    legacy_backfill_complete: bool,
    projection_state: CompatibilityProjectionStateV1,
    repair: CompatibilityMemoryRepairStatsV1,
    feedback_funnel: CompatibilityMemoryFeedbackFunnelV1,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompatibilityMemoryRepairStatsV1 {
    missing_vectors_repaired: u64,
    banks_rebuilt: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityMemoryAlgebraV1 {
    name: String,
    hrr_dim: u64,
    estimated_capacity: u64,
}

impl CompatibilityMemoryAlgebraV1 {
    pub fn new(name: String, hrr_dim: u64, estimated_capacity: u64) -> FactStoreResult<Self> {
        if name.trim().is_empty() || name.len() > MAX_COMPATIBILITY_SEARCH_BYTES {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility memory algebra name",
            }));
        }
        Ok(Self {
            name,
            hrr_dim,
            estimated_capacity,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn hrr_dim(&self) -> u64 {
        self.hrr_dim
    }
    pub fn estimated_capacity(&self) -> u64 {
        self.estimated_capacity
    }
}

impl CompatibilityMemoryRepairStatsV1 {
    pub fn new(missing_vectors_repaired: u64, banks_rebuilt: u64) -> Self {
        Self {
            missing_vectors_repaired,
            banks_rebuilt,
        }
    }

    pub fn missing_vectors_repaired(&self) -> u64 {
        self.missing_vectors_repaired
    }
    pub fn banks_rebuilt(&self) -> u64 {
        self.banks_rebuilt
    }
}

impl CompatibilityMemoryStatusV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        fact_count: u64,
        entity_count: u64,
        bank_count: u64,
        algebra: CompatibilityMemoryAlgebraV1,
        trust_0_025_count: u64,
        trust_025_050_count: u64,
        trust_050_075_count: u64,
        trust_075_100_count: u64,
        below_default_recall_threshold_count: u64,
        helpful_count: u64,
        unhelpful_count: u64,
        missing_vector_count: u64,
        legacy_backfill_complete: bool,
        projection_state: CompatibilityProjectionStateV1,
        repair: CompatibilityMemoryRepairStatsV1,
        feedback_funnel: CompatibilityMemoryFeedbackFunnelV1,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        Ok(Self {
            owner,
            fact_count,
            entity_count,
            bank_count,
            algebra,
            trust_0_025_count,
            trust_025_050_count,
            trust_050_075_count,
            trust_075_100_count,
            below_default_recall_threshold_count,
            helpful_count,
            unhelpful_count,
            missing_vector_count,
            legacy_backfill_complete,
            projection_state,
            repair,
            feedback_funnel,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn fact_count(&self) -> u64 {
        self.fact_count
    }
    pub fn entity_count(&self) -> u64 {
        self.entity_count
    }
    pub fn bank_count(&self) -> u64 {
        self.bank_count
    }
    pub fn algebra(&self) -> &CompatibilityMemoryAlgebraV1 {
        &self.algebra
    }
    pub fn trust_0_025_count(&self) -> u64 {
        self.trust_0_025_count
    }
    pub fn trust_025_050_count(&self) -> u64 {
        self.trust_025_050_count
    }
    pub fn trust_050_075_count(&self) -> u64 {
        self.trust_050_075_count
    }
    pub fn trust_075_100_count(&self) -> u64 {
        self.trust_075_100_count
    }
    pub fn below_default_recall_threshold_count(&self) -> u64 {
        self.below_default_recall_threshold_count
    }
    pub fn helpful_count(&self) -> u64 {
        self.helpful_count
    }
    pub fn unhelpful_count(&self) -> u64 {
        self.unhelpful_count
    }
    pub fn missing_vector_count(&self) -> u64 {
        self.missing_vector_count
    }
    pub fn legacy_backfill_complete(&self) -> bool {
        self.legacy_backfill_complete
    }
    pub fn projection_state(&self) -> CompatibilityProjectionStateV1 {
        self.projection_state
    }
    pub fn repair(&self) -> CompatibilityMemoryRepairStatsV1 {
        self.repair
    }
    pub fn feedback_funnel(&self) -> &CompatibilityMemoryFeedbackFunnelV1 {
        &self.feedback_funnel
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

/// A compatibility operation may target a canonical fact or an owner-bound
/// historical numeric identity.  Resolution of the latter happens inside the
/// authority transaction, never in a handler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompatibilityFactTargetV1 {
    Canonical(CompatibilityFactIdV1),
    Legacy(LegacyFactQuery),
}

impl CompatibilityFactTargetV1 {
    pub fn owner(&self) -> &FactOwnerV1 {
        match self {
            Self::Canonical(target) => target.owner(),
            Self::Legacy(target) => target.owner(),
        }
    }

    pub fn canonical_fact_id(&self) -> Option<&FactId> {
        match self {
            Self::Canonical(target) => Some(target.fact_id()),
            Self::Legacy(_) => None,
        }
    }

    pub fn legacy_query(&self) -> Option<&LegacyFactQuery> {
        match self {
            Self::Canonical(_) => None,
            Self::Legacy(target) => Some(target),
        }
    }
}

/// Deterministic compatibility list filters without exposing raw SQL fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactListQueryV1 {
    owner: FactOwnerV1,
    category: Option<FactCategoryV1>,
    min_trust: Option<Confidence>,
    after_fact_id: Option<FactId>,
    limit: usize,
}

impl CompatibilityFactListQueryV1 {
    pub fn new(
        owner: FactOwnerV1,
        category: Option<FactCategoryV1>,
        min_trust: Option<Confidence>,
        after_fact_id: Option<FactId>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if let Some(fact_id) = &after_fact_id {
            validate_owned_fact_id(fact_id, &owner)?;
        }
        validate_limit(limit, MAX_CURRENT_LIMIT)?;
        Ok(Self {
            owner,
            category,
            min_trust,
            after_fact_id,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }
    pub fn min_trust(&self) -> Option<Confidence> {
        self.min_trust
    }
    pub fn after_fact_id(&self) -> Option<&FactId> {
        self.after_fact_id.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactHistoryQueryV1 {
    target: CompatibilityFactTargetV1,
    after: Option<FactLineageCursor>,
    limit: usize,
}

impl CompatibilityFactHistoryQueryV1 {
    pub fn new(
        target: CompatibilityFactTargetV1,
        after: Option<FactLineageCursor>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        validate_limit(limit, MAX_LINEAGE_LIMIT)?;
        Ok(Self {
            target,
            after,
            limit,
        })
    }

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }
    pub fn after(&self) -> Option<&FactLineageCursor> {
        self.after.as_ref()
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// Safe representation for a migrated or deleted fact that cannot satisfy the
/// canonical active-assertion invariant of [`StoredFactV1`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibilityFactAvailabilityV1 {
    Deleted,
    Quarantined,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactUnavailableV1 {
    target: CompatibilityFactIdV1,
    availability: CompatibilityFactAvailabilityV1,
    status: CompatibilityFactStatusV1,
}

impl CompatibilityFactUnavailableV1 {
    pub fn new(
        target: CompatibilityFactIdV1,
        availability: CompatibilityFactAvailabilityV1,
        status: CompatibilityFactStatusV1,
    ) -> FactStoreResult<Self> {
        status.validate_for_owner(target.owner())?;
        if status
            .fact_id()
            .is_some_and(|fact_id| fact_id != target.fact_id())
        {
            return Err(FactStoreError::FactMismatch);
        }
        Ok(Self {
            target,
            availability,
            status,
        })
    }

    pub fn target(&self) -> &CompatibilityFactIdV1 {
        &self.target
    }
    pub fn availability(&self) -> CompatibilityFactAvailabilityV1 {
        self.availability
    }
    pub fn status(&self) -> &CompatibilityFactStatusV1 {
        &self.status
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompatibilityFactProjectionV1 {
    Available(CompatibilityFactV1),
    Unavailable(CompatibilityFactUnavailableV1),
}

impl CompatibilityFactProjectionV1 {
    pub fn owner(&self) -> &FactOwnerV1 {
        match self {
            Self::Available(fact) => fact.owner(),
            Self::Unavailable(fact) => fact.target().owner(),
        }
    }

    pub fn fact_id(&self) -> &FactId {
        match self {
            Self::Available(fact) => fact.fact_id(),
            Self::Unavailable(fact) => fact.target().fact_id(),
        }
    }

    pub fn mapping(&self) -> Option<&CompatibilityFactMappingV1> {
        match self {
            Self::Available(fact) => Some(fact.mapping()),
            Self::Unavailable(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactContradictionQueryV1 {
    owner: FactOwnerV1,
    category: Option<FactCategoryV1>,
    threshold_millionths: u32,
    limit: usize,
}

impl CompatibilityFactContradictionQueryV1 {
    pub fn new(
        owner: FactOwnerV1,
        category: Option<FactCategoryV1>,
        threshold_millionths: u32,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if threshold_millionths > 1_000_000 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact contradiction threshold",
            }));
        }
        validate_limit(limit, MAX_CURRENT_LIMIT)?;
        Ok(Self {
            owner,
            category,
            threshold_millionths,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }
    pub fn threshold_millionths(&self) -> u32 {
        self.threshold_millionths
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactContradictionV1 {
    existing: CompatibilityFactV1,
    new_content: String,
    score_millionths: u32,
    why: Option<String>,
}

impl CompatibilityFactContradictionV1 {
    pub fn new(
        existing: CompatibilityFactV1,
        new_content: String,
        score_millionths: u32,
        why: Option<String>,
    ) -> FactStoreResult<Self> {
        if new_content.trim().is_empty() || score_millionths > 1_000_000 {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact contradiction",
            }));
        }
        if why.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact contradiction reason",
            }));
        }
        Ok(Self {
            existing,
            new_content,
            score_millionths,
            why,
        })
    }

    pub fn existing(&self) -> &CompatibilityFactV1 {
        &self.existing
    }
    pub fn new_content(&self) -> &str {
        &self.new_content
    }
    pub fn score_millionths(&self) -> u32 {
        self.score_millionths
    }
    pub fn why(&self) -> Option<&str> {
        self.why.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactContradictionPageV1 {
    owner: FactOwnerV1,
    contradictions: Vec<CompatibilityFactContradictionV1>,
}

impl CompatibilityFactContradictionPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        contradictions: Vec<CompatibilityFactContradictionV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if contradictions.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: contradictions.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        for contradiction in &contradictions {
            if contradiction.existing().owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
        }
        Ok(Self {
            owner,
            contradictions,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn contradictions(&self) -> &[CompatibilityFactContradictionV1] {
        &self.contradictions
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactAddCommandV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    content: String,
    category: FactCategoryV1,
    source: Option<String>,
    tags: Vec<String>,
    entities: Vec<String>,
    metadata: Value,
    default_trust: Confidence,
    actor: Option<ActorId>,
}

impl CompatibilityFactAddCommandV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        content: String,
        category: FactCategoryV1,
        source: Option<String>,
        tags: Vec<String>,
        entities: Vec<String>,
        metadata: Value,
        default_trust: Confidence,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if content.trim().is_empty() {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact add content",
            }));
        }
        if source.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact add source",
            }));
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            owner,
            operation_id,
            content,
            category,
            source,
            tags,
            entities,
            metadata,
            default_trust,
            actor,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn category(&self) -> FactCategoryV1 {
        self.category
    }
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
    pub fn entities(&self) -> &[String] {
        &self.entities
    }
    pub fn metadata(&self) -> &Value {
        &self.metadata
    }
    pub fn default_trust(&self) -> Confidence {
        self.default_trust
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibilityFactFeedbackActionV1 {
    Helpful,
    Unhelpful,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactUpdatePatchV1 {
    content: Option<String>,
    category: Option<FactCategoryV1>,
    source: Option<Option<String>>,
    tags: Option<Vec<String>>,
    entities: Option<Vec<String>>,
    metadata: Option<Value>,
    trust: Option<Confidence>,
}

impl CompatibilityFactUpdatePatchV1 {
    pub fn new(
        content: Option<String>,
        category: Option<FactCategoryV1>,
        source: Option<Option<String>>,
        tags: Option<Vec<String>>,
        entities: Option<Vec<String>>,
        metadata: Option<Value>,
        trust: Option<Confidence>,
    ) -> FactStoreResult<Self> {
        if content.is_none()
            && category.is_none()
            && source.is_none()
            && tags.is_none()
            && entities.is_none()
            && metadata.is_none()
            && trust.is_none()
        {
            return Err(FactStoreError::Contract(DomainError::Empty {
                field: "compatibility fact update patch",
            }));
        }
        if content
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact update content",
            }));
        }
        if source.as_ref().is_some_and(|value| {
            value.as_ref().is_some_and(|source| {
                source.trim().is_empty() || source.len() > MAX_COMPATIBILITY_REASON_BYTES
            })
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact update source",
            }));
        }
        Ok(Self {
            content,
            category,
            source,
            tags,
            entities,
            metadata,
            trust,
        })
    }

    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }
    pub fn source(&self) -> Option<Option<&str>> {
        self.source.as_ref().map(|value| value.as_deref())
    }
    pub fn tags(&self) -> Option<&[String]> {
        self.tags.as_deref()
    }
    pub fn entities(&self) -> Option<&[String]> {
        self.entities.as_deref()
    }
    pub fn metadata(&self) -> Option<&Value> {
        self.metadata.as_ref()
    }
    pub fn trust(&self) -> Option<Confidence> {
        self.trust
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactUpdateCommandV1 {
    target: CompatibilityFactTargetV1,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    patch: CompatibilityFactUpdatePatchV1,
    actor: Option<ActorId>,
}

impl CompatibilityFactUpdateCommandV1 {
    pub fn new(
        target: CompatibilityFactTargetV1,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        patch: CompatibilityFactUpdatePatchV1,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            patch,
            actor,
        })
    }

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn patch(&self) -> &CompatibilityFactUpdatePatchV1 {
        &self.patch
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactRemoveCommandV1 {
    target: CompatibilityFactTargetV1,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
}

impl CompatibilityFactRemoveCommandV1 {
    pub fn new(
        target: CompatibilityFactTargetV1,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            actor,
        })
    }

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactFeedbackCommandV1 {
    target: CompatibilityFactTargetV1,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    action: CompatibilityFactFeedbackActionV1,
    actor: Option<ActorId>,
    reason: Option<String>,
}

impl CompatibilityFactFeedbackCommandV1 {
    pub fn new(
        target: CompatibilityFactTargetV1,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        action: CompatibilityFactFeedbackActionV1,
        actor: Option<ActorId>,
        reason: Option<String>,
    ) -> FactStoreResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        if reason.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact feedback reason",
            }));
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            action,
            actor,
            reason,
        })
    }

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn action(&self) -> CompatibilityFactFeedbackActionV1 {
        self.action
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactRetrievalCommandV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    targets: Vec<CompatibilityFactTargetV1>,
    recall: bool,
}

impl CompatibilityFactRetrievalCommandV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        targets: Vec<CompatibilityFactTargetV1>,
        recall: bool,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if targets.is_empty() || targets.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: targets.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        if targets.iter().any(|target| target.owner() != &owner) {
            return Err(FactStoreError::OwnerMismatch);
        }
        Ok(Self {
            owner,
            operation_id,
            targets,
            recall,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn targets(&self) -> &[CompatibilityFactTargetV1] {
        &self.targets
    }
    pub fn recall(&self) -> bool {
        self.recall
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibilityFactProposalStateV1 {
    PendingApproval,
    Applying,
    Applied,
    Rejected,
    Quarantined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompatibilityFactProposalRevisionV1(u64);

impl CompatibilityFactProposalRevisionV1 {
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
pub struct CompatibilityFactProposalPromotionV1 {
    owner: FactOwnerV1,
    proposal_id: ProvenanceId,
    expected_revision: CompatibilityFactProposalRevisionV1,
    reviewer: Option<ActorId>,
}

impl CompatibilityFactProposalPromotionV1 {
    pub fn new(
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_revision: CompatibilityFactProposalRevisionV1,
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
    pub fn expected_revision(&self) -> CompatibilityFactProposalRevisionV1 {
        self.expected_revision
    }
    pub fn reviewer(&self) -> Option<&ActorId> {
        self.reviewer.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactProposalRecordV1 {
    proposal_id: ProvenanceId,
    owner: FactOwnerV1,
    revision: CompatibilityFactProposalRevisionV1,
    state: CompatibilityFactProposalStateV1,
    request: CompatibilityFactAddCommandV1,
    reviewer: Option<ActorId>,
    reason: Option<String>,
}

impl CompatibilityFactProposalRecordV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        proposal_id: ProvenanceId,
        owner: FactOwnerV1,
        revision: CompatibilityFactProposalRevisionV1,
        state: CompatibilityFactProposalStateV1,
        request: CompatibilityFactAddCommandV1,
        reviewer: Option<ActorId>,
        reason: Option<String>,
    ) -> FactStoreResult<Self> {
        proposal_id.validate()?;
        owner.validate()?;
        if request.owner() != &owner {
            return Err(FactStoreError::OwnerMismatch);
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
        Ok(Self {
            proposal_id,
            owner,
            revision,
            state,
            request,
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
    pub fn revision(&self) -> CompatibilityFactProposalRevisionV1 {
        self.revision
    }
    pub fn state(&self) -> CompatibilityFactProposalStateV1 {
        self.state
    }
    pub fn request(&self) -> &CompatibilityFactAddCommandV1 {
        &self.request
    }
    pub fn reviewer(&self) -> Option<&ActorId> {
        self.reviewer.as_ref()
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactProposalPageV1 {
    owner: FactOwnerV1,
    proposals: Vec<CompatibilityFactProposalRecordV1>,
    next_after_proposal_id: Option<ProvenanceId>,
}

impl CompatibilityFactProposalPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        proposals: Vec<CompatibilityFactProposalRecordV1>,
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
    pub fn proposals(&self) -> &[CompatibilityFactProposalRecordV1] {
        &self.proposals
    }
    pub fn next_after_proposal_id(&self) -> Option<&ProvenanceId> {
        self.next_after_proposal_id.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactProposalLegacyRecordV1 {
    legacy_proposal_id: i64,
    state: CompatibilityFactProposalStateV1,
    request: CompatibilityFactAddCommandV1,
}

impl CompatibilityFactProposalLegacyRecordV1 {
    pub fn new(
        legacy_proposal_id: i64,
        state: CompatibilityFactProposalStateV1,
        request: CompatibilityFactAddCommandV1,
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
    pub fn state(&self) -> CompatibilityFactProposalStateV1 {
        self.state
    }
    pub fn request(&self) -> &CompatibilityFactAddCommandV1 {
        &self.request
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactProposalImportV1 {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
    sidecar_digest: LocatorDigest,
    records: Vec<CompatibilityFactProposalLegacyRecordV1>,
}

impl CompatibilityFactProposalImportV1 {
    pub fn new(
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
        sidecar_digest: LocatorDigest,
        records: Vec<CompatibilityFactProposalLegacyRecordV1>,
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
    pub fn records(&self) -> &[CompatibilityFactProposalLegacyRecordV1] {
        &self.records
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactProposalImportReceiptV1 {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
    sidecar_digest: LocatorDigest,
    imported_count: usize,
    quarantined_count: usize,
}

impl CompatibilityFactProposalImportReceiptV1 {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibilityFactAddDispositionV1 {
    Added,
    NearDuplicate,
    PossibleConflict,
    RejectedSecretLike,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactAddOutcomeV1 {
    fact: Option<CompatibilityFactProjectionV1>,
    disposition: CompatibilityFactAddDispositionV1,
    closest_fact_id: Option<CompatibilityFactIdV1>,
    similarity_millionths: Option<u32>,
    reason: Option<String>,
}

impl CompatibilityFactAddOutcomeV1 {
    pub fn new(
        fact: Option<CompatibilityFactProjectionV1>,
        disposition: CompatibilityFactAddDispositionV1,
        closest_fact_id: Option<CompatibilityFactIdV1>,
        similarity_millionths: Option<u32>,
        reason: Option<String>,
    ) -> FactStoreResult<Self> {
        if similarity_millionths.is_some_and(|value| value > 1_000_000)
            || reason.as_ref().is_some_and(|value| {
                value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
            })
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact add outcome",
            }));
        }
        Ok(Self {
            fact,
            disposition,
            closest_fact_id,
            similarity_millionths,
            reason,
        })
    }

    pub fn fact(&self) -> Option<&CompatibilityFactProjectionV1> {
        self.fact.as_ref()
    }
    pub fn disposition(&self) -> CompatibilityFactAddDispositionV1 {
        self.disposition
    }
    pub fn closest_fact_id(&self) -> Option<&CompatibilityFactIdV1> {
        self.closest_fact_id.as_ref()
    }
    pub fn similarity_millionths(&self) -> Option<u32> {
        self.similarity_millionths
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactUpdateOutcomeV1 {
    fact: CompatibilityFactProjectionV1,
    trust_delta_millionths: i32,
}

impl CompatibilityFactUpdateOutcomeV1 {
    pub fn new(
        fact: CompatibilityFactProjectionV1,
        trust_delta_millionths: i32,
    ) -> FactStoreResult<Self> {
        if !(-1_000_000..=1_000_000).contains(&trust_delta_millionths) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact update trust delta",
            }));
        }
        Ok(Self {
            fact,
            trust_delta_millionths,
        })
    }

    pub fn fact(&self) -> &CompatibilityFactProjectionV1 {
        &self.fact
    }
    pub fn trust_delta_millionths(&self) -> i32 {
        self.trust_delta_millionths
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactRemoveOutcomeV1 {
    fact: CompatibilityFactProjectionV1,
    removed: bool,
    remaining_fact_count: u64,
}

impl CompatibilityFactRemoveOutcomeV1 {
    pub fn new(
        fact: CompatibilityFactProjectionV1,
        removed: bool,
        remaining_fact_count: u64,
    ) -> Self {
        Self {
            fact,
            removed,
            remaining_fact_count,
        }
    }

    pub fn fact(&self) -> &CompatibilityFactProjectionV1 {
        &self.fact
    }
    pub fn removed(&self) -> bool {
        self.removed
    }
    pub fn remaining_fact_count(&self) -> u64 {
        self.remaining_fact_count
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactFeedbackOutcomeV1 {
    fact: CompatibilityFactProjectionV1,
    event_id: FactEventId,
    old_trust: Confidence,
    new_trust: Confidence,
    trust_delta_millionths: i32,
    helpful_count: u64,
    unhelpful_count: u64,
}

impl CompatibilityFactFeedbackOutcomeV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fact: CompatibilityFactProjectionV1,
        event_id: FactEventId,
        old_trust: Confidence,
        new_trust: Confidence,
        trust_delta_millionths: i32,
        helpful_count: u64,
        unhelpful_count: u64,
    ) -> FactStoreResult<Self> {
        event_id.validate()?;
        if !(-1_000_000..=1_000_000).contains(&trust_delta_millionths) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact feedback trust delta",
            }));
        }
        Ok(Self {
            fact,
            event_id,
            old_trust,
            new_trust,
            trust_delta_millionths,
            helpful_count,
            unhelpful_count,
        })
    }

    pub fn fact(&self) -> &CompatibilityFactProjectionV1 {
        &self.fact
    }
    pub fn event_id(&self) -> &FactEventId {
        &self.event_id
    }
    pub fn old_trust(&self) -> Confidence {
        self.old_trust
    }
    pub fn new_trust(&self) -> Confidence {
        self.new_trust
    }
    pub fn trust_delta_millionths(&self) -> i32 {
        self.trust_delta_millionths
    }
    pub fn helpful_count(&self) -> u64 {
        self.helpful_count
    }
    pub fn unhelpful_count(&self) -> u64 {
        self.unhelpful_count
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FactCompatibilityStoreError {
    #[error(transparent)]
    Store(#[from] FactStoreError),
    #[error(transparent)]
    Proposal(#[from] FactProposalStoreError),
}

pub type FactCompatibilityResult<T> = Result<T, FactCompatibilityStoreError>;

/// Single typed authority boundary for the V1 compatibility surface.
pub trait FactCompatibilityStore: FactProposalStore {
    fn list_compatibility_facts(
        &self,
        query: CompatibilityFactListQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactPageV1>> + Send;

    fn search_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactSearchPageV1>> + Send;

    fn probe_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactSearchPageV1>> + Send;

    fn related_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactSearchPageV1>> + Send;

    fn reason_compatibility_facts(
        &self,
        query: CompatibilityFactSearchQuery,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactSearchPageV1>> + Send;

    fn find_compatibility_contradictions(
        &self,
        query: CompatibilityFactContradictionQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactContradictionPageV1>> + Send;

    fn get_compatibility_fact(
        &self,
        target: CompatibilityFactTargetV1,
    ) -> impl Future<Output = FactCompatibilityResult<Option<CompatibilityFactProjectionV1>>> + Send;

    fn compatibility_fact_history(
        &self,
        query: CompatibilityFactHistoryQueryV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactHistoryV1>> + Send;

    fn compatibility_memory_status(
        &self,
        owner: FactOwnerV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityMemoryStatusV1>> + Send;

    fn inspect_compatibility_fact(
        &self,
        target: CompatibilityFactTargetV1,
    ) -> impl Future<Output = FactCompatibilityResult<Option<CompatibilityFactInspectionV1>>> + Send;

    fn add_compatibility_fact(
        &self,
        request: CompatibilityFactAddCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactAddOutcomeV1>> + Send;

    fn update_compatibility_fact(
        &self,
        request: CompatibilityFactUpdateCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactUpdateOutcomeV1>> + Send;

    fn remove_compatibility_fact(
        &self,
        request: CompatibilityFactRemoveCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactRemoveOutcomeV1>> + Send;

    fn record_compatibility_fact_feedback(
        &self,
        request: CompatibilityFactFeedbackCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactFeedbackOutcomeV1>> + Send;

    fn record_compatibility_fact_retrieval(
        &self,
        request: CompatibilityFactRetrievalCommandV1,
    ) -> impl Future<Output = FactCompatibilityResult<Vec<CompatibilityFactProjectionV1>>> + Send;

    fn submit_compatibility_fact_proposal(
        &self,
        proposal_id: ProvenanceId,
        request: CompatibilityFactAddCommandV1,
        submitter: Option<ActorId>,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalRecordV1>> + Send;

    fn get_compatibility_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
    ) -> impl Future<Output = FactCompatibilityResult<Option<CompatibilityFactProposalRecordV1>>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn list_compatibility_fact_proposals(
        &self,
        owner: FactOwnerV1,
        state: Option<CompatibilityFactProposalStateV1>,
        after_proposal_id: Option<ProvenanceId>,
        limit: usize,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalPageV1>> + Send;

    fn count_pending_compatibility_fact_proposals(
        &self,
        owner: FactOwnerV1,
    ) -> impl Future<Output = FactCompatibilityResult<u64>> + Send;

    #[allow(clippy::too_many_arguments)]
    fn reject_compatibility_fact_proposal(
        &self,
        owner: FactOwnerV1,
        proposal_id: ProvenanceId,
        expected_revision: CompatibilityFactProposalRevisionV1,
        reviewer: ActorId,
        reason: String,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalRecordV1>> + Send;

    fn import_legacy_compatibility_fact_proposals(
        &self,
        request: CompatibilityFactProposalImportV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalImportReceiptV1>> + Send;

    fn promote_compatibility_fact_proposal(
        &self,
        request: CompatibilityFactProposalPromotionV1,
    ) -> impl Future<Output = FactCompatibilityResult<CompatibilityFactProposalRecordV1>> + Send;
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
