//! Tag, entity, and grooming-batch operations for `MemoryStore`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::db::engine::{Value, params};

use crate::errors::Result;
use crate::memory::entities::normalize_entity_alias;
use crate::memory::types::{EntityGroomingResult, MemoryGroomingOperation, MemoryGroomingReport};
use crate::tracedecay::current_timestamp;

use super::{MemoryStore, db_error, db_message, relations_conflict, to_json_string};

impl MemoryStore<'_> {
    async fn normalize_fact_tags_inner(
        &self,
        fact_id: i64,
        tags: &[String],
    ) -> Result<Vec<String>> {
        let fact = self.get_fact(fact_id).await?.ok_or_else(|| {
            db_message("normalize_fact_tags", format!("fact {fact_id} not found"))
        })?;
        let normalized: Vec<String> = tags
            .iter()
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
            .collect();
        let tags_json = to_json_string(&normalized, "normalize_fact_tags")?;
        self.conn
            .execute(
                "UPDATE memory_facts SET tags = ?1, updated_at = ?2 WHERE fact_id = ?3",
                params![tags_json, current_timestamp(), fact_id],
            )
            .await
            .map_err(|e| db_error("normalize_fact_tags", e))?;
        self.mark_fact_banks_dirty(fact.category).await?;
        Ok(normalized)
    }

    async fn update_entity_aliases_inner(
        &self,
        entity_id: i64,
        aliases: &[String],
    ) -> Result<Vec<String>> {
        let mut rows = self
            .conn
            .query(
                "SELECT name, aliases FROM memory_entities WHERE entity_id = ?1",
                params![entity_id],
            )
            .await
            .map_err(|e| db_error("update_entity_aliases", e))?;
        let row = rows
            .next()
            .await
            .map_err(|e| db_error("update_entity_aliases", e))?
            .ok_or_else(|| {
                db_message(
                    "update_entity_aliases",
                    format!("entity {entity_id} not found"),
                )
            })?;
        let canonical = row
            .get::<String>(0)
            .map_err(|e| db_error("update_entity_aliases", e))?;
        let stored = row
            .get::<String>(1)
            .map_err(|e| db_error("update_entity_aliases", e))?;
        let mut merged: BTreeMap<String, String> = serde_json::from_str::<Vec<String>>(&stored)
            .unwrap_or_default()
            .into_iter()
            .chain(aliases.iter().cloned())
            .map(|alias| {
                normalize_entity_alias(&alias)
                    .map(|value| (value.to_ascii_lowercase(), value))
                    .map_err(|reason| db_message("update_entity_aliases", reason))
            })
            .collect::<Result<_>>()?;
        merged.remove(&canonical.to_ascii_lowercase());
        let aliases: Vec<String> = merged.into_values().collect();
        let aliases_json = to_json_string(&aliases, "update_entity_aliases")?;
        self.conn
            .execute(
                "UPDATE memory_entities SET aliases = ?1, updated_at = ?2 WHERE entity_id = ?3",
                params![aliases_json, current_timestamp(), entity_id],
            )
            .await
            .map_err(|e| db_error("update_entity_aliases", e))?;
        self.mark_entity_fact_banks_dirty(entity_id, false).await?;
        Ok(aliases)
    }

    pub async fn merge_entities(
        &self,
        winner_entity_id: i64,
        loser_entity_ids: Vec<i64>,
    ) -> Result<EntityGroomingResult> {
        self.with_immediate_tx("merge_entities", move |store| {
            Box::pin(store.merge_entities_inner(winner_entity_id, loser_entity_ids))
        })
        .await
    }

    async fn merge_entities_inner(
        &self,
        winner_entity_id: i64,
        loser_entity_ids: Vec<i64>,
    ) -> Result<EntityGroomingResult> {
        let mut seen = BTreeSet::new();
        let mut aliases = Vec::new();
        let mut fact_ids = BTreeSet::new();
        for entity_id in std::iter::once(winner_entity_id).chain(loser_entity_ids.iter().copied()) {
            if !seen.insert(entity_id) {
                return Err(db_message(
                    "merge_entities",
                    "duplicate winner/loser entity id",
                ));
            }
            let mut rows = self
                .conn
                .query(
                    "SELECT name, aliases FROM memory_entities WHERE entity_id = ?1",
                    params![entity_id],
                )
                .await
                .map_err(|e| db_error("merge_entities", e))?;
            let row = rows
                .next()
                .await
                .map_err(|e| db_error("merge_entities", e))?
                .ok_or_else(|| {
                    db_message("merge_entities", format!("entity {entity_id} not found"))
                })?;
            if entity_id != winner_entity_id {
                aliases.push(
                    row.get::<String>(0)
                        .map_err(|e| db_error("merge_entities", e))?,
                );
            }
            aliases.extend(
                serde_json::from_str::<Vec<String>>(
                    &row.get::<String>(1)
                        .map_err(|e| db_error("merge_entities", e))?,
                )
                .unwrap_or_default(),
            );
            fact_ids.extend(
                self.fact_ids_for_entity(entity_id, "merge_entities")
                    .await?,
            );
        }
        let aliases = self
            .update_entity_aliases_inner(winner_entity_id, &aliases)
            .await?;
        for loser_id in &loser_entity_ids {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO memory_fact_entities (fact_id, entity_id)
                     SELECT fact_id, ?1 FROM memory_fact_entities WHERE entity_id = ?2",
                    params![winner_entity_id, *loser_id],
                )
                .await
                .map_err(|e| db_error("merge_entities", e))?;
            self.conn
                .execute(
                    "DELETE FROM memory_entities WHERE entity_id = ?1",
                    params![*loser_id],
                )
                .await
                .map_err(|e| db_error("merge_entities", e))?;
        }
        for fact_id in &fact_ids {
            self.invalidate_fact_vector_and_mark_dirty(*fact_id).await?;
        }
        Ok(EntityGroomingResult {
            winner_entity_id,
            merged_entity_ids: loser_entity_ids,
            aliases,
            rewired_fact_count: fact_ids.len(),
        })
    }

    /// Applies a fully prevalidated, bounded grooming batch atomically, then
    /// repairs derived vectors and dirty banks using the store's configured
    /// encoder contract before commit.
    pub async fn apply_grooming_batch(
        &self,
        operations: &[MemoryGroomingOperation],
        min_confidence: f64,
    ) -> Result<MemoryGroomingReport> {
        let operations = operations.to_vec();
        let mut report = self
            .with_immediate_tx("apply_grooming_batch", move |store| {
                Box::pin(async move {
                    store
                        .apply_grooming_batch_inner(&operations, min_confidence)
                        .await
                })
            })
            .await?;
        // Derived state is resumable. Keep its CPU-heavy work outside the
        // logical mutation transaction and cap each pass.
        report.derived_repair.missing_vectors_repaired = self.compute_missing_vectors(500).await?;
        report.derived_repair.banks_rebuilt = self.rebuild_dirty_banks().await?;
        Ok(report)
    }

    async fn apply_grooming_batch_inner(
        &self,
        operations: &[MemoryGroomingOperation],
        min_confidence: f64,
    ) -> Result<MemoryGroomingReport> {
        self.prevalidate_grooming_batch(operations, min_confidence)
            .await?;
        let mut report = MemoryGroomingReport::default();
        for operation in operations {
            match operation {
                MemoryGroomingOperation::NormalizeTags { fact_id, tags, .. } => {
                    self.normalize_fact_tags_inner(*fact_id, tags).await?;
                    report.normalized_tags += 1;
                }
                MemoryGroomingOperation::MergeEntities {
                    winner_entity_id,
                    loser_entity_ids,
                    ..
                } => {
                    self.merge_entities_inner(*winner_entity_id, loser_entity_ids.clone())
                        .await?;
                    report.merged_entities += 1;
                }
                MemoryGroomingOperation::AddAlias {
                    entity_id, alias, ..
                } => {
                    self.update_entity_aliases_inner(*entity_id, std::slice::from_ref(alias))
                        .await?;
                    report.aliases_added += 1;
                }
                MemoryGroomingOperation::LinkFacts {
                    source_fact_id,
                    target_fact_id,
                    relation,
                    confidence,
                    source,
                    metadata,
                    ..
                } => {
                    self.upsert_fact_relation_inner(
                        *source_fact_id,
                        *target_fact_id,
                        *relation,
                        *confidence,
                        source,
                        metadata.clone(),
                    )
                    .await?;
                    report.facts_linked += 1;
                }
                MemoryGroomingOperation::RepairVector { fact_id, .. } => {
                    self.repair_fact_vector_inner(*fact_id).await?;
                    report.vectors_repaired += 1;
                }
            }
        }
        Ok(report)
    }

    async fn prevalidate_grooming_batch(
        &self,
        operations: &[MemoryGroomingOperation],
        min_confidence: f64,
    ) -> Result<()> {
        if !(0.0..=1.0).contains(&min_confidence) || !min_confidence.is_finite() {
            return Err(db_message(
                "apply_grooming_batch",
                "invalid confidence floor",
            ));
        }
        let mut merged_entities = BTreeSet::new();
        let mut proposed_relations = HashMap::new();
        let existing_relations = self.list_fact_relations(None).await?;
        for operation in operations {
            let (confidence, evidence): (f64, &[i64]) = match operation {
                MemoryGroomingOperation::NormalizeTags {
                    confidence,
                    evidence_fact_ids,
                    ..
                }
                | MemoryGroomingOperation::MergeEntities {
                    confidence,
                    evidence_fact_ids,
                    ..
                }
                | MemoryGroomingOperation::AddAlias {
                    confidence,
                    evidence_fact_ids,
                    ..
                }
                | MemoryGroomingOperation::LinkFacts {
                    confidence,
                    evidence_fact_ids,
                    ..
                }
                | MemoryGroomingOperation::RepairVector {
                    confidence,
                    evidence_fact_ids,
                    ..
                } => (*confidence, evidence_fact_ids),
            };
            if confidence < min_confidence || confidence > 1.0 || !confidence.is_finite() {
                return Err(db_message(
                    "apply_grooming_batch",
                    format!("operation confidence {confidence} is outside the accepted range"),
                ));
            }
            if evidence.is_empty() {
                return Err(db_message(
                    "apply_grooming_batch",
                    "every grooming operation requires evidence_fact_ids",
                ));
            }
            let existing_evidence = self.facts_exist(evidence).await?;
            for fact_id in evidence {
                if !existing_evidence.contains(fact_id) {
                    return Err(db_message(
                        "apply_grooming_batch",
                        format!("evidence fact {fact_id} does not exist"),
                    ));
                }
            }
            match operation {
                MemoryGroomingOperation::NormalizeTags { fact_id, .. }
                | MemoryGroomingOperation::RepairVector { fact_id, .. } => {
                    if self.get_fact(*fact_id).await?.is_none() {
                        return Err(db_message(
                            "apply_grooming_batch",
                            format!("fact {fact_id} does not exist"),
                        ));
                    }
                }
                MemoryGroomingOperation::MergeEntities {
                    winner_entity_id,
                    loser_entity_ids,
                    evidence_fact_ids,
                    ..
                } => {
                    if loser_entity_ids.is_empty() {
                        return Err(db_message(
                            "apply_grooming_batch",
                            "entity merge has no losers",
                        ));
                    }
                    for entity_id in std::iter::once(winner_entity_id).chain(loser_entity_ids) {
                        if !merged_entities.insert(*entity_id) {
                            return Err(db_message(
                                "apply_grooming_batch",
                                "an entity appears in more than one merge role",
                            ));
                        }
                        if !self.entity_exists(*entity_id).await? {
                            return Err(db_message(
                                "apply_grooming_batch",
                                format!("entity {entity_id} does not exist"),
                            ));
                        }
                        if !self
                            .entity_linked_to_evidence(*entity_id, evidence_fact_ids)
                            .await?
                        {
                            return Err(db_message(
                                "apply_grooming_batch",
                                format!(
                                    "entity {entity_id} is not linked to the supplied evidence facts"
                                ),
                            ));
                        }
                    }
                }
                MemoryGroomingOperation::AddAlias {
                    entity_id,
                    alias,
                    evidence_fact_ids,
                    ..
                } => {
                    if !self.entity_exists(*entity_id).await? {
                        return Err(db_message(
                            "apply_grooming_batch",
                            format!("entity {entity_id} does not exist"),
                        ));
                    }
                    if !self
                        .entity_linked_to_evidence(*entity_id, evidence_fact_ids)
                        .await?
                    {
                        return Err(db_message(
                            "apply_grooming_batch",
                            format!(
                                "entity {entity_id} is not linked to the supplied evidence facts"
                            ),
                        ));
                    }
                    normalize_entity_alias(alias)
                        .map_err(|reason| db_message("apply_grooming_batch", reason))?;
                }
                MemoryGroomingOperation::LinkFacts {
                    source_fact_id,
                    target_fact_id,
                    relation,
                    ..
                } => {
                    if source_fact_id == target_fact_id {
                        return Err(db_message(
                            "apply_grooming_batch",
                            "self-relations are not allowed",
                        ));
                    }
                    if self.get_fact(*source_fact_id).await?.is_none()
                        || self.get_fact(*target_fact_id).await?.is_none()
                    {
                        return Err(db_message(
                            "apply_grooming_batch",
                            "linked facts must both exist",
                        ));
                    }
                    let key = (*source_fact_id, *target_fact_id);
                    if let Some(other) = proposed_relations.insert(key, *relation)
                        && relations_conflict(other, *relation)
                    {
                        return Err(db_message(
                            "apply_grooming_batch",
                            "batch proposes contradictory relation kinds for the same facts",
                        ));
                    }
                    if existing_relations.iter().any(|existing| {
                        existing.source_fact_id == *source_fact_id
                            && existing.target_fact_id == *target_fact_id
                            && relations_conflict(existing.relation, *relation)
                    }) {
                        return Err(db_message(
                            "apply_grooming_batch",
                            "stored relation contradicts the proposed relation kind",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    async fn entity_exists(&self, entity_id: i64) -> Result<bool> {
        let mut rows = self
            .conn
            .query(
                "SELECT 1 FROM memory_entities WHERE entity_id = ?1 LIMIT 1",
                params![entity_id],
            )
            .await
            .map_err(|e| db_error("entity_exists", e))?;
        Ok(rows
            .next()
            .await
            .map_err(|e| db_error("entity_exists", e))?
            .is_some())
    }

    async fn entity_linked_to_evidence(&self, entity_id: i64, fact_ids: &[i64]) -> Result<bool> {
        if fact_ids.is_empty() {
            return Ok(false);
        }
        let placeholders = std::iter::repeat_n("?", fact_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut values = Vec::with_capacity(fact_ids.len() + 1);
        values.push(Value::Integer(entity_id));
        values.extend(fact_ids.iter().copied().map(Value::Integer));
        let mut rows = self
            .conn
            .query(
                &format!(
                    "SELECT 1 FROM memory_fact_entities WHERE entity_id = ? AND fact_id IN ({placeholders}) LIMIT 1"
                ),
                values,
            )
            .await
            .map_err(|e| db_error("entity_linked_to_evidence", e))?;
        Ok(rows
            .next()
            .await
            .map_err(|e| db_error("entity_linked_to_evidence", e))?
            .is_some())
    }

    async fn mark_entity_fact_banks_dirty(&self, entity_id: i64, invalidate: bool) -> Result<()> {
        let fact_ids = self
            .fact_ids_for_entity(entity_id, "mark_entity_fact_banks_dirty")
            .await?;
        if invalidate {
            for fact_id in fact_ids {
                self.invalidate_fact_vector_and_mark_dirty(fact_id).await?;
            }
        } else {
            let categories = self.fact_categories(&fact_ids).await?;
            for fact_id in fact_ids {
                if let Some(category) = categories.get(&fact_id) {
                    self.mark_fact_banks_dirty(*category).await?;
                }
            }
        }
        Ok(())
    }

    async fn fact_ids_for_entity(
        &self,
        entity_id: i64,
        operation: &'static str,
    ) -> Result<Vec<i64>> {
        const PAGE_SIZE: i64 = 512;
        let mut fact_ids = Vec::new();
        let mut cursor = 0_i64;
        loop {
            let mut rows = self
                .conn
                .query(
                    "SELECT fact_id FROM memory_fact_entities
                     WHERE entity_id = ?1 AND fact_id > ?2
                     ORDER BY fact_id
                     LIMIT ?3",
                    params![entity_id, cursor, PAGE_SIZE],
                )
                .await
                .map_err(|e| db_error(operation, e))?;
            let mut page_count = 0;
            while let Some(row) = rows.next().await.map_err(|e| db_error(operation, e))? {
                cursor = row.get(0).map_err(|e| db_error(operation, e))?;
                fact_ids.push(cursor);
                page_count += 1;
            }
            if page_count < PAGE_SIZE {
                break;
            }
        }
        Ok(fact_ids)
    }

    async fn invalidate_fact_vector_and_mark_dirty(&self, fact_id: i64) -> Result<()> {
        let categories = self.fact_categories(&[fact_id]).await?;
        if let Some(category) = categories.get(&fact_id) {
            self.conn
                .execute(
                    "UPDATE memory_facts SET hrr_vector = NULL WHERE fact_id = ?1",
                    params![fact_id],
                )
                .await
                .map_err(|e| db_error("invalidate_fact_vector", e))?;
            self.mark_fact_banks_dirty(*category).await?;
        }
        Ok(())
    }
}
