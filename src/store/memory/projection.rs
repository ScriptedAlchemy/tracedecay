//! Compatibility projection loads, telemetry rows, and legacy-mapping resolution.

use libsql::{Transaction, params};

use tracedecay_domain::{
    FactId, FactIdentityMaterialV1, FactOwnerV1, LegacyFactMappingV1, PayloadAccessState,
    UtcMicros, VectorWatermark,
};
use tracedecay_store::{
    CompatibilityFactAvailabilityV1, CompatibilityFactIdV1, CompatibilityFactMappingV1,
    CompatibilityFactProjectionV1, CompatibilityFactSourceV1, CompatibilityFactStatusV1,
    CompatibilityFactTargetV1, CompatibilityFactTelemetryV1, CompatibilityFactUnavailableV1,
    CompatibilityFactV1, CompatibilityProjectionStateV1, FactStoreError, FactStoreResult,
    LegacyFactQuery,
};

use super::crud::load_current_fact_tx;
use super::primitives::{
    COMPATIBILITY_WRITE_OPERATION, OwnerKey, QUERY_OPERATION, compatibility_source_label,
    compatibility_source_store_id, from_json, nonnegative_u64, parse_payload_access, row_i64,
    row_optional_i64, row_optional_string, row_string, storage_error, storage_message,
};

fn compatibility_projection_state(value: &str) -> FactStoreResult<CompatibilityProjectionStateV1> {
    match value {
        "ready" => Ok(CompatibilityProjectionStateV1::Ready),
        "rebuilding" => Ok(CompatibilityProjectionStateV1::Rebuilding),
        "stale" => Ok(CompatibilityProjectionStateV1::Stale),
        "unavailable" => Ok(CompatibilityProjectionStateV1::Unavailable),
        _ => Err(storage_message(
            QUERY_OPERATION,
            format!("unknown compatibility projection state {value:?}"),
        )),
    }
}

fn compatibility_unavailable(
    access: Option<PayloadAccessState>,
) -> CompatibilityFactAvailabilityV1 {
    match access {
        Some(PayloadAccessState::Deleted) => CompatibilityFactAvailabilityV1::Deleted,
        Some(PayloadAccessState::Quarantined) => CompatibilityFactAvailabilityV1::Quarantined,
        _ => CompatibilityFactAvailabilityV1::Unavailable,
    }
}

pub(super) async fn compatibility_fact_status_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<CompatibilityFactStatusV1>> {
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
    CompatibilityFactStatusV1::new(
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
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<LegacyFactMappingV1>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
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
        return Err(FactStoreError::OwnerMismatch);
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

pub(super) async fn compatibility_projection_metadata_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    mapping: Option<&LegacyFactMappingV1>,
) -> FactStoreResult<(
    CompatibilityFactSourceV1,
    Option<String>,
    CompatibilityFactTelemetryV1,
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
    let telemetry = CompatibilityFactTelemetryV1::new(
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
        CompatibilityFactSourceV1::Canonical(identity.source().clone()),
        source_label,
        telemetry,
    ))
}

pub(super) async fn load_compatibility_projection_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<CompatibilityFactProjectionV1>> {
    let Some(status) = compatibility_fact_status_tx(transaction, owner, fact_id).await? else {
        return Ok(None);
    };
    let mapping = compatibility_legacy_mapping_tx(transaction, owner, fact_id).await?;
    let compatibility_id = CompatibilityFactIdV1::new(owner.clone(), fact_id.clone())?;
    let mapping = CompatibilityFactMappingV1::new(compatibility_id.clone(), mapping)?;
    let key = OwnerKey::new(owner)?;
    let Some(stored) = load_current_fact_tx(transaction, &key, owner, fact_id).await? else {
        return CompatibilityFactUnavailableV1::new(
            compatibility_id,
            compatibility_unavailable(status.payload_access()),
            status,
        )
        .map(CompatibilityFactProjectionV1::Unavailable)
        .map(Some);
    };
    if stored.payload().is_none() {
        return CompatibilityFactUnavailableV1::new(
            compatibility_id,
            compatibility_unavailable(status.payload_access()),
            status,
        )
        .map(CompatibilityFactProjectionV1::Unavailable)
        .map(Some);
    }
    let (source, source_label, telemetry) =
        compatibility_projection_metadata_tx(transaction, owner, fact_id, mapping.legacy_mapping())
            .await?;
    CompatibilityFactV1::new(stored, mapping, source, telemetry)?
        .with_source_label(source_label)
        .map(Box::new)
        .map(CompatibilityFactProjectionV1::Available)
        .map(Some)
}

pub(super) async fn resolve_compatibility_target_tx(
    transaction: &Transaction,
    target: &CompatibilityFactTargetV1,
) -> FactStoreResult<Option<FactId>> {
    match target {
        CompatibilityFactTargetV1::Canonical(target) => Ok(Some(target.fact_id().clone())),
        CompatibilityFactTargetV1::Legacy(query) => {
            resolve_legacy_fact_tx(transaction, query).await
        }
    }
}

pub(super) async fn compatibility_fact_for_legacy_id_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    legacy_fact_id: i64,
) -> FactStoreResult<Option<FactId>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
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
        return Err(FactStoreError::OwnerMismatch);
    }
    FactId::new(row_string(&row, 0, QUERY_OPERATION)?)
        .map(Some)
        .map_err(FactStoreError::from)
}

pub(super) async fn compatibility_required_mapping_tx(
    transaction: &Transaction,
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
    transaction: &Transaction,
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
    snapshot: &Transaction,
    query: &LegacyFactQuery,
) -> FactStoreResult<Option<FactId>> {
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
        return Err(FactStoreError::OwnerMismatch);
    }
    let fact_id = FactId::new(row_string(&row, 0, QUERY_OPERATION)?)?;
    query.validate_resolved_fact_id(&fact_id)?;
    Ok(Some(fact_id))
}
