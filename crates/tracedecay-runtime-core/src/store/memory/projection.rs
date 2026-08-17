//! Canonical project-memory projection loads and telemetry rows.

use std::collections::BTreeMap;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::{Value, params};

use tracedecay_domain::{
    Confidence, FactAssertionId, FactEventId, FactId, FactIdentityMaterialV1, FactOwnerV1,
    FactPayloadV1, PayloadAccessState, UtcMicros,
};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactStoreResult, ProjectMemoryFactProjectionV1,
    ProjectMemoryFactSnapshotV1, ProjectMemoryFactStatusV1, ProjectMemoryFactTelemetryV1,
    ProjectMemoryFactUnavailableV1, ProjectMemoryFactV1,
};

use super::primitives::{
    OwnerKey, QUERY_OPERATION, ensure_project_memory_read_active, from_json, nonnegative_u64,
    parse_payload_access, row_i64, row_optional_f64, row_optional_i64, row_optional_string,
    row_string, storage_error, storage_message,
};

const PROJECT_MEMORY_PROJECTION_BATCH_SIZE: usize = 400;

pub(super) async fn project_memory_fact_status_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<ProjectMemoryFactStatusV1>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT current_facts.payload_access, current_facts.updated_at
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
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
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
    let access = parse_payload_access(&row_string(&row, 0, QUERY_OPERATION)?)?;
    ProjectMemoryFactStatusV1::new(
        owner.clone(),
        fact_id.clone(),
        access,
        UtcMicros(row_i64(&row, 1, QUERY_OPERATION)?),
    )
    .map(Some)
}

pub(super) async fn project_memory_projection_metadata_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<(
    tracedecay_domain::FactIdentitySourceV1,
    ProjectMemoryFactTelemetryV1,
)> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT facts.identity_json, facts.created_at,
                    current_facts.retrieval_count, current_facts.access_count,
                    current_facts.helpful_count, current_facts.unhelpful_count,
                    current_facts.updated_at, current_facts.last_retrieved_at,
                    current_facts.last_recalled_at, current_facts.last_feedback_at
             FROM memory_v2_facts AS facts
             JOIN memory_v2_current_facts AS current_facts
               ON current_facts.fact_id = facts.fact_id
              AND current_facts.owner_kind = facts.owner_kind
              AND current_facts.project_id = facts.project_id
             WHERE facts.fact_id = ?1 AND facts.owner_kind = ?2
               AND facts.project_id = ?3 AND facts.owner_json = ?4",
            params![
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
        .ok_or_else(|| storage_message(QUERY_OPERATION, "canonical fact metadata is missing"))?;
    let identity = from_json::<FactIdentityMaterialV1>(
        &row_string(&row, 0, QUERY_OPERATION)?,
        QUERY_OPERATION,
    )?;
    if identity.owner() != owner || FactId::derive(&identity)? != *fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "canonical fact identity material mismatch",
        ));
    }
    let telemetry = ProjectMemoryFactTelemetryV1::new(
        nonnegative_u64(row_i64(&row, 2, QUERY_OPERATION)?, "retrieval count")?,
        nonnegative_u64(row_i64(&row, 3, QUERY_OPERATION)?, "access count")?,
        nonnegative_u64(row_i64(&row, 4, QUERY_OPERATION)?, "helpful count")?,
        nonnegative_u64(row_i64(&row, 5, QUERY_OPERATION)?, "unhelpful count")?,
        UtcMicros(row_i64(&row, 1, QUERY_OPERATION)?),
        UtcMicros(row_i64(&row, 6, QUERY_OPERATION)?),
        row_optional_i64(&row, 7, QUERY_OPERATION)?.map(UtcMicros),
        row_optional_i64(&row, 8, QUERY_OPERATION)?.map(UtcMicros),
        row_optional_i64(&row, 9, QUERY_OPERATION)?.map(UtcMicros),
    )?;
    Ok((identity.source().clone(), telemetry))
}

pub(super) async fn load_project_memory_projection_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
    Ok(
        load_project_memory_projections_tx(transaction, owner, std::slice::from_ref(fact_id))
            .await?
            .pop(),
    )
}

pub(super) async fn load_project_memory_projection_controlled_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    read_control: &FactReadControl,
) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
    Ok(load_project_memory_projections_controlled_tx(
        transaction,
        owner,
        std::slice::from_ref(fact_id),
        read_control,
    )
    .await?
    .pop())
}

/// Loads many canonical projections with one joined query per bounded
/// batch. Search, list, and dashboard vector reads used to call
/// [`load_project_memory_projection_tx`] once per fact, multiplying each result
/// into up to six serialized actor queries while holding one read snapshot.
pub(super) async fn load_project_memory_projections_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_ids: &[FactId],
) -> FactStoreResult<Vec<ProjectMemoryFactProjectionV1>> {
    load_project_memory_projections_inner_tx(transaction, owner, fact_ids, None).await
}

pub(super) async fn load_project_memory_projections_controlled_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_ids: &[FactId],
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryFactProjectionV1>> {
    load_project_memory_projections_inner_tx(transaction, owner, fact_ids, Some(read_control)).await
}

async fn load_project_memory_projections_inner_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_ids: &[FactId],
    read_control: Option<&FactReadControl>,
) -> FactStoreResult<Vec<ProjectMemoryFactProjectionV1>> {
    let ensure_active = || match read_control {
        Some(read_control) => ensure_project_memory_read_active(read_control),
        None => Ok(()),
    };
    ensure_active()?;
    if fact_ids.is_empty() {
        return Ok(Vec::new());
    }
    let key = OwnerKey::new(owner)?;
    let mut projections = BTreeMap::new();

    for batch in fact_ids.chunks(PROJECT_MEMORY_PROJECTION_BATCH_SIZE) {
        ensure_active()?;
        let mut values = vec![
            Value::Text(key.kind.to_string()),
            Value::Text(key.project_id.clone()),
            Value::Text(key.json.clone()),
        ];
        let mut placeholders = Vec::with_capacity(batch.len());
        for fact_id in batch {
            placeholders.push(format!("?{}", values.len() + 1));
            values.push(Value::Text(fact_id.as_str().to_owned()));
        }
        let sql = format!(
            "SELECT facts.fact_id,
                    current_facts.payload_access,
                    current_facts.updated_at,
                    facts.owner_json,
                    current_facts.trust_score,
                    current_facts.active_assertion_id,
                    current_facts.last_event_id,
                    payloads.payload_json,
                    facts.identity_json,
                    facts.created_at,
                    current_facts.retrieval_count,
                    current_facts.access_count,
                    current_facts.helpful_count,
                    current_facts.unhelpful_count,
                    current_facts.last_retrieved_at,
                    current_facts.last_recalled_at,
                    current_facts.last_feedback_at
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
              AND current_facts.payload_access = 'eligible'
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.fact_id IN ({})",
            placeholders.join(", ")
        );
        let mut rows = transaction
            .query(&sql, values)
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?;
        ensure_active()?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?
        {
            ensure_active()?;
            let fact_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
            let access = parse_payload_access(&row_string(&row, 1, QUERY_OPERATION)?)?;
            let status = ProjectMemoryFactStatusV1::new(
                owner.clone(),
                fact_id.clone(),
                access,
                UtcMicros(row_i64(&row, 2, QUERY_OPERATION)?),
            )?;
            if access != PayloadAccessState::Eligible {
                projections.insert(
                    fact_id,
                    ProjectMemoryFactProjectionV1::Unavailable(
                        ProjectMemoryFactUnavailableV1::new(status)?,
                    ),
                );
                continue;
            }
            let Some(active_assertion_id) = row_optional_string(&row, 5, QUERY_OPERATION)?
                .map(FactAssertionId::new)
                .transpose()?
            else {
                return Err(FactStoreError::PayloadAccessMismatch);
            };
            let payload = from_json::<FactPayloadV1>(
                &row_optional_string(&row, 7, QUERY_OPERATION)?
                    .ok_or(FactStoreError::PayloadAccessMismatch)?,
                QUERY_OPERATION,
            )?;
            let identity = from_json::<FactIdentityMaterialV1>(
                &row_string(&row, 8, QUERY_OPERATION)?,
                QUERY_OPERATION,
            )?;
            if identity.owner() != owner || FactId::derive(&identity)? != fact_id {
                return Err(storage_message(
                    QUERY_OPERATION,
                    "canonical fact identity material mismatch",
                ));
            }
            let telemetry = ProjectMemoryFactTelemetryV1::new(
                nonnegative_u64(row_i64(&row, 10, QUERY_OPERATION)?, "retrieval count")?,
                nonnegative_u64(row_i64(&row, 11, QUERY_OPERATION)?, "access count")?,
                nonnegative_u64(row_i64(&row, 12, QUERY_OPERATION)?, "helpful count")?,
                nonnegative_u64(row_i64(&row, 13, QUERY_OPERATION)?, "unhelpful count")?,
                UtcMicros(row_i64(&row, 9, QUERY_OPERATION)?),
                UtcMicros(row_i64(&row, 2, QUERY_OPERATION)?),
                row_optional_i64(&row, 14, QUERY_OPERATION)?.map(UtcMicros),
                row_optional_i64(&row, 15, QUERY_OPERATION)?.map(UtcMicros),
                row_optional_i64(&row, 16, QUERY_OPERATION)?.map(UtcMicros),
            )?;
            let projection = ProjectMemoryFactV1::new(
                fact_id.clone(),
                owner.clone(),
                payload,
                Confidence::new(row_optional_f64(&row, 4, QUERY_OPERATION)?.ok_or_else(|| {
                    storage_message(
                        QUERY_OPERATION,
                        "current fact trust score is unexpectedly null",
                    )
                })?)?,
                ProjectMemoryFactSnapshotV1::new(
                    active_assertion_id,
                    FactEventId::new(row_string(&row, 6, QUERY_OPERATION)?)?,
                    UtcMicros(row_i64(&row, 2, QUERY_OPERATION)?),
                ),
                identity.source().clone(),
                telemetry,
            )?;
            projections.insert(
                fact_id,
                ProjectMemoryFactProjectionV1::Available(Box::new(projection)),
            );
        }
        drop(rows);
    }

    ensure_active()?;
    let ordered = fact_ids
        .iter()
        .filter_map(|fact_id| projections.get(fact_id).cloned())
        .collect();
    ensure_active()?;
    Ok(ordered)
}
