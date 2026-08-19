//! Canonical lineage events and current-projection publication.

use super::super::primitives::{
    COMMIT_OPERATION, OwnerKey, QUERY_OPERATION, identity_collision, parse_payload_access,
    payload_access_label, requires_payload_purge, row_exists, row_f64, row_i64,
    row_optional_string, row_string, storage_error, storage_message, to_json,
};
use super::super::privacy_purge::assertion_payload_exists_tx;
use super::DEFAULT_TRUST;
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use tracedecay_domain::{
    Confidence, FactAssertionId, FactCurationActionV1, FactEventId, FactEvidenceId, FactId,
    FactLineageEventKindV1, FactLineageEventV1, PayloadAccessState, UtcMicros,
};
use tracedecay_store::{
    FactCommitOutcome, FactCommitReceipt, FactStoreError, FactStoreResult, FactWriteBatch,
};
pub(super) async fn payload_is_purged_projection(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT current_facts.payload_access
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.fact_id = ?1
               AND current_facts.owner_kind = ?2
               AND current_facts.project_id = ?3
               AND facts.owner_json = ?4",
            params![
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    Ok(matches!(
        parse_payload_access(&row_string(&row, 0, QUERY_OPERATION)?)?,
        PayloadAccessState::Quarantined | PayloadAccessState::Deleted
    ))
}

pub(super) async fn ensure_event_references(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
) -> FactStoreResult<()> {
    match event.kind() {
        FactLineageEventKindV1::AssertionRecorded { assertion_id } => {
            if !owned_assertion_exists(transaction, owner, event.fact_id(), assertion_id).await? {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "lineage assertion reference is missing",
                ));
            }
            if !assertion_payload_exists_tx(transaction, owner, event.fact_id(), assertion_id)
                .await?
            {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "assertion without an available payload cannot be activated",
                ));
            }
        }
        FactLineageEventKindV1::TrustChanged { evidence_ids, .. } => {
            ensure_event_evidence(transaction, owner, event.fact_id(), evidence_ids).await?;
        }
        FactLineageEventKindV1::Curated {
            action,
            evidence_ids,
        } => {
            ensure_event_evidence(transaction, owner, event.fact_id(), evidence_ids).await?;
            match action {
                FactCurationActionV1::ContradictedBy { fact_id }
                | FactCurationActionV1::SupersededBy { fact_id }
                | FactCurationActionV1::MergedInto { fact_id } => {
                    ensure_owned_relation_fact(transaction, owner, fact_id).await?;
                }
                FactCurationActionV1::Linked { relation } => {
                    if relation.owner() != event.owner() {
                        return Err(FactStoreError::OwnerMismatch);
                    }
                    if relation.source_fact_id() != event.fact_id() {
                        return Err(FactStoreError::FactMismatch);
                    }
                    ensure_owned_relation_fact(transaction, owner, relation.source_fact_id())
                        .await?;
                    ensure_owned_relation_fact(transaction, owner, relation.target_fact_id())
                        .await?;
                    for evidence_fact_id in relation.evidence_fact_ids() {
                        ensure_owned_relation_fact(transaction, owner, evidence_fact_id).await?;
                    }
                }
                FactCurationActionV1::TagsNormalized {
                    evidence_fact_ids, ..
                } => {
                    for evidence_fact_id in evidence_fact_ids {
                        ensure_owned_relation_fact(transaction, owner, evidence_fact_id).await?;
                    }
                }
                FactCurationActionV1::Retained | FactCurationActionV1::Forgotten => {}
            }
        }
        FactLineageEventKindV1::PayloadAccessChanged { .. } => {}
    }
    Ok(())
}

async fn ensure_owned_relation_fact(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<()> {
    if !owned_fact_exists(transaction, owner, fact_id).await? {
        return Err(FactStoreError::FactNotFound {
            fact_id: fact_id.clone(),
        });
    }
    Ok(())
}

async fn ensure_event_evidence(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
    evidence_ids: &[FactEvidenceId],
) -> FactStoreResult<()> {
    for evidence_id in evidence_ids {
        if !owned_evidence_exists(transaction, owner, fact_id, evidence_id).await? {
            return Err(storage_message(
                COMMIT_OPERATION,
                "lineage evidence reference is missing",
            ));
        }
    }
    Ok(())
}

async fn owned_assertion_exists(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_assertions
         WHERE assertion_id = ?1 AND fact_id = ?2 AND owner_kind = ?3
           AND project_id = ?4 AND owner_json = ?5",
        params![
            assertion_id.as_str(),
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
        ],
    )
    .await
}

async fn owned_evidence_exists(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
    evidence_id: &FactEvidenceId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_evidence
         WHERE evidence_id = ?1 AND fact_id = ?2 AND owner_kind = ?3
           AND project_id = ?4 AND owner_json = ?5",
        params![
            evidence_id.as_str(),
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
        ],
    )
    .await
}

async fn owned_fact_exists(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_facts
         WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
           AND owner_json = ?4",
        params![
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str(),
            owner.json.as_str(),
        ],
    )
    .await
}

pub(super) async fn insert_event(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
) -> FactStoreResult<()> {
    if event_exists(transaction, event.event_id()).await? {
        if event_matches(transaction, owner, event).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "lineage event identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO memory_v2_lineage_events(
                event_id, fact_id, owner_kind, project_id,
                event_json, occurred_at, recorded_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.event_id().as_str(),
                event.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                to_json(event, "serialize fact lineage event")?,
                event.occurred_at().0,
                event.occurred_at().0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    Ok(())
}

pub(super) async fn event_exists(
    transaction: &Transaction<'_>,
    event_id: &FactEventId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_lineage_events WHERE event_id = ?1",
        [event_id.as_str()],
    )
    .await
}

pub(super) async fn event_matches(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    event: &FactLineageEventV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT fact_id, owner_kind, project_id, event_json, occurred_at
             FROM memory_v2_lineage_events WHERE event_id = ?1",
            [event.event_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(false);
    };
    Ok(
        row_string(&row, 0, QUERY_OPERATION)? == event.fact_id().as_str()
            && row_string(&row, 1, QUERY_OPERATION)? == owner.kind
            && row_string(&row, 2, QUERY_OPERATION)? == owner.project_id
            && row_string(&row, 3, QUERY_OPERATION)?
                == to_json(event, "serialize fact lineage event")?
            && row_i64(&row, 4, QUERY_OPERATION)? == event.occurred_at().0,
    )
}

#[derive(Clone)]
pub(in crate::store::memory) struct Projection {
    pub(in crate::store::memory) access: PayloadAccessState,
    pub(in crate::store::memory) trust: Confidence,
    pub(in crate::store::memory) active_assertion_id: Option<FactAssertionId>,
    pub(in crate::store::memory) last_event_id: Option<FactEventId>,
    pub(in crate::store::memory) updated_at: UtcMicros,
}

impl Projection {
    pub(super) fn empty() -> FactStoreResult<Self> {
        Ok(Self {
            access: PayloadAccessState::Eligible,
            trust: Confidence::new(DEFAULT_TRUST)?,
            active_assertion_id: None,
            last_event_id: None,
            updated_at: UtcMicros(0),
        })
    }

    pub(super) fn apply(&mut self, event: &FactLineageEventV1) -> FactStoreResult<()> {
        match event.kind() {
            FactLineageEventKindV1::AssertionRecorded { assertion_id } => {
                self.active_assertion_id = Some(assertion_id.clone());
            }
            FactLineageEventKindV1::TrustChanged {
                previous, current, ..
            } => {
                if previous != &self.trust {
                    return Err(storage_message(
                        COMMIT_OPERATION,
                        "trust transition is stale",
                    ));
                }
                self.trust = *current;
            }
            FactLineageEventKindV1::PayloadAccessChanged { previous, current } => {
                if previous != &self.access {
                    return Err(storage_message(
                        COMMIT_OPERATION,
                        "payload access transition is stale",
                    ));
                }
                self.access = *current;
                if requires_payload_purge(*current) {
                    self.active_assertion_id = None;
                }
            }
            FactLineageEventKindV1::Curated { .. } => {}
        }
        self.last_event_id = Some(event.event_id().clone());
        self.updated_at = event.occurred_at();
        Ok(())
    }
}

pub(super) async fn publish_current_projection(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    let mut projection = load_current_projection(transaction, owner, batch.fact_id())
        .await?
        .unwrap_or(Projection::empty()?);
    for event in batch.events() {
        projection.apply(event)?;
    }
    if projection.active_assertion_id.is_none() && !requires_payload_purge(projection.access) {
        return Err(storage_message(
            COMMIT_OPERATION,
            "fact projection has no active assertion",
        ));
    }
    let last = projection
        .last_event_id
        .as_ref()
        .ok_or(FactStoreError::EmptyBatch)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_current_facts(
                fact_id, owner_kind, project_id, payload_access, trust_score,
                active_assertion_id, last_event_id, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(fact_id, owner_kind, project_id) DO UPDATE SET
                payload_access = excluded.payload_access,
                trust_score = excluded.trust_score,
                active_assertion_id = excluded.active_assertion_id,
                last_event_id = excluded.last_event_id,
                updated_at = excluded.updated_at",
            params![
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                payload_access_label(projection.access),
                projection.trust.as_f64(),
                projection
                    .active_assertion_id
                    .as_ref()
                    .map(FactAssertionId::as_str),
                last.as_str(),
                projection.updated_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    if requires_payload_purge(projection.access) {
        transaction
            .execute_batch("PRAGMA secure_delete = ON;")
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_v2_assertion_payloads
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
                params![
                    batch.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        // A live transition to terminal payload access must erase feedback
        // free text so a deleted fact never retains API-reachable source/note
        // data.
        transaction
            .execute(
                "UPDATE memory_v2_feedback_history
                 SET source = NULL, note = NULL,
                     details_availability = CASE
                         WHEN details_availability = 'available' THEN 'redacted'
                         ELSE details_availability
                     END
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
                params![
                    batch.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str()
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

pub(in crate::store::memory) async fn load_current_projection(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<Option<Projection>> {
    let mut rows = transaction
        .query(
            "SELECT payload_access, trust_score, active_assertion_id,
                    last_event_id, updated_at
             FROM memory_v2_current_facts
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
            params![fact_id.as_str(), owner.kind, owner.project_id.as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    else {
        return Ok(None);
    };
    Ok(Some(Projection {
        access: parse_payload_access(&row_string(&row, 0, QUERY_OPERATION)?)?,
        trust: Confidence::new(row_f64(&row, 1, QUERY_OPERATION)?)?,
        active_assertion_id: row_optional_string(&row, 2, QUERY_OPERATION)?
            .map(FactAssertionId::new)
            .transpose()?,
        last_event_id: row_optional_string(&row, 3, QUERY_OPERATION)?
            .map(FactEventId::new)
            .transpose()?,
        updated_at: UtcMicros(row_i64(&row, 4, QUERY_OPERATION)?),
    }))
}

pub(super) async fn receipt_outcome(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    replay: bool,
) -> FactStoreResult<FactCommitOutcome> {
    for event in batch.events() {
        ensure_event_references(transaction, owner, event).await?;
    }
    let projection = load_current_projection(transaction, owner, batch.fact_id())
        .await?
        .ok_or_else(|| storage_message(COMMIT_OPERATION, "committed projection is missing"))?;
    let last = batch
        .events()
        .last()
        .map(FactLineageEventV1::event_id)
        .ok_or(FactStoreError::EmptyBatch)?;
    let receipt = FactCommitReceipt::new(
        batch.fact_id().clone(),
        batch.owner().clone(),
        batch
            .events()
            .iter()
            .map(|event| event.event_id().clone())
            .collect(),
        last.clone(),
        projection.active_assertion_id,
    )?;
    Ok(if replay {
        FactCommitOutcome::IdempotentReplay(receipt)
    } else {
        FactCommitOutcome::Committed(receipt)
    })
}

pub(super) async fn ensure_fact_identity(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT owner_kind, project_id, owner_json, identity_json
             FROM memory_v2_facts WHERE fact_id = ?1",
            [batch.fact_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    if let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?
    {
        let stored_owner_kind = row_string(&row, 0, COMMIT_OPERATION)?;
        let stored_project_id = row_string(&row, 1, COMMIT_OPERATION)?;
        let stored_owner_json = row_string(&row, 2, COMMIT_OPERATION)?;
        let stored_identity = row_string(&row, 3, COMMIT_OPERATION)?;
        let supplied_identity = batch
            .identity_material()
            .map(|identity| to_json(identity, "serialize fact identity"))
            .transpose()?;
        if stored_owner_kind != owner.kind
            || stored_project_id != owner.project_id
            || stored_owner_json != owner.json
            || supplied_identity
                .as_ref()
                .is_some_and(|identity| identity != &stored_identity)
        {
            return identity_collision("fact", batch.fact_id().as_str());
        }
        return Ok(());
    }
    let identity = batch
        .identity_material()
        .ok_or_else(|| FactStoreError::Storage {
            operation: COMMIT_OPERATION,
            source: Box::new(std::io::Error::other(
                "new fact requires deterministic identity material",
            )),
        })?;
    let identity_json = to_json(identity, "serialize fact identity")?;
    let created_at = batch
        .events()
        .first()
        .map(FactLineageEventV1::occurred_at)
        .ok_or(FactStoreError::EmptyBatch)?;
    transaction
        .execute(
            "INSERT INTO memory_v2_facts(
                fact_id, owner_kind, project_id, owner_json, identity_json, created_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                identity_json,
                created_at.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    Ok(())
}
