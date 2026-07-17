//! Compatibility fact CRUD, canonical commit path, mirror writes, feedback, and proposal promotion.

use std::collections::BTreeSet;

use crate::db::Database;
use crate::memory::encoding::HolographicEncoder;
use crate::memory::entities::normalize_entity;
use crate::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, sanitize_provider_metadata_text,
};

use libsql::{Transaction, params};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use tracedecay_domain::{
    ActorId, Confidence, FactAssertionId, FactAssertionKindV1, FactAssertionV1, FactCategoryV1,
    FactCurationActionV1, FactEventId, FactEvidenceId, FactId, FactIdentityMaterialV1,
    FactIdentitySourceV1, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1,
    LegacyFactMappingV1, LocatorDigest, PayloadAccessState, ProvenanceId, RetentionClass,
    RetrievalAnchorId, RetrievalAnchorRecordV2, SanitizerDispositionV1, UtcMicros,
};
use tracedecay_store::{
    CompatibilityFactAddCommandV1, CompatibilityFactAddDispositionV1,
    CompatibilityFactAddOutcomeV1, CompatibilityFactContentDigestQueryV1,
    CompatibilityFactFeedbackActionV1, CompatibilityFactFeedbackCommandV1,
    CompatibilityFactFeedbackDetailsAvailabilityV1, CompatibilityFactFeedbackHistoryEntryV1,
    CompatibilityFactFeedbackHistoryQueryV1, CompatibilityFactFeedbackHistoryV1,
    CompatibilityFactFeedbackOutcomeV1, CompatibilityFactHistoryQueryV1,
    CompatibilityFactHistoryV1, CompatibilityFactIdV1, CompatibilityFactInspectionV1,
    CompatibilityFactListQueryV1, CompatibilityFactPageV1, CompatibilityFactProjectionV1,
    CompatibilityFactProposalPromotionDispositionV1, CompatibilityFactProposalPromotionResultV1,
    CompatibilityFactProposalPromotionV1, CompatibilityFactProposalRecordV1,
    CompatibilityFactProposalStateV1, CompatibilityFactRemoveCommandV1,
    CompatibilityFactRemoveOutcomeV1, CompatibilityFactTargetV1, CompatibilityFactUpdateCommandV1,
    CompatibilityFactUpdateOutcomeV1, CompatibilityFeedbackRepairProgressV1, CurrentFactsQuery,
    FactAsOfQuery, FactCommitConflict, FactCommitOutcome, FactCommitReceipt,
    FactCompatibilityResult, FactLineageCursor, FactLineageQuery, FactProposalPromotionStateV1,
    FactProposalStoreError, FactStoreError, FactStoreResult, FactWriteBatch, PromoteFactProposal,
    PromoteFactProposalOutcome, RetrievalAnchorQuery, StoredFactV1,
};

use super::DatabaseFactStore;
use super::envelope::{
    CompatibilityOperationReceiptV1, compatibility_digest,
    compatibility_lookup_operation_receipt_tx, compatibility_receipt_u64,
    compatibility_record_operation_receipt_tx, compatibility_target_digest,
};
use super::primitives::{
    COMMIT_OPERATION, COMPATIBILITY_READ_OPERATION, COMPATIBILITY_WRITE_OPERATION, OwnerKey,
    QUERY_OPERATION, authority_storage_error, compatibility_category_label,
    compatibility_event_time, compatibility_legacy_timestamp, compatibility_now,
    compatibility_source_label, compatibility_source_store_id, from_json, identity_collision,
    nonnegative_u64, parse_payload_access, payload_access_label, requires_payload_purge,
    row_exists, row_exists_params, row_f64, row_i64, row_optional_f64, row_optional_string,
    row_string, storage_error, storage_message, to_json,
};
use super::projection::{
    compatibility_fact_for_legacy_id_tx, compatibility_fact_status_tx,
    compatibility_projection_metadata_tx, compatibility_required_mapping_tx,
    compatibility_source_for_fact_tx, load_compatibility_projection_tx,
    resolve_compatibility_target_tx,
};
use super::proposals::{
    compatibility_advance_proposal_tx, compatibility_proposal_action_id,
    compatibility_proposal_record_tx, compatibility_replay_proposal_tx,
};
use super::scoring::compatibility_millionths;

pub(super) const PROMOTE_OPERATION: &str = "promote canonical memory proposal";

pub(super) const DEFAULT_TRUST: f64 = 0.5;

const COMPATIBILITY_RETENTION_CLASS: &str = "compatibility-runtime-v1";

pub(super) async fn list_compatibility_facts_tx(
    transaction: &Transaction,
    query: &CompatibilityFactListQueryV1,
) -> FactCompatibilityResult<CompatibilityFactPageV1> {
    let key = OwnerKey::new(query.owner())?;
    let category = query.category().map(compatibility_category_label);
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
    let mut facts = Vec::with_capacity(fact_ids.len());
    for fact_id in fact_ids {
        if let Some(fact) =
            load_compatibility_projection_tx(transaction, query.owner(), &fact_id).await?
        {
            facts.push(fact);
        }
    }
    let next = has_more
        .then(|| facts.last().map(|fact| fact.fact_id().clone()))
        .flatten();
    CompatibilityFactPageV1::new(query.owner().clone(), facts, next).map_err(Into::into)
}

pub(super) async fn get_compatibility_fact_tx(
    transaction: &Transaction,
    target: &CompatibilityFactTargetV1,
) -> FactCompatibilityResult<Option<CompatibilityFactProjectionV1>> {
    let Some(fact_id) = resolve_compatibility_target_tx(transaction, target).await? else {
        return Ok(None);
    };
    load_compatibility_projection_tx(transaction, target.owner(), &fact_id)
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

pub(super) async fn find_compatibility_fact_by_content_digest_tx(
    transaction: &Transaction,
    query: &CompatibilityFactContentDigestQueryV1,
) -> FactCompatibilityResult<Option<CompatibilityFactProjectionV1>> {
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
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut matching_fact_id = None;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let payload = from_json::<FactPayloadV1>(
            &row_string(&row, 1, COMPATIBILITY_READ_OPERATION)?,
            COMPATIBILITY_READ_OPERATION,
        )?;
        if compatibility_content_digest(payload.content())? == *query.content_digest() {
            matching_fact_id = Some(
                FactId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                    .map_err(FactStoreError::from)?,
            );
            break;
        }
    }
    drop(rows);
    match matching_fact_id {
        Some(fact_id) => load_compatibility_projection_tx(transaction, query.owner(), &fact_id)
            .await
            .map_err(Into::into),
        None => Ok(None),
    }
}

pub(super) async fn compatibility_fact_history_tx(
    transaction: &Transaction,
    query: &CompatibilityFactHistoryQueryV1,
) -> FactCompatibilityResult<CompatibilityFactHistoryV1> {
    let fact_id = resolve_compatibility_target_tx(transaction, query.target())
        .await?
        .ok_or_else(|| storage_message(QUERY_OPERATION, "compatibility fact target is missing"))?;
    let lineage = FactLineageQuery::new(
        query.target().owner().clone(),
        fact_id.clone(),
        query.after().cloned(),
        query.limit(),
    )?;
    let events = query_fact_lineage_tx(transaction, &lineage).await?;
    CompatibilityFactHistoryV1::new(query.target().owner().clone(), fact_id, events, None)
        .map_err(Into::into)
}

pub(super) struct CompatibilitySanitizedPayload {
    pub(super) payload: FactPayloadV1,
    pub(super) access: PayloadAccessState,
}

pub(super) fn compatibility_value_strings(
    value: &Value,
    field: &'static str,
) -> FactStoreResult<Vec<String>> {
    let values = value.as_array().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            format!("sanitized compatibility {field} is not an array"),
        )
    })?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    format!("sanitized compatibility {field} contains a non-string"),
                )
            })
        })
        .collect()
}

pub(super) fn compatibility_payload_metadata(metadata: &Value) -> Value {
    let mut metadata = metadata.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.remove("automation_run_id");
    }
    metadata
}

pub(super) fn compatibility_sanitize_payload(
    content: &str,
    category: FactCategoryV1,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
) -> FactStoreResult<Option<CompatibilitySanitizedPayload>> {
    let metadata = compatibility_payload_metadata(metadata);
    let sanitized = sanitize_memory_fact_payload(json!({
        "content": content,
        "category": compatibility_category_label(category),
        "tags": tags,
        "entities": entities,
        "metadata": metadata,
    }))
    .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let MemoryFactSanitizationV1::Durable { payload, receipt } = sanitized else {
        return Ok(None);
    };
    let content = payload
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "sanitized compatibility content is missing",
            )
        })?
        .to_owned();
    let tags = compatibility_value_strings(
        payload.get("tags").ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "sanitized compatibility tags are missing",
            )
        })?,
        "tags",
    )?;
    let entities = compatibility_value_strings(
        payload.get("entities").ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "sanitized compatibility entities are missing",
            )
        })?,
        "entities",
    )?;
    let metadata = payload.get("metadata").cloned().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "sanitized compatibility metadata is missing",
        )
    })?;
    let retention = RetentionClass::new(COMPATIBILITY_RETENTION_CLASS.to_owned())
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
                COMPATIBILITY_WRITE_OPERATION,
                "durable compatibility payload has a non-durable receipt disposition",
            ));
        }
    };
    Ok(Some(CompatibilitySanitizedPayload {
        payload: fact_payload,
        access,
    }))
}

pub(super) fn compatibility_mirror_vector(payload: &FactPayloadV1) -> FactStoreResult<Vec<u8>> {
    let encoder = HolographicEncoder::new();
    HolographicEncoder::serialize(&encoder.encode_fact(payload.content(), payload.entities()))
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))
}

async fn compatibility_last_insert_rowid_tx(transaction: &Transaction) -> FactStoreResult<i64> {
    let mut rows = transaction
        .query("SELECT last_insert_rowid()", ())
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility last_insert_rowid returned no row",
            )
        })?;
    row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)
}

pub(super) async fn compatibility_mark_owner_banks_dirty_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    category: FactCategoryV1,
    updated_at: UtcMicros,
) -> FactStoreResult<()> {
    let source_store_id = compatibility_source_store_id()?;
    for bank_name in ["all", compatibility_category_label(category)] {
        db.mark_memory_v2_compatibility_bank_dirty_in_transaction(
            transaction,
            owner,
            &source_store_id,
            bank_name,
            updated_at,
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

async fn compatibility_mirror_replace_entities_tx(
    transaction: &Transaction,
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
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut old_entity_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        old_entity_ids.push(row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?);
    }
    drop(rows);
    transaction
        .execute(
            "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
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
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        let entity_id = match existing
            .next()
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        {
            Some(row) => row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
            None => {
                drop(existing);
                transaction
                    .execute(
                        "INSERT INTO memory_entities(
                            name, normalized_name, entity_type, aliases, created_at, updated_at
                         ) VALUES(?1, ?2, 'unknown', '[]', ?3, ?3)",
                        params![name.as_str(), key.as_str(), timestamp],
                    )
                    .await
                    .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
                compatibility_last_insert_rowid_tx(transaction).await?
            }
        };
        transaction
            .execute(
                "INSERT OR IGNORE INTO memory_fact_entities(fact_id, entity_id)
                 VALUES(?1, ?2)",
                params![legacy_fact_id, entity_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
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
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

enum CompatibilityMirrorInsertV1 {
    Inserted(i64),
    Existing { fact_id: FactId },
}

async fn compatibility_mirror_insert_tx(
    db: &Database,
    transaction: &Transaction,
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
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if let Some(row) = existing
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        let legacy_fact_id = row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?;
        let Some(fact_id) =
            compatibility_fact_for_legacy_id_tx(transaction, owner, legacy_fact_id).await?
        else {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility mirror content is already bound to another owner or an unmigrated row",
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
                compatibility_category_label(payload.category()),
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
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let legacy_fact_id = compatibility_last_insert_rowid_tx(transaction).await?;
    compatibility_mirror_replace_entities_tx(
        transaction,
        legacy_fact_id,
        payload.entities(),
        timestamp,
    )
    .await?;
    compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, payload.category(), now)
        .await?;
    Ok(CompatibilityMirrorInsertV1::Inserted(legacy_fact_id))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn compatibility_mirror_update_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
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
                compatibility_category_label(payload.category()),
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
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    compatibility_mirror_replace_entities_tx(
        transaction,
        legacy_fact_id,
        payload.entities(),
        timestamp,
    )
    .await?;
    compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, payload.category(), now).await
}

fn compatibility_legacy_mapping_for_new_fact(
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
fn compatibility_initial_batch(
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
    let imported_at = compatibility_event_time(now, 0)?;
    let asserted_at = compatibility_event_time(now, 1)?;
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
            compatibility_event_time(now, next_offset)?,
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
            compatibility_event_time(now, next_offset)?,
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

pub(super) async fn compatibility_commit_batch_tx(
    transaction: &Transaction,
    batch: &FactWriteBatch,
) -> FactStoreResult<(FactCommitReceipt, bool)> {
    let attempt = commit_fact_tx(transaction, batch).await?;
    match attempt.outcome {
        FactCommitOutcome::Committed(receipt) | FactCommitOutcome::IdempotentReplay(receipt) => {
            Ok((receipt, attempt.wrote))
        }
        FactCommitOutcome::Conflict(conflict) => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            format!("compatibility canonical write conflict: {conflict:?}"),
        )),
        _ => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility canonical write returned an unsupported outcome",
        )),
    }
}

async fn compatibility_active_fact_count_tx(
    transaction: &Transaction,
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
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility count is missing",
            )
        })?;
    nonnegative_u64(
        row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
        "active fact count",
    )
}

pub(super) async fn compatibility_mirror_delete_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    legacy_fact_id: i64,
    category: FactCategoryV1,
    now: UtcMicros,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT entity_id FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut entity_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        entity_ids.push(row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?);
    }
    drop(rows);
    transaction
        .execute(
            "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "DELETE FROM memory_facts WHERE fact_id = ?1",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
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
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, category, now).await
}

fn compatibility_feedback_action_label(action: CompatibilityFactFeedbackActionV1) -> &'static str {
    match action {
        CompatibilityFactFeedbackActionV1::Helpful => "helpful",
        CompatibilityFactFeedbackActionV1::Unhelpful => "unhelpful",
    }
}

fn compatibility_feedback_delta(action: CompatibilityFactFeedbackActionV1) -> f64 {
    match action {
        CompatibilityFactFeedbackActionV1::Helpful => 0.05,
        CompatibilityFactFeedbackActionV1::Unhelpful => -0.10,
    }
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_mirror_feedback_tx(
    transaction: &Transaction,
    legacy_fact_id: i64,
    action: CompatibilityFactFeedbackActionV1,
    old_trust: Confidence,
    new_trust: Confidence,
    timestamp: i64,
    source: &str,
    note: Option<&str>,
) -> FactStoreResult<i64> {
    let changed = transaction
        .execute(
            "UPDATE memory_facts SET
                trust_score = ?1,
                helpful_count = helpful_count + ?2,
                unhelpful_count = unhelpful_count + ?3,
                last_feedback_at = ?4,
                updated_at = ?4
             WHERE fact_id = ?5",
            params![
                new_trust.as_f64(),
                i64::from(matches!(action, CompatibilityFactFeedbackActionV1::Helpful)),
                i64::from(matches!(
                    action,
                    CompatibilityFactFeedbackActionV1::Unhelpful
                )),
                timestamp,
                legacy_fact_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback target is missing from the legacy mirror",
        ));
    }
    transaction
        .execute(
            "INSERT INTO memory_feedback_events (
                fact_id, action, trust_delta, old_trust, new_trust,
                created_at, source, note
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                legacy_fact_id,
                compatibility_feedback_action_label(action),
                new_trust.as_f64() - old_trust.as_f64(),
                old_trust.as_f64(),
                new_trust.as_f64(),
                timestamp,
                source,
                note,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    compatibility_last_insert_rowid_tx(transaction).await
}

async fn compatibility_update_feedback_projection_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    action: CompatibilityFactFeedbackActionV1,
    timestamp: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_current_facts SET
                helpful_count = helpful_count + ?1,
                unhelpful_count = unhelpful_count + ?2,
                last_feedback_at = ?3
             WHERE fact_id = ?4 AND owner_kind = ?5 AND project_id = ?6",
            params![
                i64::from(matches!(action, CompatibilityFactFeedbackActionV1::Helpful)),
                i64::from(matches!(
                    action,
                    CompatibilityFactFeedbackActionV1::Unhelpful
                )),
                timestamp.0,
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback target has no current projection",
        ));
    }
    Ok(())
}

fn compatibility_correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
    access: PayloadAccessState,
    trust: Confidence,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let assertion = FactAssertionV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactAssertionKindV1::Correction {
            supersedes: fact.active_assertion_id().clone(),
        },
        payload,
        Vec::new(),
        now,
        actor.clone(),
    )?;
    let mut events = vec![FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        now,
        actor.clone(),
    )?];
    let mut offset = 1;
    if access != fact.payload_access() {
        events.push(FactLineageEventV1::new(
            fact.fact_id().clone(),
            fact.owner().clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: fact.payload_access(),
                current: access,
            },
            compatibility_event_time(now, offset)?,
            actor.clone(),
        )?);
        offset += 1;
    }
    if trust != fact.trust() {
        events.push(FactLineageEventV1::new(
            fact.fact_id().clone(),
            fact.owner().clone(),
            FactLineageEventKindV1::TrustChanged {
                previous: fact.trust(),
                current: trust,
                evidence_ids: Vec::new(),
            },
            compatibility_event_time(now, offset)?,
            actor,
        )?);
    }
    FactWriteBatch::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        Some(assertion),
        events,
        Vec::new(),
        Vec::new(),
        None,
        expected_last_event_id,
    )
}

fn compatibility_removal_batch(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    previous: PayloadAccessState,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous,
            current: PayloadAccessState::Deleted,
        },
        now,
        actor,
    )?;
    FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![event],
        Vec::new(),
        Vec::new(),
        None,
        expected_last_event_id,
    )
}

async fn compatibility_replay_add_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactAddOutcomeV1> {
    let outcome = receipt
        .receipt
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility add receipt is malformed",
            )
        })?;
    match outcome {
        "rejected_secret_like" => CompatibilityFactAddOutcomeV1::new(
            None,
            CompatibilityFactAddDispositionV1::RejectedSecretLike,
            None,
            None,
            receipt
                .receipt
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        )
        .map_err(Into::into),
        "added" | "near_duplicate" => {
            let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility add receipt fact is missing",
                )
            })?;
            let fact = load_compatibility_projection_tx(transaction, owner, fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "compatibility replay fact is missing",
                    )
                })?;
            let closest = if outcome == "near_duplicate" {
                Some(CompatibilityFactIdV1::new(owner.clone(), fact_id.clone())?)
            } else {
                None
            };
            CompatibilityFactAddOutcomeV1::new(
                Some(fact),
                if outcome == "added" {
                    CompatibilityFactAddDispositionV1::Added
                } else {
                    CompatibilityFactAddDispositionV1::NearDuplicate
                },
                closest,
                None,
                None,
            )
            .map_err(Into::into)
        }
        _ => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "unknown compatibility add receipt outcome",
        )
        .into()),
    }
}

pub(super) async fn add_compatibility_fact_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactAddCommandV1,
) -> FactCompatibilityResult<CompatibilityFactAddOutcomeV1> {
    let payload_metadata = compatibility_payload_metadata(request.metadata());
    let request_digest = compatibility_digest(json!({
        "owner": request.owner(),
        "content": request.content(),
        "category": compatibility_category_label(request.category()),
        "source": request.source(),
        "tags": request.tags(),
        "entities": request.entities(),
        "metadata": &payload_metadata,
        "automation_run_id": request.automation_run_id(),
        "default_trust": request.default_trust().as_f64(),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "add",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_add_tx(transaction, request.owner(), &receipt).await;
    }
    let now = compatibility_now()?;
    let Some(sanitized) = compatibility_sanitize_payload(
        request.content(),
        request.category(),
        request.tags(),
        request.entities(),
        &payload_metadata,
    )?
    else {
        let receipt = json!({
            "outcome": "rejected_secret_like",
            "reason": "content rejected by privacy sanitizer",
        });
        compatibility_record_operation_receipt_tx(
            transaction,
            request.owner(),
            request.operation_id(),
            "add",
            &request_digest,
            None,
            None,
            &receipt,
            now,
        )
        .await?;
        return CompatibilityFactAddOutcomeV1::new(
            None,
            CompatibilityFactAddDispositionV1::RejectedSecretLike,
            None,
            None,
            Some("content rejected by privacy sanitizer".to_owned()),
        )
        .map_err(Into::into);
    };
    let source = compatibility_source_label(request.source())?;
    match compatibility_mirror_insert_tx(
        db,
        transaction,
        request.owner(),
        &sanitized.payload,
        &source,
        request.default_trust(),
        now,
    )
    .await?
    {
        CompatibilityMirrorInsertV1::Existing { fact_id, .. } => {
            let fact = load_compatibility_projection_tx(transaction, request.owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "duplicate compatibility fact projection is missing",
                    )
                })?;
            let closest = CompatibilityFactIdV1::new(request.owner().clone(), fact_id.clone())?;
            let receipt = json!({ "outcome": "near_duplicate" });
            compatibility_record_operation_receipt_tx(
                transaction,
                request.owner(),
                request.operation_id(),
                "add",
                &request_digest,
                Some(&fact_id),
                None,
                &receipt,
                now,
            )
            .await?;
            CompatibilityFactAddOutcomeV1::new(
                Some(fact),
                CompatibilityFactAddDispositionV1::NearDuplicate,
                Some(closest),
                None,
                None,
            )
            .map_err(Into::into)
        }
        CompatibilityMirrorInsertV1::Inserted(legacy_fact_id) => {
            let (identity, mapping) =
                compatibility_legacy_mapping_for_new_fact(request.owner(), legacy_fact_id, now)?;
            let batch = compatibility_initial_batch(
                request.owner(),
                identity,
                mapping.clone(),
                sanitized.payload,
                sanitized.access,
                request.default_trust(),
                request.actor().cloned(),
                now,
            )?;
            let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
            let fact =
                load_compatibility_projection_tx(transaction, request.owner(), mapping.fact_id())
                    .await?
                    .ok_or_else(|| {
                        storage_message(
                            COMPATIBILITY_WRITE_OPERATION,
                            "added compatibility fact projection is missing",
                        )
                    })?;
            let receipt = json!({ "outcome": "added" });
            compatibility_record_operation_receipt_tx(
                transaction,
                request.owner(),
                request.operation_id(),
                "add",
                &request_digest,
                Some(mapping.fact_id()),
                Some(canonical_receipt.last_event_id()),
                &receipt,
                now,
            )
            .await?;
            CompatibilityFactAddOutcomeV1::new(
                Some(fact),
                CompatibilityFactAddDispositionV1::Added,
                None,
                None,
                None,
            )
            .map_err(Into::into)
        }
    }
}

async fn compatibility_replay_update_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactUpdateOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility update receipt fact is missing",
        )
    })?;
    let fact = load_compatibility_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility update replay fact is missing",
            )
        })?;
    let trust_delta_millionths = receipt
        .receipt
        .get("trust_delta_millionths")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility update receipt is malformed",
            )
        })?;
    CompatibilityFactUpdateOutcomeV1::new(fact, trust_delta_millionths).map_err(Into::into)
}

pub(super) async fn update_compatibility_fact_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactUpdateCommandV1,
) -> FactCompatibilityResult<CompatibilityFactUpdateOutcomeV1> {
    let request_digest = compatibility_digest(json!({
        "target": compatibility_target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "content": request.patch().content(),
        "category": request.patch().category().map(compatibility_category_label),
        "source": match request.patch().source() {
            None => json!({"changed": false}),
            Some(value) => json!({"changed": true, "value": value}),
        },
        "tags": request.patch().tags(),
        "entities": request.patch().entities(),
        "metadata": request.patch().metadata(),
        "trust": request.patch().trust().map(Confidence::as_f64),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "update",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_update_tx(transaction, request.target().owner(), &receipt)
            .await;
    }
    let fact_id = resolve_compatibility_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility update target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility update target is unavailable",
            )
        })?;
    let previous_payload = current
        .payload()
        .ok_or(FactStoreError::PayloadAccessMismatch)?;
    let content = request
        .patch()
        .content()
        .unwrap_or(previous_payload.content());
    let category = request
        .patch()
        .category()
        .unwrap_or(previous_payload.category());
    let tags = request.patch().tags().unwrap_or(previous_payload.tags());
    let entities = request
        .patch()
        .entities()
        .unwrap_or(previous_payload.entities());
    let metadata = request
        .patch()
        .metadata()
        .unwrap_or(previous_payload.metadata());
    let source = match request.patch().source() {
        Some(Some(source)) => compatibility_source_label(Some(source))?,
        Some(None) => "manual".to_owned(),
        None => {
            let mapping =
                compatibility_required_mapping_tx(transaction, request.target().owner(), &fact_id)
                    .await?;
            compatibility_source_for_fact_tx(transaction, &mapping).await?
        }
    };
    let Some(sanitized) =
        compatibility_sanitize_payload(content, category, tags, entities, metadata)?
    else {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility update payload was rejected by the privacy sanitizer",
        )
        .into());
    };
    let new_trust = request.patch().trust().unwrap_or(current.trust());
    let now = compatibility_now()?;
    let batch = compatibility_correction_batch(
        &current,
        sanitized.payload.clone(),
        sanitized.access,
        new_trust,
        request
            .expected_last_event_id()
            .cloned()
            .or_else(|| Some(current.last_event_id().clone())),
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
    let mapping =
        compatibility_required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
    compatibility_mirror_update_tx(
        db,
        transaction,
        request.target().owner(),
        mapping.legacy_fact_id(),
        &sanitized.payload,
        &source,
        new_trust,
        now,
    )
    .await?;
    let fact = load_compatibility_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "updated compatibility projection is missing",
            )
        })?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - current.trust().as_f64()) * 1_000_000.0).round() as i32;
    let receipt = json!({ "trust_delta_millionths": trust_delta_millionths });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "update",
        &request_digest,
        Some(&fact_id),
        Some(canonical_receipt.last_event_id()),
        &receipt,
        now,
    )
    .await?;
    CompatibilityFactUpdateOutcomeV1::new(fact, trust_delta_millionths).map_err(Into::into)
}

async fn compatibility_replay_remove_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactRemoveOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility remove receipt fact is missing",
        )
    })?;
    let fact = load_compatibility_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility remove replay fact is missing",
            )
        })?;
    let removed = receipt
        .receipt
        .get("removed")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility remove receipt is malformed",
            )
        })?;
    let remaining_fact_count = compatibility_active_fact_count_tx(transaction, owner).await?;
    Ok(CompatibilityFactRemoveOutcomeV1::new(
        fact,
        removed,
        remaining_fact_count,
    ))
}

pub(super) async fn remove_compatibility_fact_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactRemoveCommandV1,
) -> FactCompatibilityResult<CompatibilityFactRemoveOutcomeV1> {
    let request_digest = compatibility_digest(json!({
        "target": compatibility_target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "remove",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_remove_tx(transaction, request.target().owner(), &receipt)
            .await;
    }
    let now = compatibility_now()?;
    let fact_id = resolve_compatibility_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility remove target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_projection(transaction, &owner_key, &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility remove projection is missing",
            )
        })?;
    let removed = current.access != PayloadAccessState::Deleted;
    let event_id = if removed {
        let stored =
            load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "compatibility remove target is unavailable",
                    )
                })?;
        let category = stored
            .payload()
            .ok_or(FactStoreError::PayloadAccessMismatch)?
            .category();
        let mapping =
            compatibility_required_mapping_tx(transaction, request.target().owner(), &fact_id)
                .await?;
        let batch = compatibility_removal_batch(
            request.target().owner(),
            &fact_id,
            current.access,
            request
                .expected_last_event_id()
                .cloned()
                .or_else(|| current.last_event_id.clone()),
            request.actor().cloned(),
            now,
        )?;
        let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_delete_tx(
            db,
            transaction,
            request.target().owner(),
            mapping.legacy_fact_id(),
            category,
            now,
        )
        .await?;
        Some(canonical_receipt.last_event_id().clone())
    } else {
        None
    };
    let fact = load_compatibility_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "removed compatibility projection is missing",
            )
        })?;
    let remaining_fact_count =
        compatibility_active_fact_count_tx(transaction, request.target().owner()).await?;
    let receipt = json!({ "removed": removed });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "remove",
        &request_digest,
        Some(&fact_id),
        event_id.as_ref(),
        &receipt,
        now,
    )
    .await?;
    Ok(CompatibilityFactRemoveOutcomeV1::new(
        fact,
        removed,
        remaining_fact_count,
    ))
}

fn compatibility_receipt_i32(receipt: &Value, field: &'static str) -> FactStoreResult<i32> {
    receipt
        .get(field)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                format!("compatibility receipt {field} is malformed"),
            )
        })
}

fn compatibility_receipt_confidence(
    receipt: &Value,
    field: &'static str,
) -> FactStoreResult<Confidence> {
    let millionths = compatibility_receipt_u64(receipt, field)?;
    if millionths > 1_000_000 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            format!("compatibility receipt {field} is out of range"),
        ));
    }
    Confidence::new(millionths as f64 / 1_000_000.0).map_err(FactStoreError::from)
}

fn compatibility_feedback_detail(value: Option<&str>) -> Option<String> {
    value
        .and_then(sanitize_provider_metadata_text)
        .filter(|value| !value.trim().is_empty())
}

fn compatibility_feedback_details(
    source: Option<&str>,
    reason: Option<&str>,
) -> (
    String,
    Option<String>,
    Option<String>,
    CompatibilityFactFeedbackDetailsAvailabilityV1,
) {
    let persisted_source = match source {
        Some(source) => compatibility_feedback_detail(Some(source)),
        None => Some("mcp".to_owned()),
    };
    let persisted_note = compatibility_feedback_detail(reason);
    let details_available = reason.is_none() || persisted_note.is_some();
    if let Some(source) = persisted_source
        && details_available
    {
        (
            source.clone(),
            Some(source),
            persisted_note,
            CompatibilityFactFeedbackDetailsAvailabilityV1::Available,
        )
    } else {
        (
            "mcp".to_owned(),
            None,
            None,
            CompatibilityFactFeedbackDetailsAvailabilityV1::Unknown,
        )
    }
}

fn compatibility_feedback_batch(
    fact: &StoredFactV1,
    new_trust: Confidence,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let kind = if new_trust != fact.trust() {
        FactLineageEventKindV1::TrustChanged {
            previous: fact.trust(),
            current: new_trust,
            evidence_ids: Vec::new(),
        }
    } else {
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Retained,
            evidence_ids: Vec::new(),
        }
    };
    let event = FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        kind,
        now,
        actor,
    )?;
    FactWriteBatch::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        None,
        vec![event],
        Vec::new(),
        Vec::new(),
        None,
        expected_last_event_id,
    )
}

fn compatibility_feedback_details_label(
    availability: CompatibilityFactFeedbackDetailsAvailabilityV1,
) -> &'static str {
    match availability {
        CompatibilityFactFeedbackDetailsAvailabilityV1::Available => "available",
        CompatibilityFactFeedbackDetailsAvailabilityV1::LegacyRedacted => "legacy_redacted",
        CompatibilityFactFeedbackDetailsAvailabilityV1::Unknown => "unknown",
    }
}

fn compatibility_feedback_details_availability(
    value: &str,
) -> FactStoreResult<CompatibilityFactFeedbackDetailsAvailabilityV1> {
    match value {
        "available" => Ok(CompatibilityFactFeedbackDetailsAvailabilityV1::Available),
        "legacy_redacted" => Ok(CompatibilityFactFeedbackDetailsAvailabilityV1::LegacyRedacted),
        "unknown" => Ok(CompatibilityFactFeedbackDetailsAvailabilityV1::Unknown),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("unknown compatibility feedback detail availability {value:?}"),
        )),
    }
}

fn compatibility_feedback_action(
    value: &str,
) -> FactStoreResult<CompatibilityFactFeedbackActionV1> {
    match value {
        "helpful" => Ok(CompatibilityFactFeedbackActionV1::Helpful),
        "unhelpful" => Ok(CompatibilityFactFeedbackActionV1::Unhelpful),
        _ => Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            format!("unknown compatibility feedback action {value:?}"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_record_feedback_history_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    event_id: &FactEventId,
    legacy_feedback_event_id: i64,
    action: CompatibilityFactFeedbackActionV1,
    old_trust: Confidence,
    new_trust: Confidence,
    occurred_at: UtcMicros,
    source: Option<&str>,
    note: Option<&str>,
    availability: CompatibilityFactFeedbackDetailsAvailabilityV1,
) -> FactStoreResult<()> {
    if legacy_feedback_event_id <= 0 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility legacy feedback event id must be positive",
        ));
    }
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    transaction
        .execute(
            "INSERT INTO memory_v2_legacy_feedback_event_map(
                owner_kind, project_id, source_store_id, legacy_feedback_event_id, fact_id, event_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                key.kind,
                key.project_id.as_str(),
                source_store_id.as_str(),
                legacy_feedback_event_id,
                fact_id.as_str(),
                event_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    transaction
        .execute(
            "INSERT INTO memory_v2_feedback_history(
                owner_kind, project_id, fact_id, event_id, action, old_trust, new_trust,
                occurred_at, source, note, details_availability
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                key.kind,
                key.project_id.as_str(),
                fact_id.as_str(),
                event_id.as_str(),
                compatibility_feedback_action_label(action),
                old_trust.as_f64(),
                new_trust.as_f64(),
                occurred_at.0,
                source,
                note,
                compatibility_feedback_details_label(availability),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

async fn compatibility_replay_feedback_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactFeedbackOutcomeV1> {
    let fact_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback receipt fact is missing",
        )
    })?;
    let event_id = receipt.event_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback receipt event is missing",
        )
    })?;
    let fact = load_compatibility_projection_tx(transaction, owner, fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback replay fact is missing",
            )
        })?;
    let legacy_feedback_event_id = i64::try_from(compatibility_receipt_u64(
        &receipt.receipt,
        "legacy_feedback_event_id",
    )?)
    .map_err(|_| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility feedback receipt legacy event id is out of range",
        )
    })?;
    CompatibilityFactFeedbackOutcomeV1::new(
        fact,
        event_id.clone(),
        Some(legacy_feedback_event_id),
        compatibility_receipt_confidence(&receipt.receipt, "old_trust_millionths")?,
        compatibility_receipt_confidence(&receipt.receipt, "new_trust_millionths")?,
        compatibility_receipt_i32(&receipt.receipt, "trust_delta_millionths")?,
        compatibility_receipt_u64(&receipt.receipt, "helpful_count")?,
        compatibility_receipt_u64(&receipt.receipt, "unhelpful_count")?,
    )
    .map_err(Into::into)
}

pub(super) async fn record_compatibility_fact_feedback_tx(
    transaction: &Transaction,
    request: &CompatibilityFactFeedbackCommandV1,
) -> FactCompatibilityResult<CompatibilityFactFeedbackOutcomeV1> {
    let request_digest = compatibility_digest(json!({
        "target": compatibility_target_digest(request.target())?,
        "expected_last_event_id": request.expected_last_event_id().map(FactEventId::as_str),
        "action": compatibility_feedback_action_label(request.action()),
        "actor": request.actor().map(ActorId::as_str),
        "source": request.source(),
        "reason": request.reason(),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "feedback",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_feedback_tx(transaction, request.target().owner(), &receipt)
            .await;
    }
    let fact_id = resolve_compatibility_target_tx(transaction, request.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(request.target().owner())?;
    let current = load_current_fact_tx(transaction, &owner_key, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback target is unavailable",
            )
        })?;
    let old_trust = current.trust();
    let new_trust = Confidence::new(
        (old_trust.as_f64() + compatibility_feedback_delta(request.action())).clamp(0.0, 1.0),
    )
    .map_err(FactStoreError::from)?;
    let now = compatibility_now()?;
    let batch = compatibility_feedback_batch(
        &current,
        new_trust,
        request
            .expected_last_event_id()
            .cloned()
            .or_else(|| Some(current.last_event_id().clone())),
        request.actor().cloned(),
        now,
    )?;
    let (canonical_receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
    let event_id = canonical_receipt.last_event_id().clone();
    let mapping =
        compatibility_required_mapping_tx(transaction, request.target().owner(), &fact_id).await?;
    let (mirror_source, history_source, history_note, availability) =
        compatibility_feedback_details(request.source(), request.reason());
    let legacy_feedback_event_id = compatibility_mirror_feedback_tx(
        transaction,
        mapping.legacy_fact_id(),
        request.action(),
        old_trust,
        new_trust,
        compatibility_legacy_timestamp(now),
        &mirror_source,
        history_note.as_deref(),
    )
    .await?;
    compatibility_record_feedback_history_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        &event_id,
        legacy_feedback_event_id,
        request.action(),
        old_trust,
        new_trust,
        now,
        history_source.as_deref(),
        history_note.as_deref(),
        availability,
    )
    .await?;
    compatibility_update_feedback_projection_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        request.action(),
        now,
    )
    .await?;
    let fact = load_compatibility_projection_tx(transaction, request.target().owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility feedback projection is missing",
            )
        })?;
    let (_, _, telemetry) = compatibility_projection_metadata_tx(
        transaction,
        request.target().owner(),
        &fact_id,
        Some(&mapping),
    )
    .await?;
    let trust_delta_millionths =
        ((new_trust.as_f64() - old_trust.as_f64()) * 1_000_000.0).round() as i32;
    let receipt = json!({
        "old_trust_millionths": compatibility_millionths(old_trust.as_f64()),
        "new_trust_millionths": compatibility_millionths(new_trust.as_f64()),
        "trust_delta_millionths": trust_delta_millionths,
        "helpful_count": telemetry.helpful_count(),
        "unhelpful_count": telemetry.unhelpful_count(),
        "legacy_feedback_event_id": legacy_feedback_event_id,
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.target().owner(),
        request.operation_id(),
        "feedback",
        &request_digest,
        Some(&fact_id),
        Some(&event_id),
        &receipt,
        now,
    )
    .await?;
    CompatibilityFactFeedbackOutcomeV1::new(
        fact,
        event_id,
        Some(legacy_feedback_event_id),
        old_trust,
        new_trust,
        trust_delta_millionths,
        telemetry.helpful_count(),
        telemetry.unhelpful_count(),
    )
    .map_err(Into::into)
}

pub(super) async fn compatibility_fact_feedback_history_tx(
    transaction: &Transaction,
    query: &CompatibilityFactFeedbackHistoryQueryV1,
    repair_progress: CompatibilityFeedbackRepairProgressV1,
) -> FactCompatibilityResult<CompatibilityFactFeedbackHistoryV1> {
    let fact_id = resolve_compatibility_target_tx(transaction, query.target())
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility feedback history target is missing",
            )
        })?;
    let key = OwnerKey::new(query.target().owner())?;
    let fetch_limit = i64::try_from(query.limit().saturating_add(1)).map_err(|_| {
        FactStoreError::InvalidQueryLimit {
            limit: query.limit(),
            max: usize::MAX,
        }
    })?;
    let after_time = query
        .after()
        .map(FactLineageCursor::occurred_at)
        .map(|time| time.0);
    let after_event = query.after().map(|cursor| cursor.event_id().as_str());
    let mut rows = transaction
        .query(
            "SELECT event_id, occurred_at, action, old_trust, new_trust,
                    source, note, details_availability
             FROM memory_v2_feedback_history
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
               AND (
                    ?4 IS NULL
                    OR occurred_at > ?4
                    OR (occurred_at = ?4 AND event_id > ?5)
               )
             ORDER BY occurred_at ASC, event_id ASC
             LIMIT ?6",
            params![
                key.kind,
                key.project_id.as_str(),
                fact_id.as_str(),
                after_time,
                after_event,
                fetch_limit,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut events = Vec::with_capacity(query.limit().saturating_add(1));
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        events.push(CompatibilityFactFeedbackHistoryEntryV1::new(
            FactEventId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            UtcMicros(row_i64(&row, 1, COMPATIBILITY_READ_OPERATION)?),
            compatibility_feedback_action(&row_string(&row, 2, COMPATIBILITY_READ_OPERATION)?)?,
            Confidence::new(row_f64(&row, 3, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            Confidence::new(row_f64(&row, 4, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            row_optional_string(&row, 5, COMPATIBILITY_READ_OPERATION)?,
            row_optional_string(&row, 6, COMPATIBILITY_READ_OPERATION)?,
            compatibility_feedback_details_availability(&row_string(
                &row,
                7,
                COMPATIBILITY_READ_OPERATION,
            )?)?,
        )?);
    }
    let has_more = events.len() > query.limit();
    events.truncate(query.limit());
    let next_after = has_more
        .then(|| {
            events
                .last()
                .map(|event| FactLineageCursor::new(event.occurred_at(), event.event_id().clone()))
        })
        .flatten()
        .transpose()?;
    CompatibilityFactFeedbackHistoryV1::new_with_repair_progress(
        query.target().owner().clone(),
        events,
        next_after,
        repair_progress,
    )
    .map_err(Into::into)
}

pub(super) async fn inspect_compatibility_fact_tx(
    transaction: &Transaction,
    target: &CompatibilityFactTargetV1,
) -> FactCompatibilityResult<Option<CompatibilityFactInspectionV1>> {
    let Some(fact_id) = resolve_compatibility_target_tx(transaction, target).await? else {
        return Ok(None);
    };
    let Some(CompatibilityFactProjectionV1::Available(fact)) =
        load_compatibility_projection_tx(transaction, target.owner(), &fact_id).await?
    else {
        return Ok(None);
    };
    let lineage = FactLineageQuery::new(target.owner().clone(), fact_id.clone(), None, 1_000)?;
    let history = CompatibilityFactHistoryV1::new(
        target.owner().clone(),
        fact_id.clone(),
        query_fact_lineage_tx(transaction, &lineage).await?,
        None,
    )?;
    let key = OwnerKey::new(target.owner())?;
    let mut rows = transaction
        .query(
            "SELECT DISTINCT anchors.anchor_json
             FROM memory_v2_evidence AS evidence
             JOIN retrieval_anchors AS anchors
               ON anchors.anchor_id = evidence.anchor_id
              AND anchors.owner_json = evidence.owner_json
             WHERE evidence.fact_id = ?1
               AND evidence.owner_kind = ?2
               AND evidence.project_id = ?3
               AND evidence.owner_json = ?4
             ORDER BY anchors.anchor_id ASC
             LIMIT 1000",
            params![
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut anchors = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        let anchor = from_json::<RetrievalAnchorRecordV2>(
            &row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?,
            COMPATIBILITY_READ_OPERATION,
        )?;
        if FactOwnerV1::from(anchor.owner().clone()) != *target.owner() {
            return Err(FactStoreError::OwnerMismatch.into());
        }
        anchors.push(anchor);
    }
    let status = compatibility_fact_status_tx(transaction, target.owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_READ_OPERATION,
                "compatibility inspection status is missing",
            )
        })?;
    CompatibilityFactInspectionV1::new(*fact, history, anchors, status)
        .map(Some)
        .map_err(Into::into)
}

struct CommitAttempt {
    outcome: FactCommitOutcome,
    wrote: bool,
}

pub(super) struct PromotionAttempt {
    pub(super) outcome: PromoteFactProposalOutcome,
    pub(super) wrote: bool,
}

pub(super) async fn promote_compatibility_fact_proposal_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactProposalPromotionV1,
) -> FactCompatibilityResult<CompatibilityFactProposalRecordV1> {
    let result =
        promote_compatibility_fact_proposal_with_disposition_tx(db, transaction, request).await?;
    Ok(result.proposal().clone())
}

pub(super) async fn promote_compatibility_fact_proposal_with_disposition_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactProposalPromotionV1,
) -> FactCompatibilityResult<CompatibilityFactProposalPromotionResultV1> {
    let material = json!({
        "proposal_id": request.proposal_id().as_str(),
        "expected_revision": request.expected_revision().get(),
        "reviewer": request.reviewer().map(ActorId::as_str),
    });
    let request_digest = compatibility_digest(material.clone())?;
    let operation_id = compatibility_proposal_action_id("proposal-promote", material)?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_promote",
        &request_digest,
    )
    .await?
    {
        let proposal =
            compatibility_replay_proposal_tx(transaction, request.owner(), &receipt).await?;
        let disposition = match proposal.state() {
            CompatibilityFactProposalStateV1::Applied => {
                CompatibilityFactProposalPromotionDispositionV1::AlreadyPromoted
            }
            CompatibilityFactProposalStateV1::Quarantined => {
                CompatibilityFactProposalPromotionDispositionV1::Quarantined
            }
            _ => {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility promotion receipt does not resolve to a terminal proposal",
                )
                .into());
            }
        };
        return CompatibilityFactProposalPromotionResultV1::new(proposal, disposition)
            .map_err(Into::into);
    }
    let proposal =
        compatibility_proposal_record_tx(transaction, request.owner(), request.proposal_id())
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility proposal is missing",
                )
            })?;
    if proposal.state() != CompatibilityFactProposalStateV1::PendingApproval
        || proposal.revision() != request.expected_revision()
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility proposal revision or state changed before promotion",
        )
        .into());
    }
    let now = compatibility_now()?;
    let payload_metadata = compatibility_payload_metadata(proposal.request().metadata());
    let sanitized = compatibility_sanitize_payload(
        proposal.request().content(),
        proposal.request().category(),
        proposal.request().tags(),
        proposal.request().entities(),
        &payload_metadata,
    )?;
    let Some(sanitized) = sanitized else {
        let reason = "content rejected by privacy sanitizer";
        compatibility_advance_proposal_tx(
            transaction,
            request.owner(),
            request.proposal_id(),
            CompatibilityFactProposalStateV1::PendingApproval,
            request.expected_revision(),
            CompatibilityFactProposalStateV1::Quarantined,
            request.reviewer(),
            Some(reason),
            &request_digest,
            None,
            None,
            None,
            now,
        )
        .await?;
        let receipt = json!({
            "proposal_id": request.proposal_id().as_str(),
            "state": "quarantined",
            "revision": request.expected_revision().get().saturating_add(1),
        });
        compatibility_record_operation_receipt_tx(
            transaction,
            request.owner(),
            &operation_id,
            "proposal_promote",
            &request_digest,
            None,
            None,
            &receipt,
            now,
        )
        .await?;
        let quarantined = compatibility_replay_proposal_tx(
            transaction,
            request.owner(),
            &CompatibilityOperationReceiptV1 {
                fact_id: None,
                event_id: None,
                receipt,
            },
        )
        .await?;
        return CompatibilityFactProposalPromotionResultV1::new(
            quarantined,
            CompatibilityFactProposalPromotionDispositionV1::Quarantined,
        )
        .map_err(Into::into);
    };
    let source = compatibility_source_label(proposal.request().source())?;
    let (fact_id, assertion_id, event_id) = match compatibility_mirror_insert_tx(
        db,
        transaction,
        request.owner(),
        &sanitized.payload,
        &source,
        proposal.request().default_trust(),
        now,
    )
    .await?
    {
        CompatibilityMirrorInsertV1::Existing { fact_id, .. } => {
            let key = OwnerKey::new(request.owner())?;
            let fact = load_current_fact_tx(transaction, &key, request.owner(), &fact_id)
                .await?
                .ok_or_else(|| {
                    storage_message(
                        COMPATIBILITY_WRITE_OPERATION,
                        "existing compatibility mirror has no canonical current fact",
                    )
                })?;
            (
                fact_id,
                fact.active_assertion_id().clone(),
                fact.last_event_id().clone(),
            )
        }
        CompatibilityMirrorInsertV1::Inserted(legacy_fact_id) => {
            let (identity, mapping) =
                compatibility_legacy_mapping_for_new_fact(request.owner(), legacy_fact_id, now)?;
            let batch = compatibility_initial_batch(
                request.owner(),
                identity,
                mapping.clone(),
                sanitized.payload,
                sanitized.access,
                proposal.request().default_trust(),
                proposal.request().actor().cloned(),
                now,
            )?;
            let (receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
            let assertion_id = receipt.active_assertion_id().cloned().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "promoted compatibility fact has no active assertion",
                )
            })?;
            (
                mapping.fact_id().clone(),
                assertion_id,
                receipt.last_event_id().clone(),
            )
        }
    };
    compatibility_advance_proposal_tx(
        transaction,
        request.owner(),
        request.proposal_id(),
        CompatibilityFactProposalStateV1::PendingApproval,
        request.expected_revision(),
        CompatibilityFactProposalStateV1::Applied,
        request.reviewer(),
        None,
        &request_digest,
        Some(&fact_id),
        Some(&assertion_id),
        Some(&event_id),
        now,
    )
    .await?;
    let receipt = json!({
        "proposal_id": request.proposal_id().as_str(),
        "state": "applied",
        "revision": request.expected_revision().get().saturating_add(1),
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        &operation_id,
        "proposal_promote",
        &request_digest,
        Some(&fact_id),
        Some(&event_id),
        &receipt,
        now,
    )
    .await?;
    let promoted = compatibility_replay_proposal_tx(
        transaction,
        request.owner(),
        &CompatibilityOperationReceiptV1 {
            fact_id: Some(fact_id),
            event_id: Some(event_id),
            receipt,
        },
    )
    .await?;
    CompatibilityFactProposalPromotionResultV1::new(
        promoted,
        CompatibilityFactProposalPromotionDispositionV1::NewlyPromoted,
    )
    .map_err(Into::into)
}

/// The immutable assertion record deliberately excludes `FactPayloadV1`.
/// Payload bytes belong only in `memory_v2_assertion_payloads`, which is the
/// storage locus erased when an access transition reaches `Deleted`.
#[derive(Serialize)]
struct StoredAssertionHeaderV1<'a> {
    assertion_id: &'a FactAssertionId,
    fact_id: &'a FactId,
    owner: &'a FactOwnerV1,
    kind: &'a FactAssertionKindV1,
    payload_reference: &'a tracedecay_domain::PayloadReferenceV1,
    evidence: &'a [tracedecay_domain::FactEvidenceRefV1],
    asserted_at: UtcMicros,
    actor_id: Option<&'a tracedecay_domain::ActorId>,
}

fn assertion_header_json(assertion: &FactAssertionV1) -> FactStoreResult<String> {
    let payload_reference = assertion.payload().payload_reference()?;
    to_json(
        &StoredAssertionHeaderV1 {
            assertion_id: assertion.assertion_id(),
            fact_id: assertion.fact_id(),
            owner: assertion.owner(),
            kind: assertion.kind(),
            payload_reference: &payload_reference,
            evidence: assertion.evidence(),
            asserted_at: assertion.asserted_at(),
            actor_id: assertion.actor_id(),
        },
        "serialize payload-free fact assertion header",
    )
}

async fn commit_fact_tx(
    transaction: &Transaction,
    batch: &FactWriteBatch,
) -> FactStoreResult<CommitAttempt> {
    let owner = OwnerKey::new(batch.owner())?;
    let actual_last = current_last_event(transaction, &owner, batch.fact_id()).await?;
    if batch_is_exact_replay(transaction, &owner, batch, actual_last.as_ref()).await? {
        return Ok(CommitAttempt {
            outcome: receipt_outcome(transaction, &owner, batch, true).await?,
            wrote: false,
        });
    }
    if let Some(conflict) = batch_identity_collision(transaction, &owner, batch).await? {
        return Ok(CommitAttempt {
            outcome: FactCommitOutcome::Conflict(conflict),
            wrote: false,
        });
    }
    if actual_last.as_ref() != batch.expected_last_event_id() {
        return Ok(CommitAttempt {
            outcome: FactCommitOutcome::Conflict(FactCommitConflict::LastEventMismatch {
                expected: batch.expected_last_event_id().cloned(),
                actual: actual_last,
            }),
            wrote: false,
        });
    }
    ensure_append_order(transaction, &owner, batch, actual_last.as_ref()).await?;

    ensure_fact_identity(transaction, &owner, batch).await?;
    ensure_referenced_anchors(transaction, &owner, batch).await?;
    for anchor in batch.new_anchors() {
        insert_or_verify_anchor(transaction, &owner, anchor).await?;
    }
    if let Some(assertion) = batch.assertion() {
        insert_assertion(transaction, &owner, assertion).await?;
    }
    if let Some(mapping) = batch.legacy_mapping() {
        insert_legacy_mapping(transaction, &owner, mapping).await?;
    }
    for event in batch.events() {
        ensure_event_references(transaction, &owner, event).await?;
    }
    for event in batch.events() {
        insert_event(transaction, &owner, event).await?;
    }
    publish_current_projection(transaction, &owner, batch).await?;

    Ok(CommitAttempt {
        outcome: receipt_outcome(transaction, &owner, batch, false).await?,
        wrote: true,
    })
}

async fn current_last_event(
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<Option<FactEventId>> {
    let mut rows = transaction
        .query(
            "SELECT last_event_id FROM memory_v2_current_facts
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
    Ok(Some(FactEventId::new(row_string(
        &row,
        0,
        QUERY_OPERATION,
    )?)?))
}

async fn ensure_append_order(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    actual_last: Option<&FactEventId>,
) -> FactStoreResult<()> {
    let Some(last_event_id) = actual_last else {
        return Ok(());
    };
    let first = batch.events().first().ok_or(FactStoreError::EmptyBatch)?;
    let mut rows = transaction
        .query(
            "SELECT occurred_at, event_id FROM memory_v2_lineage_events
             WHERE event_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                last_event_id.as_str(),
                batch.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?
        .ok_or_else(|| storage_message(COMMIT_OPERATION, "current fact points at missing event"))?;
    let last = (
        UtcMicros(row_i64(&row, 0, COMMIT_OPERATION)?),
        FactEventId::new(row_string(&row, 1, COMMIT_OPERATION)?)?,
    );
    if (first.occurred_at(), first.event_id()) <= (last.0, &last.1) {
        return Err(FactStoreError::EventsOutOfOrder);
    }
    Ok(())
}

async fn batch_is_exact_replay(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    actual_last: Option<&FactEventId>,
) -> FactStoreResult<bool> {
    if actual_last != batch.events().last().map(FactLineageEventV1::event_id) {
        return Ok(false);
    }
    if !fact_identity_matches(transaction, owner, batch).await? {
        return Ok(false);
    }
    for anchor in batch.new_anchors() {
        if !anchor_matches(transaction, owner, anchor).await? {
            return Ok(false);
        }
    }
    if let Some(assertion) = batch.assertion()
        && !assertion_matches(transaction, owner, assertion).await?
    {
        return Ok(false);
    }
    if let Some(mapping) = batch.legacy_mapping()
        && !legacy_mapping_matches(transaction, owner, mapping).await?
    {
        return Ok(false);
    }
    for event in batch.events() {
        if !event_matches(transaction, owner, event).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn batch_identity_collision(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<Option<FactCommitConflict>> {
    if fact_exists(transaction, batch.fact_id()).await?
        && !fact_identity_matches(transaction, owner, batch).await?
    {
        return Ok(Some(collision("fact", batch.fact_id().as_str())));
    }
    for anchor in batch.new_anchors() {
        if anchor_exists(transaction, anchor.anchor_id()).await?
            && !anchor_matches(transaction, owner, anchor).await?
        {
            return Ok(Some(collision(
                "retrieval anchor",
                anchor.anchor_id().as_str(),
            )));
        }
    }
    if let Some(assertion) = batch.assertion()
        && assertion_exists(transaction, assertion.assertion_id()).await?
        && !assertion_matches(transaction, owner, assertion).await?
    {
        return Ok(Some(collision(
            "assertion",
            assertion.assertion_id().as_str(),
        )));
    }
    if let Some(mapping) = batch.legacy_mapping()
        && legacy_mapping_exists(transaction, owner, mapping).await?
        && !legacy_mapping_matches(transaction, owner, mapping).await?
    {
        return Ok(Some(collision(
            "legacy mapping",
            mapping.fact_id().as_str(),
        )));
    }
    for event in batch.events() {
        if event_exists(transaction, event.event_id()).await?
            && !event_matches(transaction, owner, event).await?
        {
            return Ok(Some(collision("event", event.event_id().as_str())));
        }
    }
    Ok(None)
}

fn collision(kind: &'static str, id: &str) -> FactCommitConflict {
    FactCommitConflict::IdentityCollision {
        kind,
        id: id.to_owned(),
    }
}

async fn fact_exists(transaction: &Transaction, fact_id: &FactId) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_facts WHERE fact_id = ?1",
        [fact_id.as_str()],
    )
    .await
}

async fn fact_identity_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT owner_kind, project_id, owner_json, identity_json
             FROM memory_v2_facts WHERE fact_id = ?1",
            [batch.fact_id().as_str()],
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
    let identity_matches = match batch.identity_material() {
        Some(identity) => {
            row_string(&row, 3, QUERY_OPERATION)? == to_json(identity, "serialize fact identity")?
        }
        None => true,
    };
    Ok(row_string(&row, 0, QUERY_OPERATION)? == owner.kind
        && row_string(&row, 1, QUERY_OPERATION)? == owner.project_id
        && row_string(&row, 2, QUERY_OPERATION)? == owner.json
        && identity_matches)
}

async fn ensure_referenced_anchors(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
) -> FactStoreResult<()> {
    for anchor_id in batch.referenced_anchor_ids() {
        let mut rows = transaction
            .query(
                "SELECT 1 FROM retrieval_anchors
                 WHERE anchor_id = ?1 AND owner_json = ?2",
                params![anchor_id.as_str(), owner.json.as_str()],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        let Some(_row) = rows
            .next()
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?
        else {
            return Err(FactStoreError::MissingEvidenceAnchor {
                anchor_id: anchor_id.clone(),
            });
        };
    }
    Ok(())
}

async fn insert_or_verify_anchor(
    transaction: &Transaction,
    owner: &OwnerKey,
    anchor: &RetrievalAnchorRecordV2,
) -> FactStoreResult<()> {
    if anchor_exists(transaction, anchor.anchor_id()).await? {
        if anchor_matches(transaction, owner, anchor).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "retrieval anchor identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO retrieval_anchors(
                anchor_id, anchor_json, owner_json, projection_generation
             ) VALUES(?1, ?2, ?3, ?4)",
            params![
                anchor.anchor_id().as_str(),
                to_json(anchor, "serialize retrieval anchor")?,
                owner.json.as_str(),
                anchor.projection_generation().as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    for alias in anchor.aliases() {
        transaction
            .execute(
                "INSERT INTO retrieval_anchor_aliases(
                    owner_json, alias_kind, locator_digest, anchor_id
                 ) VALUES(?1, ?2, ?3, ?4)",
                params![
                    owner.json.as_str(),
                    to_json(&alias.kind(), "serialize anchor alias kind")?,
                    to_json(alias.locator_digest(), "serialize anchor locator digest")?,
                    anchor.anchor_id().as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

async fn anchor_exists(
    transaction: &Transaction,
    anchor_id: &RetrievalAnchorId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM retrieval_anchors WHERE anchor_id = ?1",
        [anchor_id.as_str()],
    )
    .await
}

async fn anchor_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    anchor: &RetrievalAnchorRecordV2,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT anchor_json, owner_json, projection_generation
             FROM retrieval_anchors WHERE anchor_id = ?1",
            [anchor.anchor_id().as_str()],
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
    if row_string(&row, 0, QUERY_OPERATION)? != to_json(anchor, "serialize retrieval anchor")?
        || row_string(&row, 1, QUERY_OPERATION)? != owner.json
        || row_string(&row, 2, QUERY_OPERATION)? != anchor.projection_generation().as_str()
    {
        return Ok(false);
    }
    let mut aliases = transaction
        .query(
            "SELECT alias_kind, locator_digest FROM retrieval_anchor_aliases
             WHERE anchor_id = ?1 ORDER BY alias_kind, locator_digest",
            [anchor.anchor_id().as_str()],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored = Vec::new();
    while let Some(row) = aliases
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored.push((
            row_string(&row, 0, QUERY_OPERATION)?,
            row_string(&row, 1, QUERY_OPERATION)?,
        ));
    }
    let mut expected = anchor
        .aliases()
        .iter()
        .map(|alias| {
            Ok((
                to_json(&alias.kind(), "serialize anchor alias kind")?,
                to_json(alias.locator_digest(), "serialize anchor locator digest")?,
            ))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    expected.sort();
    Ok(stored == expected)
}

async fn insert_assertion(
    transaction: &Transaction,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> FactStoreResult<()> {
    if assertion_exists(transaction, assertion.assertion_id()).await? {
        if assertion_matches(transaction, owner, assertion).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "assertion identity collision",
        ));
    }
    let header_json = assertion_header_json(assertion)?;
    let actor_id = assertion.actor_id().map(ToString::to_string);
    transaction
        .execute(
            "INSERT INTO memory_v2_assertions(
                assertion_id, fact_id, owner_kind, project_id, owner_json,
                assertion_header_json, kind_json, payload_reference_json,
                receipt_json, asserted_at, actor_id
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                header_json,
                to_json(assertion.kind(), "serialize assertion kind")?,
                to_json(
                    &assertion.payload().payload_reference()?,
                    "serialize assertion payload reference",
                )?,
                to_json(assertion.payload().receipt(), "serialize assertion receipt")?,
                assertion.asserted_at().0,
                actor_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;

    for (ordinal, superseded) in superseded_assertions(assertion.kind()).iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO memory_v2_assertion_supersession(
                    assertion_id, fact_id, owner_kind, project_id,
                    superseded_assertion_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    superseded.as_str(),
                    ordinal as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }

    transaction
        .execute(
            "INSERT INTO memory_v2_assertion_payloads(
                assertion_id, fact_id, owner_kind, project_id, payload_json, content
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
                to_json(assertion.payload(), "serialize assertion payload")?,
                assertion.payload().content(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;

    for (ordinal, evidence) in assertion.evidence().iter().enumerate() {
        let evidence_json = to_json(evidence, "serialize fact evidence")?;
        let changed = transaction
            .execute(
                "INSERT OR IGNORE INTO memory_v2_evidence(
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    owner.json.as_str(),
                    evidence.anchor_id().as_str(),
                    evidence_json.as_str(),
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
        if changed == 0 {
            let mut rows = transaction
                .query(
                    "SELECT evidence_json, owner_json, anchor_id
                     FROM memory_v2_evidence
                     WHERE evidence_id = ?1 AND fact_id = ?2
                       AND owner_kind = ?3 AND project_id = ?4",
                    params![
                        evidence.evidence_id().as_str(),
                        assertion.fact_id().as_str(),
                        owner.kind,
                        owner.project_id.as_str(),
                    ],
                )
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| storage_error(COMMIT_OPERATION, error))?
            else {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "evidence insert disappeared",
                ));
            };
            if row_string(&row, 0, COMMIT_OPERATION)? != evidence_json
                || row_string(&row, 1, COMMIT_OPERATION)? != owner.json
                || row_string(&row, 2, COMMIT_OPERATION)? != evidence.anchor_id().as_str()
            {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "evidence identity collision",
                ));
            }
        }
        transaction
            .execute(
                "INSERT INTO memory_v2_assertion_evidence(
                    assertion_id, evidence_id, fact_id, owner_kind, project_id, ordinal
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    assertion.assertion_id().as_str(),
                    evidence.evidence_id().as_str(),
                    assertion.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str(),
                    ordinal as i64,
                ],
            )
            .await
            .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    }
    Ok(())
}

fn superseded_assertions(kind: &FactAssertionKindV1) -> Vec<&FactAssertionId> {
    match kind {
        FactAssertionKindV1::Correction { supersedes } => vec![supersedes],
        FactAssertionKindV1::Merge { supersedes } => supersedes.iter().collect(),
        FactAssertionKindV1::Initial | FactAssertionKindV1::LegacyImport => Vec::new(),
    }
}

async fn assertion_exists(
    transaction: &Transaction,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_assertions WHERE assertion_id = ?1",
        [assertion_id.as_str()],
    )
    .await
}

async fn assertion_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    assertion: &FactAssertionV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT fact_id, owner_kind, project_id, owner_json,
                    assertion_header_json, kind_json, payload_reference_json,
                    receipt_json, asserted_at, actor_id
             FROM memory_v2_assertions WHERE assertion_id = ?1",
            [assertion.assertion_id().as_str()],
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
    let stored_actor = row_optional_string(&row, 9, QUERY_OPERATION)?;
    let expected_actor = assertion.actor_id().map(ToString::to_string);
    if row_string(&row, 0, QUERY_OPERATION)? != assertion.fact_id().as_str()
        || row_string(&row, 1, QUERY_OPERATION)? != owner.kind
        || row_string(&row, 2, QUERY_OPERATION)? != owner.project_id
        || row_string(&row, 3, QUERY_OPERATION)? != owner.json
        || row_string(&row, 4, QUERY_OPERATION)? != assertion_header_json(assertion)?
        || row_string(&row, 5, QUERY_OPERATION)?
            != to_json(assertion.kind(), "serialize assertion kind")?
        || row_string(&row, 6, QUERY_OPERATION)?
            != to_json(
                &assertion.payload().payload_reference()?,
                "serialize assertion payload reference",
            )?
        || row_string(&row, 7, QUERY_OPERATION)?
            != to_json(assertion.payload().receipt(), "serialize assertion receipt")?
        || row_i64(&row, 8, QUERY_OPERATION)? != assertion.asserted_at().0
        || stored_actor != expected_actor
    {
        return Ok(false);
    }

    let mut supersession = transaction
        .query(
            "SELECT superseded_assertion_id FROM memory_v2_assertion_supersession
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4 ORDER BY ordinal",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored_supersession = Vec::new();
    while let Some(row) = supersession
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored_supersession.push(row_string(&row, 0, QUERY_OPERATION)?);
    }
    let expected_supersession = superseded_assertions(assertion.kind())
        .into_iter()
        .map(|id| id.as_str().to_owned())
        .collect::<Vec<_>>();
    if stored_supersession != expected_supersession {
        return Ok(false);
    }

    let mut payload = transaction
        .query(
            "SELECT payload_json, content FROM memory_v2_assertion_payloads
             WHERE assertion_id = ?1 AND fact_id = ?2
               AND owner_kind = ?3 AND project_id = ?4",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let payload_row = payload
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    drop(payload);
    let payload_matches = match payload_row {
        Some(row) => {
            row_string(&row, 0, QUERY_OPERATION)?
                == to_json(assertion.payload(), "serialize assertion payload")?
                && row_string(&row, 1, QUERY_OPERATION)? == assertion.payload().content()
        }
        None => payload_is_purged_projection(transaction, owner, assertion.fact_id()).await?,
    };
    if !payload_matches {
        return Ok(false);
    }

    let mut evidence = transaction
        .query(
            "SELECT ae.evidence_id, e.evidence_json, e.owner_json, e.anchor_id
             FROM memory_v2_assertion_evidence ae
             JOIN memory_v2_evidence e ON
                e.evidence_id = ae.evidence_id AND e.fact_id = ae.fact_id AND
                e.owner_kind = ae.owner_kind AND e.project_id = ae.project_id
             WHERE ae.assertion_id = ?1 AND ae.fact_id = ?2
               AND ae.owner_kind = ?3 AND ae.project_id = ?4 ORDER BY ae.ordinal",
            params![
                assertion.assertion_id().as_str(),
                assertion.fact_id().as_str(),
                owner.kind,
                owner.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?;
    let mut stored_evidence = Vec::new();
    while let Some(row) = evidence
        .next()
        .await
        .map_err(|error| storage_error(QUERY_OPERATION, error))?
    {
        stored_evidence.push((
            row_string(&row, 0, QUERY_OPERATION)?,
            row_string(&row, 1, QUERY_OPERATION)?,
            row_string(&row, 2, QUERY_OPERATION)?,
            row_string(&row, 3, QUERY_OPERATION)?,
        ));
    }
    let expected_evidence = assertion
        .evidence()
        .iter()
        .map(|evidence| {
            Ok((
                evidence.evidence_id().as_str().to_owned(),
                to_json(evidence, "serialize fact evidence")?,
                owner.json.clone(),
                evidence.anchor_id().as_str().to_owned(),
            ))
        })
        .collect::<FactStoreResult<Vec<_>>>()?;
    Ok(stored_evidence == expected_evidence)
}

async fn payload_is_purged_projection(
    transaction: &Transaction,
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

async fn insert_legacy_mapping(
    transaction: &Transaction,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<()> {
    if legacy_mapping_exists(transaction, owner, mapping).await? {
        if legacy_mapping_matches(transaction, owner, mapping).await? {
            return Ok(());
        }
        return Err(storage_message(
            COMMIT_OPERATION,
            "legacy mapping identity collision",
        ));
    }
    transaction
        .execute(
            "INSERT INTO memory_v2_legacy_map(
                owner_kind, project_id, owner_json, source_store_id,
                legacy_fact_id, fact_id, mapping_json
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                owner.kind,
                owner.project_id.as_str(),
                owner.json.as_str(),
                mapping.source_store_id().as_str(),
                mapping.legacy_fact_id(),
                mapping.fact_id().as_str(),
                to_json(mapping, "serialize legacy fact mapping")?,
            ],
        )
        .await
        .map_err(|error| storage_error(COMMIT_OPERATION, error))?;
    Ok(())
}

async fn legacy_mapping_exists(
    transaction: &Transaction,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<bool> {
    row_exists_params(
        transaction,
        "SELECT 1 FROM memory_v2_legacy_map
         WHERE owner_kind = ?1 AND project_id = ?2
           AND source_store_id = ?3 AND legacy_fact_id = ?4",
        params![
            owner.kind,
            owner.project_id.as_str(),
            mapping.source_store_id().as_str(),
            mapping.legacy_fact_id(),
        ],
    )
    .await
}

async fn legacy_mapping_matches(
    transaction: &Transaction,
    owner: &OwnerKey,
    mapping: &LegacyFactMappingV1,
) -> FactStoreResult<bool> {
    let mut rows = transaction
        .query(
            "SELECT owner_json, fact_id, mapping_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2
               AND source_store_id = ?3 AND legacy_fact_id = ?4",
            params![
                owner.kind,
                owner.project_id.as_str(),
                mapping.source_store_id().as_str(),
                mapping.legacy_fact_id(),
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
    Ok(row_string(&row, 0, QUERY_OPERATION)? == owner.json
        && row_string(&row, 1, QUERY_OPERATION)? == mapping.fact_id().as_str()
        && row_string(&row, 2, QUERY_OPERATION)?
            == to_json(mapping, "serialize legacy fact mapping")?)
}

async fn ensure_event_references(
    transaction: &Transaction,
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
        }
        FactLineageEventKindV1::TrustChanged { evidence_ids, .. } => {
            ensure_event_evidence(transaction, owner, event.fact_id(), evidence_ids).await?;
        }
        FactLineageEventKindV1::Curated {
            action,
            evidence_ids,
        } => {
            ensure_event_evidence(transaction, owner, event.fact_id(), evidence_ids).await?;
            if let FactCurationActionV1::ContradictedBy { fact_id }
            | FactCurationActionV1::SupersededBy { fact_id }
            | FactCurationActionV1::MergedInto { fact_id } = action
                && !owned_fact_exists(transaction, owner, fact_id).await?
            {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "lineage curation target is missing",
                ));
            }
        }
        FactLineageEventKindV1::PayloadAccessChanged { .. } => {}
        FactLineageEventKindV1::LegacyImported { mapping } => {
            if !legacy_mapping_matches(transaction, owner, mapping).await? {
                return Err(storage_message(
                    COMMIT_OPERATION,
                    "lineage legacy mapping reference is missing",
                ));
            }
        }
    }
    Ok(())
}

async fn ensure_event_evidence(
    transaction: &Transaction,
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
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    assertion_id: &FactAssertionId,
) -> FactStoreResult<bool> {
    row_exists_params(
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
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
    evidence_id: &FactEvidenceId,
) -> FactStoreResult<bool> {
    row_exists_params(
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
    transaction: &Transaction,
    owner: &OwnerKey,
    fact_id: &FactId,
) -> FactStoreResult<bool> {
    row_exists_params(
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

async fn insert_event(
    transaction: &Transaction,
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

async fn event_exists(transaction: &Transaction, event_id: &FactEventId) -> FactStoreResult<bool> {
    row_exists(
        transaction,
        "SELECT 1 FROM memory_v2_lineage_events WHERE event_id = ?1",
        [event_id.as_str()],
    )
    .await
}

async fn event_matches(
    transaction: &Transaction,
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
pub(super) struct Projection {
    pub(super) access: PayloadAccessState,
    pub(super) trust: Confidence,
    pub(super) active_assertion_id: Option<FactAssertionId>,
    pub(super) last_event_id: Option<FactEventId>,
    pub(super) updated_at: UtcMicros,
}

impl Projection {
    fn empty() -> FactStoreResult<Self> {
        Ok(Self {
            access: PayloadAccessState::Eligible,
            trust: Confidence::new(DEFAULT_TRUST)?,
            active_assertion_id: None,
            last_event_id: None,
            updated_at: UtcMicros(0),
        })
    }

    fn apply(&mut self, event: &FactLineageEventV1) -> FactStoreResult<()> {
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
            FactLineageEventKindV1::Curated { .. }
            | FactLineageEventKindV1::LegacyImported { .. } => {}
        }
        self.last_event_id = Some(event.event_id().clone());
        self.updated_at = event.occurred_at();
        Ok(())
    }
}

async fn publish_current_projection(
    transaction: &Transaction,
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
                "DELETE FROM memory_v2_assertion_vectors
                 WHERE fact_id = ?1 AND owner_kind = ?2 AND project_id = ?3",
                params![
                    batch.fact_id().as_str(),
                    owner.kind,
                    owner.project_id.as_str()
                ],
            )
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
        // A live transition to a terminal payload access must erase the same
        // free-text feedback surface as the canonical purge path
        // (`purge_payload_rows`), so a deleted fact never retains
        // API-reachable feedback source/note text.
        transaction
            .execute(
                "UPDATE memory_v2_feedback_history
                 SET source = NULL, note = NULL,
                     details_availability = CASE
                         WHEN details_availability = 'available' THEN 'legacy_redacted'
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

pub(super) async fn load_current_projection(
    transaction: &Transaction,
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

async fn receipt_outcome(
    transaction: &Transaction,
    owner: &OwnerKey,
    batch: &FactWriteBatch,
    replay: bool,
) -> FactStoreResult<FactCommitOutcome> {
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

async fn ensure_fact_identity(
    transaction: &Transaction,
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

pub(super) async fn query_current_facts_tx(
    snapshot: &Transaction,
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

pub(super) async fn query_fact_current_tx(
    snapshot: &Transaction,
    owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<StoredFactV1>> {
    let key = OwnerKey::new(owner)?;
    load_current_fact_tx(snapshot, &key, owner, fact_id).await
}

pub(super) async fn load_current_fact_tx(
    snapshot: &Transaction,
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

pub(super) async fn query_fact_as_of_tx(
    snapshot: &Transaction,
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

async fn load_assertion_payload_tx(
    snapshot: &Transaction,
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

pub(super) async fn query_fact_lineage_tx(
    snapshot: &Transaction,
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

pub(super) async fn get_retrieval_anchor_tx(
    snapshot: &Transaction,
    query: &RetrievalAnchorQuery,
) -> FactStoreResult<Option<RetrievalAnchorRecordV2>> {
    let owner = OwnerKey::new(query.owner())?;
    let mut rows = snapshot
        .query(
            "SELECT anchor_json FROM retrieval_anchors
             WHERE anchor_id = ?1 AND owner_json = ?2",
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
    snapshot: &Transaction,
    owner: &OwnerKey,
    typed_owner: &FactOwnerV1,
    fact_id: &FactId,
) -> FactStoreResult<Option<LegacyFactMappingV1>> {
    let mut rows = snapshot
        .query(
            "SELECT mapping_json FROM memory_v2_legacy_map
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
             ORDER BY source_store_id ASC LIMIT 1",
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
    let mapping =
        from_json::<LegacyFactMappingV1>(&row_string(&row, 0, QUERY_OPERATION)?, QUERY_OPERATION)?;
    if mapping.owner() != typed_owner || mapping.fact_id() != fact_id {
        return Err(storage_message(
            QUERY_OPERATION,
            "legacy mapping identity mismatch",
        ));
    }
    Ok(Some(mapping))
}

pub(super) async fn promote_fact_proposal_tx(
    transaction: &Transaction,
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
    transaction: &Transaction,
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
    transaction: &Transaction,
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
            "reviewer": promotion.reviewer().map(|reviewer| reviewer.as_str()),
            "fact_id": receipt.fact_id().as_str(),
            "active_assertion_id": receipt.active_assertion_id().map(FactAssertionId::as_str),
            "last_event_id": receipt.last_event_id().as_str(),
        }),
        PROMOTE_OPERATION,
    )
    .map_err(FactProposalStoreError::from)
}

pub(super) fn proposal_transition_id(transition_json: &str) -> String {
    let digest = Sha256::digest(transition_json.as_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut id = String::from("proposal-transition:");
    for byte in digest {
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    id
}

impl<'a> DatabaseFactStore<'a> {
    pub(super) async fn commit_batch(
        &self,
        batch: &FactWriteBatch,
    ) -> FactStoreResult<FactCommitOutcome> {
        let transaction = self
            .db
            .begin_write_transaction(COMMIT_OPERATION)
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
