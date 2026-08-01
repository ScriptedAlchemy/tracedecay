//! Read and access-tracking queries for `MemoryStore`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::db::engine::{Value, params};
use crate::errors::Result;
use crate::memory::encoding::HolographicEncoder;
use crate::memory::trust::DEFAULT_MIN_TRUST;
use crate::memory::types::{FactRecord, MemoryCategory};
use crate::tracedecay::current_timestamp;

use super::{
    ENTITY_BATCH_SIZE, MemoryStore, db_error, db_message, fact_from_row, normalized_limit,
    parse_category, sql_i64_list,
};

impl MemoryStore<'_> {
    pub async fn list_facts(
        &self,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactRecord>> {
        let min_trust = min_trust.unwrap_or(DEFAULT_MIN_TRUST);
        let limit = normalized_limit(limit);
        let sql = if category.is_some() {
            "SELECT fact_id, content, category, tags, trust_score, source,
                    retrieval_count, helpful_count, unhelpful_count,
                    created_at, updated_at, last_retrieved_at, last_feedback_at,
                    metadata, access_count, last_recalled_at
             FROM memory_facts
             WHERE category = ?1 AND trust_score >= ?2
             ORDER BY updated_at DESC, fact_id DESC
             LIMIT ?3"
        } else {
            "SELECT fact_id, content, category, tags, trust_score, source,
                    retrieval_count, helpful_count, unhelpful_count,
                    created_at, updated_at, last_retrieved_at, last_feedback_at,
                    metadata, access_count, last_recalled_at
             FROM memory_facts
             WHERE trust_score >= ?1
             ORDER BY updated_at DESC, fact_id DESC
             LIMIT ?2"
        };

        let mut rows = if let Some(category) = category {
            self.conn
                .query(sql, params![category.as_str(), min_trust, limit as i64])
                .await
        } else {
            self.conn.query(sql, params![min_trust, limit as i64]).await
        }
        .map_err(|e| db_error("list_facts", e))?;

        let mut fact_ids = Vec::new();
        let mut facts = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| db_error("list_facts", e))? {
            let fact = fact_from_row(&row, "list_facts", Vec::new())?;
            fact_ids.push(fact.fact_id);
            facts.push(fact);
        }

        let mut entities_by_fact = self.load_entities_for_facts(&fact_ids).await?;
        for fact in &mut facts {
            fact.entities = entities_by_fact.remove(&fact.fact_id).unwrap_or_default();
        }
        Ok(facts)
    }

    pub async fn get_fact(&self, fact_id: i64) -> Result<Option<FactRecord>> {
        let mut rows = self
            .conn
            .query(
                "SELECT fact_id, content, category, tags, trust_score, source,
                        retrieval_count, helpful_count, unhelpful_count,
                        created_at, updated_at, last_retrieved_at, last_feedback_at,
                        metadata, access_count, last_recalled_at
                 FROM memory_facts
                 WHERE fact_id = ?1",
                params![fact_id],
            )
            .await
            .map_err(|e| db_error("get_fact", e))?;

        let Some(row) = rows.next().await.map_err(|e| db_error("get_fact", e))? else {
            return Ok(None);
        };

        Ok(Some(self.row_to_fact(&row, "get_fact").await?))
    }

    /// Bulk-loads facts by id, returning a map keyed by `fact_id`. Missing ids
    /// are simply absent from the map. Entities are batch-loaded for the whole
    /// set via [`Self::load_entities_for_facts`] rather than per fact, so this
    /// replaces the per-id `get_fact` round-trips in the retrieval hot path.
    ///
    /// Ids are chunked at 256 per `IN (...)` statement to stay well clear of
    /// `SQLite`'s 999-parameter limit.
    pub(crate) async fn get_facts(&self, fact_ids: &[i64]) -> Result<HashMap<i64, FactRecord>> {
        const CHUNK: usize = 256;
        let mut facts: HashMap<i64, FactRecord> = HashMap::new();
        for chunk in fact_ids.chunks(CHUNK) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT fact_id, content, category, tags, trust_score, source,
                        retrieval_count, helpful_count, unhelpful_count,
                        created_at, updated_at, last_retrieved_at, last_feedback_at,
                        metadata, access_count, last_recalled_at
                 FROM memory_facts
                 WHERE fact_id IN ({placeholders})"
            );
            let values: Vec<Value> = chunk.iter().map(|id| Value::Integer(*id)).collect();
            let mut rows = self
                .conn
                .query(&sql, values)
                .await
                .map_err(|e| db_error("get_facts", e))?;
            while let Some(row) = rows.next().await.map_err(|e| db_error("get_facts", e))? {
                let fact = fact_from_row(&row, "get_facts", Vec::new())?;
                facts.insert(fact.fact_id, fact);
            }
        }

        if facts.is_empty() {
            return Ok(facts);
        }
        let ids: Vec<i64> = facts.keys().copied().collect();
        let mut entities_by_fact = self.load_entities_for_facts(&ids).await?;
        for fact in facts.values_mut() {
            fact.entities = entities_by_fact.remove(&fact.fact_id).unwrap_or_default();
        }
        Ok(facts)
    }

    /// Batched existence check for fact ids. Returns the subset of
    /// `fact_ids` present in `memory_facts`, without the full 16-column
    /// row fetch or per-id entity `JOIN` that [`Self::get_fact`] performs.
    /// Callers that only need to know whether facts exist should use this
    /// instead of discarding the record returned by `get_fact`.
    pub(crate) async fn facts_exist(&self, fact_ids: &[i64]) -> Result<HashSet<i64>> {
        let mut existing = HashSet::with_capacity(fact_ids.len());
        for chunk in fact_ids.chunks(ENTITY_BATCH_SIZE) {
            let Some(id_list) = sql_i64_list(chunk) else {
                continue;
            };
            let sql = format!("SELECT fact_id FROM memory_facts WHERE fact_id IN ({id_list})");
            let mut rows = self
                .conn
                .query(&sql, params![])
                .await
                .map_err(|e| db_error("facts_exist", e))?;
            while let Some(row) = rows.next().await.map_err(|e| db_error("facts_exist", e))? {
                existing.insert(row.get::<i64>(0).map_err(|e| db_error("facts_exist", e))?);
            }
        }
        Ok(existing)
    }

    /// Batched category lookup for fact ids, for callers (bank/dirty
    /// marking) that only need the stored `category` rather than the full
    /// record [`Self::get_fact`] would return. Ids absent from
    /// `memory_facts` are simply absent from the returned map.
    pub(crate) async fn fact_categories(
        &self,
        fact_ids: &[i64],
    ) -> Result<HashMap<i64, MemoryCategory>> {
        let mut categories = HashMap::with_capacity(fact_ids.len());
        for chunk in fact_ids.chunks(ENTITY_BATCH_SIZE) {
            let Some(id_list) = sql_i64_list(chunk) else {
                continue;
            };
            let sql =
                format!("SELECT fact_id, category FROM memory_facts WHERE fact_id IN ({id_list})");
            let mut rows = self
                .conn
                .query(&sql, params![])
                .await
                .map_err(|e| db_error("fact_categories", e))?;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|e| db_error("fact_categories", e))?
            {
                let fact_id = row
                    .get::<i64>(0)
                    .map_err(|e| db_error("fact_categories", e))?;
                let category = parse_category(
                    &row.get::<String>(1)
                        .map_err(|e| db_error("fact_categories", e))?,
                    "fact_categories",
                )?;
                categories.insert(fact_id, category);
            }
        }
        Ok(categories)
    }

    /// Bulk-loads stored HRR vectors by `fact_id`. Facts whose vector is NULL or
    /// fails to decode are omitted from the map so callers fall back to encoding
    /// the vector on the fly (preserving the per-fact fallback behaviour).
    ///
    /// Ids are chunked at 256 per `IN (...)` statement to stay well clear of
    /// `SQLite`'s 999-parameter limit.
    pub(crate) async fn fact_vectors(&self, fact_ids: &[i64]) -> Result<HashMap<i64, Vec<f64>>> {
        const CHUNK: usize = 256;
        let mut vectors: HashMap<i64, Vec<f64>> = HashMap::new();
        for chunk in fact_ids.chunks(CHUNK) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT fact_id, hrr_vector FROM memory_facts WHERE fact_id IN ({placeholders})"
            );
            let values: Vec<Value> = chunk.iter().map(|id| Value::Integer(*id)).collect();
            let mut rows = self
                .conn
                .query(&sql, values)
                .await
                .map_err(|e| db_error("fact_vectors", e))?;
            while let Some(row) = rows.next().await.map_err(|e| db_error("fact_vectors", e))? {
                let fact_id = row.get::<i64>(0).map_err(|e| db_error("fact_vectors", e))?;
                let value = row
                    .get::<Value>(1)
                    .map_err(|e| db_error("fact_vectors", e))?;
                if let Value::Blob(bytes) = value
                    && let Ok(vector) = HolographicEncoder::deserialize(&bytes)
                {
                    vectors.insert(fact_id, vector);
                }
            }
        }
        Ok(vectors)
    }

    pub async fn increment_retrieval_counts(&self, fact_ids: &[i64]) -> Result<()> {
        if fact_ids.is_empty() {
            return Ok(());
        }
        let now = current_timestamp();
        let mut counts = BTreeMap::new();
        for fact_id in fact_ids {
            *counts.entry(*fact_id).or_insert(0_i64) += 1;
        }
        let ids: Vec<i64> = counts.keys().copied().collect();
        let id_list = sql_i64_list(&ids).ok_or_else(|| {
            db_message(
                "increment_retrieval_counts",
                "retrieval count update had no fact ids",
            )
        })?;
        let increment_cases = counts
            .iter()
            .map(|(fact_id, count)| format!("WHEN {fact_id} THEN {count}"))
            .collect::<Vec<_>>()
            .join(" ");
        let sql = format!(
            "UPDATE memory_facts
             SET retrieval_count = retrieval_count + CASE fact_id {increment_cases} ELSE 0 END,
                 last_retrieved_at = ?1
             WHERE fact_id IN ({id_list})"
        );
        self.conn
            .execute(sql.as_str(), params![now])
            .await
            .map_err(|e| db_error("increment_retrieval_counts", e))?;
        Ok(())
    }

    /// Batched access-tracking bump for facts RETURNED from a recall search
    /// (`FactRetriever::search`). Distinct from `increment_retrieval_counts`,
    /// which also counts probe/list/related/reason scans. One UPDATE for the
    /// whole result set; callers treat it as fire-and-forget (a failure must
    /// never fail the search that triggered it).
    pub async fn record_fact_recalls(&self, fact_ids: &[i64]) -> Result<()> {
        if fact_ids.is_empty() {
            return Ok(());
        }
        let now = current_timestamp();
        let unique: BTreeSet<i64> = fact_ids.iter().copied().collect();
        let ids: Vec<i64> = unique.into_iter().collect();
        let id_list = sql_i64_list(&ids)
            .ok_or_else(|| db_message("record_fact_recalls", "recall update had no fact ids"))?;
        let sql = format!(
            "UPDATE memory_facts
             SET access_count = access_count + 1,
                 last_recalled_at = ?1
             WHERE fact_id IN ({id_list})"
        );
        self.conn
            .execute(sql.as_str(), params![now])
            .await
            .map_err(|e| db_error("record_fact_recalls", e))?;
        Ok(())
    }
}
