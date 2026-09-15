use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tracedecay_domain::{
    DomainError, FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactCurationActionV1,
    FactEventId, FactId, FactIdentityMaterialV1, FactLineageEventKindV1, FactLineageEventV1,
    FactOwnerV1, ManifestDigest, RetrievalAnchorId, RetrievalAnchorRecordV2, canonical_sha256,
};

use super::{FactStoreError, FactStoreResult, validate_owned_fact_id};

pub(super) const MAX_FACT_WRITE_BATCH_EVENTS: usize = 256;

pub(super) const MAX_FACT_WRITE_BATCH_NEW_ANCHORS: usize = 256;

/// Caller-owned admission and commit arbitration for one fact mutation.
///
/// The store intentionally provides no default or allow-all control: every
/// mutation must bind interruption and commit admission to its caller's
/// lifecycle.
#[derive(Clone)]
pub struct FactWriteControl {
    interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
    try_begin_commit: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl FactWriteControl {
    pub fn new(
        interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        try_begin_commit: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            interrupted,
            try_begin_commit,
        }
    }

    pub fn interrupted(&self) -> bool {
        (self.interrupted)()
    }

    pub fn try_begin_commit(&self) -> bool {
        (self.try_begin_commit)()
    }
}

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
        if events.len() > MAX_FACT_WRITE_BATCH_EVENTS {
            return Err(FactStoreError::BatchLimitExceeded {
                field: "fact write batch events",
                count: events.len(),
                max: MAX_FACT_WRITE_BATCH_EVENTS,
            });
        }
        if new_anchors.len() > MAX_FACT_WRITE_BATCH_NEW_ANCHORS {
            return Err(FactStoreError::BatchLimitExceeded {
                field: "fact write batch new anchors",
                count: new_anchors.len(),
                max: MAX_FACT_WRITE_BATCH_NEW_ANCHORS,
            });
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
        validate_normalized_tag_curation(assertion.as_ref(), &events)?;

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
            self.expected_last_event_id,
        )
    }
}

fn validate_normalized_tag_curation(
    assertion: Option<&FactAssertionV1>,
    events: &[FactLineageEventV1],
) -> FactStoreResult<()> {
    let normalized_count = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind(),
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::TagsNormalized { .. },
                    ..
                }
            )
        })
        .count();
    if normalized_count == 0 {
        return Ok(());
    }
    let (Some(assertion), [recorded, normalized]) = (assertion, events) else {
        return Err(invalid_normalized_tag_batch());
    };
    let FactAssertionKindV1::Correction { .. } = assertion.kind() else {
        return Err(invalid_normalized_tag_batch());
    };
    let FactLineageEventKindV1::AssertionRecorded { assertion_id } = recorded.kind() else {
        return Err(invalid_normalized_tag_batch());
    };
    let FactLineageEventKindV1::Curated {
        action: FactCurationActionV1::TagsNormalized { .. },
        evidence_ids,
    } = normalized.kind()
    else {
        return Err(invalid_normalized_tag_batch());
    };
    if normalized_count != 1
        || assertion_id != assertion.assertion_id()
        || assertion.fact_id() != recorded.fact_id()
        || assertion.fact_id() != normalized.fact_id()
        || assertion.owner() != recorded.owner()
        || assertion.owner() != normalized.owner()
        || assertion.actor_id() != recorded.actor_id()
        || assertion.actor_id() != normalized.actor_id()
        || assertion.asserted_at() != recorded.occurred_at()
        || assertion.asserted_at().0.checked_add(1) != Some(normalized.occurred_at().0)
        || !evidence_ids.is_empty()
    {
        return Err(invalid_normalized_tag_batch());
    }
    Ok(())
}

fn invalid_normalized_tag_batch() -> FactStoreError {
    FactStoreError::Contract(DomainError::NonCanonical {
        field: "normalized tag curation batch",
    })
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactCommitReceipt {
    fact_id: FactId,
    owner: FactOwnerV1,
    committed_event_ids: Vec<FactEventId>,
    last_event_id: FactEventId,
    active_assertion_id: Option<FactAssertionId>,
    committed_state_digest: ManifestDigest,
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
        let committed_state_digest = canonical_sha256(&(
            "tracedecay.fact-commit-receipt.committed-state.v1",
            &fact_id,
            &owner,
            &committed_event_ids,
            &last_event_id,
            &active_assertion_id,
        ))?;
        Ok(Self {
            fact_id,
            owner,
            committed_event_ids,
            last_event_id,
            active_assertion_id,
            committed_state_digest,
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

    /// Infallible digest of the validated durable fields in this receipt.
    /// Delivery-only replay state is not part of a fact commit receipt.
    pub fn committed_state_digest(&self) -> &ManifestDigest {
        &self.committed_state_digest
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct FactCommitReceiptRef<'a> {
    fact_id: &'a FactId,
    owner: &'a FactOwnerV1,
    committed_event_ids: &'a [FactEventId],
    last_event_id: &'a FactEventId,
    active_assertion_id: Option<&'a FactAssertionId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FactCommitReceiptWire {
    fact_id: FactId,
    owner: FactOwnerV1,
    committed_event_ids: Vec<FactEventId>,
    last_event_id: FactEventId,
    active_assertion_id: Option<FactAssertionId>,
}

impl Serialize for FactCommitReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        FactCommitReceiptRef {
            fact_id: self.fact_id(),
            owner: self.owner(),
            committed_event_ids: self.committed_event_ids(),
            last_event_id: self.last_event_id(),
            active_assertion_id: self.active_assertion_id(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FactCommitReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FactCommitReceiptWire::deserialize(deserializer)?;
        Self::new(
            wire.fact_id,
            wire.owner,
            wire.committed_event_ids,
            wire.last_event_id,
            wire.active_assertion_id,
        )
        .map_err(serde::de::Error::custom)
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
