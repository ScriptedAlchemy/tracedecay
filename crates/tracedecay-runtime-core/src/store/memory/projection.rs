//! Compatibility projection loads, telemetry rows, and legacy-mapping resolution.

use std::collections::BTreeMap;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::{Value, params};

use tracedecay_domain::{
    Confidence, FactAssertionId, FactEventId, FactId, FactIdentityMaterialV1, FactOwnerV1,
    FactPayloadV1, LegacyFactMappingV1, PayloadAccessState, UtcMicros, VectorWatermark,
};
use tracedecay_store::{
    Fact, FactAvailability, FactLineageError, FactLineageResult, FactMapping, FactProjection,
    FactSource, FactStatus, FactTarget, FactTelemetry, FactUnavailable, LegacyFactQuery,
    OwnedFactId, ProjectionState, StoredFactV1,
};

use super::primitives::{
    FACT_WRITE_OPERATION, OwnerKey, QUERY_OPERATION, fact_source_label, from_json, nonnegative_u64,
    parse_payload_access, row_i64, row_optional_f64, row_optional_i64, row_optional_string,
    row_string, source_store_id, storage_error, storage_message,
};

const FACT_PROJECTION_BATCH_SIZE: usize = 400;

fn projection_state(value: &str) -> FactLineageResult<ProjectionState> {
    match value {
        "ready" => Ok(ProjectionState::Ready),
        "rebuilding" => Ok(ProjectionState::Rebuilding),
        "stale" => Ok(ProjectionState::Stale),
        "unavailable" => Ok(ProjectionState::Unavailable),
        _ => Err(storage_message(
            QUERY_OPERATION,
            format!("unknown compatibility projection state {value:?}"),
        )),
    }
}

fn unavailable(access: Option<PayloadAccessState>) -> FactAvailability {
    match access {
        Some(PayloadAccessState::Deleted) => FactAvailability::Deleted,
        Some(PayloadAccessState::Quarantined) => FactAvailability::Quarantined,
        _ => FactAvailability::Unavailable,
    }
}

pub(super) async fn fact_status_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactLineageResult<Option<FactStatus>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT current_facts.payload_access, current_facts.projection_state,
                    current_facts.updated_at, current_facts.vector_watermark_json
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
    let state = projection_state(&row_string(&row, 1, QUERY_OPERATION)?)?;
    let watermark = row_optional_string(&row, 3, QUERY_OPERATION)?
        .as_deref()
        .map(|value| from_json::<VectorWatermark>(value, QUERY_OPERATION))
        .transpose()?;
    FactStatus::new(
        owner.clone(),
        Some(fact_id.clone()),
        Some(access),
        state,
        Some(UtcMicros(row_i64(&row, 2, QUERY_OPERATION)?)),
        watermark,
    )
    .map(Some)
}

pub(super) async fn legacy_mapping_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactLineageResult<Option<LegacyFactMappingV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT mapping_json, owner_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
               AND source_store_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                fact_id.as_str(),
                source_store_id.as_str(),
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
    if row_string(&row, 1, QUERY_OPERATION)? != key.json {
        return Err(FactLineageError::OwnerMismatch);
    }
    let mapping =
        from_json::<LegacyFactMappingV1>(&row_string(&row, 0, QUERY_OPERATION)?, QUERY_OPERATION)?;
    if mapping.owner() != owner || mapping.fact_id() != fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "compatibility legacy mapping identity mismatch",
        ));
    }
    Ok(Some(mapping))
}

pub(super) async fn projection_metadata_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    mapping: Option<&LegacyFactMappingV1>,
) -> FactLineageResult<(FactSource, Option<String>, FactTelemetry)> {
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
        .ok_or_else(|| {
            storage_message(QUERY_OPERATION, "compatibility fact metadata is missing")
        })?;
    let identity = from_json::<FactIdentityMaterialV1>(
        &row_string(&row, 0, QUERY_OPERATION)?,
        QUERY_OPERATION,
    )?;
    if identity.owner() != owner || FactId::derive(&identity)? != *fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "compatibility fact identity material mismatch",
        ));
    }
    let source_label = match mapping {
        Some(mapping) => {
            let mut source_rows = transaction
                .query(
                    "SELECT source FROM memory_facts WHERE fact_id = ?1",
                    params![mapping.legacy_fact_id()],
                )
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?;
            source_rows
                .next()
                .await
                .map_err(|error| storage_error(QUERY_OPERATION, error))?
                .map(|row| row_optional_string(&row, 0, QUERY_OPERATION))
                .transpose()?
                .flatten()
        }
        None => None,
    };
    let telemetry = FactTelemetry::new(
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
    Ok((
        FactSource::Canonical(identity.source().clone()),
        source_label,
        telemetry,
    ))
}

pub(super) async fn load_projection_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactLineageResult<Option<FactProjection>> {
    Ok(
        load_projections_tx(transaction, owner, std::slice::from_ref(fact_id))
            .await?
            .pop(),
    )
}

/// Loads many compatibility projections with one joined query per bounded
/// batch. Search, list, and dashboard vector reads used to call
/// [`load_projection_tx`] once per fact, multiplying each result
/// into up to six serialized actor queries while holding one read snapshot.
pub(super) async fn load_projections_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_ids: &[FactId],
) -> FactLineageResult<Vec<FactProjection>> {
    if fact_ids.is_empty() {
        return Ok(Vec::new());
    }
    let key = OwnerKey::new(owner)?;
    let source_store_id = source_store_id()?;
    let mut projections = BTreeMap::new();

    for batch in fact_ids.chunks(FACT_PROJECTION_BATCH_SIZE) {
        let mut values = vec![
            Value::Text(key.kind.to_string()),
            Value::Text(key.project_id.clone()),
            Value::Text(key.json.clone()),
            Value::Text(source_store_id.as_str().to_owned()),
        ];
        let mut placeholders = Vec::with_capacity(batch.len());
        for fact_id in batch {
            placeholders.push(format!("?{}", values.len() + 1));
            values.push(Value::Text(fact_id.as_str().to_owned()));
        }
        let sql = format!(
            "SELECT facts.fact_id,
                    current_facts.payload_access,
                    current_facts.projection_state,
                    current_facts.updated_at,
                    current_facts.vector_watermark_json,
                    mappings.mapping_json,
                    mappings.owner_json,
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
                    current_facts.last_feedback_at,
                    legacy_facts.source
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
             LEFT JOIN memory_v2_legacy_map AS mappings
               ON mappings.fact_id = current_facts.fact_id
              AND mappings.owner_kind = current_facts.owner_kind
              AND mappings.project_id = current_facts.project_id
              AND mappings.source_store_id = ?4
             LEFT JOIN memory_facts AS legacy_facts
               ON legacy_facts.fact_id = mappings.legacy_fact_id
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
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(QUERY_OPERATION, error))?
        {
            let fact_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
            let access = parse_payload_access(&row_string(&row, 1, QUERY_OPERATION)?)?;
            let status = FactStatus::new(
                owner.clone(),
                Some(fact_id.clone()),
                Some(access),
                projection_state(&row_string(&row, 2, QUERY_OPERATION)?)?,
                Some(UtcMicros(row_i64(&row, 3, QUERY_OPERATION)?)),
                row_optional_string(&row, 4, QUERY_OPERATION)?
                    .as_deref()
                    .map(|value| from_json::<VectorWatermark>(value, QUERY_OPERATION))
                    .transpose()?,
            )?;
            let legacy_mapping = match row_optional_string(&row, 5, QUERY_OPERATION)? {
                Some(mapping_json) => {
                    if row_optional_string(&row, 6, QUERY_OPERATION)?.as_deref()
                        != Some(key.json.as_str())
                    {
                        return Err(FactLineageError::OwnerMismatch);
                    }
                    let mapping = from_json::<LegacyFactMappingV1>(&mapping_json, QUERY_OPERATION)?;
                    if mapping.owner() != owner || mapping.fact_id() != &fact_id {
                        return Err(storage_message(
                            QUERY_OPERATION,
                            "compatibility legacy mapping identity mismatch",
                        ));
                    }
                    Some(mapping)
                }
                None => None,
            };
            let id = OwnedFactId::new(owner.clone(), fact_id.clone())?;
            let mapping = FactMapping::new(id.clone(), legacy_mapping.clone())?;
            let Some(active_assertion_id) = row_optional_string(&row, 8, QUERY_OPERATION)?
                .map(FactAssertionId::new)
                .transpose()?
            else {
                projections.insert(
                    fact_id,
                    FactProjection::Unavailable(FactUnavailable::new(
                        id,
                        unavailable(status.payload_access()),
                        status,
                    )?),
                );
                continue;
            };
            let payload = match access {
                PayloadAccessState::Eligible => Some(from_json::<FactPayloadV1>(
                    &row_optional_string(&row, 10, QUERY_OPERATION)?
                        .ok_or(FactLineageError::PayloadAccessMismatch)?,
                    QUERY_OPERATION,
                )?),
                _ => None,
            };
            let stored = StoredFactV1::new(
                fact_id.clone(),
                owner.clone(),
                payload,
                access,
                Confidence::new(row_optional_f64(&row, 7, QUERY_OPERATION)?.ok_or_else(|| {
                    storage_message(
                        QUERY_OPERATION,
                        "current fact trust score is unexpectedly null",
                    )
                })?)?,
                active_assertion_id,
                FactEventId::new(row_string(&row, 9, QUERY_OPERATION)?)?,
                legacy_mapping,
                UtcMicros(row_i64(&row, 3, QUERY_OPERATION)?),
            )?;
            if stored.payload().is_none() {
                projections.insert(
                    fact_id,
                    FactProjection::Unavailable(FactUnavailable::new(
                        id,
                        unavailable(status.payload_access()),
                        status,
                    )?),
                );
                continue;
            }
            let identity = from_json::<FactIdentityMaterialV1>(
                &row_string(&row, 11, QUERY_OPERATION)?,
                QUERY_OPERATION,
            )?;
            if identity.owner() != owner || FactId::derive(&identity)? != fact_id {
                return Err(storage_message(
                    QUERY_OPERATION,
                    "compatibility fact identity material mismatch",
                ));
            }
            let telemetry = FactTelemetry::new(
                nonnegative_u64(row_i64(&row, 13, QUERY_OPERATION)?, "retrieval count")?,
                nonnegative_u64(row_i64(&row, 14, QUERY_OPERATION)?, "access count")?,
                nonnegative_u64(row_i64(&row, 15, QUERY_OPERATION)?, "helpful count")?,
                nonnegative_u64(row_i64(&row, 16, QUERY_OPERATION)?, "unhelpful count")?,
                UtcMicros(row_i64(&row, 12, QUERY_OPERATION)?),
                UtcMicros(row_i64(&row, 3, QUERY_OPERATION)?),
                row_optional_i64(&row, 17, QUERY_OPERATION)?.map(UtcMicros),
                row_optional_i64(&row, 18, QUERY_OPERATION)?.map(UtcMicros),
                row_optional_i64(&row, 19, QUERY_OPERATION)?.map(UtcMicros),
            )?;
            let projection = Fact::new(
                stored,
                mapping,
                FactSource::Canonical(identity.source().clone()),
                telemetry,
            )?
            .with_source_label(row_optional_string(&row, 20, QUERY_OPERATION)?)?;
            projections.insert(fact_id, FactProjection::Available(Box::new(projection)));
        }
    }

    Ok(fact_ids
        .iter()
        .filter_map(|fact_id| projections.get(fact_id).cloned())
        .collect())
}

pub(super) async fn resolve_target_tx(
    transaction: &Transaction<'_>,
    target: &FactTarget,
) -> FactLineageResult<Option<FactId>> {
    match target {
        FactTarget::Canonical(target) => Ok(Some(target.fact_id().clone())),
        FactTarget::Legacy(query) => resolve_legacy_fact_tx(transaction, query).await,
    }
}

pub(super) async fn fact_for_legacy_id_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    legacy_fact_id: i64,
) -> FactLineageResult<Option<FactId>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT fact_id, owner_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
               AND legacy_fact_id = ?4",
            params![
                key.kind,
                key.project_id.as_str(),
                source_store_id.as_str(),
                legacy_fact_id,
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
    if row_string(&row, 1, QUERY_OPERATION)? != key.json {
        return Err(FactLineageError::OwnerMismatch);
    }
    FactId::new(row_string(&row, 0, QUERY_OPERATION)?)
        .map(Some)
        .map_err(FactLineageError::from)
}

pub(super) async fn required_mapping_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactLineageResult<LegacyFactMappingV1> {
    legacy_mapping_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility fact has no fixed legacy-memory-v1 mapping",
            )
        })
}

pub(super) async fn source_for_fact_tx(
    transaction: &Transaction<'_>,
    mapping: &LegacyFactMappingV1,
) -> FactLineageResult<String> {
    let mut rows = transaction
        .query(
            "SELECT source FROM memory_facts WHERE fact_id = ?1",
            params![mapping.legacy_fact_id()],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    let source = rows
        .next()
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?
        .map(|row| row_optional_string(&row, 0, FACT_WRITE_OPERATION))
        .transpose()?
        .flatten()
        .unwrap_or_else(|| "manual".to_owned());
    fact_source_label(Some(source.as_str()))
}

pub(super) async fn resolve_legacy_fact_tx(
    snapshot: &Transaction<'_>,
    query: &LegacyFactQuery,
) -> FactLineageResult<Option<FactId>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT fact_id, owner_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_fact_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                query.source_store_id().as_str(),
                query.legacy_fact_id(),
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
    if row_string(&row, 1, QUERY_OPERATION)? != owner.json {
        return Err(FactLineageError::OwnerMismatch);
    }
    let fact_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
    query.validate_resolved_fact_id(&fact_id)?;
    Ok(Some(fact_id))
}
