use std::collections::BTreeSet;
use std::error::Error;
use std::future::Future;

use tracedecay_domain::{
    Confidence, DomainError, FactAssertionId, FactAssertionV1, FactEventId, FactId,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, LegacyFactMappingV1,
    PayloadAccessState, RetrievalAnchorId, RetrievalAnchorRecordV1, SourceStoreId, UtcMicros,
};

const MAX_CURRENT_LIMIT: usize = 1_000;
const MAX_LINEAGE_LIMIT: usize = 1_000;

/// One validated, atomic append to a fact's authoritative lineage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactWriteBatch {
    fact_id: FactId,
    owner: FactOwnerV1,
    assertion: Option<FactAssertionV1>,
    events: Vec<FactLineageEventV1>,
    new_anchors: Vec<RetrievalAnchorRecordV1>,
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
        new_anchors: Vec<RetrievalAnchorRecordV1>,
        referenced_anchor_ids: Vec<RetrievalAnchorId>,
        legacy_mapping: Option<LegacyFactMappingV1>,
        expected_last_event_id: Option<FactEventId>,
    ) -> FactStoreResult<Self> {
        fact_id.validate()?;
        owner.validate()?;
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
            if !available_anchor_ids.insert(&anchor.anchor_id) {
                return Err(FactStoreError::DuplicateAnchorId {
                    anchor_id: anchor.anchor_id.clone(),
                });
            }
        }
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

    pub fn assertion(&self) -> Option<&FactAssertionV1> {
        self.assertion.as_ref()
    }

    pub fn events(&self) -> &[FactLineageEventV1] {
        &self.events
    }

    pub fn new_anchors(&self) -> &[RetrievalAnchorRecordV1] {
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
        Option<FactAssertionV1>,
        Vec<FactLineageEventV1>,
        Vec<RetrievalAnchorRecordV1>,
        Vec<RetrievalAnchorId>,
        Option<LegacyFactMappingV1>,
        Option<FactEventId>,
    ) {
        (
            self.fact_id,
            self.owner,
            self.assertion,
            self.events,
            self.new_anchors,
            self.referenced_anchor_ids,
            self.legacy_mapping,
            self.expected_last_event_id,
        )
    }
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

impl CurrentFactsQuery {
    pub fn new(
        owner: FactOwnerV1,
        after_fact_id: Option<FactId>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if let Some(fact_id) = &after_fact_id {
            fact_id.validate()?;
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

/// Page of lineage events ordered by `(occurred_at, FactEventId)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactLineageQuery {
    owner: FactOwnerV1,
    fact_id: FactId,
    after_event_id: Option<FactEventId>,
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
}

impl FactLineageQuery {
    pub fn new(
        owner: FactOwnerV1,
        fact_id: FactId,
        after_event_id: Option<FactEventId>,
        limit: usize,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        fact_id.validate()?;
        if let Some(event_id) = &after_event_id {
            event_id.validate()?;
        }
        validate_limit(limit, MAX_LINEAGE_LIMIT)?;
        Ok(Self {
            owner,
            fact_id,
            after_event_id,
            limit,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn after_event_id(&self) -> Option<&FactEventId> {
        self.after_event_id.as_ref()
    }

    pub fn limit(&self) -> usize {
        self.limit
    }
}

fn validate_limit(limit: usize, max: usize) -> FactStoreResult<()> {
    if !(1..=max).contains(&limit) {
        return Err(FactStoreError::InvalidQueryLimit { limit, max });
    }
    Ok(())
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
        anchor_id: &RetrievalAnchorId,
    ) -> impl Future<Output = FactStoreResult<Option<RetrievalAnchorRecordV1>>> + Send;
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::{
        ComponentVersion, EvidenceClass, FactAssertionKindV1, FactCategoryV1, FactEvidenceRefV1,
        FactEvidenceRelationV1, FactIdentityMaterialV1, FactIdentitySourceV1, PayloadReferenceV1,
        ProvenanceId, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
        SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
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

    #[test]
    fn batch_rejects_owner_mismatch() {
        let fact_id = fact_id(FactOwnerV1::Profile, "operation.owner");
        let event = payload_event(
            fact_id.clone(),
            FactOwnerV1::Project {
                project_id: id("project.other"),
            },
            1,
        );
        let error = FactWriteBatch::new(
            fact_id,
            FactOwnerV1::Profile,
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
    fn tombstone_rejects_payload() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id(owner.clone(), "operation.tombstone");
        let error = StoredFactV1::new(
            fact_id,
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
}
