//! Current/as-of/lineage read paths, proposal promotion, and the commit-batch entry point.

use super::super::DatabaseFactStore;
use super::super::primitives::{
    COMMIT_OPERATION, OwnerKey, QUERY_OPERATION, authority_storage_error,
    compatibility_source_store_id, from_json, parse_payload_access, row_i64, row_optional_f64,
    row_optional_string, row_string, storage_error, storage_message, to_json,
};
use super::{
    PROMOTE_OPERATION, Projection, PromotionAttempt, anchor_matches, commit_fact_tx,
    current_last_event,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::json;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ActorId, Confidence, CoverageUniverseKnowledgeV1, FactAssertionId, FactEventId, FactId,
    FactLineageEventV1, FactOwnerV1, FactPayloadV1, LegacyFactMappingV1, LegacyHistoryCoverageV1,
    PayloadAccessState, ProvenanceId, RetrievalAnchorRecordV2, ShardDispositionV1, UtcMicros,
};
use tracedecay_store::{
    CurrentFactsQuery, FactAsOfQuery, FactAsOfResponseV1, FactCommitOutcome, FactCommitReceipt,
    FactContradictionStateV1, FactCurrentQuery, FactCurrentResponseV1, FactLineageQuery,
    FactLineageResponseV1, FactProposalPromotionStateV1, FactProposalStoreError,
    FactQueryCoverageV1, FactStoreError, FactStoreResult, FactWriteBatch,
    MAX_FACT_QUERY_CONTRADICTIONS, PromoteFactProposal, PromoteFactProposalOutcome,
    RetrievalAnchorQuery, StoredFactV1,
};
pub(in crate::store::memory) async fn query_current_facts_tx(
    snapshot: &Transaction<'_>,
    query: &CurrentFactsQuery,
) -> FactStoreResult<Vec<StoredFactV1>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = match query.after_fact_id() {
        Some(after) => {
            snapshot
                .query(
                    "SELECT fact_id FROM memory_v2_current_facts
                 WHERE owner_kind = ?1 AND project_id = ?2
                   AND active_assertion_id IS NOT NULL AND fact_id > ?3
                 ORDER BY fact_id ASC LIMIT ?4",
                    params![
                        owner.kind,
                        owner.project_id.as_str(),
                        after.as_str(),
                        query.limit() as i64,
                    ],
                )
                .await
        }
        None => {
            snapshot
                .query(
                    "SELECT fact_id FROM memory_v2_current_facts
                 WHERE owner_kind = ?1 AND project_id = ?2
                   AND active_assertion_id IS NOT NULL
                 ORDER BY fact_id ASC LIMIT ?3",
                    params![owner.kind, owner.project_id.as_str(), query.limit() as i64],
                )
                .await
        }
    }
    .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        fact_ids.push(FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?);
    }
    drop(rows);

    let mut facts = Vec::with_capacity(fact_ids.len());
    for fact_id in fact_ids {
        let fact = load_current_fact_tx(snapshot, &owner, query.owner(), &fact_id)
            .await?
            .ok_or_else(|| {
                storage_message(QUERY_OPERATION, "current fact disappeared in snapshot")
            })?;
        facts.push(fact);
    }
    Ok(facts)
}

pub(in crate::store::memory) async fn query_fact_current_tx(
    snapshot: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<StoredFactV1>> {
    let key = OwnerKey::new(owner)?;
    load_current_fact_tx(snapshot, &key, owner, fact_id).await
}

pub(in crate::store::memory) async fn query_fact_current_response_tx(
    snapshot: &Transaction<'_>,
    query: &FactCurrentQuery,
) -> FactStoreResult<FactCurrentResponseV1> {
    let fact = query_fact_current_tx(snapshot, query.owner(), query.fact_id()).await?;
    let metadata = query_fact_response_metadata_tx(
        snapshot,
        query.owner(),
        query.fact_id(),
        None,
        fact.as_ref(),
    )
    .await?;
    Ok(FactCurrentResponseV1::new(
        fact,
        metadata.coverage,
        metadata.contradiction,
    ))
}

pub(in crate::store::memory) async fn load_current_fact_tx(
    snapshot: &Transaction<'_>,
    owner: &OwnerKey,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<StoredFactV1>> {
    let mut rows = snapshot
        .query(
            "SELECT facts.fact_id, current_facts.payload_access, current_facts.trust_score,
                    current_facts.active_assertion_id, current_facts.last_event_id,
                    current_facts.updated_at, payloads.payload_json
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             LEFT JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
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
        return Ok(None);
    };
    let stored_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
    if &stored_id != fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "current fact identity mismatch",
        ));
    }
    let access = parse_payload_access(&row_string(&row, 1, QUERY_OPERATION)?)?;
    let trust = Confidence::new(row_optional_f64(&row, 2, QUERY_OPERATION)?.ok_or_else(|| {
        storage_message(
            QUERY_OPERATION,
            "current fact trust score is unexpectedly null",
        )
    })?)?;
    let Some(active_assertion_id) = row_optional_string(&row, 3, QUERY_OPERATION)? else {
        return Ok(None);
    };
    let active_assertion_id = FactAssertionId::new(active_assertion_id)?;
    let last_event_id = FactEventId::new(row_string(&row, 4, QUERY_OPERATION)?)?;
    let projected_as_of = UtcMicros(row_i64(&row, 5, QUERY_OPERATION)?);
    let payload = match access {
        PayloadAccessState::Eligible => {
            let payload_json = row_optional_string(&row, 6, QUERY_OPERATION)?
                .ok_or(FactStoreError::PayloadAccessMismatch)?;
            Some(from_json::<FactPayloadV1>(&payload_json, QUERY_OPERATION)?)
        }
        _ => None,
    };
    let mapping = load_current_legacy_mapping_tx(snapshot, owner, typed_owner, fact_id).await?;
    StoredFactV1::new(
        stored_id,
        typed_owner.clone(),
        payload,
        access,
        trust,
        active_assertion_id,
        last_event_id,
        mapping,
        projected_as_of,
    )
    .map(Some)
}

pub(in crate::store::memory) async fn query_fact_as_of_tx(
    snapshot: &Transaction<'_>,
    query: &FactAsOfQuery,
) -> FactStoreResult<Option<StoredFactV1>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT event_json FROM memory_v2_lineage_events
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
               AND occurred_at <= ?4
             ORDER BY occurred_at ASC, event_id ASC",
            params![
                query.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                query.as_of().0,
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut projection = Projection::empty()?;
    let mut observed_event = false;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        let event = from_json::<FactLineageEventV1>(
            &row_string(&row, 0, QUERY_OPERATION)?,
            QUERY_OPERATION,
        )?;
        if event.fact_id() != query.fact_id() || event.owner() != query.owner() {
            return Err(storage_message(
                QUERY_OPERATION,
                "stored lineage event identity mismatch",
            ));
        }
        projection.apply(&event)?;
        observed_event = true;
    }
    drop(rows);
    if !observed_event {
        return Ok(None);
    }
    let Some(active_assertion_id) = projection.active_assertion_id.clone() else {
        return Ok(None);
    };
    let last_event_id = projection
        .last_event_id
        .clone()
        .ok_or(FactStoreError::EmptyBatch)?;
    let (payload, payload_access) = match projection.access {
        PayloadAccessState::Eligible => {
            match load_assertion_payload_tx(snapshot, &owner, query.fact_id(), &active_assertion_id)
                .await?
            {
                Some(payload) => (Some(payload), PayloadAccessState::Eligible),
                // A later deletion physically erases the payload and FTS/vector
                // copies. Do not resurrect that data merely because an as-of
                // projection predates the deletion event; retain the lineage but
                // make the unavailable payload explicit.
                None => (None, PayloadAccessState::Unavailable),
            }
        }
        access => (None, access),
    };
    let mapping = load_current_legacy_mapping_tx(snapshot, &owner, query.owner(), query.fact_id())
        .await?
        .filter(|mapping| mapping.migrated_at() <= query.as_of());
    StoredFactV1::new(
        query.fact_id().clone(),
        query.owner().clone(),
        payload,
        payload_access,
        projection.trust,
        active_assertion_id,
        last_event_id,
        mapping,
        projection.updated_at,
    )
    .map(Some)
}

pub(in crate::store::memory) async fn query_fact_as_of_response_tx(
    snapshot: &Transaction<'_>,
    query: &FactAsOfQuery,
) -> FactStoreResult<FactAsOfResponseV1> {
    let fact = query_fact_as_of_tx(snapshot, query).await?;
    let metadata = query_fact_response_metadata_tx(
        snapshot,
        query.owner(),
        query.fact_id(),
        Some(query.as_of()),
        fact.as_ref(),
    )
    .await?;
    Ok(FactAsOfResponseV1::new(
        fact,
        metadata.coverage,
        metadata.contradiction,
    ))
}

async fn load_assertion_payload_tx(
    snapshot: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<Option<FactPayloadV1>> {
    let mut rows = snapshot
        .query(
            "SELECT payload_json FROM memory_v2_assertion_payloads
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion_id.as_str(),
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
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
    from_json(&row_string(&row, 0, QUERY_OPERATION)?, QUERY_OPERATION).map(Some)
}

pub(in crate::store::memory) async fn query_fact_lineage_tx(
    snapshot: &Transaction<'_>,
    query: &FactLineageQuery,
) -> FactStoreResult<Vec<FactLineageEventV1>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = match query.after() {
        Some(after) => {
            snapshot
                .query(
                    "SELECT event_json FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                   AND (occurred_at > ?4 OR (occurred_at = ?4 AND event_id > ?5))
                 ORDER BY occurred_at ASC, event_id ASC LIMIT ?6",
                    params![
                        query.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                        after.occurred_at().0,
                        after.event_id().as_str(),
                        query.limit() as i64,
                    ],
                )
                .await
        }
        None => {
            snapshot
                .query(
                    "SELECT event_json FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                 ORDER BY occurred_at ASC, event_id ASC LIMIT ?4",
                    params![
                        query.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                        query.limit() as i64,
                    ],
                )
                .await
        }
    }
    .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut events = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        let event = from_json::<FactLineageEventV1>(
            &row_string(&row, 0, QUERY_OPERATION)?,
            QUERY_OPERATION,
        )?;
        if event.fact_id() != query.fact_id() || event.owner() != query.owner() {
            return Err(storage_message(
                QUERY_OPERATION,
                "stored lineage event identity mismatch",
            ));
        }
        events.push(event);
    }
    Ok(events)
}

pub(in crate::store::memory) async fn query_fact_lineage_response_tx(
    snapshot: &Transaction<'_>,
    query: &FactLineageQuery,
) -> FactStoreResult<FactLineageResponseV1> {
    let events = query_fact_lineage_tx(snapshot, query).await?;
    let current = query_fact_current_tx(snapshot, query.owner(), query.fact_id()).await?;
    let metadata = query_fact_response_metadata_tx(
        snapshot,
        query.owner(),
        query.fact_id(),
        None,
        current.as_ref(),
    )
    .await?;
    Ok(FactLineageResponseV1::new(
        events,
        metadata.coverage,
        metadata.contradiction,
    ))
}

struct FactResponseMetadata {
    coverage: FactQueryCoverageV1,
    contradiction: FactContradictionStateV1,
}

/// Renders an optional `as_of` bound as an always-bindable upper cutoff.
///
/// `memory_v2_lineage_events.occurred_at` is `INTEGER NOT NULL`, so an
/// unbounded read is exactly `occurred_at <= i64::MAX`. Binding the sentinel
/// lets every lineage helper carry a single SQL literal instead of forking the
/// whole statement on `Option`.
fn as_of_cutoff(as_of: Option<UtcMicros>) -> i64 {
    as_of.map_or(i64::MAX, |cutoff| cutoff.0)
}

async fn query_fact_response_metadata_tx(
    snapshot: &Transaction<'_>,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
    as_of: Option<UtcMicros>,
    fact: Option<&StoredFactV1>,
) -> FactStoreResult<FactResponseMetadata> {
    let owner = OwnerKey::new(typed_owner)?;
    let probe = probe_fact_lineage_tx(snapshot, &owner, fact_id, as_of).await?;
    let observed_event = probe.observed_event;
    let latest_assertion_id = probe
        .latest_assertion
        .require("lineage assertion event is missing an assertion identifier")?
        .map(FactAssertionId::new)
        .transpose()?;
    // Only read back the stored access state when the caller has no fact to
    // take it from; a malformed access event must not fail reads that never
    // needed it.
    let effective_access = match fact {
        Some(fact) => fact.payload_access(),
        None => probe
            .latest_payload_access
            .require("lineage payload access event is missing its current state")?
            .as_deref()
            .map(parse_payload_access)
            .transpose()?
            .unwrap_or(PayloadAccessState::Eligible),
    };
    let legacy_unknown = fact
        .and_then(StoredFactV1::legacy_mapping)
        .is_some_and(|mapping| mapping.history_coverage() == LegacyHistoryCoverageV1::Unknown);
    let coverage = query_fact_coverage_tx(
        snapshot,
        &owner,
        typed_owner,
        fact_id,
        latest_assertion_id.as_ref(),
        effective_access,
        legacy_unknown,
        observed_event,
    )
    .await?;
    let contradicted_by =
        fact_contradiction_ids_tx(snapshot, &owner, typed_owner, fact_id, as_of).await?;
    let contradiction = if !contradicted_by.is_empty() {
        FactContradictionStateV1::from_positive(contradicted_by)
    } else if !observed_event || coverage.unknown() > 0 {
        FactContradictionStateV1::Unknown
    } else {
        FactContradictionStateV1::NotObserved
    };
    Ok(FactResponseMetadata {
        coverage,
        contradiction,
    })
}

/// Measure the coverage and contradiction metadata that accompanies a fact
/// response, for callers that obtained the fact itself from another engine.
///
/// The typed runtime read port ([`FactReadOperationV1`]) admits only `Current`
/// and `Lineage`, so a runtime-mounted shard has no read operation that can
/// answer coverage or contradiction. The retained [`Database`] the runtime is
/// mounted on is proven by `validate_mount` to be the identical `SQLite` file, so
/// this snapshot measures the same authority the runtime would read rather than
/// substituting a constant for a measurement.
///
/// [`FactReadOperationV1`]: tracedecay_store::FactReadOperationV1
/// [`Database`]: crate::db::Database
pub(in crate::store::memory) async fn fact_response_metadata_tx(
    snapshot: &Transaction<'_>,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
    fact: Option<&StoredFactV1>,
) -> FactStoreResult<(FactQueryCoverageV1, FactContradictionStateV1)> {
    let metadata =
        query_fact_response_metadata_tx(snapshot, typed_owner, fact_id, None, fact).await?;
    Ok((metadata.coverage, metadata.contradiction))
}

/// The newest matching lineage event's projected JSON field.
///
/// Distinguishes "no such event" (a normal empty history) from "the event
/// exists but its payload is malformed", which the callers treat as a store
/// integrity failure.
enum LatestLineageField {
    Absent,
    Present(Option<String>),
}

impl LatestLineageField {
    fn read(row: &crate::db::engine::Row, present: i32, value: i32) -> FactStoreResult<Self> {
        if row_i64(row, present, QUERY_OPERATION)? == 0 {
            return Ok(Self::Absent);
        }
        Ok(Self::Present(row_optional_string(
            row,
            value,
            QUERY_OPERATION,
        )?))
    }

    /// Yields the field, rejecting an event that exists without one.
    fn require(self, malformed: &'static str) -> FactStoreResult<Option<String>> {
        match self {
            Self::Absent => Ok(None),
            Self::Present(Some(value)) => Ok(Some(value)),
            Self::Present(None) => Err(storage_message(QUERY_OPERATION, malformed)),
        }
    }
}

/// Everything the fact-response metadata needs from the lineage log.
struct FactLineageProbe {
    observed_event: bool,
    latest_assertion: LatestLineageField,
    latest_payload_access: LatestLineageField,
}

/// Reads the lineage log once for all three metadata projections.
///
/// Every projection filters the same `(fact_id, owner, occurred_at <= cutoff)`
/// key, so they ride one statement as correlated subqueries rather than three
/// sequential round trips.
async fn probe_fact_lineage_tx(
    snapshot: &Transaction<'_>,
    owner: &OwnerKey,
    fact_id: &FactId,
    as_of: Option<UtcMicros>,
) -> FactStoreResult<FactLineageProbe> {
    let mut rows = snapshot
        .query(
            "SELECT
               EXISTS (
                 SELECT 1 FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                   AND occurred_at <= ?4
               ),
               EXISTS (
                 SELECT 1 FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                   AND occurred_at <= ?4
                   AND json_extract(event_json, '$.kind.kind') = 'assertion_recorded'
               ),
               (
                 SELECT json_extract(event_json, '$.kind.assertion_id')
                 FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                   AND occurred_at <= ?4
                   AND json_extract(event_json, '$.kind.kind') = 'assertion_recorded'
                 ORDER BY occurred_at DESC, event_id DESC
                 LIMIT 1
               ),
               EXISTS (
                 SELECT 1 FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                   AND occurred_at <= ?4
                   AND json_extract(event_json, '$.kind.kind') = 'payload_access_changed'
               ),
               (
                 SELECT json_extract(event_json, '$.kind.current')
                 FROM memory_v2_lineage_events
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
                   AND occurred_at <= ?4
                   AND json_extract(event_json, '$.kind.kind') = 'payload_access_changed'
                 ORDER BY occurred_at DESC, event_id DESC
                 LIMIT 1
               )",
            params![
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                as_of_cutoff(as_of),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
        .ok_or_else(|| storage_message(QUERY_OPERATION, "lineage probe returned no rows"))?;
    let probe = FactLineageProbe {
        observed_event: row_i64(&row, 0, QUERY_OPERATION)? != 0,
        latest_assertion: LatestLineageField::read(&row, 1, 2)?,
        latest_payload_access: LatestLineageField::read(&row, 3, 4)?,
    };
    drop(rows);
    Ok(probe)
}

async fn fact_contradiction_ids_tx(
    snapshot: &Transaction<'_>,
    owner: &OwnerKey,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
    as_of: Option<UtcMicros>,
) -> FactStoreResult<Vec<FactId>> {
    let mut rows = snapshot
        .query(
            "SELECT DISTINCT json_extract(event_json, '$.kind.action.fact_id')
             FROM memory_v2_lineage_events
             WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3
               AND occurred_at <= ?4
               AND json_extract(event_json, '$.kind.kind') = 'curated'
               AND json_extract(event_json, '$.kind.action.kind') = 'contradicted_by'
             ORDER BY 1 ASC
             LIMIT ?5",
            params![
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
                as_of_cutoff(as_of),
                MAX_FACT_QUERY_CONTRADICTIONS as i64,
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut contradicted_by = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        let contradicting_fact_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
        contradicting_fact_id.validate_owner(typed_owner)?;
        contradicted_by.push(contradicting_fact_id);
    }
    drop(rows);
    Ok(contradicted_by)
}

#[allow(clippy::too_many_arguments)]
async fn query_fact_coverage_tx(
    snapshot: &Transaction<'_>,
    owner: &OwnerKey,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
    assertion_id: Option<&FactAssertionId>,
    effective_access: PayloadAccessState,
    legacy_unknown: bool,
    observed_event: bool,
) -> FactStoreResult<FactQueryCoverageV1> {
    let Some(assertion_id) = assertion_id else {
        return Ok(if observed_event {
            classify_fact_coverage(effective_access, legacy_unknown, None)
        } else {
            FactQueryCoverageV1::default()
        });
    };
    let mut rows = snapshot
        .query(
            "SELECT evidence.anchor_id, anchors.anchor_json
             FROM memory_v2_assertion_evidence AS assertion_evidence
             JOIN memory_v2_evidence AS evidence
               ON evidence.evidence_id = assertion_evidence.evidence_id
              AND evidence.fact_id = assertion_evidence.fact_id
              AND evidence.owner_kind = assertion_evidence.owner_kind
              AND evidence.project_id = assertion_evidence.project_id
             JOIN retrieval_anchors AS anchors
               ON anchors.anchor_id = evidence.anchor_id
              AND anchors.owner_json = evidence.owner_json
             WHERE assertion_evidence.assertion_id = ?1
               AND assertion_evidence.fact_id = ?2
               AND assertion_evidence.owner_kind = ?3
               AND assertion_evidence.project_id = ?4
             ORDER BY assertion_evidence.ordinal ASC",
            params![
                assertion_id.as_str(),
                fact_id.as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut visible = 0;
    let mut hidden = 0;
    let mut unknown = 0;
    let mut redacted = 0;
    let mut saw_anchor = false;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        let anchor_id = row_string(&row, 0, QUERY_OPERATION)?;
        let anchor = from_json::<RetrievalAnchorRecordV2>(
            &row_string(&row, 1, QUERY_OPERATION)?,
            QUERY_OPERATION,
        )?;
        if anchor.anchor_id().as_str() != anchor_id
            || FactOwnerV1::from(anchor.owner().clone()) != *typed_owner
        {
            return Err(storage_message(
                QUERY_OPERATION,
                "fact evidence anchor identity mismatch",
            ));
        }
        saw_anchor = true;
        let count = classify_fact_coverage(effective_access, legacy_unknown, Some(&anchor));
        visible += count.visible();
        hidden += count.hidden();
        unknown += count.unknown();
        redacted += count.redacted();
    }
    drop(rows);
    if !saw_anchor && observed_event {
        return Ok(classify_fact_coverage(
            effective_access,
            legacy_unknown,
            None,
        ));
    }
    Ok(FactQueryCoverageV1::new(visible, hidden, unknown, redacted))
}

fn classify_fact_coverage(
    effective_access: PayloadAccessState,
    legacy_unknown: bool,
    anchor: Option<&RetrievalAnchorRecordV2>,
) -> FactQueryCoverageV1 {
    let (visible, hidden, unknown, mut redacted, frontier_count) = if legacy_unknown {
        (0, 0, 1, 0, 1)
    } else {
        match anchor {
            None => (0, 0, 1, 0, 1),
            Some(anchor) if anchor.coverage().universe == CoverageUniverseKnowledgeV1::Unknown => {
                (0, 0, 1, 0, 1)
            }
            Some(anchor) => {
                let mut visible = 0;
                let mut hidden = 0;
                let mut redacted = 0;
                for disposition in anchor.coverage().dispositions.values() {
                    match disposition {
                        ShardDispositionV1::Searched => visible += 1,
                        ShardDispositionV1::Redacted => redacted += 1,
                        ShardDispositionV1::Skipped
                        | ShardDispositionV1::Stale
                        | ShardDispositionV1::Unavailable
                        | ShardDispositionV1::Incompatible
                        | ShardDispositionV1::Locked
                        | ShardDispositionV1::Truncated => hidden += 1,
                    }
                }
                (
                    visible,
                    hidden,
                    0,
                    redacted,
                    anchor.coverage().dispositions.len() as u64,
                )
            }
        }
    };
    let anchor_access = anchor.map_or(
        PayloadAccessState::Eligible,
        RetrievalAnchorRecordV2::payload_access,
    );
    if effective_access == PayloadAccessState::Redacted
        || anchor_access == PayloadAccessState::Redacted
    {
        redacted = redacted.max(frontier_count.max(1));
        return FactQueryCoverageV1::new(visible, hidden, unknown, redacted);
    }
    if effective_access != PayloadAccessState::Eligible
        || anchor_access != PayloadAccessState::Eligible
    {
        return FactQueryCoverageV1::new(0, 1, 0, 0);
    }
    FactQueryCoverageV1::new(visible, hidden, unknown, redacted)
}

pub(in crate::store::memory) async fn get_retrieval_anchor_tx(
    snapshot: &Transaction<'_>,
    query: &RetrievalAnchorQuery,
) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT anchor.anchor_json
             FROM retrieval_anchors AS anchor
             WHERE anchor.anchor_id = ?1 AND anchor.owner_json = ?2
               AND COALESCE((
                   SELECT disposition.state
                   FROM retrieval_anchor_dispositions AS disposition
                   WHERE disposition.anchor_id = anchor.anchor_id
                     AND disposition.owner_json = anchor.owner_json
                   ORDER BY disposition.sequence DESC LIMIT 1
               ), 'active') = 'active'",
            params![query.anchor_id().as_str(), owner.json.as_str()],
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
    let anchor = from_json::<RetrievalAnchorRecordV2>(
        &row_string(&row, 0, QUERY_OPERATION)?,
        QUERY_OPERATION,
    )?;
    if anchor.anchor_id() != query.anchor_id()
        || FactOwnerV1::from(anchor.owner().clone()) != *query.owner()
        || !anchor_matches(snapshot, &owner, &anchor).await?
    {
        return Err(storage_message(
            QUERY_OPERATION,
            "retrieval anchor identity mismatch",
        ));
    }
    Ok(Some(anchor))
}

async fn load_current_legacy_mapping_tx(
    snapshot: &Transaction<'_>,
    owner: &OwnerKey,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<LegacyFactMappingV1>> {
    let mut rows = snapshot
        .query(
            "SELECT projections.fact_id, facts.created_at, facts.owner_json
             FROM memory_facts AS projections
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = projections.canonical_fact_id
             WHERE facts.owner_kind = ?1 AND facts.project_id = ?2
               AND facts.fact_id = ?3",
            params![owner.kind, owner.project_id.as_str(), fact_id.as_str()],
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
    if row_string(&row, 2, QUERY_OPERATION)? != owner.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    Ok(Some(LegacyFactMappingV1::new(
        typed_owner.clone(),
        compatibility_source_store_id()?,
        row_i64(&row, 0, QUERY_OPERATION)?,
        fact_id.clone(),
        LegacyHistoryCoverageV1::Complete,
        UtcMicros(row_i64(&row, 1, QUERY_OPERATION)?),
    )?))
}

pub(in crate::store::memory) async fn promote_fact_proposal_tx(
    transaction: &Transaction<'_>,
    promotion: &PromoteFactProposal,
) -> Result<PromotionAttempt, FactProposalStoreError> {
    let owner = OwnerKey::new(promotion.owner())?;
    let actual = proposal_current_state(transaction, &owner, promotion.proposal_id()).await?;
    if actual != Some(promotion.expected_state()) {
        if let Some(stored_transition_json) =
            matching_applied_promotion_transition(transaction, &owner, promotion).await?
        {
            let actual_last =
                current_last_event(transaction, &owner, promotion.batch().fact_id()).await?;
            if actual_last.as_ref()
                == promotion
                    .batch()
                    .events()
                    .last()
                    .map(FactLineageEventV1::event_id)
            {
                let commit = commit_fact_tx(transaction, promotion.batch())
                    .await?
                    .outcome;
                if let FactCommitOutcome::IdempotentReplay(receipt) = &commit
                    && promotion_transition_json(promotion, receipt)? == stored_transition_json
                {
                    return Ok(PromotionAttempt {
                        outcome: PromoteFactProposalOutcome::new(
                            promotion.proposal_id().clone(),
                            promotion.expected_state(),
                            commit,
                        )
                        .map_err(FactStoreError::from)?,
                        wrote: false,
                    });
                }
            }
        }
        return Err(FactProposalStoreError::ProposalStateConflict {
            proposal_id: promotion.proposal_id().clone(),
            expected: promotion.expected_state(),
            actual,
        });
    }

    let commit = commit_fact_tx(transaction, promotion.batch())
        .await?
        .outcome;
    if matches!(&commit, FactCommitOutcome::Conflict(_)) {
        return Ok(PromotionAttempt {
            outcome: PromoteFactProposalOutcome::new(
                promotion.proposal_id().clone(),
                promotion.expected_state(),
                commit,
            )
            .map_err(FactStoreError::from)?,
            wrote: false,
        });
    }
    let receipt = match &commit {
        FactCommitOutcome::Committed(receipt) | FactCommitOutcome::IdempotentReplay(receipt) => {
            receipt
        }
        FactCommitOutcome::Conflict(_) => unreachable!("handled above"),
        _ => {
            return Err(authority_storage_error(
                PROMOTE_OPERATION,
                std::io::Error::other("unrecognized fact commit outcome"),
            ));
        }
    };
    let transition_json = promotion_transition_json(promotion, receipt)?;
    let transition_id = proposal_transition_id(&transition_json);
    let reviewer_json = promotion
        .reviewer()
        .map(|reviewer| to_json(reviewer, PROMOTE_OPERATION))
        .transpose()?;
    let occurred_at = promotion
        .batch()
        .events()
        .last()
        .ok_or(FactStoreError::EmptyBatch)?
        .occurred_at()
        .0;
    transaction
        .execute(
            "INSERT INTO memory_v2_proposal_transitions(
                transition_id, proposal_id, owner_kind, project_id,
                previous_state, current_state, reviewer_json, validation_json,
                origin, promoted_fact_id, promoted_assertion_id, promoted_event_id,
                transition_json, occurred_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, 'applied', ?6, NULL,
                      'runtime', ?7, ?8, ?9, ?10, ?11)",
            params![
                transition_id.as_str(),
                promotion.proposal_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                proposal_state_label(promotion.expected_state()),
                reviewer_json,
                receipt.fact_id().as_str(),
                receipt.active_assertion_id().map(FactAssertionId::as_str),
                receipt.last_event_id().as_str(),
                transition_json,
                occurred_at,
            ],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_proposal_current
             SET state = 'applied', revision = revision + 1,
                 last_transition_id = ?1, updated_at = ?2
             WHERE proposal_id = ?3 AND owner_kind = ?4 AND project_id = ?5
               AND state = ?6",
            params![
                transition_id.as_str(),
                occurred_at,
                promotion.proposal_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                proposal_state_label(promotion.expected_state()),
            ],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    if changed != 1 {
        return Err(FactProposalStoreError::ProposalStateConflict {
            proposal_id: promotion.proposal_id().clone(),
            expected: promotion.expected_state(),
            actual: proposal_current_state(transaction, &owner, promotion.proposal_id()).await?,
        });
    }
    Ok(PromotionAttempt {
        outcome: PromoteFactProposalOutcome::new(
            promotion.proposal_id().clone(),
            promotion.expected_state(),
            commit,
        )
        .map_err(FactStoreError::from)?,
        wrote: true,
    })
}

async fn proposal_current_state(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    proposal_id: &ProvenanceId,
) -> Result<Option<FactProposalPromotionStateV1>, FactProposalStoreError> {
    let mut rows = transaction
        .query(
            "SELECT current_state.state, proposals.owner_json
             FROM memory_v2_proposal_current AS current_state
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = current_state.proposal_id
              AND proposals.owner_kind = current_state.owner_kind
              AND proposals.project_id = current_state.project_id
             WHERE current_state.proposal_id = ?1
               AND current_state.owner_kind = ?2
               AND current_state.project_id = ?3",
            params![proposal_id.as_str(), owner.kind, owner.project_id.as_str(),],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?
    else {
        return Ok(None);
    };
    let owner_json = row
        .get::<String>(1)
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    if owner_json != owner.json {
        return Err(authority_storage_error(
            PROMOTE_OPERATION,
            std::io::Error::other("proposal owner identity mismatch"),
        ));
    }
    let state = row
        .get::<String>(0)
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    parse_proposal_current_state(&state)
}

async fn matching_applied_promotion_transition(
    transaction: &Transaction<'_>,
    owner: &OwnerKey,
    promotion: &PromoteFactProposal,
) -> Result<Option<String>, FactProposalStoreError> {
    let mut rows = transaction
        .query(
            "SELECT current_state.state, proposals.owner_json,
                    transition.previous_state, transition.current_state,
                    transition.promoted_fact_id, transition.promoted_event_id,
                    transition.transition_json
             FROM memory_v2_proposal_current AS current_state
             JOIN memory_v2_proposals AS proposals
               ON proposals.proposal_id = current_state.proposal_id
              AND proposals.owner_kind = current_state.owner_kind
              AND proposals.project_id = current_state.project_id
             JOIN memory_v2_proposal_transitions AS transition
               ON transition.transition_id = current_state.last_transition_id
              AND transition.proposal_id = current_state.proposal_id
              AND transition.owner_kind = current_state.owner_kind
              AND transition.project_id = current_state.project_id
             WHERE current_state.proposal_id = ?1
               AND current_state.owner_kind = ?2
               AND current_state.project_id = ?3",
            params![
                promotion.proposal_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| authority_storage_error(PROMOTE_OPERATION, error))?
    else {
        return Ok(None);
    };
    if row_string(&row, 1, PROMOTE_OPERATION)? != owner.json {
        return Err(authority_storage_error(
            PROMOTE_OPERATION,
            std::io::Error::other("proposal owner identity mismatch"),
        ));
    }
    let last_event_id = promotion
        .batch()
        .events()
        .last()
        .map(FactLineageEventV1::event_id)
        .ok_or(FactStoreError::EmptyBatch)?;
    if row_string(&row, 0, PROMOTE_OPERATION)? != "applied"
        || row_string(&row, 2, PROMOTE_OPERATION)?
            != proposal_state_label(promotion.expected_state())
        || row_string(&row, 3, PROMOTE_OPERATION)? != "applied"
        || row_optional_string(&row, 4, PROMOTE_OPERATION)?.as_deref()
            != Some(promotion.batch().fact_id().as_str())
        || row_optional_string(&row, 5, PROMOTE_OPERATION)?.as_deref()
            != Some(last_event_id.as_str())
    {
        return Ok(None);
    }
    Ok(Some(row_string(&row, 6, PROMOTE_OPERATION)?))
}

fn proposal_state_label(state: FactProposalPromotionStateV1) -> &'static str {
    match state {
        FactProposalPromotionStateV1::PendingApproval => "pending",
        FactProposalPromotionStateV1::Applying => "applying",
    }
}

fn parse_proposal_current_state(
    state: &str,
) -> Result<Option<FactProposalPromotionStateV1>, FactProposalStoreError> {
    match state {
        "pending" => Ok(Some(FactProposalPromotionStateV1::PendingApproval)),
        "applying" => Ok(Some(FactProposalPromotionStateV1::Applying)),
        "applied" | "rejected" => Ok(None),
        _ => Err(authority_storage_error(
            PROMOTE_OPERATION,
            std::io::Error::other(format!("unknown proposal state {state:?}")),
        )),
    }
}

fn promotion_transition_json(
    promotion: &PromoteFactProposal,
    receipt: &FactCommitReceipt,
) -> Result<String, FactProposalStoreError> {
    to_json(
        &json!({
            "proposal_id": promotion.proposal_id().as_str(),
            "previous_state": proposal_state_label(promotion.expected_state()),
            "current_state": "applied",
            "reviewer": promotion.reviewer().map(ActorId::as_str),
            "fact_id": receipt.fact_id().as_str(),
            "active_assertion_id": receipt.active_assertion_id().map(FactAssertionId::as_str),
            "last_event_id": receipt.last_event_id().as_str(),
        }),
        PROMOTE_OPERATION,
    )
    .map_err(FactProposalStoreError::from)
}

pub(in crate::store::memory) fn proposal_transition_id(transition_json: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(transition_json.as_bytes());
    let mut id = String::from("proposal-transition:");
    for byte in digest {
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    id
}

impl DatabaseFactStore<'_> {
    pub(in crate::store::memory) async fn commit_batch(
        &self,
        batch: &FactWriteBatch,
    ) -> FactStoreResult<FactCommitOutcome> {
        if self
            .write_control
            .as_ref()
            .is_some_and(super::super::FactWriteControl::interrupted)
        {
            return Err(storage_message(
                COMMIT_OPERATION,
                "fact commit was interrupted before transaction admission",
            ));
        }
        let transaction = self
            .db
            .begin_memory_write_transaction(COMMIT_OPERATION)
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        let attempt = match commit_fact_tx(&transaction, batch).await {
            Ok(attempt) => attempt,
            Err(error) => {
                return match transaction.rollback().await {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(storage_error(
                        COMMIT_OPERATION,
                        std::io::Error::other(format!(
                            "{error}; transaction rollback also failed and writer connection was retired: {rollback}"
                        )),
                    )),
                };
            }
        };
        if attempt.wrote {
            if self
                .write_control
                .as_ref()
                .is_some_and(|control| !control.try_begin_commit())
            {
                transaction
                    .rollback()
                    .await
                    .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "fact commit was interrupted before durable commit",
                ));
            }
            transaction
                .commit()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        } else {
            transaction
                .rollback()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        }
        Ok(attempt.outcome)
    }
}
