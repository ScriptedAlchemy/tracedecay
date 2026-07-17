//! Compatibility curation apply, relations, entity merges, and fact merges.

use std::collections::BTreeSet;

use crate::db::Database;
use crate::memory::entities::normalize_entity;
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};

use libsql::{Transaction, params};
use serde_json::{Value, json};

use tracedecay_domain::{
    ActorId, Confidence, FactAssertionKindV1, FactAssertionV1, FactCategoryV1,
    FactCurationActionV1, FactEventId, FactId, FactLineageEventKindV1, FactLineageEventV1,
    FactOwnerV1, FactPayloadV1, PayloadAccessState, UtcMicros,
};
use tracedecay_store::{
    CompatibilityFactAddAliasV1, CompatibilityFactCurationBatchV1,
    CompatibilityFactCurationOperationV1, CompatibilityFactCurationReceiptV1,
    CompatibilityFactIdV1, CompatibilityFactLinkV1, CompatibilityFactMappingV1,
    CompatibilityFactMergeCommandV1, CompatibilityFactMergeEntitiesV1,
    CompatibilityFactMergeOutcomeV1, CompatibilityFactNormalizeTagsV1, CompatibilityFactRelationV1,
    CompatibilityFactTargetV1, CompatibilityMemoryRepairStatsV1, FactCompatibilityResult,
    FactStoreError, FactStoreResult, FactWriteBatch, StoredFactV1,
};

use super::crud::{
    compatibility_commit_batch_tx, compatibility_mark_owner_banks_dirty_tx,
    compatibility_mirror_delete_tx, compatibility_mirror_update_tx, compatibility_sanitize_payload,
    load_current_fact_tx, load_current_projection,
};
use super::envelope::{
    CompatibilityOperationReceiptV1, compatibility_digest,
    compatibility_lookup_operation_receipt_tx, compatibility_receipt_u64,
    compatibility_record_operation_receipt_tx, compatibility_target_digest,
};
use super::primitives::{
    COMPATIBILITY_WRITE_OPERATION, OwnerKey, compatibility_event_time,
    compatibility_legacy_timestamp, compatibility_now, compatibility_source_label,
    compatibility_source_store_id, from_json, row_f64, row_i64, row_string, storage_error,
    storage_message, to_json,
};
use super::projection::{
    compatibility_fact_for_legacy_id_tx, compatibility_required_mapping_tx,
    compatibility_source_for_fact_tx, resolve_compatibility_target_tx,
};
use super::proposals::compatibility_proposal_category;
use super::repair::{
    COMPATIBILITY_REPAIR_VECTOR_BATCH, compatibility_rebuild_dirty_banks_tx,
    compatibility_repair_missing_vectors_tx, compatibility_repair_vector_for_fact_tx,
};

fn compatibility_relation_label(relation: CompatibilityFactRelationV1) -> &'static str {
    match relation {
        CompatibilityFactRelationV1::Supports => "supports",
        CompatibilityFactRelationV1::Contradicts => "contradicts",
        CompatibilityFactRelationV1::Supersedes => "supersedes",
        CompatibilityFactRelationV1::DerivedFrom => "derived_from",
    }
}

fn compatibility_relations_conflict(
    left: CompatibilityFactRelationV1,
    right: CompatibilityFactRelationV1,
) -> bool {
    matches!(
        (left, right),
        (
            CompatibilityFactRelationV1::Supports,
            CompatibilityFactRelationV1::Contradicts
        ) | (
            CompatibilityFactRelationV1::Contradicts,
            CompatibilityFactRelationV1::Supports
        )
    )
}

fn compatibility_normalize_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .map(|tag| {
            tag.trim()
                .to_ascii_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("_")
                .replace('-', "_")
        })
        .filter(|tag| !tag.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(super) async fn compatibility_available_curation_fact_tx(
    transaction: &Transaction,
    target: &CompatibilityFactTargetV1,
) -> FactStoreResult<(FactId, StoredFactV1, CompatibilityFactMappingV1)> {
    let fact_id = resolve_compatibility_target_tx(transaction, target)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility curation target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(target.owner())?;
    let fact = load_current_fact_tx(transaction, &owner_key, target.owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility curation target is unavailable",
            )
        })?;
    if fact.payload().is_none() {
        return Err(FactStoreError::PayloadAccessMismatch);
    }
    let mapping = compatibility_required_mapping_tx(transaction, target.owner(), &fact_id).await?;
    let mapping = CompatibilityFactMappingV1::new(
        CompatibilityFactIdV1::new(target.owner().clone(), fact_id.clone())?,
        Some(mapping),
    )?;
    Ok((fact_id, fact, mapping))
}

pub(super) async fn compatibility_curation_evidence_ids_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    evidence: &[CompatibilityFactTargetV1],
) -> FactStoreResult<Vec<FactId>> {
    let mut ids = Vec::with_capacity(evidence.len());
    let mut seen = BTreeSet::new();
    for target in evidence {
        if target.owner() != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        let (fact_id, _, _) = compatibility_available_curation_fact_tx(transaction, target).await?;
        if !seen.insert(fact_id.clone()) {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility curation evidence resolved to duplicate facts",
            ));
        }
        ids.push(fact_id);
    }
    Ok(ids)
}

async fn compatibility_curation_mappings_from_ids_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    ids: &[FactId],
) -> FactStoreResult<Vec<CompatibilityFactMappingV1>> {
    let mut mappings = Vec::with_capacity(ids.len());
    let mut seen = BTreeSet::new();
    for fact_id in ids {
        if !seen.insert(fact_id.clone()) {
            continue;
        }
        let legacy_mapping = compatibility_required_mapping_tx(transaction, owner, fact_id).await?;
        mappings.push(CompatibilityFactMappingV1::new(
            CompatibilityFactIdV1::new(owner.clone(), fact_id.clone())?,
            Some(legacy_mapping),
        )?);
    }
    Ok(mappings)
}

async fn compatibility_sanitized_relation_metadata(metadata: &Value) -> FactStoreResult<Value> {
    match sanitize_memory_fact_payload(metadata.clone())
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        MemoryFactSanitizationV1::Durable { payload, .. } => Ok(payload),
        MemoryFactSanitizationV1::Quarantined => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility relation metadata was rejected by the privacy sanitizer",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
async fn compatibility_upsert_legacy_relation_tx(
    transaction: &Transaction,
    source_legacy_fact_id: i64,
    target_legacy_fact_id: i64,
    relation: CompatibilityFactRelationV1,
    confidence: Confidence,
    source_label: &str,
    metadata: &Value,
    timestamp: i64,
) -> FactStoreResult<()> {
    let mut rows = transaction
        .query(
            "SELECT relation FROM memory_fact_relations
             WHERE source_fact_id = ?1 AND target_fact_id = ?2",
            params![source_legacy_fact_id, target_legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        let stored = match row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?.as_str() {
            "supports" => CompatibilityFactRelationV1::Supports,
            "contradicts" => CompatibilityFactRelationV1::Contradicts,
            "supersedes" => CompatibilityFactRelationV1::Supersedes,
            "derived_from" => CompatibilityFactRelationV1::DerivedFrom,
            _ => {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "legacy compatibility relation has an unsupported kind",
                ));
            }
        };
        if compatibility_relations_conflict(stored, relation) {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility relation conflicts with an existing relation",
            ));
        }
    }
    drop(rows);
    transaction
        .execute(
            "INSERT INTO memory_fact_relations(
                source_fact_id, target_fact_id, relation, confidence, source, metadata, created_at, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(source_fact_id, target_fact_id, relation) DO UPDATE SET
                confidence = excluded.confidence,
                source = excluded.source,
                metadata = excluded.metadata,
                updated_at = excluded.updated_at",
            params![
                source_legacy_fact_id,
                target_legacy_fact_id,
                compatibility_relation_label(relation),
                confidence.as_f64(),
                source_label,
                to_json(metadata, "serialize compatibility relation metadata")?,
                timestamp,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

async fn compatibility_link_facts_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &CompatibilityFactLinkV1,
    now: UtcMicros,
) -> FactStoreResult<(Vec<FactId>, Option<FactEventId>)> {
    let (source_fact_id, source_fact, source_mapping) =
        compatibility_available_curation_fact_tx(transaction, operation.source()).await?;
    let (target_fact_id, _, target_mapping) =
        compatibility_available_curation_fact_tx(transaction, operation.target()).await?;
    if source_fact_id == target_fact_id {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility curation relation cannot target itself",
        ));
    }
    let evidence_fact_ids =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let source_label = compatibility_source_label(Some(operation.source_label()))?;
    let metadata = compatibility_sanitized_relation_metadata(operation.metadata()).await?;
    let key = OwnerKey::new(owner)?;
    let evidence_fact_ids_json = to_json(
        &evidence_fact_ids
            .iter()
            .map(FactId::as_str)
            .collect::<Vec<_>>(),
        "serialize compatibility relation evidence",
    )?;
    let provenance_json = to_json(&metadata, "serialize compatibility relation provenance")?;
    transaction
        .execute(
            "INSERT INTO memory_v2_fact_relations(
                owner_kind, project_id, source_fact_id, target_fact_id, relation,
                confidence, source_label, provenance_json, evidence_fact_ids_json,
                occurred_at, updated_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
             ON CONFLICT(owner_kind, project_id, source_fact_id, target_fact_id, relation)
             DO UPDATE SET confidence = excluded.confidence,
                           source_label = excluded.source_label,
                           provenance_json = excluded.provenance_json,
                           evidence_fact_ids_json = excluded.evidence_fact_ids_json,
                           updated_at = excluded.updated_at",
            params![
                key.kind,
                key.project_id.as_str(),
                source_fact_id.as_str(),
                target_fact_id.as_str(),
                compatibility_relation_label(operation.relation()),
                operation.confidence().as_f64(),
                source_label.clone(),
                provenance_json,
                evidence_fact_ids_json,
                now.0,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let event_id = match operation.relation() {
        CompatibilityFactRelationV1::Supports | CompatibilityFactRelationV1::DerivedFrom => None,
        CompatibilityFactRelationV1::Contradicts | CompatibilityFactRelationV1::Supersedes => {
            let action = match operation.relation() {
                CompatibilityFactRelationV1::Contradicts => FactCurationActionV1::ContradictedBy {
                    fact_id: target_fact_id.clone(),
                },
                CompatibilityFactRelationV1::Supersedes => FactCurationActionV1::SupersededBy {
                    fact_id: target_fact_id.clone(),
                },
                _ => unreachable!("handled typed relation variants above"),
            };
            let event = FactLineageEventV1::new(
                source_fact_id.clone(),
                owner.clone(),
                FactLineageEventKindV1::Curated {
                    action,
                    // LinkFacts provenance is owner-scoped FactId data above. This V1 lineage
                    // field accepts only source-owned FactEvidenceId values.
                    evidence_ids: Vec::new(),
                },
                now,
                actor.cloned(),
            )?;
            let batch = FactWriteBatch::new(
                source_fact_id.clone(),
                owner.clone(),
                None,
                vec![event],
                Vec::new(),
                Vec::new(),
                None,
                Some(source_fact.last_event_id().clone()),
            )?;
            let (receipt, _) = compatibility_commit_batch_tx(transaction, &batch).await?;
            Some(receipt.last_event_id().clone())
        }
    };
    compatibility_upsert_legacy_relation_tx(
        transaction,
        source_mapping
            .legacy_fact_id()
            .ok_or(FactStoreError::FactMismatch)?,
        target_mapping
            .legacy_fact_id()
            .ok_or(FactStoreError::FactMismatch)?,
        operation.relation(),
        operation.confidence(),
        &source_label,
        &metadata,
        compatibility_legacy_timestamp(now),
    )
    .await?;
    Ok((vec![source_fact_id, target_fact_id], event_id))
}

fn compatibility_curated_correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
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
    let recorded = FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactLineageEventKindV1::AssertionRecorded {
            assertion_id: assertion.assertion_id().clone(),
        },
        now,
        actor.clone(),
    )?;
    let curated = FactLineageEventV1::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Retained,
            evidence_ids: Vec::new(),
        },
        compatibility_event_time(now, 1)?,
        actor,
    )?;
    FactWriteBatch::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        Some(assertion),
        vec![recorded, curated],
        Vec::new(),
        Vec::new(),
        None,
        Some(fact.last_event_id().clone()),
    )
}

async fn compatibility_normalize_tags_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &CompatibilityFactNormalizeTagsV1,
    now: UtcMicros,
) -> FactStoreResult<FactId> {
    let _evidence =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let (fact_id, fact, mapping) =
        compatibility_available_curation_fact_tx(transaction, operation.fact()).await?;
    let payload = fact
        .payload()
        .ok_or(FactStoreError::PayloadAccessMismatch)?;
    let tags = compatibility_normalize_tags(operation.tags());
    let Some(sanitized) = compatibility_sanitize_payload(
        payload.content(),
        payload.category(),
        &tags,
        payload.entities(),
        payload.metadata(),
    )?
    else {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility normalized tags were rejected by the privacy sanitizer",
        ));
    };
    let source = compatibility_source_for_fact_tx(
        transaction,
        mapping
            .legacy_mapping()
            .ok_or(FactStoreError::FactMismatch)?,
    )
    .await?;
    let batch = compatibility_curated_correction_batch(
        &fact,
        sanitized.payload.clone(),
        actor.cloned(),
        now,
    )?;
    compatibility_commit_batch_tx(transaction, &batch).await?;
    compatibility_mirror_update_tx(
        db,
        transaction,
        owner,
        mapping
            .legacy_fact_id()
            .ok_or(FactStoreError::FactMismatch)?,
        &sanitized.payload,
        &source,
        fact.trust(),
        now,
    )
    .await?;
    Ok(fact_id)
}

async fn compatibility_owner_entity_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    entity_id: i64,
) -> FactStoreResult<(String, Vec<String>)> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let foreign_links = transaction
        .query(
            "SELECT COUNT(*)
             FROM memory_fact_entities AS links
             LEFT JOIN memory_v2_legacy_map AS mappings
               ON mappings.legacy_fact_id = links.fact_id
             WHERE links.entity_id = ?1
               AND (
                    mappings.legacy_fact_id IS NULL
                    OR mappings.owner_kind <> ?2
                    OR mappings.project_id <> ?3
                    OR mappings.owner_json <> ?4
                    OR mappings.source_store_id <> ?5
               )",
            params![
                entity_id,
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut foreign_links = foreign_links;
    let row = foreign_links
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility entity ownership count is missing",
            )
        })?;
    if row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)? != 0 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility curation entity is shared outside this owner",
        ));
    }
    drop(foreign_links);
    let mut rows = transaction
        .query(
            "SELECT name, aliases FROM memory_entities WHERE entity_id = ?1",
            params![entity_id],
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
                "compatibility curation entity is missing",
            )
        })?;
    Ok((
        row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
        from_json::<Vec<String>>(
            &row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?,
            COMPATIBILITY_WRITE_OPERATION,
        )?,
    ))
}

async fn compatibility_entity_linked_to_evidence_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    entity_id: i64,
    evidence_ids: &[FactId],
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let placeholders = std::iter::repeat_n("?", evidence_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT 1
         FROM memory_fact_entities AS links
         JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
         WHERE links.entity_id = ?
           AND mappings.owner_kind = ? AND mappings.project_id = ?
           AND mappings.owner_json = ? AND mappings.source_store_id = ?
           AND mappings.fact_id IN ({placeholders})
         LIMIT 1"
    );
    let mut values = Vec::with_capacity(evidence_ids.len() + 5);
    values.push(libsql::Value::Integer(entity_id));
    values.push(libsql::Value::Text(key.kind.to_string()));
    values.push(libsql::Value::Text(key.project_id.clone()));
    values.push(libsql::Value::Text(key.json.clone()));
    values.push(libsql::Value::Text(source_store_id.as_str().to_owned()));
    values.extend(
        evidence_ids
            .iter()
            .map(|fact_id| libsql::Value::Text(fact_id.as_str().to_owned())),
    );
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
        .is_none()
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility curation entity is not linked to supplied evidence",
        ));
    }
    Ok(())
}

async fn compatibility_owner_entity_fact_ids_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    entity_ids: &[i64],
) -> FactStoreResult<Vec<FactId>> {
    let key = OwnerKey::new(owner)?;
    let source_store_id = compatibility_source_store_id()?;
    let placeholders = std::iter::repeat_n("?", entity_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT DISTINCT mappings.fact_id
         FROM memory_fact_entities AS links
         JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
         WHERE mappings.owner_kind = ? AND mappings.project_id = ?
           AND mappings.owner_json = ? AND mappings.source_store_id = ?
           AND links.entity_id IN ({placeholders})
         ORDER BY mappings.fact_id ASC LIMIT 257"
    );
    let mut values = Vec::with_capacity(entity_ids.len() + 4);
    values.push(libsql::Value::Text(key.kind.to_string()));
    values.push(libsql::Value::Text(key.project_id.clone()));
    values.push(libsql::Value::Text(key.json.clone()));
    values.push(libsql::Value::Text(source_store_id.as_str().to_owned()));
    values.extend(entity_ids.iter().copied().map(libsql::Value::Integer));
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        fact_ids.push(
            FactId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    if fact_ids.len() > 256 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility entity curation exceeds the fixed 256-fact bound",
        ));
    }
    Ok(fact_ids)
}

async fn compatibility_fact_entities_tx(
    transaction: &Transaction,
    legacy_fact_id: i64,
) -> FactStoreResult<Vec<String>> {
    let mut rows = transaction
        .query(
            "SELECT entities.name
             FROM memory_fact_entities AS links
             JOIN memory_entities AS entities ON entities.entity_id = links.entity_id
             WHERE links.fact_id = ?1
             ORDER BY entities.normalized_name ASC, entities.entity_id ASC",
            params![legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut entities = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        entities.push(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?);
    }
    Ok(entities)
}

async fn compatibility_merge_entities_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &CompatibilityFactMergeEntitiesV1,
    now: UtcMicros,
) -> FactStoreResult<Vec<FactId>> {
    let evidence =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let winner_id = operation.winner().legacy_entity_id();
    let (winner_name, winner_aliases) =
        compatibility_owner_entity_tx(transaction, owner, winner_id).await?;
    compatibility_entity_linked_to_evidence_tx(transaction, owner, winner_id, &evidence).await?;
    let mut entity_ids = vec![winner_id];
    let mut aliases = winner_aliases;
    for loser in operation.losers() {
        let loser_id = loser.legacy_entity_id();
        let (name, loser_aliases) =
            compatibility_owner_entity_tx(transaction, owner, loser_id).await?;
        compatibility_entity_linked_to_evidence_tx(transaction, owner, loser_id, &evidence).await?;
        entity_ids.push(loser_id);
        aliases.push(name);
        aliases.extend(loser_aliases);
    }
    let fact_ids = compatibility_owner_entity_fact_ids_tx(transaction, owner, &entity_ids).await?;
    let mut normalized_aliases = std::collections::BTreeMap::new();
    for alias in aliases {
        let alias = normalize_entity(&alias);
        if !alias.is_empty() && !alias.eq_ignore_ascii_case(&winner_name) {
            normalized_aliases
                .entry(alias.to_ascii_lowercase())
                .or_insert(alias);
        }
    }
    transaction
        .execute(
            "UPDATE memory_entities SET aliases = ?1, updated_at = ?2 WHERE entity_id = ?3",
            params![
                to_json(
                    &normalized_aliases.into_values().collect::<Vec<_>>(),
                    "serialize compatibility entity aliases",
                )?,
                compatibility_legacy_timestamp(now),
                winner_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    for loser in operation.losers() {
        let loser_id = loser.legacy_entity_id();
        transaction
            .execute(
                "INSERT OR IGNORE INTO memory_fact_entities(fact_id, entity_id)
                 SELECT fact_id, ?1 FROM memory_fact_entities WHERE entity_id = ?2",
                params![winner_id, loser_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_fact_entities WHERE entity_id = ?1",
                params![loser_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_entities WHERE entity_id = ?1
                 AND NOT EXISTS(SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1)",
                params![loser_id],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    let owner_key = OwnerKey::new(owner)?;
    for fact_id in &fact_ids {
        let Some(fact) = load_current_fact_tx(transaction, &owner_key, owner, fact_id).await?
        else {
            continue;
        };
        let Some(payload) = fact.payload() else {
            continue;
        };
        let mapping = compatibility_required_mapping_tx(transaction, owner, fact_id).await?;
        let entities =
            compatibility_fact_entities_tx(transaction, mapping.legacy_fact_id()).await?;
        let Some(sanitized) = compatibility_sanitize_payload(
            payload.content(),
            payload.category(),
            payload.tags(),
            &entities,
            payload.metadata(),
        )?
        else {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merged entities were rejected by the privacy sanitizer",
            ));
        };
        let source = compatibility_source_for_fact_tx(transaction, &mapping).await?;
        let batch = compatibility_curated_correction_batch(
            &fact,
            sanitized.payload.clone(),
            actor.cloned(),
            now,
        )?;
        compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_update_tx(
            db,
            transaction,
            owner,
            mapping.legacy_fact_id(),
            &sanitized.payload,
            &source,
            fact.trust(),
            now,
        )
        .await?;
    }
    Ok(fact_ids)
}

async fn compatibility_add_entity_alias_tx(
    db: &Database,
    transaction: &Transaction,
    owner: &FactOwnerV1,
    operation: &CompatibilityFactAddAliasV1,
    now: UtcMicros,
) -> FactStoreResult<Vec<FactId>> {
    let evidence =
        compatibility_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let entity_id = operation.entity().legacy_entity_id();
    let (name, mut aliases) = compatibility_owner_entity_tx(transaction, owner, entity_id).await?;
    compatibility_entity_linked_to_evidence_tx(transaction, owner, entity_id, &evidence).await?;
    let alias = normalize_entity(operation.alias());
    if alias.is_empty() || alias.eq_ignore_ascii_case(&name) {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility alias is not distinct from its entity",
        ));
    }
    aliases.push(alias);
    let mut canonical_aliases = std::collections::BTreeMap::new();
    for value in aliases {
        let value = normalize_entity(&value);
        if !value.is_empty() && !value.eq_ignore_ascii_case(&name) {
            canonical_aliases
                .entry(value.to_ascii_lowercase())
                .or_insert(value);
        }
    }
    transaction
        .execute(
            "UPDATE memory_entities SET aliases = ?1, updated_at = ?2 WHERE entity_id = ?3",
            params![
                to_json(
                    &canonical_aliases.into_values().collect::<Vec<_>>(),
                    "serialize compatibility entity aliases",
                )?,
                compatibility_legacy_timestamp(now),
                entity_id,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let fact_ids = compatibility_owner_entity_fact_ids_tx(transaction, owner, &[entity_id]).await?;
    for fact_id in &fact_ids {
        let mapping = compatibility_required_mapping_tx(transaction, owner, fact_id).await?;
        let mut rows = transaction
            .query(
                "SELECT category FROM memory_facts WHERE fact_id = ?1",
                params![mapping.legacy_fact_id()],
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
                    "compatibility alias fact is missing from the legacy mirror",
                )
            })?;
        let category =
            compatibility_proposal_category(&row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)?;
        compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, category, now).await?;
    }
    Ok(fact_ids)
}

fn compatibility_curation_operation_digest(
    operation: &CompatibilityFactCurationOperationV1,
) -> FactStoreResult<Value> {
    let evidence = |targets: &[CompatibilityFactTargetV1]| {
        targets
            .iter()
            .map(compatibility_target_digest)
            .collect::<FactStoreResult<Vec<_>>>()
    };
    match operation {
        CompatibilityFactCurationOperationV1::NormalizeTags(operation) => Ok(json!({
            "kind": "normalize_tags",
            "fact": compatibility_target_digest(operation.fact())?,
            "tags": operation.tags(),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
        CompatibilityFactCurationOperationV1::MergeEntities(operation) => Ok(json!({
            "kind": "merge_entities",
            "winner": operation.winner().legacy_entity_id(),
            "losers": operation.losers().iter().map(|target| target.legacy_entity_id()).collect::<Vec<_>>(),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
        CompatibilityFactCurationOperationV1::AddAlias(operation) => Ok(json!({
            "kind": "add_alias",
            "entity": operation.entity().legacy_entity_id(),
            "alias": operation.alias(),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
        CompatibilityFactCurationOperationV1::LinkFacts(operation) => Ok(json!({
            "kind": "link_facts",
            "source": compatibility_target_digest(operation.source())?,
            "target": compatibility_target_digest(operation.target())?,
            "relation": compatibility_relation_label(operation.relation()),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
            "source_label": operation.source_label(),
            "metadata": operation.metadata(),
        })),
        CompatibilityFactCurationOperationV1::RepairVector(operation) => Ok(json!({
            "kind": "repair_vector",
            "fact": compatibility_target_digest(operation.fact())?,
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
    }
}

async fn compatibility_record_oplog_tx(
    transaction: &Transaction,
    operation: &str,
    mapping: Option<&CompatibilityFactMappingV1>,
    detail: &Value,
    now: UtcMicros,
) -> FactStoreResult<()> {
    transaction
        .execute(
            "INSERT INTO memory_oplog(ts, op, fact_id, detail_json) VALUES(?1, ?2, ?3, ?4)",
            params![
                compatibility_legacy_timestamp(now),
                operation,
                mapping.and_then(CompatibilityFactMappingV1::legacy_fact_id),
                to_json(detail, "serialize compatibility oplog detail")?,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

async fn compatibility_replay_curation_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactCurationReceiptV1> {
    let ids = receipt
        .receipt
        .get("changed_fact_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility curation receipt changed facts are malformed",
            )
        })?;
    let mut fact_ids = Vec::with_capacity(ids.len());
    for id in ids {
        fact_ids.push(
            FactId::new(id.as_str().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility curation receipt fact id is malformed",
                )
            })?)
            .map_err(FactStoreError::from)?,
        );
    }
    let mappings =
        compatibility_curation_mappings_from_ids_tx(transaction, owner, &fact_ids).await?;
    let derived_repair = CompatibilityMemoryRepairStatsV1::new(
        compatibility_receipt_u64(&receipt.receipt, "missing_vectors_repaired")?,
        compatibility_receipt_u64(&receipt.receipt, "banks_rebuilt")?,
    );
    CompatibilityFactCurationReceiptV1::new(
        owner.clone(),
        mappings,
        compatibility_receipt_u64(&receipt.receipt, "normalized_tags")?,
        compatibility_receipt_u64(&receipt.receipt, "merged_entities")?,
        compatibility_receipt_u64(&receipt.receipt, "aliases_added")?,
        compatibility_receipt_u64(&receipt.receipt, "facts_linked")?,
        compatibility_receipt_u64(&receipt.receipt, "vectors_repaired")?,
        derived_repair,
    )
    .map_err(Into::into)
}

pub(super) async fn apply_compatibility_fact_curation_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactCurationBatchV1,
) -> FactCompatibilityResult<CompatibilityFactCurationReceiptV1> {
    let request_digest = compatibility_digest(json!({
        "owner": request.owner(),
        "actor": request.actor().map(ActorId::as_str),
        "min_confidence": request.min_confidence().as_f64(),
        "operations": request
            .operations()
            .iter()
            .map(compatibility_curation_operation_digest)
            .collect::<FactStoreResult<Vec<_>>>()?,
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "curation",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_curation_tx(transaction, request.owner(), &receipt).await;
    }
    let now = compatibility_now()?;
    let mut changed = Vec::new();
    let mut normalized_tags = 0_u64;
    let mut merged_entities = 0_u64;
    let mut aliases_added = 0_u64;
    let mut facts_linked = 0_u64;
    let mut vectors_repaired = 0_u64;
    for operation in request.operations() {
        match operation {
            CompatibilityFactCurationOperationV1::NormalizeTags(operation) => {
                changed.push(
                    compatibility_normalize_tags_tx(
                        db,
                        transaction,
                        request.owner(),
                        request.actor(),
                        operation,
                        now,
                    )
                    .await?,
                );
                normalized_tags = normalized_tags.saturating_add(1);
            }
            CompatibilityFactCurationOperationV1::MergeEntities(operation) => {
                changed.extend(
                    compatibility_merge_entities_tx(
                        db,
                        transaction,
                        request.owner(),
                        request.actor(),
                        operation,
                        now,
                    )
                    .await?,
                );
                merged_entities = merged_entities.saturating_add(1);
            }
            CompatibilityFactCurationOperationV1::AddAlias(operation) => {
                changed.extend(
                    compatibility_add_entity_alias_tx(
                        db,
                        transaction,
                        request.owner(),
                        operation,
                        now,
                    )
                    .await?,
                );
                aliases_added = aliases_added.saturating_add(1);
            }
            CompatibilityFactCurationOperationV1::LinkFacts(operation) => {
                let (fact_ids, _) = compatibility_link_facts_tx(
                    transaction,
                    request.owner(),
                    request.actor(),
                    operation,
                    now,
                )
                .await?;
                changed.extend(fact_ids);
                facts_linked = facts_linked.saturating_add(1);
            }
            CompatibilityFactCurationOperationV1::RepairVector(operation) => {
                changed.push(
                    compatibility_repair_vector_for_fact_tx(
                        db,
                        transaction,
                        request.owner(),
                        operation,
                        now,
                    )
                    .await?,
                );
                vectors_repaired = vectors_repaired.saturating_add(1);
            }
        }
    }
    let missing_vectors_repaired = compatibility_repair_missing_vectors_tx(
        db,
        transaction,
        request.owner(),
        COMPATIBILITY_REPAIR_VECTOR_BATCH,
    )
    .await?;
    let banks_rebuilt =
        compatibility_rebuild_dirty_banks_tx(db, transaction, request.owner()).await?;
    let mappings =
        compatibility_curation_mappings_from_ids_tx(transaction, request.owner(), &changed).await?;
    if mappings.len() > 256 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility curation changes exceed the fixed 256-fact receipt bound",
        )
        .into());
    }
    let receipt = json!({
        "changed_fact_ids": mappings.iter().map(|mapping| mapping.fact_id().as_str()).collect::<Vec<_>>(),
        "normalized_tags": normalized_tags,
        "merged_entities": merged_entities,
        "aliases_added": aliases_added,
        "facts_linked": facts_linked,
        "vectors_repaired": vectors_repaired,
        "missing_vectors_repaired": missing_vectors_repaired,
        "banks_rebuilt": banks_rebuilt,
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "curation",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    if let Some(mapping) = mappings.first() {
        compatibility_record_oplog_tx(
            transaction,
            "curate_apply",
            Some(mapping),
            &json!({
                "normalized_tags": normalized_tags,
                "merged_entities": merged_entities,
                "aliases_added": aliases_added,
                "facts_linked": facts_linked,
                "vectors_repaired": vectors_repaired,
            }),
            now,
        )
        .await?;
    }
    CompatibilityFactCurationReceiptV1::new(
        request.owner().clone(),
        mappings,
        normalized_tags,
        merged_entities,
        aliases_added,
        facts_linked,
        vectors_repaired,
        CompatibilityMemoryRepairStatsV1::new(missing_vectors_repaired, banks_rebuilt),
    )
    .map_err(Into::into)
}

fn compatibility_merge_removal_batch(
    owner: &FactOwnerV1,
    fact_id: &FactId,
    previous: PayloadAccessState,
    expected_last_event_id: Option<FactEventId>,
    winner: &FactId,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    let curated = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::MergedInto {
                fact_id: winner.clone(),
            },
            evidence_ids: Vec::new(),
        },
        now,
        actor.clone(),
    )?;
    let deleted = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous,
            current: PayloadAccessState::Deleted,
        },
        compatibility_event_time(now, 1)?,
        actor,
    )?;
    FactWriteBatch::new(
        fact_id.clone(),
        owner.clone(),
        None,
        vec![curated, deleted],
        Vec::new(),
        Vec::new(),
        None,
        expected_last_event_id,
    )
}

async fn compatibility_mirror_category_tx(
    transaction: &Transaction,
    legacy_fact_id: i64,
) -> FactStoreResult<FactCategoryV1> {
    let mut rows = transaction
        .query(
            "SELECT category FROM memory_facts WHERE fact_id = ?1",
            params![legacy_fact_id],
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
                "compatibility legacy mirror fact is missing",
            )
        })?;
    compatibility_proposal_category(&row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)
}

async fn compatibility_replay_merge_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<CompatibilityFactMergeOutcomeV1> {
    let winner_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility merge receipt winner is missing",
        )
    })?;
    let winner = compatibility_curation_mappings_from_ids_tx(
        transaction,
        owner,
        std::slice::from_ref(winner_id),
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility merge receipt winner mapping is missing",
        )
    })?;
    let deleted_ids = receipt
        .receipt
        .get("deleted_loser_fact_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merge receipt deleted losers are malformed",
            )
        })?;
    let mut ids = Vec::with_capacity(deleted_ids.len());
    for id in deleted_ids {
        ids.push(
            FactId::new(id.as_str().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility merge receipt loser id is malformed",
                )
            })?)
            .map_err(FactStoreError::from)?,
        );
    }
    let deleted_losers =
        compatibility_curation_mappings_from_ids_tx(transaction, owner, &ids).await?;
    let content_updated = receipt
        .receipt
        .get("content_updated")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merge receipt content flag is malformed",
            )
        })?;
    CompatibilityFactMergeOutcomeV1::new(owner.clone(), winner, content_updated, deleted_losers)
        .map_err(Into::into)
}

async fn compatibility_rewire_merge_relations_tx(
    transaction: &Transaction,
    owner: &FactOwnerV1,
    winner_fact_id: &FactId,
    winner_legacy_fact_id: i64,
    loser_fact_ids: &[FactId],
    loser_legacy_fact_ids: &[i64],
    now: UtcMicros,
) -> FactStoreResult<()> {
    if loser_fact_ids.is_empty() {
        return Ok(());
    }
    let legacy_placeholders = std::iter::repeat_n("?", loser_legacy_fact_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let legacy_sql = format!(
        "SELECT source_fact_id, target_fact_id, relation, confidence, source, metadata
         FROM memory_fact_relations
         WHERE source_fact_id IN ({legacy_placeholders})
            OR target_fact_id IN ({legacy_placeholders})
         ORDER BY source_fact_id ASC, target_fact_id ASC, relation ASC
         LIMIT 257"
    );
    let mut legacy_values = Vec::with_capacity(loser_legacy_fact_ids.len() * 2);
    legacy_values.extend(
        loser_legacy_fact_ids
            .iter()
            .copied()
            .map(libsql::Value::Integer),
    );
    legacy_values.extend(
        loser_legacy_fact_ids
            .iter()
            .copied()
            .map(libsql::Value::Integer),
    );
    let mut legacy_rows = transaction
        .query(&legacy_sql, legacy_values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut legacy_relations = Vec::new();
    while let Some(row) = legacy_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        legacy_relations.push((
            row_i64(&row, 0, COMPATIBILITY_WRITE_OPERATION)?,
            row_i64(&row, 1, COMPATIBILITY_WRITE_OPERATION)?,
            row_string(&row, 2, COMPATIBILITY_WRITE_OPERATION)?,
            Confidence::new(row_f64(&row, 3, COMPATIBILITY_WRITE_OPERATION)?)?,
            row_string(&row, 4, COMPATIBILITY_WRITE_OPERATION)?,
            from_json::<Value>(
                &row_string(&row, 5, COMPATIBILITY_WRITE_OPERATION)?,
                COMPATIBILITY_WRITE_OPERATION,
            )?,
        ));
    }
    drop(legacy_rows);
    if legacy_relations.len() > 256 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility merge relation rewiring exceeds the fixed 256-relation bound",
        ));
    }
    let loser_legacy = loser_legacy_fact_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    for (source, target, _, _, _, _) in &legacy_relations {
        for endpoint in [source, target] {
            if compatibility_fact_for_legacy_id_tx(transaction, owner, *endpoint)
                .await?
                .is_none()
            {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility merge relation crosses an owner boundary",
                ));
            }
        }
    }
    transaction
        .execute(
            &format!(
                "DELETE FROM memory_fact_relations
                 WHERE source_fact_id IN ({legacy_placeholders})
                    OR target_fact_id IN ({legacy_placeholders})"
            ),
            {
                let mut values = Vec::with_capacity(loser_legacy_fact_ids.len() * 2);
                values.extend(
                    loser_legacy_fact_ids
                        .iter()
                        .copied()
                        .map(libsql::Value::Integer),
                );
                values.extend(
                    loser_legacy_fact_ids
                        .iter()
                        .copied()
                        .map(libsql::Value::Integer),
                );
                values
            },
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    for (source, target, relation, confidence, source_label, metadata) in legacy_relations {
        let source = if loser_legacy.contains(&source) {
            winner_legacy_fact_id
        } else {
            source
        };
        let target = if loser_legacy.contains(&target) {
            winner_legacy_fact_id
        } else {
            target
        };
        if source == target {
            continue;
        }
        let relation = match relation.as_str() {
            "supports" => CompatibilityFactRelationV1::Supports,
            "contradicts" => CompatibilityFactRelationV1::Contradicts,
            "supersedes" => CompatibilityFactRelationV1::Supersedes,
            "derived_from" => CompatibilityFactRelationV1::DerivedFrom,
            _ => {
                return Err(storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility merge found an unsupported legacy relation",
                ));
            }
        };
        compatibility_upsert_legacy_relation_tx(
            transaction,
            source,
            target,
            relation,
            confidence,
            &compatibility_source_label(Some(&source_label))?,
            &compatibility_sanitized_relation_metadata(&metadata).await?,
            compatibility_legacy_timestamp(now),
        )
        .await?;
    }

    let key = OwnerKey::new(owner)?;
    let canonical_placeholders = std::iter::repeat_n("?", loser_fact_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let canonical_sql = format!(
        "SELECT source_fact_id, target_fact_id, relation, confidence, source_label,
                provenance_json, evidence_fact_ids_json, occurred_at
         FROM memory_v2_fact_relations
         WHERE owner_kind = ? AND project_id = ?
           AND (source_fact_id IN ({canonical_placeholders})
                OR target_fact_id IN ({canonical_placeholders}))
         ORDER BY source_fact_id ASC, target_fact_id ASC, relation ASC
         LIMIT 257"
    );
    let mut canonical_values = Vec::with_capacity(loser_fact_ids.len() * 2 + 2);
    canonical_values.push(libsql::Value::Text(key.kind.to_string()));
    canonical_values.push(libsql::Value::Text(key.project_id.clone()));
    for _ in 0..2 {
        canonical_values.extend(
            loser_fact_ids
                .iter()
                .map(|fact_id| libsql::Value::Text(fact_id.as_str().to_owned())),
        );
    }
    let mut canonical_rows = transaction
        .query(&canonical_sql, canonical_values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    let mut canonical_relations = Vec::new();
    while let Some(row) = canonical_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        canonical_relations.push((
            FactId::new(row_string(&row, 0, COMPATIBILITY_WRITE_OPERATION)?)?,
            FactId::new(row_string(&row, 1, COMPATIBILITY_WRITE_OPERATION)?)?,
            row_string(&row, 2, COMPATIBILITY_WRITE_OPERATION)?,
            Confidence::new(row_f64(&row, 3, COMPATIBILITY_WRITE_OPERATION)?)?,
            row_string(&row, 4, COMPATIBILITY_WRITE_OPERATION)?,
            row_string(&row, 5, COMPATIBILITY_WRITE_OPERATION)?,
            row_string(&row, 6, COMPATIBILITY_WRITE_OPERATION)?,
            row_i64(&row, 7, COMPATIBILITY_WRITE_OPERATION)?,
        ));
    }
    drop(canonical_rows);
    if canonical_relations.len() > 256 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "canonical merge relation rewiring exceeds the fixed 256-relation bound",
        ));
    }
    let loser_canonical = loser_fact_ids.iter().cloned().collect::<BTreeSet<_>>();
    transaction
        .execute(
            &format!(
                "DELETE FROM memory_v2_fact_relations
                 WHERE owner_kind = ? AND project_id = ?
                   AND (source_fact_id IN ({canonical_placeholders})
                        OR target_fact_id IN ({canonical_placeholders}))"
            ),
            {
                let mut values = Vec::with_capacity(loser_fact_ids.len() * 2 + 2);
                values.push(libsql::Value::Text(key.kind.to_string()));
                values.push(libsql::Value::Text(key.project_id.clone()));
                for _ in 0..2 {
                    values.extend(
                        loser_fact_ids
                            .iter()
                            .map(|fact_id| libsql::Value::Text(fact_id.as_str().to_owned())),
                    );
                }
                values
            },
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    for (
        source,
        target,
        relation,
        confidence,
        source_label,
        provenance_json,
        evidence_json,
        occurred_at,
    ) in canonical_relations
    {
        let source = if loser_canonical.contains(&source) {
            winner_fact_id
        } else {
            &source
        };
        let target = if loser_canonical.contains(&target) {
            winner_fact_id
        } else {
            &target
        };
        if source == target {
            continue;
        }
        transaction
            .execute(
                "INSERT INTO memory_v2_fact_relations(
                    owner_kind, project_id, source_fact_id, target_fact_id, relation,
                    confidence, source_label, provenance_json, evidence_fact_ids_json,
                    occurred_at, updated_at
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(owner_kind, project_id, source_fact_id, target_fact_id, relation)
                 DO UPDATE SET confidence = excluded.confidence,
                               source_label = excluded.source_label,
                               provenance_json = excluded.provenance_json,
                               evidence_fact_ids_json = excluded.evidence_fact_ids_json,
                               updated_at = excluded.updated_at",
                params![
                    key.kind,
                    key.project_id.as_str(),
                    source.as_str(),
                    target.as_str(),
                    relation,
                    confidence.as_f64(),
                    compatibility_source_label(Some(&source_label))?,
                    provenance_json,
                    evidence_json,
                    occurred_at,
                    now.0,
                ],
            )
            .await
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

pub(super) async fn merge_compatibility_facts_tx(
    db: &Database,
    transaction: &Transaction,
    request: &CompatibilityFactMergeCommandV1,
) -> FactCompatibilityResult<CompatibilityFactMergeOutcomeV1> {
    let request_digest = compatibility_digest(json!({
        "owner": request.owner(),
        "winner": compatibility_target_digest(request.winner())?,
        "losers": request
            .losers()
            .iter()
            .map(compatibility_target_digest)
            .collect::<FactStoreResult<Vec<_>>>()?,
        "merged_content": request.merged_content(),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "merge",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_merge_tx(transaction, request.owner(), &receipt).await;
    }
    let now = compatibility_now()?;
    let (winner_id, winner_fact, winner_mapping) =
        compatibility_available_curation_fact_tx(transaction, request.winner()).await?;
    let mut content_updated = false;
    if let Some(content) = request.merged_content() {
        let payload = winner_fact
            .payload()
            .ok_or(FactStoreError::PayloadAccessMismatch)?;
        let Some(sanitized) = compatibility_sanitize_payload(
            content,
            payload.category(),
            payload.tags(),
            payload.entities(),
            payload.metadata(),
        )?
        else {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merged content was rejected by the privacy sanitizer",
            )
            .into());
        };
        let source = compatibility_source_for_fact_tx(
            transaction,
            winner_mapping
                .legacy_mapping()
                .ok_or(FactStoreError::FactMismatch)?,
        )
        .await?;
        let batch = compatibility_curated_correction_batch(
            &winner_fact,
            sanitized.payload.clone(),
            request.actor().cloned(),
            now,
        )?;
        compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_update_tx(
            db,
            transaction,
            request.owner(),
            winner_mapping
                .legacy_fact_id()
                .ok_or(FactStoreError::FactMismatch)?,
            &sanitized.payload,
            &source,
            winner_fact.trust(),
            now,
        )
        .await?;
        content_updated = true;
    }
    let owner_key = OwnerKey::new(request.owner())?;
    let mut loser_ids = Vec::with_capacity(request.losers().len());
    let mut loser_legacy_ids = Vec::with_capacity(request.losers().len());
    let mut pending_deletes = Vec::with_capacity(request.losers().len());
    for target in request.losers() {
        let loser_id = resolve_compatibility_target_tx(transaction, target)
            .await?
            .ok_or_else(|| {
                let loser_label = target
                    .legacy_query()
                    .map(|query| query.legacy_fact_id().to_string())
                    .or_else(|| {
                        target
                            .canonical_fact_id()
                            .map(|fact_id| fact_id.as_str().to_string())
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    format!("compatibility merge loser fact {loser_label} not found"),
                )
            })?;
        if loser_id == winner_id {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility merge winner cannot be a loser",
            )
            .into());
        }
        let projection = load_current_projection(transaction, &owner_key, &loser_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility merge loser projection is missing",
                )
            })?;
        let mapping =
            compatibility_required_mapping_tx(transaction, request.owner(), &loser_id).await?;
        loser_ids.push(loser_id.clone());
        loser_legacy_ids.push(mapping.legacy_fact_id());
        if projection.access != PayloadAccessState::Deleted {
            let category =
                compatibility_mirror_category_tx(transaction, mapping.legacy_fact_id()).await?;
            pending_deletes.push((
                loser_id,
                projection.access,
                projection.last_event_id.clone(),
                mapping,
                category,
            ));
        }
    }
    compatibility_rewire_merge_relations_tx(
        transaction,
        request.owner(),
        &winner_id,
        winner_mapping
            .legacy_fact_id()
            .ok_or(FactStoreError::FactMismatch)?,
        &loser_ids,
        &loser_legacy_ids,
        now,
    )
    .await?;
    let mut deleted_ids = Vec::new();
    for (loser_id, previous_access, expected_last_event_id, mapping, category) in pending_deletes {
        let batch = compatibility_merge_removal_batch(
            request.owner(),
            &loser_id,
            previous_access,
            expected_last_event_id,
            &winner_id,
            request.actor().cloned(),
            now,
        )?;
        compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_delete_tx(
            db,
            transaction,
            request.owner(),
            mapping.legacy_fact_id(),
            category,
            now,
        )
        .await?;
        deleted_ids.push(loser_id);
    }
    let winner = compatibility_curation_mappings_from_ids_tx(
        transaction,
        request.owner(),
        std::slice::from_ref(&winner_id),
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| {
        storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility merge winner mapping is missing",
        )
    })?;
    let deleted_losers =
        compatibility_curation_mappings_from_ids_tx(transaction, request.owner(), &deleted_ids)
            .await?;
    let receipt = json!({
        "content_updated": content_updated,
        "deleted_loser_fact_ids": deleted_ids.iter().map(FactId::as_str).collect::<Vec<_>>(),
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "merge",
        &request_digest,
        Some(&winner_id),
        None,
        &receipt,
        now,
    )
    .await?;
    compatibility_record_oplog_tx(
        transaction,
        "curate_apply",
        Some(&winner),
        &json!({
            "merged_fact_count": deleted_losers.len(),
            "content_updated": content_updated,
        }),
        now,
    )
    .await?;
    CompatibilityFactMergeOutcomeV1::new(
        request.owner().clone(),
        winner,
        content_updated,
        deleted_losers,
    )
    .map_err(Into::into)
}
