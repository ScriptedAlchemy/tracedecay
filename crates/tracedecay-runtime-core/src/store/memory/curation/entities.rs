//! Owner-entity resolution, entity merges/aliases, and curation oplog replay.

use super::super::crud::{
    compatibility_commit_batch_tx, compatibility_mark_owner_banks_dirty_tx,
    compatibility_mirror_update_tx, compatibility_sanitize_payload, load_current_fact_tx,
};
use super::super::envelope::{
    ProjectMemoryOperationReceiptV1, project_memory_receipt_u64, project_memory_target_digest,
};
use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, compatibility_legacy_timestamp, from_json, row_i64,
    row_string, storage_error, storage_message, to_json,
};
use super::super::projection::{
    project_memory_required_mapping_tx, project_memory_source_for_fact_tx,
};
use super::super::proposals::project_memory_proposal_category;
use super::{
    project_memory_curated_correction_batch, project_memory_curation_evidence_ids_tx,
    project_memory_curation_mappings_from_ids_tx,
    project_memory_record_curated_correction_provenance_tx, project_memory_relation_label,
};
use crate::db::Database;
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::memory::entities::normalize_entity;
use serde_json::{Value, json};
use tracedecay_domain::{ActorId, FactId, FactOwnerV1, UtcMicros};
use tracedecay_store::{
    FactStoreError, FactStoreResult, ProjectMemoryFactAddAliasV1,
    ProjectMemoryFactCurationOperationV1, ProjectMemoryFactCurationReceiptV1,
    ProjectMemoryFactMappingV1, ProjectMemoryFactMergeEntitiesV1, ProjectMemoryFactTargetV1,
    ProjectMemoryLegacyEntityTargetV1, ProjectMemoryMemoryRepairStatsV1, ProjectMemoryResult,
};
async fn project_memory_owner_entity_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    entity_id: i64,
) -> FactStoreResult<(String, Vec<String>)> {
    let key = OwnerKey::new(owner)?;
    let foreign_links = transaction
        .query(
            "SELECT COUNT(*)
             FROM memory_fact_entities AS links
             LEFT JOIN memory_facts AS projections
               ON projections.fact_id = links.fact_id
             LEFT JOIN memory_v2_facts AS facts
               ON facts.fact_id = projections.canonical_fact_id
             WHERE links.entity_id = ?1
               AND (
                    projections.canonical_fact_id IS NULL
                    OR facts.owner_kind <> ?2
                    OR facts.project_id <> ?3
                    OR facts.owner_json <> ?4
               )",
            params![
                entity_id,
                key.kind,
                key.project_id.as_str(),
                key.json.as_str()
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut foreign_links = foreign_links;
    let row = foreign_links
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility entity ownership count is missing",
            )
        })?;
    if row_i64(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)? != 0 {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
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
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let row = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility curation entity is missing",
            )
        })?;
    Ok((
        row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?,
        from_json::<Vec<String>>(
            &row_string(&row, 1, PROJECT_MEMORY_WRITE_OPERATION)?,
            PROJECT_MEMORY_WRITE_OPERATION,
        )?,
    ))
}

async fn project_memory_entity_linked_to_evidence_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    entity_id: i64,
    evidence_ids: &[FactId],
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let placeholders = std::iter::repeat_n("?", evidence_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT 1
         FROM memory_fact_entities AS links
         JOIN memory_facts AS projections ON projections.fact_id = links.fact_id
         JOIN memory_v2_facts AS facts ON facts.fact_id = projections.canonical_fact_id
         WHERE links.entity_id = ?
           AND facts.owner_kind = ? AND facts.project_id = ?
           AND facts.owner_json = ?
           AND projections.canonical_fact_id IN ({placeholders})
         LIMIT 1"
    );
    let mut values = Vec::with_capacity(evidence_ids.len() + 4);
    values.push(crate::db::engine::Value::Integer(entity_id));
    values.push(crate::db::engine::Value::Text(key.kind.to_string()));
    values.push(crate::db::engine::Value::Text(key.project_id.clone()));
    values.push(crate::db::engine::Value::Text(key.json.clone()));
    values.extend(
        evidence_ids
            .iter()
            .map(|fact_id| crate::db::engine::Value::Text(fact_id.as_str().to_owned())),
    );
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    if rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
        .is_none()
    {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility curation entity is not linked to supplied evidence",
        ));
    }
    Ok(())
}

async fn project_memory_owner_entity_fact_ids_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    entity_ids: &[i64],
) -> FactStoreResult<Vec<FactId>> {
    let key = OwnerKey::new(owner)?;
    let placeholders = std::iter::repeat_n("?", entity_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT DISTINCT projections.canonical_fact_id
         FROM memory_fact_entities AS links
         JOIN memory_facts AS projections ON projections.fact_id = links.fact_id
         JOIN memory_v2_facts AS facts ON facts.fact_id = projections.canonical_fact_id
         WHERE facts.owner_kind = ? AND facts.project_id = ?
           AND facts.owner_json = ?
           AND links.entity_id IN ({placeholders})
         ORDER BY projections.canonical_fact_id ASC LIMIT 257"
    );
    let mut values = Vec::with_capacity(entity_ids.len() + 3);
    values.push(crate::db::engine::Value::Text(key.kind.to_string()));
    values.push(crate::db::engine::Value::Text(key.project_id.clone()));
    values.push(crate::db::engine::Value::Text(key.json.clone()));
    values.extend(
        entity_ids
            .iter()
            .copied()
            .map(crate::db::engine::Value::Integer),
    );
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        fact_ids.push(
            FactId::new(row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    if fact_ids.len() > 256 {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "compatibility entity curation exceeds the fixed 256-fact bound",
        ));
    }
    Ok(fact_ids)
}

async fn project_memory_fact_entities_tx(
    transaction: &Transaction<'_>,
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
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut entities = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        entities.push(row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?);
    }
    Ok(entities)
}

pub(super) async fn project_memory_merge_entities_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    actor: Option<&ActorId>,
    operation: &ProjectMemoryFactMergeEntitiesV1,
    now: UtcMicros,
) -> FactStoreResult<Vec<FactId>> {
    let evidence =
        project_memory_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let winner_id = operation.winner().legacy_entity_id();
    let (winner_name, winner_aliases) =
        project_memory_owner_entity_tx(transaction, owner, winner_id).await?;
    project_memory_entity_linked_to_evidence_tx(transaction, owner, winner_id, &evidence).await?;
    let mut entity_ids = vec![winner_id];
    let mut aliases = winner_aliases;
    for loser in operation.losers() {
        let loser_id = loser.legacy_entity_id();
        let (name, loser_aliases) =
            project_memory_owner_entity_tx(transaction, owner, loser_id).await?;
        project_memory_entity_linked_to_evidence_tx(transaction, owner, loser_id, &evidence)
            .await?;
        entity_ids.push(loser_id);
        aliases.push(name);
        aliases.extend(loser_aliases);
    }
    let fact_ids = project_memory_owner_entity_fact_ids_tx(transaction, owner, &entity_ids).await?;
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
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    for loser in operation.losers() {
        let loser_id = loser.legacy_entity_id();
        transaction
            .execute(
                "INSERT OR IGNORE INTO memory_fact_entities(fact_id, entity_id)
                 SELECT fact_id, ?1 FROM memory_fact_entities WHERE entity_id = ?2",
                params![winner_id, loser_id],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_fact_entities WHERE entity_id = ?1",
                params![loser_id],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
        transaction
            .execute(
                "DELETE FROM memory_entities WHERE entity_id = ?1
                 AND NOT EXISTS(SELECT 1 FROM memory_fact_entities WHERE entity_id = ?1)",
                params![loser_id],
            )
            .await
            .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
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
        let mapping = project_memory_required_mapping_tx(transaction, owner, fact_id).await?;
        let entities =
            project_memory_fact_entities_tx(transaction, mapping.legacy_fact_id()).await?;
        let Some(sanitized) = compatibility_sanitize_payload(
            payload.content(),
            payload.category(),
            payload.tags(),
            &entities,
            payload.metadata(),
        )?
        else {
            return Err(storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility merged entities were rejected by the privacy sanitizer",
            ));
        };
        let source = project_memory_source_for_fact_tx(transaction, &mapping).await?;
        let batch = project_memory_curated_correction_batch(
            &fact,
            sanitized.payload.clone(),
            actor.cloned(),
            now,
        )?;
        compatibility_commit_batch_tx(transaction, &batch).await?;
        // The evidence facts are themselves linked to the merged entities, so
        // they are always members of `fact_ids`. Recording one as derived from
        // itself trips the self-reference guard and would roll back the whole
        // merge, so correct each fact against the remaining evidence only and
        // skip the relation entirely when it was the sole evidence fact.
        let fact_evidence = evidence
            .iter()
            .filter(|evidence_fact_id| *evidence_fact_id != fact_id)
            .cloned()
            .collect::<Vec<_>>();
        if !fact_evidence.is_empty() {
            project_memory_record_curated_correction_provenance_tx(
                transaction,
                owner,
                fact_id,
                &fact_evidence,
                operation.confidence(),
                "merge_entities",
                actor,
                now,
            )
            .await?;
        }
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

pub(super) async fn project_memory_add_entity_alias_tx(
    db: &Database,
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    operation: &ProjectMemoryFactAddAliasV1,
    now: UtcMicros,
) -> FactStoreResult<Vec<FactId>> {
    let evidence =
        project_memory_curation_evidence_ids_tx(transaction, owner, operation.evidence_facts())
            .await?;
    let entity_id = operation.entity().legacy_entity_id();
    let (name, mut aliases) = project_memory_owner_entity_tx(transaction, owner, entity_id).await?;
    project_memory_entity_linked_to_evidence_tx(transaction, owner, entity_id, &evidence).await?;
    let alias = normalize_entity(operation.alias());
    if alias.is_empty() || alias.eq_ignore_ascii_case(&name) {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
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
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let fact_ids =
        project_memory_owner_entity_fact_ids_tx(transaction, owner, &[entity_id]).await?;
    for fact_id in &fact_ids {
        let mapping = project_memory_required_mapping_tx(transaction, owner, fact_id).await?;
        let mut rows = transaction
            .query(
                "SELECT category FROM memory_facts WHERE fact_id = ?1",
                params![mapping.legacy_fact_id()],
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
                    "compatibility alias fact is missing from the legacy mirror",
                )
            })?;
        let category = project_memory_proposal_category(&row_string(
            &row,
            0,
            PROJECT_MEMORY_WRITE_OPERATION,
        )?)?;
        compatibility_mark_owner_banks_dirty_tx(db, transaction, owner, category, now).await?;
    }
    Ok(fact_ids)
}

pub(super) fn project_memory_curation_operation_digest(
    operation: &ProjectMemoryFactCurationOperationV1,
) -> FactStoreResult<Value> {
    let evidence = |targets: &[ProjectMemoryFactTargetV1]| {
        targets
            .iter()
            .map(project_memory_target_digest)
            .collect::<FactStoreResult<Vec<_>>>()
    };
    match operation {
        ProjectMemoryFactCurationOperationV1::NormalizeTags(operation) => Ok(json!({
            "kind": "normalize_tags",
            "fact": project_memory_target_digest(operation.fact())?,
            "tags": operation.tags(),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
        ProjectMemoryFactCurationOperationV1::MergeEntities(operation) => Ok(json!({
            "kind": "merge_entities",
            "winner": operation.winner().legacy_entity_id(),
            "losers": operation.losers().iter().map(ProjectMemoryLegacyEntityTargetV1::legacy_entity_id).collect::<Vec<_>>(),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
        ProjectMemoryFactCurationOperationV1::AddAlias(operation) => Ok(json!({
            "kind": "add_alias",
            "entity": operation.entity().legacy_entity_id(),
            "alias": operation.alias(),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
        ProjectMemoryFactCurationOperationV1::LinkFacts(operation) => Ok(json!({
            "kind": "link_facts",
            "source": project_memory_target_digest(operation.source())?,
            "target": project_memory_target_digest(operation.target())?,
            "relation": project_memory_relation_label(operation.relation()),
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
            "source_label": operation.source_label(),
            "provenance": operation.provenance(),
        })),
        ProjectMemoryFactCurationOperationV1::RepairVector(operation) => Ok(json!({
            "kind": "repair_vector",
            "fact": project_memory_target_digest(operation.fact())?,
            "evidence": evidence(operation.evidence_facts())?,
            "confidence": operation.confidence().as_f64(),
        })),
    }
}

pub(super) async fn project_memory_record_oplog_tx(
    transaction: &Transaction<'_>,
    operation: &str,
    mapping: Option<&ProjectMemoryFactMappingV1>,
    detail: &Value,
    now: UtcMicros,
) -> FactStoreResult<()> {
    transaction
        .execute(
            "INSERT INTO memory_oplog(ts, op, fact_id, detail_json) VALUES(?1, ?2, ?3, ?4)",
            params![
                compatibility_legacy_timestamp(now),
                operation,
                mapping.and_then(ProjectMemoryFactMappingV1::legacy_fact_id),
                to_json(detail, "serialize compatibility oplog detail")?,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    Ok(())
}

pub(super) async fn project_memory_replay_curation_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &ProjectMemoryOperationReceiptV1,
) -> ProjectMemoryResult<ProjectMemoryFactCurationReceiptV1> {
    let ids = receipt
        .receipt
        .get("changed_fact_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            storage_message(
                PROJECT_MEMORY_WRITE_OPERATION,
                "compatibility curation receipt changed facts are malformed",
            )
        })?;
    let mut fact_ids = Vec::with_capacity(ids.len());
    for id in ids {
        fact_ids.push(
            FactId::new(id.as_str().ok_or_else(|| {
                storage_message(
                    PROJECT_MEMORY_WRITE_OPERATION,
                    "compatibility curation receipt fact id is malformed",
                )
            })?)
            .map_err(FactStoreError::from)?,
        );
    }
    let mappings =
        project_memory_curation_mappings_from_ids_tx(transaction, owner, &fact_ids).await?;
    let derived_repair = ProjectMemoryMemoryRepairStatsV1::new(
        project_memory_receipt_u64(&receipt.receipt, "missing_vectors_repaired")?,
        project_memory_receipt_u64(&receipt.receipt, "banks_rebuilt")?,
    );
    ProjectMemoryFactCurationReceiptV1::new(
        owner.clone(),
        mappings,
        project_memory_receipt_u64(&receipt.receipt, "normalized_tags")?,
        project_memory_receipt_u64(&receipt.receipt, "merged_entities")?,
        project_memory_receipt_u64(&receipt.receipt, "aliases_added")?,
        project_memory_receipt_u64(&receipt.receipt, "facts_linked")?,
        project_memory_receipt_u64(&receipt.receipt, "vectors_repaired")?,
        derived_repair,
    )
    .map_err(Into::into)
}
