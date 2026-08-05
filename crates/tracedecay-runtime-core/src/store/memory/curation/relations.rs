//! Relation labels, tag normalization, fact links, and curation correction batches.

use super::super::crud::{
    compatibility_commit_batch_tx, compatibility_mirror_update_tx, compatibility_sanitize_payload,
    load_current_fact_tx,
};
use super::super::primitives::{
    COMPATIBILITY_WRITE_OPERATION, OwnerKey, compatibility_event_time,
    compatibility_legacy_timestamp, compatibility_source_label, row_string, storage_error,
    storage_message, to_json,
};
use super::super::projection::{
    compatibility_required_mapping_tx, compatibility_source_for_fact_tx,
    resolve_compatibility_target_tx,
};
use crate::db::Database;
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::privacy::{
    MemoryFactSanitizationV1, sanitize_memory_fact_payload, verify_memory_fact_sanitization,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tracedecay_domain::{
    ActorId, Confidence, FactAssertionKindV1, FactAssertionV1, FactCurationActionV1, FactEventId,
    FactId, FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, UtcMicros,
};
use tracedecay_store::{
    CompatibilityFactIdV1, CompatibilityFactLinkV1, CompatibilityFactMappingV1,
    CompatibilityFactNormalizeTagsV1, CompatibilityFactRelationV1, CompatibilityFactTargetV1,
    CompatibilityRelationProvenanceV1, FactStoreError, FactStoreResult, FactWriteBatch,
    StoredFactV1,
};
pub(super) fn compatibility_relation_label(relation: CompatibilityFactRelationV1) -> &'static str {
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

pub(in crate::store::memory) async fn compatibility_available_curation_fact_tx(
    transaction: &Transaction<'_>,
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

pub(in crate::store::memory) async fn compatibility_curation_evidence_ids_tx(
    transaction: &Transaction<'_>,
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

pub(super) async fn compatibility_record_curated_correction_provenance_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    corrected_fact_id: &FactId,
    evidence_fact_ids: &[FactId],
    confidence: Confidence,
    operation: &str,
    actor: Option<&ActorId>,
    now: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let evidence_json = to_json(
        &evidence_fact_ids
            .iter()
            .map(FactId::as_str)
            .collect::<Vec<_>>(),
        "serialize curated correction evidence facts",
    )?;
    let source_label =
        compatibility_source_label(Some(&format!("compatibility_curation_{operation}")))?;
    let provenance = compatibility_sanitized_relation_provenance(&json!({
        "actor_id": actor.map(ActorId::as_str),
        "operation": operation,
    }))
    .await?;
    let provenance_json = to_json(&provenance, "serialize curated correction provenance")?;
    if evidence_fact_ids
        .iter()
        .any(|evidence_fact_id| evidence_fact_id == corrected_fact_id)
    {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
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
            .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

pub(super) async fn compatibility_curation_mappings_from_ids_tx(
    transaction: &Transaction<'_>,
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

pub(super) async fn compatibility_sanitized_relation_provenance(
    metadata: &Value,
) -> FactStoreResult<CompatibilityRelationProvenanceV1> {
    match sanitize_memory_fact_payload(metadata.clone())
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?
    {
        MemoryFactSanitizationV1::Durable { payload, receipt } => {
            CompatibilityRelationProvenanceV1::new(payload, receipt)
        }
        MemoryFactSanitizationV1::Quarantined => Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility relation metadata was rejected by the privacy sanitizer",
        )),
    }
}

pub(super) async fn compatibility_legacy_relation_provenance(
    value: &Value,
) -> FactStoreResult<CompatibilityRelationProvenanceV1> {
    if value.get("metadata").is_some() || value.get("sanitization_receipt").is_some() {
        return serde_json::from_value(value.clone()).map_err(|error| {
            storage_error(
                COMPATIBILITY_WRITE_OPERATION,
                format!("invalid receipt-bound legacy relation provenance: {error}"),
            )
        });
    }
    compatibility_sanitized_relation_provenance(value).await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn compatibility_upsert_legacy_relation_tx(
    transaction: &Transaction<'_>,
    source_legacy_fact_id: i64,
    target_legacy_fact_id: i64,
    relation: CompatibilityFactRelationV1,
    confidence: Confidence,
    source_label: &str,
    provenance: &CompatibilityRelationProvenanceV1,
    timestamp: i64,
) -> FactStoreResult<()> {
    verify_memory_fact_sanitization(provenance.metadata(), provenance.sanitization_receipt())
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
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
                to_json(provenance, "serialize compatibility relation provenance")?,
                timestamp,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    Ok(())
}

pub(super) async fn compatibility_link_facts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &CompatibilityFactLinkV1,
    now: UtcMicros,
) -> FactStoreResult<(Vec<FactId>, Option<FactEventId>)> {
    verify_memory_fact_sanitization(
        operation.provenance().metadata(),
        operation.provenance().sanitization_receipt(),
    )
    .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
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
    let provenance = operation.provenance();
    let key = OwnerKey::new(owner)?;
    let evidence_fact_ids_json = to_json(
        &evidence_fact_ids
            .iter()
            .map(FactId::as_str)
            .collect::<Vec<_>>(),
        "serialize compatibility relation evidence",
    )?;
    let provenance_json = to_json(provenance, "serialize compatibility relation provenance")?;
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
        provenance,
        compatibility_legacy_timestamp(now),
    )
    .await?;
    Ok((vec![source_fact_id, target_fact_id], event_id))
}

pub(super) fn compatibility_curated_correction_batch(
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

pub(super) async fn compatibility_normalize_tags_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &CompatibilityFactNormalizeTagsV1,
    now: UtcMicros,
) -> FactStoreResult<FactId> {
    let evidence =
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
    compatibility_record_curated_correction_provenance_tx(
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
