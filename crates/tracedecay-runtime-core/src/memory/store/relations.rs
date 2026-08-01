//! Fact-relation operations for `MemoryStore`.

use std::collections::BTreeSet;

use crate::db::engine::{Value, params};

use crate::errors::Result;
use crate::memory::types::{FactRelationKind, FactRelationRecord};
use crate::tracedecay::current_timestamp;

use super::{
    MemoryStore, db_error, db_message, relation_from_row, relations_conflict, to_json_string,
};

impl MemoryStore<'_> {
    pub async fn upsert_fact_relation(
        &self,
        source_fact_id: i64,
        target_fact_id: i64,
        relation: FactRelationKind,
        confidence: f64,
        source: &str,
        metadata: serde_json::Value,
    ) -> Result<FactRelationRecord> {
        let source = source.to_owned();
        self.with_immediate_tx("upsert_fact_relation", move |store| {
            Box::pin(async move {
                store
                    .upsert_fact_relation_inner(
                        source_fact_id,
                        target_fact_id,
                        relation,
                        confidence,
                        &source,
                        metadata,
                    )
                    .await
            })
        })
        .await
    }

    pub(crate) async fn upsert_fact_relation_inner(
        &self,
        source_fact_id: i64,
        target_fact_id: i64,
        relation: FactRelationKind,
        confidence: f64,
        source: &str,
        metadata: serde_json::Value,
    ) -> Result<FactRelationRecord> {
        if source_fact_id == target_fact_id {
            return Err(db_message(
                "upsert_fact_relation",
                "self-relations are not allowed",
            ));
        }
        if !(0.0..=1.0).contains(&confidence) || !confidence.is_finite() {
            return Err(db_message(
                "upsert_fact_relation",
                "confidence must be finite and between 0 and 1",
            ));
        }
        let source = source.trim();
        if source.is_empty() {
            return Err(db_message("upsert_fact_relation", "source cannot be empty"));
        }
        let existing = self.facts_exist(&[source_fact_id, target_fact_id]).await?;
        if !existing.contains(&source_fact_id) || !existing.contains(&target_fact_id) {
            return Err(db_message(
                "upsert_fact_relation",
                "source and target facts must both exist in this project store",
            ));
        }
        let mut existing_rows = self
            .conn
            .query(
                "SELECT relation FROM memory_fact_relations
                 WHERE source_fact_id = ?1 AND target_fact_id = ?2",
                params![source_fact_id, target_fact_id],
            )
            .await
            .map_err(|e| db_error("upsert_fact_relation", e))?;
        while let Some(row) = existing_rows
            .next()
            .await
            .map_err(|e| db_error("upsert_fact_relation", e))?
        {
            let existing = row
                .get::<String>(0)
                .map_err(|e| db_error("upsert_fact_relation", e))?
                .parse::<FactRelationKind>()
                .map_err(|e| db_message("upsert_fact_relation", e))?;
            if relations_conflict(existing, relation) {
                return Err(db_message(
                    "upsert_fact_relation",
                    "supports and contradicts cannot coexist for the same directed fact pair",
                ));
            }
        }
        let metadata_json = to_json_string(&metadata, "upsert_fact_relation")?;
        let now = current_timestamp();
        self.conn
            .execute(
                "INSERT INTO memory_fact_relations (
                    source_fact_id, target_fact_id, relation, confidence,
                    source, metadata, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(source_fact_id, target_fact_id, relation) DO UPDATE SET
                    confidence = excluded.confidence,
                    source = excluded.source,
                    metadata = excluded.metadata,
                    updated_at = excluded.updated_at",
                params![
                    source_fact_id,
                    target_fact_id,
                    relation.as_str(),
                    confidence,
                    source,
                    metadata_json,
                    now,
                ],
            )
            .await
            .map_err(|e| db_error("upsert_fact_relation", e))?;
        self.get_fact_relation(source_fact_id, target_fact_id, relation)
            .await?
            .ok_or_else(|| {
                db_message(
                    "upsert_fact_relation",
                    "relation was not found after upsert",
                )
            })
    }

    pub async fn list_fact_relations(
        &self,
        fact_id: Option<i64>,
    ) -> Result<Vec<FactRelationRecord>> {
        const PAGE_SIZE: i64 = 512;
        let mut relations = Vec::new();
        let mut cursor: Option<(i64, i64, String)> = None;
        loop {
            let mut rows =
                match (fact_id, cursor.as_ref()) {
                    (Some(fact_id), Some((source_id, target_id, relation))) => self
                        .conn
                        .query(
                            "SELECT source_fact_id, target_fact_id, relation, confidence, source,
                                    metadata, created_at, updated_at
                             FROM memory_fact_relations
                             WHERE (source_fact_id = ?1 OR target_fact_id = ?1)
                               AND (
                                   source_fact_id > ?2
                                   OR (source_fact_id = ?2 AND target_fact_id > ?3)
                                   OR (
                                       source_fact_id = ?2
                                       AND target_fact_id = ?3
                                       AND relation > ?4
                                   )
                               )
                             ORDER BY source_fact_id, target_fact_id, relation
                             LIMIT ?5",
                            params![fact_id, *source_id, *target_id, relation, PAGE_SIZE],
                        )
                        .await,
                    (Some(fact_id), None) => self
                        .conn
                        .query(
                            "SELECT source_fact_id, target_fact_id, relation, confidence, source,
                                    metadata, created_at, updated_at
                             FROM memory_fact_relations
                             WHERE source_fact_id = ?1 OR target_fact_id = ?1
                             ORDER BY source_fact_id, target_fact_id, relation
                             LIMIT ?2",
                            params![fact_id, PAGE_SIZE],
                        )
                        .await,
                    (None, Some((source_id, target_id, relation))) => self
                        .conn
                        .query(
                            "SELECT source_fact_id, target_fact_id, relation, confidence, source,
                                    metadata, created_at, updated_at
                             FROM memory_fact_relations
                             WHERE source_fact_id > ?1
                                OR (source_fact_id = ?1 AND target_fact_id > ?2)
                                OR (
                                    source_fact_id = ?1
                                    AND target_fact_id = ?2
                                    AND relation > ?3
                                )
                             ORDER BY source_fact_id, target_fact_id, relation
                             LIMIT ?4",
                            params![*source_id, *target_id, relation, PAGE_SIZE],
                        )
                        .await,
                    (None, None) => self
                        .conn
                        .query(
                            "SELECT source_fact_id, target_fact_id, relation, confidence, source,
                                    metadata, created_at, updated_at
                             FROM memory_fact_relations
                             ORDER BY source_fact_id, target_fact_id, relation
                             LIMIT ?1",
                            params![PAGE_SIZE],
                        )
                        .await,
                }
                .map_err(|e| db_error("list_fact_relations", e))?;
            let mut page_count = 0;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|e| db_error("list_fact_relations", e))?
            {
                cursor = Some((
                    row.get(0).map_err(|e| db_error("list_fact_relations", e))?,
                    row.get(1).map_err(|e| db_error("list_fact_relations", e))?,
                    row.get(2).map_err(|e| db_error("list_fact_relations", e))?,
                ));
                relations.push(relation_from_row(&row, "list_fact_relations")?);
                page_count += 1;
            }
            if page_count < PAGE_SIZE {
                break;
            }
        }
        Ok(relations)
    }

    pub(crate) async fn related_fact_ids(
        &self,
        fact_ids: &[i64],
        limit: usize,
    ) -> Result<Vec<i64>> {
        if fact_ids.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let fact_ids = &fact_ids[..fact_ids.len().min(128)];
        let placeholders = std::iter::repeat_n("?", fact_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut values = Vec::with_capacity(fact_ids.len() * 3 + 1);
        for _ in 0..3 {
            values.extend(fact_ids.iter().copied().map(Value::Integer));
        }
        values.push(Value::Integer(limit.min(256) as i64));
        let mut rows = self
            .conn
            .query(
                &format!(
                    "SELECT CASE WHEN source_fact_id IN ({placeholders})
                                 THEN target_fact_id ELSE source_fact_id END AS related_fact_id
                     FROM memory_fact_relations
                     WHERE source_fact_id IN ({placeholders}) OR target_fact_id IN ({placeholders})
                     ORDER BY confidence DESC, updated_at DESC
                     LIMIT ?"
                ),
                values,
            )
            .await
            .map_err(|e| db_error("related_fact_ids", e))?;
        let mut related = BTreeSet::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("related_fact_ids", e))?
        {
            related.insert(
                row.get::<i64>(0)
                    .map_err(|e| db_error("related_fact_ids", e))?,
            );
        }
        Ok(related.into_iter().collect())
    }
}
