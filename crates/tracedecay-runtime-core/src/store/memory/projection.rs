//! Compatibility projection loads, telemetry rows, and legacy-mapping resolution.

use std::collections::BTreeMap;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::{Value, params};

use tracedecay_domain::{
    Confidence, FactAssertionId, FactEventId, FactId, FactIdentityMaterialV1, FactOwnerV1,
    FactPayloadV1, LegacyFactMappingV1, LegacyHistoryCoverageV1, PayloadAccessState, UtcMicros,
    VectorWatermark,
};
use tracedecay_store::{
    FactStoreError, FactStoreResult, LegacyFactQuery, ProjectMemoryFactAvailabilityV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactMappingV1, ProjectMemoryFactProjectionV1,
    ProjectMemoryFactSourceV1, ProjectMemoryFactStatusV1, ProjectMemoryFactTargetV1,
    ProjectMemoryFactTelemetryV1, ProjectMemoryFactUnavailableV1, ProjectMemoryFactV1,
    ProjectMemoryProjectionStateV1, StoredFactV1,
};

use super::primitives::{
    COMPATIBILITY_WRITE_OPERATION, OwnerKey, QUERY_OPERATION, compatibility_source_label,
    compatibility_source_store_id, from_json, nonnegative_u64, parse_payload_access, row_i64,
    row_optional_f64, row_optional_i64, row_optional_string, row_string, storage_error,
    storage_message,
};

const COMPATIBILITY_PROJECTION_BATCH_SIZE: usize = 400;

fn compatibility_projection_state(value: &str) -> FactStoreResult<ProjectMemoryProjectionStateV1> {
    match value {
        "ready" => Ok(ProjectMemoryProjectionStateV1::Ready),
        "rebuilding" => Ok(ProjectMemoryProjectionStateV1::Rebuilding),
        "stale" => Ok(ProjectMemoryProjectionStateV1::Stale),
        "unavailable" => Ok(ProjectMemoryProjectionStateV1::Unavailable),
        _ => Err(storage_message(
            QUERY_OPERATION,
            format!("unknown compatibility projection state {value:?}"),
        )),
    }
}

fn compatibility_unavailable(
    access: Option<PayloadAccessState>,
) -> ProjectMemoryFactAvailabilityV1 {
    match access {
        Some(PayloadAccessState::Deleted) => ProjectMemoryFactAvailabilityV1::Deleted,
        Some(PayloadAccessState::Quarantined) => ProjectMemoryFactAvailabilityV1::Quarantined,
        _ => ProjectMemoryFactAvailabilityV1::Unavailable,
    }
}

pub(super) async fn compatibility_fact_status_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<ProjectMemoryFactStatusV1>> {
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
    let state = compatibility_projection_state(&row_string(&row, 1, QUERY_OPERATION)?)?;
    let watermark = row_optional_string(&row, 3, QUERY_OPERATION)?
        .as_deref()
        .map(|value| from_json::<VectorWatermark>(value, QUERY_OPERATION))
        .transpose()?;
    ProjectMemoryFactStatusV1::new(
        owner.clone(),
        Some(fact_id.clone()),
        Some(access),
        state,
        Some(UtcMicros(row_i64(&row, 2, QUERY_OPERATION)?)),
        watermark,
    )
    .map(Some)
}

pub(super) async fn compatibility_legacy_mapping_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<LegacyFactMappingV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut rows = transaction
        .query(
            "SELECT projections.fact_id, facts.owner_json, facts.created_at
             FROM memory_facts AS projections
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = projections.canonical_fact_id
             WHERE facts.owner_kind = ?1 AND facts.project_id = ?2
               AND facts.fact_id = ?3",
            params![key.kind, key.project_id.as_str(), fact_id.as_str()],
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
        return Err(FactStoreError::OwnerMismatch);
    }
    Ok(Some(LegacyFactMappingV1::new(
        owner.clone(),
        source_store_id,
        row_i64(&row, 0, QUERY_OPERATION)?,
        fact_id.clone(),
        LegacyHistoryCoverageV1::Complete,
        UtcMicros(row_i64(&row, 2, QUERY_OPERATION)?),
    )?))
}

pub(super) async fn compatibility_projection_metadata_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    mapping: Option<&LegacyFactMappingV1>,
) -> FactStoreResult<(
    ProjectMemoryFactSourceV1,
    Option<String>,
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
    Ok((
        ProjectMemoryFactSourceV1::Canonical(identity.source().clone()),
        source_label,
        telemetry,
    ))
}

pub(super) async fn load_compatibility_projection_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<ProjectMemoryFactProjectionV1>> {
    Ok(
        load_compatibility_projections_tx(transaction, owner, std::slice::from_ref(fact_id))
            .await?
            .pop(),
    )
}

/// Loads many compatibility projections with one joined query per bounded
/// batch. Search, list, and dashboard vector reads used to call
/// [`load_compatibility_projection_tx`] once per fact, multiplying each result
/// into up to six serialized actor queries while holding one read snapshot.
pub(super) async fn load_compatibility_projections_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_ids: &[FactId],
) -> FactStoreResult<Vec<ProjectMemoryFactProjectionV1>> {
    if fact_ids.is_empty() {
        return Ok(Vec::new());
    }
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let mut projections = BTreeMap::new();

    for batch in fact_ids.chunks(COMPATIBILITY_PROJECTION_BATCH_SIZE) {
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
                    current_facts.projection_state,
                    current_facts.updated_at,
                    current_facts.vector_watermark_json,
                    legacy_facts.fact_id,
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
             LEFT JOIN memory_facts AS legacy_facts
               ON legacy_facts.canonical_fact_id = current_facts.fact_id
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
            let status = ProjectMemoryFactStatusV1::new(
                owner.clone(),
                Some(fact_id.clone()),
                Some(access),
                compatibility_projection_state(&row_string(&row, 2, QUERY_OPERATION)?)?,
                Some(UtcMicros(row_i64(&row, 3, QUERY_OPERATION)?)),
                row_optional_string(&row, 4, QUERY_OPERATION)?
                    .as_deref()
                    .map(|value| from_json::<VectorWatermark>(value, QUERY_OPERATION))
                    .transpose()?,
            )?;
            let legacy_mapping = match row_optional_i64(&row, 5, QUERY_OPERATION)? {
                Some(legacy_fact_id) => {
                    if row_optional_string(&row, 6, QUERY_OPERATION)?.as_deref()
                        != Some(key.json.as_str())
                    {
                        return Err(FactStoreError::OwnerMismatch);
                    }
                    Some(LegacyFactMappingV1::new(
                        owner.clone(),
                        source_store_id.clone(),
                        legacy_fact_id,
                        fact_id.clone(),
                        LegacyHistoryCoverageV1::Complete,
                        UtcMicros(row_i64(&row, 12, QUERY_OPERATION)?),
                    )?)
                }
                None => None,
            };
            let compatibility_id = ProjectMemoryFactIdV1::new(owner.clone(), fact_id.clone())?;
            let mapping =
                ProjectMemoryFactMappingV1::new(compatibility_id.clone(), legacy_mapping.clone())?;
            let Some(active_assertion_id) = row_optional_string(&row, 8, QUERY_OPERATION)?
                .map(FactAssertionId::new)
                .transpose()?
            else {
                projections.insert(
                    fact_id,
                    ProjectMemoryFactProjectionV1::Unavailable(
                        ProjectMemoryFactUnavailableV1::new(
                            compatibility_id,
                            compatibility_unavailable(status.payload_access()),
                            status,
                        )?,
                    ),
                );
                continue;
            };
            let payload = match access {
                PayloadAccessState::Eligible => Some(from_json::<FactPayloadV1>(
                    &row_optional_string(&row, 10, QUERY_OPERATION)?
                        .ok_or(FactStoreError::PayloadAccessMismatch)?,
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
                    ProjectMemoryFactProjectionV1::Unavailable(
                        ProjectMemoryFactUnavailableV1::new(
                            compatibility_id,
                            compatibility_unavailable(status.payload_access()),
                            status,
                        )?,
                    ),
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
            let telemetry = ProjectMemoryFactTelemetryV1::new(
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
            let projection = ProjectMemoryFactV1::new(
                stored,
                mapping,
                ProjectMemoryFactSourceV1::Canonical(identity.source().clone()),
                telemetry,
            )?
            .with_source_label(row_optional_string(&row, 20, QUERY_OPERATION)?)?;
            projections.insert(
                fact_id,
                ProjectMemoryFactProjectionV1::Available(Box::new(projection)),
            );
        }
    }

    Ok(fact_ids
        .iter()
        .filter_map(|fact_id| projections.get(fact_id).cloned())
        .collect())
}

pub(super) async fn resolve_compatibility_target_tx(
    transaction: &Transaction<'_>,
    target: &ProjectMemoryFactTargetV1,
) -> FactStoreResult<Option<FactId>> {
    match target {
        ProjectMemoryFactTargetV1::Canonical(target) => Ok(Some(target.fact_id().clone())),
        ProjectMemoryFactTargetV1::Legacy(query) => {
            resolve_legacy_fact_tx(transaction, query).await
        }
    }
}

pub(super) async fn compatibility_fact_for_legacy_id_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    legacy_fact_id: i64,
) -> FactStoreResult<Option<FactId>> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT projections.canonical_fact_id, facts.owner_json
             FROM memory_facts AS projections
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = projections.canonical_fact_id
             WHERE projections.fact_id = ?1
               AND facts.owner_kind = ?2 AND facts.project_id = ?3",
            params![legacy_fact_id, key.kind, key.project_id.as_str()],
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
        return Err(FactStoreError::OwnerMismatch);
    }
    FactId::new(row_string(&row, 0, QUERY_OPERATION)?)
        .map(Some)
        .map_err(FactStoreError::from)
}

pub(super) async fn compatibility_required_mapping_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<LegacyFactMappingV1> {
    compatibility_legacy_mapping_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility fact has no fixed legacy-memory-v1 mapping",
            )
        })
}

pub(super) async fn compatibility_source_for_fact_tx(
    transaction: &Transaction<'_>,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<String> {
    let mut rows = transaction
        .query(
            "SELECT source FROM memory_facts WHERE fact_id = ?1",
            params![mapping.legacy_fact_id()],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let source = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .map(|row| row_optional_string(&row, 0, COMPATIBILITY_WRITE_OPERATION))
        .transpose()?
        .flatten()
        .unwrap_or_else(|| "manual".to_owned());
    compatibility_source_label(Some(source.as_str()))
}

pub(super) async fn resolve_legacy_fact_tx(
    snapshot: &Transaction<'_>,
    query: &LegacyFactQuery,
) -> FactStoreResult<Option<FactId>> {
    let owner = OwnerKey::new(query.owner())?;
    if query.source_store_id() != &compatibility_source_store_id()? {
        return Ok(None);
    }
    let mut rows = snapshot
        .query(
            "SELECT projections.canonical_fact_id, facts.owner_json
             FROM memory_facts AS projections
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = projections.canonical_fact_id
             WHERE projections.fact_id = ?1
               AND facts.owner_kind = ?2 AND facts.project_id = ?3",
            params![
                query.legacy_fact_id(),
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
    if row_string(&row, 1, QUERY_OPERATION)? != owner.json {
        return Err(FactStoreError::OwnerMismatch);
    }
    let fact_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
    query.validate_resolved_fact_id(&fact_id)?;
    Ok(Some(fact_id))
}
