use std::collections::BTreeSet;

use serde_json::{Value, json};
use tracedecay_domain::{
    Confidence, FactAssertionKindV1, FactAssertionV1, FactId, FactIdentityMaterialV1,
    FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1,
    LegacyFactMappingV1, LegacyHistoryCoverageV1, PayloadAccessState, RetentionClass,
    SanitizerDispositionV1, SourceStoreId, UtcMicros,
};

use crate::db::engine::params;
use crate::errors::Result;
use crate::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};
use crate::tracedecay::current_timestamp;

use super::super::types::{
    LegacyFact, LegacyFactTelemetry, MemoryV2BackfillBatchOutcome, OwnerKey, Progress,
};
use super::super::writers::{
    ensure_current, insert_assertion, insert_event, insert_fact_identity, insert_mapping,
    mark_memory_v2_compatibility_bank_dirty_in_transaction, quarantine_fact, update_current,
};
use super::super::{
    MemoryV2Executor, OPERATION, RETENTION_CLASS, category_label, current_fact_state, db_error,
    db_message, json_text, load_legacy_entities, load_legacy_entity_ids, optional_i64,
    optional_string, parse_category, seconds_to_micros, update_cursor, update_phase, value_strings,
};

pub(in crate::db) async fn backfill_fact_batch(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    progress: &Progress,
    limit: i64,
) -> Result<MemoryV2BackfillBatchOutcome> {
    let mut rows = conn
        .query(
            "SELECT fact_id, content, category, tags, trust_score, source, metadata, updated_at,
                    retrieval_count, access_count, helpful_count, unhelpful_count
             FROM memory_facts
             WHERE fact_id > ?1 AND fact_id <= ?2 ORDER BY fact_id LIMIT ?3",
            params![progress.fact_cursor, progress.fact_frontier, limit],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    let mut batch = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| db_error(OPERATION, error))?
    {
        batch.push(LegacyFact {
            fact_id: row.get(0).map_err(|error| db_error(OPERATION, error))?,
            content: row.get(1).map_err(|error| db_error(OPERATION, error))?,
            category: row.get(2).map_err(|error| db_error(OPERATION, error))?,
            tags_json: row.get(3).map_err(|error| db_error(OPERATION, error))?,
            trust_score: row.get(4).map_err(|error| db_error(OPERATION, error))?,
            source: row.get(5).map_err(|error| db_error(OPERATION, error))?,
            metadata_json: row.get(6).map_err(|error| db_error(OPERATION, error))?,
            updated_at: row.get(7).map_err(|error| db_error(OPERATION, error))?,
            telemetry: LegacyFactTelemetry {
                retrieval_count: row.get(8).map_err(|error| db_error(OPERATION, error))?,
                access_count: row.get(9).map_err(|error| db_error(OPERATION, error))?,
                helpful_count: row.get(10).map_err(|error| db_error(OPERATION, error))?,
                unhelpful_count: row.get(11).map_err(|error| db_error(OPERATION, error))?,
            },
        });
    }
    if batch.is_empty() {
        update_phase(conn, owner_key, source_store_id, "awaiting_cutover").await?;
        return Ok(MemoryV2BackfillBatchOutcome::AwaitingCutover);
    }
    for legacy in &batch {
        let fact_id = ensure_legacy_identity(
            conn,
            owner,
            owner_key,
            source_store_id,
            legacy.fact_id,
            progress.started_at,
        )
        .await?;
        if let Err(reason) = backfill_fact_payload(
            conn,
            owner,
            owner_key,
            source_store_id,
            &fact_id,
            legacy,
            progress.started_at,
        )
        .await?
        {
            quarantine_fact(
                conn,
                owner,
                owner_key,
                source_store_id,
                &fact_id,
                legacy.fact_id,
                reason,
                progress.started_at,
            )
            .await?;
        }
    }
    let cursor = batch
        .last()
        .map_or(progress.fact_cursor, |item| item.fact_id);
    update_cursor(conn, owner_key, source_store_id, "fact_cursor", cursor).await?;
    Ok(MemoryV2BackfillBatchOutcome::Advanced {
        processed: batch.len(),
    })
}

#[allow(clippy::too_many_arguments)]
async fn backfill_fact_payload(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    fact_id: &FactId,
    legacy: &LegacyFact,
    recorded_at: i64,
) -> Result<std::result::Result<(), &'static str>> {
    let Some(asserted_at) = seconds_to_micros(legacy.updated_at) else {
        return Ok(Err("invalid_fact_timestamp"));
    };
    let Ok(trust) = Confidence::new(legacy.trust_score) else {
        return Ok(Err("invalid_fact_trust"));
    };
    let Ok(category) = parse_category(&legacy.category) else {
        return Ok(Err("invalid_fact_category"));
    };
    let Ok(mut tags) = serde_json::from_str::<Vec<String>>(&legacy.tags_json) else {
        return Ok(Err("invalid_fact_tags"));
    };
    let Ok(metadata) = serde_json::from_str::<Value>(&legacy.metadata_json) else {
        return Ok(Err("invalid_fact_metadata"));
    };
    let mut entities = load_legacy_entities(conn, legacy.fact_id).await?;
    tags.sort_unstable();
    entities.sort_unstable();
    let original = json!({
        "content": legacy.content,
        "category": category_label(category),
        "tags": tags,
        "entities": entities,
        "metadata": metadata
    });
    let sanitized = sanitize_memory_fact_payload(original)
        .map_err(|_| db_message(OPERATION, "fact privacy sanitizer failed"))?;
    let MemoryFactSanitizationV1::Durable { payload, receipt } = sanitized else {
        return Ok(Err("fact_payload_quarantined"));
    };
    let Some(source) = sanitize_provider_metadata_text(&legacy.source) else {
        return Ok(Err("fact_source_quarantined"));
    };
    let Some(content) = payload.get("content").and_then(Value::as_str) else {
        return Ok(Err("sanitized_fact_content_invalid"));
    };
    let Some(tags) = payload.get("tags").and_then(value_strings) else {
        return Ok(Err("sanitized_fact_tags_invalid"));
    };
    let Some(entities) = payload.get("entities").and_then(value_strings) else {
        return Ok(Err("sanitized_fact_entities_invalid"));
    };
    let Some(metadata) = payload.get("metadata").cloned() else {
        return Ok(Err("sanitized_fact_metadata_invalid"));
    };
    let Ok(retention) = RetentionClass::new(RETENTION_CLASS) else {
        return Err(db_message(
            OPERATION,
            "retention class configuration is invalid",
        ));
    };
    let Ok(fact_payload) = FactPayloadV1::new(
        content.to_owned(),
        category,
        tags.clone(),
        entities.clone(),
        metadata.clone(),
        receipt,
        retention,
    ) else {
        return Ok(Err("sanitized_fact_contract_invalid"));
    };
    let payload_reference = fact_payload
        .payload_reference()
        .map_err(|_| db_message(OPERATION, "typed payload reference construction failed"))?;
    let current = current_fact_state(conn, owner_key, fact_id).await?;
    let assertion_kind = match current.active_assertion_id.as_ref() {
        Some(_) if current.active_payload_reference.as_ref() == Some(&payload_reference) => current
            .active_kind
            .clone()
            .ok_or_else(|| db_message(OPERATION, "active assertion kind is missing"))?,
        Some(active) => FactAssertionKindV1::Correction {
            supersedes: active.clone(),
        },
        None => FactAssertionKindV1::LegacyImport,
    };
    let Ok(assertion) = FactAssertionV1::new(
        fact_id.clone(),
        owner.clone(),
        assertion_kind,
        fact_payload,
        Vec::new(),
        asserted_at,
        None,
    ) else {
        return Ok(Err("typed_assertion_invalid"));
    };
    insert_assertion(conn, owner_key, &assertion).await?;
    let assertion_event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        asserted_at,
        None,
    )
    .map_err(|_| db_message(OPERATION, "typed assertion event construction failed"))?;
    insert_event(conn, owner_key, &assertion_event, recorded_at).await?;
    let access = match assertion.payload().receipt().disposition() {
        SanitizerDispositionV1::Accepted => PayloadAccessState::Eligible,
        SanitizerDispositionV1::Redacted => PayloadAccessState::Redacted,
        SanitizerDispositionV1::Rejected | SanitizerDispositionV1::Quarantined => {
            return Ok(Err("durable_receipt_disposition_invalid"));
        }
    };
    let last_event_id = if current.access == access {
        assertion_event.event_id().clone()
    } else {
        let access_event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: current.access,
                current: access,
            },
            asserted_at,
            None,
        )
        .map_err(|_| db_message(OPERATION, "typed payload access event construction failed"))?;
        insert_event(conn, owner_key, &access_event, recorded_at).await?;
        access_event.event_id().clone()
    };
    update_current(
        conn,
        owner_key,
        fact_id,
        Some((assertion.assertion_id(), access)),
        Some(trust.as_f64()),
        &last_event_id,
        asserted_at.0,
    )
    .await?;
    merge_legacy_fact_telemetry(conn, owner_key, fact_id, &legacy.telemetry).await?;
    // Live commits mark the affected HRR banks dirty so the daemon repair
    // pass rebuilds them; imported facts need the same marks or a migrated
    // store reports zero banks until every category sees a fresh write.
    for bank_name in ["all", category_label(category)] {
        mark_memory_v2_compatibility_bank_dirty_in_transaction(
            conn,
            owner,
            source_store_id,
            bank_name,
            UtcMicros(recorded_at),
        )
        .await?;
    }
    mirror_sanitized_legacy(
        conn,
        SanitizedLegacyMirror {
            legacy_fact_id: legacy.fact_id,
            content,
            category: category_label(category),
            tags: &tags,
            metadata: &metadata,
            entities: &entities,
            source: &source,
            invalidate_vector: access == PayloadAccessState::Redacted,
        },
    )
    .await?;
    Ok(Ok(()))
}

async fn existing_legacy_mapping_fact_id(
    conn: &impl MemoryV2Executor,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    legacy_fact_id: i64,
) -> Result<Option<String>> {
    optional_string(
        conn,
        "SELECT fact_id FROM memory_v2_legacy_map
         WHERE owner_kind = ?1 AND project_id = ?2 AND source_store_id = ?3
           AND legacy_fact_id = ?4",
        params![
            owner_key.kind,
            owner_key.project_id.as_str(),
            source_store_id.as_str(),
            legacy_fact_id
        ],
    )
    .await
}

pub(in crate::db) async fn ensure_legacy_identity(
    conn: &impl MemoryV2Executor,
    owner: &FactOwnerV1,
    owner_key: &OwnerKey,
    source_store_id: &SourceStoreId,
    legacy_fact_id: i64,
    migrated_at: i64,
) -> Result<FactId> {
    let material = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Legacy {
            source_store_id: source_store_id.clone(),
            legacy_fact_id,
        },
    )
    .map_err(|_| db_message(OPERATION, "typed legacy identity construction failed"))?;
    let fact_id = FactId::derive(&material)
        .map_err(|_| db_message(OPERATION, "typed fact identity derivation failed"))?;
    let identity_json = json_text(&material)?;
    // A compatibility write may have already imported this legacy fact; its
    // mapping carries different import attributes (Complete/now versus the
    // backfill's Unknown/started_at), which insert_mapping tolerates as a
    // replay. The first importer already recorded the import event, so a
    // second one must not be appended.
    let mapping_existed =
        existing_legacy_mapping_fact_id(conn, owner_key, source_store_id, legacy_fact_id)
            .await?
            .is_some();
    insert_fact_identity(conn, owner_key, &fact_id, &identity_json, migrated_at).await?;
    let mapping = LegacyFactMappingV1::new(
        owner.clone(),
        source_store_id.clone(),
        legacy_fact_id,
        fact_id.clone(),
        LegacyHistoryCoverageV1::Unknown,
        UtcMicros(migrated_at),
    )
    .map_err(|_| db_message(OPERATION, "typed legacy mapping construction failed"))?;
    insert_mapping(conn, owner_key, &mapping).await?;
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::LegacyImported { mapping },
        UtcMicros(migrated_at),
        None,
    )
    .map_err(|_| db_message(OPERATION, "typed legacy import event construction failed"))?;
    if !mapping_existed {
        insert_event(conn, owner_key, &event, migrated_at).await?;
    }
    ensure_current(conn, owner_key, &fact_id, event.event_id(), migrated_at).await?;
    Ok(fact_id)
}

/// Merges legacy usage counters into the canonical projection with
/// take-the-maximum semantics: counters only grow, so replaying a crashed
/// backfill batch is idempotent and live retrievals or feedback recorded
/// mid-cutover are never rolled back. The legacy `last_*` recency timestamps
/// are deliberately not carried: a migrated fact's canonical `created_at` is
/// its migration time, and `CompatibilityFactTelemetryV1` rejects recency
/// timestamps earlier than creation, so historical values can never validate.
async fn merge_legacy_fact_telemetry(
    conn: &impl MemoryV2Executor,
    owner: &OwnerKey,
    fact_id: &FactId,
    telemetry: &LegacyFactTelemetry,
) -> Result<()> {
    conn.execute(
        "UPDATE memory_v2_current_facts SET
            retrieval_count = MAX(retrieval_count, ?1),
            access_count = MAX(access_count, ?2),
            helpful_count = MAX(helpful_count, ?3),
            unhelpful_count = MAX(unhelpful_count, ?4)
         WHERE fact_id = ?5 AND owner_kind = ?6 AND project_id = ?7",
        params![
            telemetry.retrieval_count.max(0),
            telemetry.access_count.max(0),
            telemetry.helpful_count.max(0),
            telemetry.unhelpful_count.max(0),
            fact_id.as_str(),
            owner.kind,
            owner.project_id.as_str()
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    Ok(())
}

struct SanitizedLegacyMirror<'a> {
    legacy_fact_id: i64,
    content: &'a str,
    category: &'a str,
    tags: &'a [String],
    metadata: &'a Value,
    entities: &'a [String],
    source: &'a str,
    invalidate_vector: bool,
}

async fn mirror_sanitized_legacy(
    conn: &impl MemoryV2Executor,
    mirror: SanitizedLegacyMirror<'_>,
) -> Result<()> {
    let SanitizedLegacyMirror {
        legacy_fact_id,
        content,
        category,
        tags,
        metadata,
        entities,
        source,
        invalidate_vector,
    } = mirror;
    conn.execute(
        "UPDATE memory_facts SET
            content = ?1, category = ?2, tags = ?3, metadata = ?4, source = ?5,
            hrr_vector = CASE WHEN ?6 THEN NULL ELSE hrr_vector END
         WHERE fact_id = ?7",
        params![
            content,
            category,
            json_text(tags)?,
            json_text(metadata)?,
            source,
            i64::from(invalidate_vector),
            legacy_fact_id
        ],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    rewrite_legacy_entity_links(conn, legacy_fact_id, entities).await?;
    if invalidate_vector {
        conn.execute(
            "INSERT INTO memory_bank_dirty(bank_name, updated_at)
             SELECT bank_name, ?1 FROM memory_banks
             WHERE 1
             ON CONFLICT(bank_name) DO UPDATE SET updated_at = excluded.updated_at",
            params![current_timestamp()],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
        conn.execute("DELETE FROM memory_banks", ())
            .await
            .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}

async fn rewrite_legacy_entity_links(
    conn: &impl MemoryV2Executor,
    legacy_fact_id: i64,
    entities: &[String],
) -> Result<()> {
    let old_ids = load_legacy_entity_ids(conn, legacy_fact_id, OPERATION).await?;
    conn.execute(
        "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
        params![legacy_fact_id],
    )
    .await
    .map_err(|error| db_error(OPERATION, error))?;
    let mut seen = BTreeSet::new();
    for entity in entities {
        let name = crate::memory::entities::normalize_entity(entity);
        let normalized = name.to_ascii_lowercase();
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            continue;
        }
        let entity_id = if let Some(id) = optional_i64(
            conn,
            "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
            params![normalized.as_str()],
        )
        .await?
        {
            id
        } else {
            conn.execute(
                "INSERT INTO memory_entities(
                    name, normalized_name, entity_type, aliases, created_at, updated_at
                 ) VALUES(?1, ?2, 'unknown', '[]', ?3, ?3)",
                params![name.as_str(), normalized.as_str(), current_timestamp()],
            )
            .await
            .map_err(|error| db_error(OPERATION, error))?;
            optional_i64(
                conn,
                "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                params![normalized.as_str()],
            )
            .await?
            .ok_or_else(|| db_message(OPERATION, "sanitized entity insert was not visible"))?
        };
        conn.execute(
            "INSERT INTO memory_fact_entities(fact_id, entity_id) VALUES(?1, ?2)",
            params![legacy_fact_id, entity_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    for entity_id in old_ids {
        conn.execute(
            "DELETE FROM memory_entities
             WHERE entity_id = ?1
               AND NOT EXISTS(
                   SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1
               )",
            params![entity_id],
        )
        .await
        .map_err(|error| db_error(OPERATION, error))?;
    }
    Ok(())
}
