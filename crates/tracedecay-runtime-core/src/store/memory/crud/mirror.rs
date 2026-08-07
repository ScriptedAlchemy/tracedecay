//! Permanent V1 compatibility projection over the canonical V2 fact authority.
//!
//! Named production owner: [`crate::store::memory::DatabaseFactStore`]'s
//! `ProjectMemoryFactStore` implementation. It owns every runtime projection
//! write and keeps `memory_facts` plus its V1 entities, relations, feedback,
//! oplog, and FTS rows transactionally aligned with the V2 lineage write.
//! Per Plan 39 Task 7 (owner decision 2026-08-07, second) there is no derived
//! bank projection to keep aligned: those rows are deleted, and recall
//! re-encodes candidate vectors from canonical content at query time.
//! Dashboard, retrieval, curation, repair, scheduler, and offline branch-union
//! consumers may read this projection through that compatibility store; they
//! do not make the mirror an independent write authority.
//!
//! The raw V1 rows remain durable compatibility data. Cutover backfills and
//! verifies their V2 representation, but never bulk-reclaims them with the
//! canonical fact-deletion primitive. Direct legacy-store mutation is limited
//! to schema/data migration and tests; production fact mutations enter through
//! `DatabaseFactStore`.

use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, PROJECT_MEMORY_WRITE_OPERATION, QUERY_OPERATION,
    compatibility_legacy_timestamp, compatibility_source_store_id, from_json, nonnegative_u64,
    project_memory_category_label, project_memory_event_time, row_i64, row_string, storage_error,
    storage_message, to_json,
};
use super::super::projection::{
    load_project_memory_projection_tx, load_project_memory_projections_tx,
    project_memory_fact_for_legacy_id_tx, resolve_project_memory_target_tx,
};
use super::{DEFAULT_TRUST, PROJECT_MEMORY_RETENTION_CLASS, commit_fact_tx, query_fact_lineage_tx};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::memory::encoding::HolographicEncoder;
use crate::memory::entities::normalize_entity;
use crate::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, verify_memory_fact_sanitization,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionKindV1, FactAssertionV1, FactCategoryV1, FactId,
    FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1,
    FactOwnerV1, FactPayloadV1, LegacyFactMappingV1, LocatorDigest, PayloadAccessState,
    RetentionClass, SanitizationReceiptV1, SanitizerDispositionV1, UtcMicros,
};
use tracedecay_store::{
    FactCommitOutcome, FactCommitReceipt, FactLineageQuery, FactStoreError, FactStoreResult,
    FactWriteBatch, ProjectMemoryFactContentDigestQueryV1, ProjectMemoryFactHistoryQueryV1,
    ProjectMemoryFactHistoryV1, ProjectMemoryFactListQueryV1, ProjectMemoryFactPageV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactTargetV1, ProjectMemoryResult,
};
pub(in crate::store::memory) async fn list_project_memory_facts_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactListQueryV1,
) -> ProjectMemoryResult<ProjectMemoryFactPageV1> {
    let key = OwnerKey::new(query.owner())?;
    let category = query.category().map(project_memory_category_label);
    let min_trust = query.min_trust().map(Confidence::as_f64);
    let fetch_limit = i64::try_from(query.limit().saturating_add(1)).map_err(|_| {
        FactStoreError::InvalidQueryLimit {
            limit: query.limit(),
            max: usize::MAX,
        }
    })?;
    let mut rows = match (query.after_fact_id(), category) {
        (Some(after), Some(category)) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                 FROM memory_v2_current_facts AS current_facts
                 JOIN memory_v2_facts AS facts
                   ON facts.fact_id = current_facts.fact_id
                  AND facts.owner_kind = current_facts.owner_kind
                  AND facts.project_id = current_facts.project_id
                 JOIN memory_v2_assertion_payloads AS payloads
                   ON payloads.assertion_id = current_facts.active_assertion_id
                  AND payloads.fact_id = current_facts.fact_id
                  AND payloads.owner_kind = current_facts.owner_kind
                  AND payloads.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND facts.owner_json = ?3 AND current_facts.fact_id > ?4
                   AND current_facts.active_assertion_id IS NOT NULL
                   AND current_facts.trust_score >= ?5
                   AND json_extract(payloads.payload_json, '$.category') = ?6
                 ORDER BY current_facts.fact_id ASC LIMIT ?7",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        after.as_str(),
                        min_trust.unwrap_or(0.0),
                        category,
                        fetch_limit,
                    ],
                )
                .await
        }
        (Some(after), None) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                 FROM memory_v2_current_facts AS current_facts
                 JOIN memory_v2_facts AS facts
                   ON facts.fact_id = current_facts.fact_id
                  AND facts.owner_kind = current_facts.owner_kind
                  AND facts.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND facts.owner_json = ?3 AND current_facts.fact_id > ?4
                   AND current_facts.active_assertion_id IS NOT NULL
                   AND current_facts.trust_score >= ?5
                 ORDER BY current_facts.fact_id ASC LIMIT ?6",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        after.as_str(),
                        min_trust.unwrap_or(0.0),
                        fetch_limit,
                    ],
                )
                .await
        }
        (None, Some(category)) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                 FROM memory_v2_current_facts AS current_facts
                 JOIN memory_v2_facts AS facts
                   ON facts.fact_id = current_facts.fact_id
                  AND facts.owner_kind = current_facts.owner_kind
                  AND facts.project_id = current_facts.project_id
                 JOIN memory_v2_assertion_payloads AS payloads
                   ON payloads.assertion_id = current_facts.active_assertion_id
                  AND payloads.fact_id = current_facts.fact_id
                  AND payloads.owner_kind = current_facts.owner_kind
                  AND payloads.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND facts.owner_json = ?3 AND current_facts.active_assertion_id IS NOT NULL
                   AND current_facts.trust_score >= ?4
                   AND json_extract(payloads.payload_json, '$.category') = ?5
                 ORDER BY current_facts.fact_id ASC LIMIT ?6",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        min_trust.unwrap_or(0.0),
                        category,
                        fetch_limit,
                    ],
                )
                .await
        }
        (None, None) => {
            transaction
                .query(
                    "SELECT current_facts.fact_id
                 FROM memory_v2_current_facts AS current_facts
                 JOIN memory_v2_facts AS facts
                   ON facts.fact_id = current_facts.fact_id
                  AND facts.owner_kind = current_facts.owner_kind
                  AND facts.project_id = current_facts.project_id
                 WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
                   AND facts.owner_json = ?3 AND current_facts.active_assertion_id IS NOT NULL
                   AND current_facts.trust_score >= ?4
                 ORDER BY current_facts.fact_id ASC LIMIT ?5",
                    params![
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        min_trust.unwrap_or(0.0),
                        fetch_limit,
                    ],
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
        fact_ids.push(
            FactId::new(row_string(&row, 0, QUERY_OPERATION)?).map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let has_more = fact_ids.len() > query.limit();
    fact_ids.truncate(query.limit());
    let facts = load_project_memory_projections_tx(transaction, query.owner(), &fact_ids).await?;
    let next = has_more
        .then(|| facts.last().map(|fact| fact.fact_id().clone()))
        .flatten();
    ProjectMemoryFactPageV1::new(query.owner().clone(), facts, next).map_err(Into::into)
}

pub(in crate::store::memory) async fn get_project_memory_fact_tx(
    transaction: &Transaction<'_>,
    target: &ProjectMemoryFactTargetV1,
) -> ProjectMemoryResult<Option<ProjectMemoryFactProjectionV1>> {
    let Some(fact_id) = resolve_project_memory_target_tx(transaction, target).await? else {
        return Ok(None);
    };
    load_project_memory_projection_tx(transaction, target.owner(), &fact_id)
        .await
        .map_err(Into::into)
}

fn compatibility_content_digest(content: &str) -> FactStoreResult<LocatorDigest> {
    LocatorDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(content.as_bytes()))
    ))
    .map_err(FactStoreError::from)
}

pub(in crate::store::memory) async fn find_project_memory_fact_by_content_digest_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactContentDigestQueryV1,
) -> ProjectMemoryResult<Option<ProjectMemoryFactProjectionV1>> {
    let key = OwnerKey::new(query.owner())?;
    let mut rows = transaction
        .query(
            "SELECT current_facts.fact_id, payloads.payload_json
             FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             JOIN memory_v2_assertion_payloads AS payloads
               ON payloads.assertion_id = current_facts.active_assertion_id
              AND payloads.fact_id = current_facts.fact_id
              AND payloads.owner_kind = current_facts.owner_kind
              AND payloads.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1
               AND current_facts.project_id = ?2
               AND facts.owner_json = ?3
               AND current_facts.payload_access = 'eligible'
               AND current_facts.active_assertion_id IS NOT NULL
             ORDER BY current_facts.fact_id ASC",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let mut matching_fact_id = None;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        let payload = from_json::<FactPayloadV1>(
            &row_string(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
            PROJECT_MEMORY_READ_OPERATION,
        )?;
        if compatibility_content_digest(payload.content())? == *query.content_digest() {
            matching_fact_id = Some(
                FactId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
                    .map_err(FactStoreError::from)?,
            );
            break;
        }
    }
    drop(rows);
    match matching_fact_id {
        Some(fact_id) => load_project_memory_projection_tx(transaction, query.owner(), &fact_id)
            .await
            .map_err(Into::into),
        None => Ok(None),
    }
}

pub(in crate::store::memory) async fn project_memory_fact_history_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactHistoryQueryV1,
) -> ProjectMemoryResult<ProjectMemoryFactHistoryV1> {
    let fact_id = resolve_project_memory_target_tx(transaction, query.target())
        .await?
        .ok_or_else(|| storage_message(QUERY_OPERATION, "compatibility fact target is missing"))?;
    let lineage = FactLineageQuery::new(
        query.target().owner().clone(),
        fact_id.clone(),
        query.after().cloned(),
        query.limit(),
    )?;
    let events = query_fact_lineage_tx(transaction, &lineage).await?;
    ProjectMemoryFactHistoryV1::new(query.target().owner().clone(), fact_id, events, None)
        .map_err(Into::into)
}

pub(in crate::store::memory) struct CompatibilitySanitizedPayload {
    pub(in crate::store::memory) payload: FactPayloadV1,
    pub(in crate::store::memory) access: PayloadAccessState,
}

pub(in crate::store::memory) fn compatibility_value_strings(
    value: &Value,
    field: &'static str,
) -> FactStoreResult<Vec<String>> {
    let values = value.as_array().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            format!("sanitized compatibility {field} is not an array"),
        )
    })?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    format!("sanitized compatibility {field} contains a non-string"),
                )
            })
        })
        .collect()
}

pub(in crate::store::memory) fn compatibility_payload_metadata(metadata: &Value) -> Value {
    let mut metadata = metadata.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.remove("automation_run_id");
    }
    metadata
}

pub(in crate::store::memory) fn compatibility_sanitize_payload(
    content: &str,
    category: FactCategoryV1,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
) -> FactStoreResult<Option<CompatibilitySanitizedPayload>> {
    let metadata = compatibility_payload_metadata(metadata);
    let sanitized = sanitize_memory_fact_payload(compatibility_payload_material(
        content, category, tags, entities, &metadata,
    ))
    .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let MemoryFactSanitizationV1::Durable { payload, receipt } = sanitized else {
        return Ok(None);
    };
    compatibility_payload_from_parts(payload, category, receipt).map(Some)
}

pub(in crate::store::memory) fn compatibility_verified_payload(
    content: &str,
    category: FactCategoryV1,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
    receipt: SanitizationReceiptV1,
) -> FactStoreResult<CompatibilitySanitizedPayload> {
    let metadata = compatibility_payload_metadata(metadata);
    let payload = compatibility_payload_material(content, category, tags, entities, &metadata);
    verify_memory_fact_sanitization(&payload, &receipt)
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    compatibility_payload_from_parts(payload, category, receipt)
}

fn compatibility_payload_material(
    content: &str,
    category: FactCategoryV1,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
) -> Value {
    json!({
        "content": content,
        "category": project_memory_category_label(category),
        "tags": tags,
        "entities": entities,
        "metadata": metadata,
    })
}

fn compatibility_payload_from_parts(
    payload: Value,
    category: FactCategoryV1,
    receipt: SanitizationReceiptV1,
) -> FactStoreResult<CompatibilitySanitizedPayload> {
    let content = payload
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "sanitized compatibility content is missing",
            )
        })?
        .to_owned();
    let tags = compatibility_value_strings(
        payload.get("tags").ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "sanitized compatibility tags are missing",
            )
        })?,
        "tags",
    )?;
    let entities = compatibility_value_strings(
        payload.get("entities").ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "sanitized compatibility entities are missing",
            )
        })?,
        "entities",
    )?;
    let metadata = payload.get("metadata").cloned().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "sanitized compatibility metadata is missing",
        )
    })?;
    let retention = RetentionClass::new(PROJECT_MEMORY_RETENTION_CLASS.to_owned())
        .map_err(FactStoreError::from)?;
    let fact_payload = FactPayloadV1::new(
        content, category, tags, entities, metadata, receipt, retention,
    )
    .map_err(FactStoreError::from)?;
    let access = match fact_payload.receipt().disposition() {
        SanitizerDispositionV1::Accepted => PayloadAccessState::Eligible,
        SanitizerDispositionV1::Redacted => PayloadAccessState::Redacted,
        SanitizerDispositionV1::Rejected | SanitizerDispositionV1::Quarantined => {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "durable compatibility payload has a non-durable receipt disposition",
            ));
        }
    };
    Ok(CompatibilitySanitizedPayload {
        payload: fact_payload,
        access,
    })
}

pub(in crate::store::memory) fn compatibility_mirror_vector(
    payload: &FactPayloadV1,
) -> FactStoreResult<Vec<u8>> {
    let encoder = HolographicEncoder::new();
    HolographicEncoder::serialize(&encoder.encode_fact(payload.content(), payload.entities()))
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))
}

pub(super) async fn compatibility_last_insert_rowid_tx(
    transaction: &Transaction<'_>,
) -> FactStoreResult<i64> {
    let mut rows = transaction
        .query("SELECT last_insert_rowid()", ())
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility last_insert_rowid returned no row",
            )
        })?;
    row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)
}

async fn compatibility_mirror_replace_entities_tx(
    transaction: &Transaction<'_>,
    legacy_fact_id: i64,
    entities: &[String],
    timestamp: i64,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT entity_id FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut old_entity_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        old_entity_ids.push(row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?);
    }
    drop(rows);
    transaction
        .execute(
            "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut normalized = BTreeSet::new();
    for entity in entities {
        let name = normalize_entity(entity);
        let key = name.to_ascii_lowercase();
        if name.is_empty() || !normalized.insert(key.clone()) {
            continue;
        }
        let mut existing = transaction
            .query(
                "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                params![key.as_str()],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        let entity_id = if let Some(row) = existing
            .next()
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        {
            row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?
        } else {
            drop(existing);
            transaction
                .execute(
                    "INSERT INTO memory_entities(
                        name, normalized_name, entity_type, aliases, created_at, updated_at
                     ) VALUES(?1, ?2, 'unknown', '[]', ?3, ?3)",
                    params![name.as_str(), key.as_str(), timestamp],
                )
                .await
                .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
            compatibility_last_insert_rowid_tx(transaction).await?
        };
        transaction
            .execute(
                "INSERT OR IGNORE INTO memory_fact_entities(fact_id, entity_id)
                 VALUES(?1, ?2)",
                params![legacy_fact_id, entity_id],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    }
    for entity_id in old_entity_ids {
        transaction
            .execute(
                "DELETE FROM memory_entities
                 WHERE entity_id = ?1
                   AND NOT EXISTS(
                     SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
                   )",
                params![entity_id],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

pub(super) enum CompatibilityMirrorInsertV1 {
    Inserted(i64),
    Existing { fact_id: FactId },
}

pub(super) async fn compatibility_mirror_insert_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    payload: &FactPayloadV1,
    source: &str,
    trust: Confidence,
    now: UtcMicros,
) -> FactStoreResult<CompatibilityMirrorInsertV1> {
    let timestamp = compatibility_legacy_timestamp(now);
    let mut existing = transaction
        .query(
            "SELECT fact_id FROM memory_facts WHERE content = ?1",
            params![payload.content()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    if let Some(row) = existing
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        let legacy_fact_id = row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?;
        let Some(fact_id) =
            project_memory_fact_for_legacy_id_tx(transaction, owner, legacy_fact_id).await?
        else {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility mirror content is already bound to another owner or canonical fact",
            ));
        };
        return Ok(CompatibilityMirrorInsertV1::Existing { fact_id });
    }
    drop(existing);
    let vector = compatibility_mirror_vector(payload)?;
    transaction
        .execute(
            "INSERT INTO memory_facts(
                content, category, tags, trust_score, created_at, updated_at, source,
                metadata, hrr_vector, hrr_algebra, hrr_dim, hrr_precision
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, 'amari_fhrr', ?9, 'f32')",
            params![
                payload.content(),
                project_memory_category_label(payload.category()),
                to_json(payload.tags(), "serialize compatibility mirror tags")?,
                trust.as_f64(),
                timestamp,
                source,
                to_json(
                    payload.metadata(),
                    "serialize compatibility mirror metadata"
                )?,
                vector,
                HolographicEncoder::DIMENSIONS as i64,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let legacy_fact_id = compatibility_last_insert_rowid_tx(transaction).await?;
    compatibility_mirror_replace_entities_tx(
        transaction,
        legacy_fact_id,
        payload.entities(),
        timestamp,
    )
    .await?;
    Ok(CompatibilityMirrorInsertV1::Inserted(legacy_fact_id))
}

pub(in crate::store::memory) async fn compatibility_mirror_update_tx(
    transaction: &Transaction<'_>,
    legacy_fact_id: i64,
    payload: &FactPayloadV1,
    source: &str,
    trust: Confidence,
    now: UtcMicros,
) -> FactStoreResult<()> {
    let timestamp = compatibility_legacy_timestamp(now);
    let vector = compatibility_mirror_vector(payload)?;
    transaction
        .execute(
            "UPDATE memory_facts SET
                content = ?1, category = ?2, tags = ?3, trust_score = ?4,
                source = ?5, metadata = ?6, hrr_vector = ?7, hrr_algebra = 'amari_fhrr',
                hrr_dim = ?8, hrr_precision = 'f32', updated_at = ?9
             WHERE fact_id = ?10",
            params![
                payload.content(),
                project_memory_category_label(payload.category()),
                to_json(payload.tags(), "serialize compatibility mirror tags")?,
                trust.as_f64(),
                source,
                to_json(
                    payload.metadata(),
                    "serialize compatibility mirror metadata"
                )?,
                vector,
                HolographicEncoder::DIMENSIONS as i64,
                timestamp,
                legacy_fact_id,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    compatibility_mirror_replace_entities_tx(
        transaction,
        legacy_fact_id,
        payload.entities(),
        timestamp,
    )
    .await?;
    Ok(())
}

pub(super) fn compatibility_legacy_mapping_for_new_fact(
    owner: &FactOwnerV1,
    legacy_fact_id: i64,
    now: UtcMicros,
) -> FactStoreResult<(FactIdentityMaterialV1, LegacyFactMappingV1)> {
    let source_store_id = compatibility_source_store_id()?;
    let identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Legacy {
            source_store_id: source_store_id.clone(),
            legacy_fact_id,
        },
    )?;
    let fact_id = FactId::derive(&identity)?;
    let mapping = LegacyFactMappingV1::new(
        owner.clone(),
        source_store_id,
        legacy_fact_id,
        fact_id,
        tracedecay_domain::LegacyHistoryCoverageV1::Complete,
        now,
    )?;
    Ok((identity, mapping))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compatibility_initial_batch(
    owner: &FactOwnerV1,
    identity: FactIdentityMaterialV1,
    mapping: LegacyFactMappingV1,
    payload: FactPayloadV1,
    access: PayloadAccessState,
    trust: Confidence,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let fact_id = mapping.fact_id().clone();
    let imported_at = project_memory_event_time(now, 0)?;
    let asserted_at = project_memory_event_time(now, 1)?;
    let assertion = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        FactAssertionKindV1::Initial,
        payload,
        Vec::new(),
        asserted_at,
        actor.clone(),
    )?;
    let mut events = vec![
        FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::LegacyImported {
                mapping: mapping.clone(),
            },
            imported_at,
            actor.clone(),
        )?,
        FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::AssertionRecorded {
                assertion_id: assertion.assertion_id().clone(),
            },
            asserted_at,
            actor.clone(),
        )?,
    ];
    let mut next_offset = 2;
    if access != PayloadAccessState::Eligible {
        events.push(FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: access,
            },
            project_memory_event_time(now, next_offset)?,
            actor.clone(),
        )?);
        next_offset += 1;
    }
    let default_trust = Confidence::new(DEFAULT_TRUST)?;
    if trust != default_trust {
        events.push(FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: default_trust,
                current: trust,
                evidence_ids: Vec::new(),
            },
            project_memory_event_time(now, next_offset)?,
            actor.clone(),
        )?);
    }
    FactWriteBatch::new(
        fact_id,
        owner.clone(),
        Some(assertion),
        events,
        Vec::new(),
        Vec::new(),
        Some(mapping),
        None,
    )?
    .with_identity_material(identity)
}

pub(in crate::store::memory) async fn compatibility_commit_batch_tx(
    transaction: &Transaction<'_>,
    batch: &FactWriteBatch,
) -> FactStoreResult<(FactCommitReceipt, bool)> {
    let attempt = commit_fact_tx(transaction, batch).await?;
    match attempt.outcome {
        FactCommitOutcome::Committed(receipt) | FactCommitOutcome::IdempotentReplay(receipt) => {
            Ok((receipt, attempt.wrote))
        }
        FactCommitOutcome::Conflict(conflict) => Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            format!("compatibility canonical write conflict: {conflict:?}"),
        )),
        _ => Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility canonical write returned an unsupported outcome",
        )),
    }
}

pub(super) async fn compatibility_active_fact_count_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
) -> FactStoreResult<u64> {
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT COUNT(*) FROM memory_v2_current_facts AS current_facts
             JOIN memory_v2_facts AS facts
               ON facts.fact_id = current_facts.fact_id
              AND facts.owner_kind = current_facts.owner_kind
              AND facts.project_id = current_facts.project_id
             WHERE current_facts.owner_kind = ?1 AND current_facts.project_id = ?2
               AND facts.owner_json = ?3 AND current_facts.active_assertion_id IS NOT NULL",
            params![key.kind, key.project_id.as_str(), key.json.as_str()],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility count is missing",
            )
        })?;
    nonnegative_u64(
        row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?,
        "active fact count",
    )
}

pub(in crate::store::memory) async fn compatibility_mirror_delete_tx(
    transaction: &Transaction<'_>,
    legacy_fact_id: i64,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT entity_id FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut entity_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        entity_ids.push(row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?);
    }
    drop(rows);
    transaction
        .execute(
            "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "DELETE FROM memory_facts WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    for entity_id in entity_ids {
        transaction
            .execute(
                "DELETE FROM memory_entities
                 WHERE entity_id = ?1
                   AND NOT EXISTS(
                     SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
                   )",
                params![entity_id],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    }
    Ok(())
}
