//! Canonical relation and tag curation over fact lineage and assertions.

use std::collections::BTreeSet;

use tracedecay_domain::{
    ActorId, Confidence, FactAssertionKindV1, FactAssertionV1, FactCurationActionV1, FactId,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, FactPayloadV1, FactRelationKindV1,
    UtcMicros,
};
use tracedecay_store::{
    FactCommitReceipt, FactStoreError, FactStoreResult, FactWriteBatch, ProjectMemoryFactIdV1,
    ProjectMemoryFactLinkV1, ProjectMemoryFactNormalizeTagsV1, StoredFactV1,
};

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;

use super::super::crud::{commit_batch_tx, load_current_fact_tx, sanitize_payload};
use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, project_memory_event_time, row_string, storage_error,
    storage_message,
};

fn relation_kinds_conflict(left: FactRelationKindV1, right: FactRelationKindV1) -> bool {
    matches!(
        (left, right),
        (
            FactRelationKindV1::Supports,
            FactRelationKindV1::Contradicts
        ) | (
            FactRelationKindV1::Contradicts,
            FactRelationKindV1::Supports
        )
    )
}

async fn ensure_relation_conflict_free_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    relation: &tracedecay_domain::FactRelationV1,
) -> FactStoreResult<()> {
    if !matches!(
        relation.kind(),
        FactRelationKindV1::Supports | FactRelationKindV1::Contradicts
    ) {
        return Ok(());
    }
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT event_json
             FROM memory_v2_lineage_events
             WHERE owner_kind = ?1 AND project_id = ?2 AND fact_id = ?3
               AND json_extract(event_json, '$.kind.kind') = 'curated'
               AND (
                 (?5 = 'supports'
                  AND json_extract(event_json, '$.kind.action.kind') = 'contradicted_by'
                  AND json_extract(event_json, '$.kind.action.fact_id') = ?4)
                 OR
                 (json_extract(event_json, '$.kind.action.kind') = 'linked'
                  AND json_extract(event_json, '$.kind.action.relation.target_fact_id') = ?4
                  AND (
                    (?5 = 'supports'
                     AND json_extract(event_json, '$.kind.action.relation.kind') = 'contradicts')
                    OR
                    (?5 = 'contradicts'
                     AND json_extract(event_json, '$.kind.action.relation.kind') = 'supports')
                  ))
               )
             ORDER BY event_sequence
             LIMIT 1",
            params![
                key.kind,
                key.project_id.as_str(),
                relation.source_fact_id().as_str(),
                relation.target_fact_id().as_str(),
                relation.kind().as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    else {
        return Ok(());
    };
    let event = serde_json::from_str::<FactLineageEventV1>(&row_string(
        &row,
        0,
        PROJECT_MEMORY_WRITE_OPERATION,
    )?)
    .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    if event.owner() != owner || event.fact_id() != relation.source_fact_id() {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "canonical relation event does not match its owner-scoped storage key",
        ));
    }
    let existing = match event.kind() {
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::ContradictedBy { fact_id },
            ..
        } if fact_id == relation.target_fact_id() => FactRelationKindV1::Contradicts,
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Linked { relation: existing },
            ..
        } if existing.target_fact_id() == relation.target_fact_id() => existing.kind(),
        _ => {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "canonical relation conflict query returned an unsupported event",
            ));
        }
    };
    if !relation_kinds_conflict(existing, relation.kind()) {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "canonical relation conflict query returned a non-conflicting event",
        ));
    }
    Err(FactStoreError::RelationConflict {
        source_fact_id: relation.source_fact_id().clone(),
        target_fact_id: relation.target_fact_id().clone(),
        existing,
        requested: relation.kind(),
    })
}

pub(super) fn normalize_tags(tags: &[String]) -> Vec<String> {
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
    target: &ProjectMemoryFactIdV1,
) -> FactStoreResult<StoredFactV1> {
    let owner = OwnerKey::new(target.owner())?;
    let fact = load_current_fact_tx(transaction, &owner, target.owner(), target.fact_id())
        .await?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "canonical curation target is missing",
            )
        })?;
    if fact.payload().is_none() {
        return Err(FactStoreError::PayloadAccessMismatch);
    }
    Ok(fact)
}

pub(in crate::store::memory) async fn curation_evidence_ids_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    evidence: &[ProjectMemoryFactIdV1],
) -> FactStoreResult<Vec<FactId>> {
    let mut ids = Vec::with_capacity(evidence.len());
    let mut seen = BTreeSet::new();
    for target in evidence {
        if target.owner() != owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        let fact = available_curation_fact_tx(transaction, target).await?;
        if !seen.insert(fact.fact_id().clone()) {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "curation evidence contains duplicate facts",
            ));
        }
        ids.push(fact.fact_id().clone());
    }
    Ok(ids)
}

pub(super) async fn link_facts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &ProjectMemoryFactLinkV1,
    now: UtcMicros,
) -> FactStoreResult<(Vec<FactId>, FactCommitReceipt)> {
    let relation = operation.relation();
    if relation.owner() != owner {
        return Err(FactStoreError::OwnerMismatch);
    }
    let source = available_curation_fact_tx(
        transaction,
        &ProjectMemoryFactIdV1::new(owner.clone(), relation.source_fact_id().clone())?,
    )
    .await?;
    available_curation_fact_tx(
        transaction,
        &ProjectMemoryFactIdV1::new(owner.clone(), relation.target_fact_id().clone())?,
    )
    .await?;
    for fact_id in relation.evidence_fact_ids() {
        available_curation_fact_tx(
            transaction,
            &ProjectMemoryFactIdV1::new(owner.clone(), fact_id.clone())?,
        )
        .await?;
    }
    ensure_relation_conflict_free_tx(transaction, owner, relation).await?;
    let event = FactLineageEventV1::new(
        relation.source_fact_id().clone(),
        owner.clone(),
        FactLineageEventKindV1::Curated {
            action: FactCurationActionV1::Linked {
                relation: relation.clone(),
            },
            evidence_ids: Vec::new(),
        },
        now,
        actor.cloned(),
    )?;
    let batch = FactWriteBatch::new(
        relation.source_fact_id().clone(),
        owner.clone(),
        None,
        vec![event],
        Vec::new(),
        Vec::new(),
        Some(source.last_event_id().clone()),
    )?;
    let (receipt, _) = commit_batch_tx(transaction, &batch).await?;
    Ok((
        vec![
            relation.source_fact_id().clone(),
            relation.target_fact_id().clone(),
        ],
        receipt,
    ))
}

pub(super) fn curated_correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    correction_batch(fact, payload, FactCurationActionV1::Retained, actor, now)
}

fn normalized_tags_correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
    evidence_fact_ids: Vec<FactId>,
    confidence: Confidence,
    actor: Option<ActorId>,
    now: UtcMicros,
) -> FactStoreResult<FactWriteBatch> {
    correction_batch(
        fact,
        payload,
        FactCurationActionV1::TagsNormalized {
            evidence_fact_ids,
            confidence,
        },
        actor,
        now,
    )
}

fn correction_batch(
    fact: &StoredFactV1,
    payload: FactPayloadV1,
    action: FactCurationActionV1,
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
            action,
            evidence_ids: Vec::new(),
        },
        project_memory_event_time(now, 1)?,
        actor,
    )?;
    FactWriteBatch::new(
        fact.fact_id().clone(),
        fact.owner().clone(),
        Some(assertion),
        vec![recorded, curated],
        Vec::new(),
        Vec::new(),
        Some(fact.last_event_id().clone()),
    )
}

pub(super) async fn normalize_tags_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &ProjectMemoryFactNormalizeTagsV1,
    now: UtcMicros,
) -> FactStoreResult<(FactId, FactCommitReceipt)> {
    let evidence_fact_ids =
        curation_evidence_ids_tx(transaction, owner, operation.evidence_facts()).await?;
    let fact = available_curation_fact_tx(transaction, operation.fact()).await?;
    let payload = fact
        .payload()
        .ok_or(FactStoreError::PayloadAccessMismatch)?;
    let Some(sanitized) = sanitize_payload(
        payload.content(),
        payload.category(),
        &normalize_tags(operation.tags()),
        payload.entities(),
        payload.metadata(),
        payload.source_label(),
    )?
    else {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "normalized tags were rejected by the privacy sanitizer",
        ));
    };
    let batch = normalized_tags_correction_batch(
        &fact,
        sanitized.payload,
        evidence_fact_ids,
        operation.confidence(),
        actor.cloned(),
        now,
    )?;
    let (receipt, _) = commit_batch_tx(transaction, &batch).await?;
    Ok((fact.fact_id().clone(), receipt))
}
