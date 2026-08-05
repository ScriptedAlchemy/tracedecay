//! Relation labels, tag normalization, fact links, and curation correction batches.

use super::super::crud::{
    commit_batch_tx, load_current_fact_tx, mirror_update_tx, sanitize_payload,
};
use super::super::primitives::{
    FACT_WRITE_OPERATION, OwnerKey, event_time, fact_source_label, legacy_timestamp, row_string,
    storage_error, storage_message, to_json,
};
use super::super::projection::{required_mapping_tx, resolve_target_tx, source_for_fact_tx};
use crate::db::Database;
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::privacy::{MemoryFactSanitizationV1, sanitize_memory_fact_payload};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionKindV1, FactAssertionV1, FactCurationActionV1, FactEventId,
    FactId, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, UtcMicros,
};
use tracedecay_store::{
    FactLineageError, FactLineageResult, FactLink, FactMapping, FactNormalizeTags, FactRelation,
    FactTarget, FactWriteBatch, OwnedFactId, StoredFactV1,
};
pub(super) fn relation_label(relation: FactRelation) -> &'static str {
    match relation {
        FactRelation::Supports => "supports",
        FactRelation::Contradicts => "contradicts",
        FactRelation::Supersedes => "supersedes",
        FactRelation::DerivedFrom => "derived_from",
    }
}

fn relations_conflict(left: FactRelation, right: FactRelation) -> bool {
    matches!(
        (left, right),
        (FactRelation::Supports, FactRelation::Contradicts)
            | (FactRelation::Contradicts, FactRelation::Supports)
    )
}

fn normalize_tags(tags: &[String]) -> Vec<String> {
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

pub(in crate::store::memory) async fn available_curation_fact_tx(
    transaction: &Transaction<'_>,
    target: &FactTarget,
) -> FactLineageResult<(FactId, StoredFactV1, FactMapping)> {
    let fact_id = resolve_target_tx(transaction, target)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility curation target is missing",
            )
        })?;
    let owner_key = OwnerKey::new(target.owner())?;
    let fact = load_current_fact_tx(transaction, &owner_key, target.owner(), &fact_id)
        .await?
        .ok_or_else(|| {
            storage_message(
                FACT_WRITE_OPERATION,
                "compatibility curation target is unavailable",
            )
        })?;
    if fact.payload().is_none() {
        return Err(FactLineageError::PayloadAccessMismatch);
    }
    let mapping = required_mapping_tx(transaction, target.owner(), &fact_id).await?;
    let mapping = FactMapping::new(
        OwnedFactId::new(target.owner().clone(), fact_id.clone())?,
        Some(mapping),
    )?;
    Ok((fact_id, fact, mapping))
}

pub(in crate::store::memory) async fn curation_evidence_ids_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    evidence: &[FactTarget],
) -> FactLineageResult<Vec<FactId>> {
    let mut ids = Vec::with_capacity(evidence.len());
    let mut seen = BTreeSet::new();
    for target in evidence {
        if target.owner() != owner {
            return Err(FactLineageError::OwnerMismatch);
        }
        let (fact_id, _, _) = available_curation_fact_tx(transaction, target).await?;
        if !seen.insert(fact_id.clone()) {
            return Err(storage_message(
                FACT_WRITE_OPERATION,
                "compatibility curation evidence resolved to duplicate facts",
            ));
        }
        ids.push(fact_id);
    }
    Ok(ids)
}

pub(super) async fn record_curated_correction_provenance_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    corrected_fact_id: &FactId,
    evidence_fact_ids: &[FactId],
    confidence: Confidence,
    operation: &str,
    actor: Option<&ActorId>,
    now: UtcMicros,
) -> FactLineageResult<()> {
    let key = OwnerKey::new(owner)?;
    let evidence_json = to_json(
        &evidence_fact_ids
            .iter()
            .map(FactId::as_str)
            .collect::<Vec<_>>(),
        "serialize curated correction evidence facts",
    )?;
    let source_label = fact_source_label(Some(&format!("curation_{operation}")))?;
    let provenance_json = to_json(
        &json!({
            "actor_id": actor.map(ActorId::as_str),
            "operation": operation,
        }),
        "serialize curated correction provenance",
    )?;
    if evidence_fact_ids
        .iter()
        .any(|evidence_fact_id| evidence_fact_id == corrected_fact_id)
    {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "curated correction evidence cannot be the corrected fact",
        ));
    }
    for evidence_fact_id in evidence_fact_ids {
        transaction
            .execute(
                "INSERT INTO memory_v2_fact_relations(
                    owner_kind, project_id, source_fact_id, target_fact_id, relation,
                    confidence, source_label, provenance_json, evidence_fact_ids_json,
                    occurred_at, updated_at
                 ) VALUES(?1, ?2, ?3, ?4, 'derived_from', ?5, ?6, ?7, ?8, ?9, ?9)
                 ON CONFLICT(owner_kind, project_id, source_fact_id, target_fact_id, relation)
                 DO UPDATE SET confidence = excluded.confidence,
                               source_label = excluded.source_label,
                               provenance_json = excluded.provenance_json,
                               evidence_fact_ids_json = excluded.evidence_fact_ids_json,
                               updated_at = excluded.updated_at",
                params![
                    key.kind,
                    key.project_id.as_str(),
                    corrected_fact_id.as_str(),
                    evidence_fact_id.as_str(),
                    confidence.as_f64(),
                    source_label.as_str(),
                    provenance_json.as_str(),
                    evidence_json.as_str(),
                    now.0,
                ],
            )
            .await
            .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    }
    Ok(())
}

pub(super) async fn curation_mappings_from_ids_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    ids: &[FactId],
) -> FactLineageResult<Vec<FactMapping>> {
    let mut mappings = Vec::with_capacity(ids.len());
    let mut seen = BTreeSet::new();
    for fact_id in ids {
        if !seen.insert(fact_id.clone()) {
            continue;
        }
        let legacy_mapping = required_mapping_tx(transaction, owner, fact_id).await?;
        mappings.push(FactMapping::new(
            OwnedFactId::new(owner.clone(), fact_id.clone())?,
            Some(legacy_mapping),
        )?);
    }
    Ok(mappings)
}

pub(super) async fn sanitized_relation_metadata(metadata: &Value) -> FactLineageResult<Value> {
    match sanitize_memory_fact_payload(metadata.clone())
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?
    {
        MemoryFactSanitizationV1::Durable { payload, .. } => Ok(payload),
        MemoryFactSanitizationV1::Quarantined => Err(storage_message(
            FACT_WRITE_OPERATION,
            "compatibility relation metadata was rejected by the privacy sanitizer",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn upsert_legacy_relation_tx(
    transaction: &Transaction<'_>,
    source_legacy_fact_id: i64,
    target_legacy_fact_id: i64,
    relation: FactRelation,
    confidence: Confidence,
    source_label: &str,
    metadata: &Value,
    timestamp: i64,
) -> FactLineageResult<()> {
    let mut rows = transaction
        .query(
            "SELECT relation FROM memory_fact_relations
             WHERE source_fact_id = ?1 AND target_fact_id = ?2",
            params![source_legacy_fact_id, target_legacy_fact_id],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?
    {
        let stored = match row_string(&row, 0, FACT_WRITE_OPERATION)?.as_str() {
            "supports" => FactRelation::Supports,
            "contradicts" => FactRelation::Contradicts,
            "supersedes" => FactRelation::Supersedes,
            "derived_from" => FactRelation::DerivedFrom,
            _ => {
                return Err(storage_message(
                    FACT_WRITE_OPERATION,
                    "legacy compatibility relation has an unsupported kind",
                ));
            }
        };
        if relations_conflict(stored, relation) {
            return Err(storage_message(
                FACT_WRITE_OPERATION,
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
                relation_label(relation),
                confidence.as_f64(),
                source_label,
                to_json(metadata, "serialize compatibility relation metadata")?,
                timestamp,
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    Ok(())
}

pub(super) async fn link_facts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &FactLink,
    now: UtcMicros,
) -> FactLineageResult<(Vec<FactId>, Option<FactEventId>)> {
    let (source_fact_id, source_fact, source_mapping) =
        available_curation_fact_tx(transaction, operation.source()).await?;
    let (target_fact_id, _, target_mapping) =
        available_curation_fact_tx(transaction, operation.target()).await?;
    if source_fact_id == target_fact_id {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "compatibility curation relation cannot target itself",
        ));
    }
    let evidence_fact_ids =
        curation_evidence_ids_tx(transaction, owner, operation.evidence_facts()).await?;
    let source_label = fact_source_label(Some(operation.source_label()))?;
    let metadata = sanitized_relation_metadata(operation.metadata()).await?;
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
                relation_label(operation.relation()),
                operation.confidence().as_f64(),
                source_label.clone(),
                provenance_json,
                evidence_fact_ids_json,
                now.0,
            ],
        )
        .await
        .map_err(|error| storage_error(FACT_WRITE_OPERATION, error))?;
    let event_id = match operation.relation() {
        FactRelation::Supports | FactRelation::DerivedFrom => None,
        FactRelation::Contradicts | FactRelation::Supersedes => {
            let action = match operation.relation() {
                FactRelation::Contradicts => FactCurationActionV1::ContradictedBy {
                    fact_id: target_fact_id.clone(),
                },
                FactRelation::Supersedes => FactCurationActionV1::SupersededBy {
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
            let (receipt, _) = commit_batch_tx(transaction, &batch).await?;
            Some(receipt.last_event_id().clone())
        }
    };
    upsert_legacy_relation_tx(
        transaction,
        source_mapping
            .legacy_fact_id()
            .ok_or(FactLineageError::FactMismatch)?,
        target_mapping
            .legacy_fact_id()
            .ok_or(FactLineageError::FactMismatch)?,
        operation.relation(),
        operation.confidence(),
        &source_label,
        &metadata,
        legacy_timestamp(now),
    )
    .await?;
    Ok((vec![source_fact_id, target_fact_id], event_id))
}

pub(super) fn curated_correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactLineageResult<FactWriteBatch> {
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
        event_time(now, 1)?,
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

pub(super) async fn normalize_tags_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &FactNormalizeTags,
    now: UtcMicros,
) -> FactLineageResult<FactId> {
    let evidence = curation_evidence_ids_tx(transaction, owner, operation.evidence_facts()).await?;
    let (fact_id, fact, mapping) =
        available_curation_fact_tx(transaction, operation.fact()).await?;
    let payload = fact
        .payload()
        .ok_or(FactLineageError::PayloadAccessMismatch)?;
    let tags = normalize_tags(operation.tags());
    let Some(sanitized) = sanitize_payload(
        payload.content(),
        payload.category(),
        &tags,
        payload.entities(),
        payload.metadata(),
    )?
    else {
        return Err(storage_message(
            FACT_WRITE_OPERATION,
            "compatibility normalized tags were rejected by the privacy sanitizer",
        ));
    };
    let source = source_for_fact_tx(
        transaction,
        mapping
            .legacy_mapping()
            .ok_or(FactLineageError::FactMismatch)?,
    )
    .await?;
    let batch = curated_correction_batch(&fact, sanitized.payload.clone(), actor.cloned(), now)?;
    commit_batch_tx(transaction, &batch).await?;
    record_curated_correction_provenance_tx(
        transaction,
        owner,
        &fact_id,
        &evidence,
        operation.confidence(),
        "normalize_tags",
        actor,
        now,
    )
    .await?;
    mirror_update_tx(
        db,
        transaction,
        owner,
        mapping
            .legacy_fact_id()
            .ok_or(FactLineageError::FactMismatch)?,
        &sanitized.payload,
        &source,
        fact.trust(),
        now,
    )
    .await?;
    Ok(fact_id)
}
