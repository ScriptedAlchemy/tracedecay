//! The curation apply entry point and the compatibility fact-merge path.

use super::super::crud::{
    compatibility_commit_batch_tx, compatibility_mirror_delete_tx, compatibility_mirror_update_tx,
    compatibility_sanitize_payload, load_current_projection,
};
use super::super::envelope::{
    ProjectMemoryOperationReceiptV1, project_memory_digest,
    project_memory_lookup_operation_receipt_tx, project_memory_record_operation_receipt_tx,
    project_memory_target_digest,
};
use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, compatibility_legacy_timestamp, from_json,
    project_memory_event_time, project_memory_now, project_memory_source_label, row_f64, row_i64,
    row_string, storage_error, storage_message,
};
use super::super::projection::{
    project_memory_fact_for_legacy_id_tx, project_memory_required_mapping_tx,
    project_memory_source_for_fact_tx, resolve_project_memory_target_tx,
};
use super::super::repair::{
    COMPATIBILITY_REPAIR_VECTOR_BATCH, compatibility_repair_missing_vectors_tx,
    compatibility_repair_vector_for_fact_tx,
};
use super::{
    project_memory_add_entity_alias_tx, project_memory_available_curation_fact_tx,
    project_memory_curated_correction_batch, project_memory_curation_mappings_from_ids_tx,
    project_memory_curation_operation_digest, project_memory_legacy_relation_provenance,
    project_memory_link_facts_tx, project_memory_merge_entities_tx,
    project_memory_normalize_tags_tx, project_memory_record_oplog_tx,
    project_memory_replay_curation_tx, project_memory_upsert_legacy_relation_tx,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tracedecay_domain::{
    ActorId, Confidence, FactCurationActionV1, FactEventId, FactId,
    FactLineageEventKindV1, FactLineageEventV1, FactOwnerV1, PayloadAccessState, UtcMicros,
};
use tracedecay_store::{
    FactStoreError, FactStoreResult, FactWriteBatch, ProjectMemoryFactCurationBatchV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactIdV1, ProjectMemoryFactMappingV1, ProjectMemoryFactMergeCommandV1,
    ProjectMemoryFactMergeOutcomeV1, ProjectMemoryFactRelationV1, ProjectMemoryMemoryRepairStatsV1,
    ProjectMemoryResult,
};
pub(in crate::store::memory) async fn apply_project_memory_fact_curation_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactCurationBatchV1,
) -> ProjectMemoryResult<ProjectMemoryFactCurationReceiptV1> {
    let request_digest = project_memory_digest(json!({
        "owner": request.owner(),
        "actor": request.actor().map(ActorId::as_str),
        "min_confidence": request.min_confidence().as_f64(),
        "operations": request
            .operations()
            .iter()
            .map(project_memory_curation_operation_digest)
            .collect::<FactStoreResult<Vec<_>>>()?,
    }))?;
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "curation",
        &request_digest,
    )
    .await?
    {
        return project_memory_replay_curation_tx(transaction, request.owner(), &receipt).await;
    }
    let now = project_memory_now()?;
    let mut changed = Vec::new();
    let mut normalized_tags = 0_u64;
    let mut merged_entities = 0_u64;
    let mut aliases_added = 0_u64;
    let mut facts_linked = 0_u64;
    let mut vectors_repaired = 0_u64;
    for operation in request.operations() {
        match operation {
            ProjectMemoryFactCurationOperationV1::NormalizeTags(operation) => {
                changed.push(
                    project_memory_normalize_tags_tx(
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
            ProjectMemoryFactCurationOperationV1::MergeEntities(operation) => {
                changed.extend(
                    project_memory_merge_entities_tx(
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
            ProjectMemoryFactCurationOperationV1::AddAlias(operation) => {
                changed.extend(
                    project_memory_add_entity_alias_tx(transaction, request.owner(), operation, now)
                        .await?,
                );
                aliases_added = aliases_added.saturating_add(1);
            }
            ProjectMemoryFactCurationOperationV1::LinkFacts(operation) => {
                let (fact_ids, _) = project_memory_link_facts_tx(
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
            ProjectMemoryFactCurationOperationV1::RepairVector(operation) => {
                changed.push(
                    compatibility_repair_vector_for_fact_tx(
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
        transaction,
        request.owner(),
        COMPATIBILITY_REPAIR_VECTOR_BATCH,
    )
    .await?;
    // Plan 39 Task 7 (owner decision 2026-08-07, second): the derived bank
    // projection is deleted, so a curation pass never rebuilds bank rows.
    let banks_rebuilt = 0_u64;
    let mappings =
        project_memory_curation_mappings_from_ids_tx(transaction, request.owner(), &changed)
            .await?;
    if mappings.len() > 256 {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
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
    project_memory_record_operation_receipt_tx(
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
        project_memory_record_oplog_tx(
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
    ProjectMemoryFactCurationReceiptV1::new(
        request.owner().clone(),
        mappings,
        normalized_tags,
        merged_entities,
        aliases_added,
        facts_linked,
        vectors_repaired,
        ProjectMemoryMemoryRepairStatsV1::new(missing_vectors_repaired, banks_rebuilt),
    )
    .map_err(Into::into)
}

fn project_memory_merge_removal_batch(
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
        project_memory_event_time(now, 1)?,
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

async fn project_memory_replay_merge_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &ProjectMemoryOperationReceiptV1,
) -> ProjectMemoryResult<ProjectMemoryFactMergeOutcomeV1> {
    let winner_id = receipt.fact_id.as_ref().ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility merge receipt winner is missing",
        )
    })?;
    let winner = project_memory_curation_mappings_from_ids_tx(
        transaction,
        owner,
        std::slice::from_ref(winner_id),
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility merge receipt winner mapping is missing",
        )
    })?;
    let deleted_ids = receipt
        .receipt
        .get("deleted_loser_fact_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility merge receipt deleted losers are malformed",
            )
        })?;
    let mut ids = Vec::with_capacity(deleted_ids.len());
    for id in deleted_ids {
        ids.push(
            FactId::new(id.as_str().ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "compatibility merge receipt loser id is malformed",
                )
            })?)
            .map_err(FactStoreError::from)?,
        );
    }
    // The original merge already retired each loser's fixed legacy mapping;
    // at replay time only the canonical identity survives, so the replayed
    // outcome carries the mapping in its retired shape (no legacy binding)
    // rather than demanding a projection row the deletion removed.
    let mut deleted_losers = Vec::with_capacity(ids.len());
    for fact_id in &ids {
        deleted_losers.push(ProjectMemoryFactMappingV1::new(
            ProjectMemoryFactIdV1::new(owner.clone(), fact_id.clone())?,
            None,
        )?);
    }
    let content_updated = receipt
        .receipt
        .get("content_updated")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility merge receipt content flag is malformed",
            )
        })?;
    ProjectMemoryFactMergeOutcomeV1::new(owner.clone(), winner, content_updated, deleted_losers)
        .map_err(Into::into)
}

async fn project_memory_rewire_merge_relations_tx(
    transaction: &Transaction<'_>,
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
            .map(crate::db::engine::Value::Integer),
    );
    legacy_values.extend(
        loser_legacy_fact_ids
            .iter()
            .copied()
            .map(crate::db::engine::Value::Integer),
    );
    let mut legacy_rows = transaction
        .query(&legacy_sql, legacy_values)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut legacy_relations = Vec::new();
    while let Some(row) = legacy_rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        legacy_relations.push((
            row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?,
            row_i64(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)?,
            row_string(&row, 2, PROJECT_MEMORY_WRITE_OPERATION)?,
            Confidence::new(row_f64(&row, 3, PROJECT_MEMORY_WRITE_OPERATION)?)?,
            row_string(&row, 4, PROJECT_MEMORY_WRITE_OPERATION)?,
            from_json::<Value>(
                &row_string(&row, 5, PROJECT_MEMORY_WRITE_OPERATION)?,
                PROJECT_MEMORY_WRITE_OPERATION,
            )?,
        ));
    }
    drop(legacy_rows);
    if legacy_relations.len() > 256 {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility merge relation rewiring exceeds the fixed 256-relation bound",
        ));
    }
    let loser_legacy = loser_legacy_fact_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    for (source, target, _, _, _, _) in &legacy_relations {
        for endpoint in [source, target] {
            if project_memory_fact_for_legacy_id_tx(transaction, owner, *endpoint)
                .await?
                .is_none()
            {
                return Err(storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
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
                        .map(crate::db::engine::Value::Integer),
                );
                values.extend(
                    loser_legacy_fact_ids
                        .iter()
                        .copied()
                        .map(crate::db::engine::Value::Integer),
                );
                values
            },
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
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
            "supports" => ProjectMemoryFactRelationV1::Supports,
            "contradicts" => ProjectMemoryFactRelationV1::Contradicts,
            "supersedes" => ProjectMemoryFactRelationV1::Supersedes,
            "derived_from" => ProjectMemoryFactRelationV1::DerivedFrom,
            _ => {
                return Err(storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "compatibility merge found an unsupported legacy relation",
                ));
            }
        };
        project_memory_upsert_legacy_relation_tx(
            transaction,
            source,
            target,
            relation,
            confidence,
            &project_memory_source_label(Some(&source_label))?,
            &project_memory_legacy_relation_provenance(&metadata).await?,
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
    canonical_values.push(crate::db::engine::Value::Text(key.kind.to_string()));
    canonical_values.push(crate::db::engine::Value::Text(key.project_id.clone()));
    for _ in 0..2 {
        canonical_values.extend(
            loser_fact_ids
                .iter()
                .map(|fact_id| crate::db::engine::Value::Text(fact_id.as_str().to_owned())),
        );
    }
    let mut canonical_rows = transaction
        .query(&canonical_sql, canonical_values)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut canonical_relations = Vec::new();
    while let Some(row) = canonical_rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        canonical_relations.push((
            FactId::new(row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?)?,
            FactId::new(row_string(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)?)?,
            row_string(&row, 2, PROJECT_MEMORY_WRITE_OPERATION)?,
            Confidence::new(row_f64(&row, 3, PROJECT_MEMORY_WRITE_OPERATION)?)?,
            row_string(&row, 4, PROJECT_MEMORY_WRITE_OPERATION)?,
            row_string(&row, 5, PROJECT_MEMORY_WRITE_OPERATION)?,
            row_string(&row, 6, PROJECT_MEMORY_WRITE_OPERATION)?,
            row_i64(&row, 7, PROJECT_MEMORY_WRITE_OPERATION)?,
        ));
    }
    drop(canonical_rows);
    if canonical_relations.len() > 256 {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
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
                values.push(crate::db::engine::Value::Text(key.kind.to_string()));
                values.push(crate::db::engine::Value::Text(key.project_id.clone()));
                for _ in 0..2 {
                    values.extend(loser_fact_ids.iter().map(|fact_id| {
                        crate::db::engine::Value::Text(fact_id.as_str().to_owned())
                    }));
                }
                values
            },
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
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
                    project_memory_source_label(Some(&source_label))?,
                    provenance_json,
                    evidence_json,
                    occurred_at,
                    now.0,
                ],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    }
    Ok(())
}

pub(in crate::store::memory) async fn merge_project_memory_facts_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactMergeCommandV1,
) -> ProjectMemoryResult<ProjectMemoryFactMergeOutcomeV1> {
    let request_digest = project_memory_digest(json!({
        "owner": request.owner(),
        "winner": project_memory_target_digest(request.winner())?,
        "losers": request
            .losers()
            .iter()
            .map(project_memory_target_digest)
            .collect::<FactStoreResult<Vec<_>>>()?,
        "merged_content": request.merged_content(),
        "actor": request.actor().map(ActorId::as_str),
    }))?;
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "merge",
        &request_digest,
    )
    .await?
    {
        return project_memory_replay_merge_tx(transaction, request.owner(), &receipt).await;
    }
    let now = project_memory_now()?;
    let (winner_id, winner_fact, winner_mapping) =
        project_memory_available_curation_fact_tx(transaction, request.winner()).await?;
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
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility merged content was rejected by the privacy sanitizer",
            )
            .into());
        };
        let source = project_memory_source_for_fact_tx(
            transaction,
            winner_mapping
                .legacy_mapping()
                .ok_or(FactStoreError::FactMismatch)?,
        )
        .await?;
        let batch = project_memory_curated_correction_batch(
            &winner_fact,
            sanitized.payload.clone(),
            request.actor().cloned(),
            now,
        )?;
        compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_update_tx(
            transaction,
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
        let loser_id = resolve_project_memory_target_tx(transaction, target)
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
                    PROJECT_MEMORY_WRITE_OPERATION,
                    format!("compatibility merge loser fact {loser_label} not found"),
                )
            })?;
        if loser_id == winner_id {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility merge winner cannot be a loser",
            )
            .into());
        }
        let projection = load_current_projection(transaction, &owner_key, &loser_id)
            .await?
            .ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "compatibility merge loser projection is missing",
                )
            })?;
        let mapping =
            project_memory_required_mapping_tx(transaction, request.owner(), &loser_id).await?;
        loser_ids.push(loser_id.clone());
        loser_legacy_ids.push(mapping.legacy_fact_id());
        if projection.access != PayloadAccessState::Deleted {
            pending_deletes.push((
                loser_id,
                projection.access,
                projection.last_event_id.clone(),
                mapping,
            ));
        }
    }
    project_memory_rewire_merge_relations_tx(
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
    let mut deleted_losers = Vec::new();
    for (loser_id, previous_access, expected_last_event_id, mapping) in pending_deletes {
        let batch = project_memory_merge_removal_batch(
            request.owner(),
            &loser_id,
            previous_access,
            expected_last_event_id,
            &winner_id,
            request.actor().cloned(),
            now,
        )?;
        compatibility_commit_batch_tx(transaction, &batch).await?;
        compatibility_mirror_delete_tx(transaction, mapping.legacy_fact_id()).await?;
        // The mirror delete just retired this loser's fixed legacy mapping;
        // the merge outcome carries the mapping captured before the deletion
        // (the only remaining witness of the legacy binding) instead of
        // re-querying a row that no longer exists.
        deleted_losers.push(ProjectMemoryFactMappingV1::new(
            ProjectMemoryFactIdV1::new(request.owner().clone(), loser_id.clone())?,
            Some(mapping),
        )?);
        deleted_ids.push(loser_id);
    }
    let winner = project_memory_curation_mappings_from_ids_tx(
        transaction,
        request.owner(),
        std::slice::from_ref(&winner_id),
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(|| {
        storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility merge winner mapping is missing",
        )
    })?;
    let receipt = json!({
        "content_updated": content_updated,
        "deleted_loser_fact_ids": deleted_ids.iter().map(FactId::as_str).collect::<Vec<_>>(),
    });
    project_memory_record_operation_receipt_tx(
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
    project_memory_record_oplog_tx(
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
    ProjectMemoryFactMergeOutcomeV1::new(
        request.owner().clone(),
        winner,
        content_updated,
        deleted_losers,
    )
    .map_err(Into::into)
}
