//! Persistence layer for memory facts, entities, vectors, and feedback.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use libsql::{params, Connection};

use super::encoding::HolographicEncoder;
use super::entities::{extract_entities, normalize_entity};
use super::trust::{apply_feedback, clamp_trust, DEFAULT_MIN_TRUST};
use super::types::{
    AddFactRequest, FactRecord, FeedbackAction, FeedbackRequest, FeedbackResult, MemoryCategory,
    UpdateFactRequest,
};
use crate::errors::{Result, TokenSaveError};
use crate::tokensave::current_timestamp;

const DEFAULT_LIMIT: usize = 50;
const ENTITY_BATCH_SIZE: usize = 500;
const MEMORY_SOURCE_DEFAULT: &str = "manual";
const HRR_ALGEBRA: &str = "amari_fhrr";

pub struct MemoryStore<'a> {
    conn: &'a Connection,
    encoder: HolographicEncoder,
}

impl<'a> MemoryStore<'a> {
    pub const fn new(conn: &'a Connection) -> Self {
        Self {
            conn,
            encoder: HolographicEncoder::new(),
        }
    }

    /// Runs `work` inside a `BEGIN IMMEDIATE` transaction, committing on success
    /// and rolling back on error. The inner future is built before the
    /// transaction opens, which is safe because async fns do no work until
    /// polled — `work.await` is the first time any statement runs.
    async fn with_immediate_tx<T>(
        &self,
        operation: &str,
        work: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        self.conn
            .execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|e| db_error(operation, e))?;
        match work.await {
            Ok(value) => {
                if let Err(error) = self.conn.execute("COMMIT", ()).await {
                    let _ = self.conn.execute("ROLLBACK", ()).await;
                    return Err(db_error(operation, error));
                }
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute("ROLLBACK", ()).await;
                Err(error)
            }
        }
    }

    pub async fn add_fact(
        &self,
        request: AddFactRequest,
        default_trust: f64,
    ) -> Result<FactRecord> {
        self.with_immediate_tx("add_fact", self.add_fact_inner(request, default_trust))
            .await
    }

    async fn add_fact_inner(
        &self,
        request: AddFactRequest,
        default_trust: f64,
    ) -> Result<FactRecord> {
        let content = request.content.trim().to_string();
        if content.is_empty() {
            return Err(db_message("add_fact", "fact content cannot be empty"));
        }

        let now = current_timestamp();
        let entities = merge_entities(&content, &request.entities);
        let tags_json = to_json_string(&request.tags, "add_fact")?;
        let metadata_json = to_json_string(&request.metadata, "add_fact")?;
        let vector = self.encode_vector(&content, &entities, "add_fact")?;
        let source = request
            .source
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| MEMORY_SOURCE_DEFAULT.to_string());

        self.conn
            .execute(
                "INSERT OR IGNORE INTO memory_facts (
                    content, category, tags, trust_score, created_at,
                    updated_at, source, metadata, hrr_vector, hrr_algebra, hrr_dim
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    content.as_str(),
                    request.category.as_str(),
                    tags_json,
                    clamp_trust(request.trust.unwrap_or(default_trust)),
                    now,
                    now,
                    source,
                    metadata_json,
                    vector,
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                ],
            )
            .await
            .map_err(|e| db_error("add_fact", e))?;

        let Some(existing) = self.get_fact_by_content(&content).await? else {
            return Err(db_message(
                "add_fact",
                "inserted or existing fact was not found by content",
            ));
        };
        let mut merged_entities = existing.entities.clone();
        let original_entities = merged_entities.clone();
        for entity in entities {
            if !merged_entities
                .iter()
                .any(|stored| stored.eq_ignore_ascii_case(&entity))
            {
                merged_entities.push(entity);
            }
        }
        self.replace_fact_entities(existing.fact_id, &merged_entities)
            .await?;
        if merged_entities != original_entities {
            self.update_fact_vector(
                existing.fact_id,
                &existing.content,
                &merged_entities,
                "add_fact",
            )
            .await?;
        }
        let fact = self.get_fact(existing.fact_id).await?.ok_or_else(|| {
            db_message(
                "add_fact",
                "inserted fact was not found when reading it back",
            )
        })?;
        self.mark_fact_banks_dirty(fact.category).await?;
        Ok(fact)
    }

    pub async fn update_fact(&self, request: UpdateFactRequest) -> Result<FactRecord> {
        self.with_immediate_tx("update_fact", self.update_fact_inner(request))
            .await
    }

    async fn update_fact_inner(&self, request: UpdateFactRequest) -> Result<FactRecord> {
        let existing = self.get_fact(request.fact_id).await?.ok_or_else(|| {
            db_message(
                "update_fact",
                format!("fact {} does not exist", request.fact_id),
            )
        })?;

        let content = request.content.map_or_else(
            || existing.content.clone(),
            |value| value.trim().to_string(),
        );
        if content.is_empty() {
            return Err(db_message("update_fact", "fact content cannot be empty"));
        }

        let category = request.category.unwrap_or(existing.category);
        let tags = request.tags.unwrap_or(existing.tags);
        let explicit_entities = request.entities.unwrap_or(existing.entities);
        let entities = merge_entities(&content, &explicit_entities);
        let trust = request.trust.map_or(existing.trust_score, clamp_trust);
        let source = request.source.or(existing.source);
        let metadata = request.metadata.unwrap_or(existing.metadata);
        let tags_json = to_json_string(&tags, "update_fact")?;
        let metadata_json = to_json_string(&metadata, "update_fact")?;
        let vector = self.encode_vector(&content, &entities, "update_fact")?;
        let now = current_timestamp();

        self.conn
            .execute(
                "UPDATE memory_facts
                 SET content = ?1,
                     category = ?2,
                     tags = ?3,
                     trust_score = ?4,
                     source = ?5,
                     metadata = ?6,
                     hrr_vector = ?7,
                     hrr_algebra = ?8,
                     hrr_dim = ?9,
                     updated_at = ?10
                 WHERE fact_id = ?11",
                params![
                    content,
                    category.as_str(),
                    tags_json,
                    trust,
                    source.unwrap_or_else(|| MEMORY_SOURCE_DEFAULT.to_string()),
                    metadata_json,
                    vector,
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    now,
                    request.fact_id,
                ],
            )
            .await
            .map_err(|e| db_error("update_fact", e))?;

        self.replace_fact_entities(request.fact_id, &entities)
            .await?;
        let updated = self.get_fact(request.fact_id).await?.ok_or_else(|| {
            db_message(
                "update_fact",
                "updated fact was not found when reading it back",
            )
        })?;
        self.mark_fact_banks_dirty(existing.category).await?;
        self.mark_fact_banks_dirty(updated.category).await?;
        Ok(updated)
    }

    pub async fn remove_fact(&self, fact_id: i64) -> Result<bool> {
        self.with_immediate_tx("remove_fact", self.remove_fact_inner(fact_id))
            .await
    }

    async fn remove_fact_inner(&self, fact_id: i64) -> Result<bool> {
        let existing = self.get_fact(fact_id).await?;
        let changed = self
            .conn
            .execute(
                "DELETE FROM memory_facts WHERE fact_id = ?1",
                params![fact_id],
            )
            .await
            .map_err(|e| db_error("remove_fact", e))?;
        if changed > 0 {
            if let Some(fact) = existing {
                self.mark_fact_banks_dirty(fact.category).await?;
            }
        }
        Ok(changed > 0)
    }

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
                    metadata
             FROM memory_facts
             WHERE category = ?1 AND trust_score >= ?2
             ORDER BY updated_at DESC, fact_id DESC
             LIMIT ?3"
        } else {
            "SELECT fact_id, content, category, tags, trust_score, source,
                    retrieval_count, helpful_count, unhelpful_count,
                    created_at, updated_at, last_retrieved_at, last_feedback_at,
                    metadata
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
                        metadata
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
    pub async fn get_facts(&self, fact_ids: &[i64]) -> Result<HashMap<i64, FactRecord>> {
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
                        metadata
                 FROM memory_facts
                 WHERE fact_id IN ({placeholders})"
            );
            let values: Vec<libsql::Value> =
                chunk.iter().map(|id| libsql::Value::Integer(*id)).collect();
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

    /// Bulk-loads stored HRR vectors by `fact_id`. Facts whose vector is NULL or
    /// fails to decode are omitted from the map so callers fall back to encoding
    /// the vector on the fly (preserving the per-fact fallback behaviour).
    ///
    /// Ids are chunked at 256 per `IN (...)` statement to stay well clear of
    /// `SQLite`'s 999-parameter limit.
    pub async fn fact_vectors(&self, fact_ids: &[i64]) -> Result<HashMap<i64, Vec<f64>>> {
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
            let values: Vec<libsql::Value> =
                chunk.iter().map(|id| libsql::Value::Integer(*id)).collect();
            let mut rows = self
                .conn
                .query(&sql, values)
                .await
                .map_err(|e| db_error("fact_vectors", e))?;
            while let Some(row) = rows.next().await.map_err(|e| db_error("fact_vectors", e))? {
                let fact_id = row.get::<i64>(0).map_err(|e| db_error("fact_vectors", e))?;
                let value = row
                    .get::<libsql::Value>(1)
                    .map_err(|e| db_error("fact_vectors", e))?;
                if let libsql::Value::Blob(bytes) = value {
                    if let Ok(vector) = HolographicEncoder::deserialize(&bytes) {
                        vectors.insert(fact_id, vector);
                    }
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

    pub async fn record_feedback_event(&self, request: FeedbackRequest) -> Result<FeedbackResult> {
        self.with_immediate_tx(
            "record_feedback_event",
            self.record_feedback_event_inner(request),
        )
        .await
    }

    async fn record_feedback_event_inner(
        &self,
        request: FeedbackRequest,
    ) -> Result<FeedbackResult> {
        let existing = self.get_fact(request.fact_id).await?.ok_or_else(|| {
            db_message(
                "record_feedback_event",
                format!("fact {} does not exist", request.fact_id),
            )
        })?;
        let old_trust = existing.trust_score;
        let new_trust = apply_feedback(old_trust, request.action);
        let delta = new_trust - old_trust;
        let now = current_timestamp();
        let action = feedback_action_str(request.action);
        let source = request
            .source
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "mcp".to_string());

        self.conn
            .execute(
                "UPDATE memory_facts
                 SET trust_score = ?1,
                     helpful_count = helpful_count + ?2,
                     unhelpful_count = unhelpful_count + ?3,
                     last_feedback_at = ?4,
                     updated_at = ?4
                 WHERE fact_id = ?5",
                params![
                    new_trust,
                    i64::from(request.action == FeedbackAction::Helpful),
                    i64::from(request.action == FeedbackAction::Unhelpful),
                    now,
                    request.fact_id,
                ],
            )
            .await
            .map_err(|e| db_error("record_feedback_event", e))?;

        self.conn
            .execute(
                "INSERT INTO memory_feedback_events (
                    fact_id, action, trust_delta, old_trust, new_trust,
                    created_at, source, note
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    request.fact_id,
                    action,
                    delta,
                    old_trust,
                    new_trust,
                    now,
                    source,
                    request.note,
                ],
            )
            .await
            .map_err(|e| db_error("record_feedback_event", e))?;

        let event_id = self.last_insert_rowid("record_feedback_event").await?;
        Ok(FeedbackResult {
            event_id,
            fact_id: request.fact_id,
            action: request.action,
            old_trust,
            new_trust,
            trust_delta: delta,
            helpful_count: existing.helpful_count
                + i64::from(request.action == FeedbackAction::Helpful),
            unhelpful_count: existing.unhelpful_count
                + i64::from(request.action == FeedbackAction::Unhelpful),
        })
    }

    pub async fn compute_missing_vectors(&self, limit: usize) -> Result<usize> {
        let limit = normalized_limit(limit);
        let mut rows = self
            .conn
            .query(
                "SELECT fact_id FROM memory_facts
                 WHERE hrr_vector IS NULL
                    OR hrr_algebra != ?1
                    OR hrr_dim != ?2
                 ORDER BY updated_at DESC
                 LIMIT ?3",
                params![
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    limit as i64
                ],
            )
            .await
            .map_err(|e| db_error("compute_missing_vectors", e))?;

        let mut fact_ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("compute_missing_vectors", e))?
        {
            fact_ids.push(
                row.get::<i64>(0)
                    .map_err(|e| db_error("compute_missing_vectors", e))?,
            );
        }

        for fact_id in &fact_ids {
            if let Some(fact) = self.get_fact(*fact_id).await? {
                let vector =
                    self.encode_vector(&fact.content, &fact.entities, "compute_missing_vectors")?;
                self.conn
                    .execute(
                        "UPDATE memory_facts
                         SET hrr_vector = ?1, hrr_algebra = ?2, hrr_dim = ?3, updated_at = ?4
                         WHERE fact_id = ?5",
                        params![
                            vector,
                            HRR_ALGEBRA,
                            HolographicEncoder::DIMENSIONS as i64,
                            current_timestamp(),
                            *fact_id,
                        ],
                    )
                    .await
                    .map_err(|e| db_error("compute_missing_vectors", e))?;
            }
        }

        Ok(fact_ids.len())
    }

    pub async fn rebuild_bank(
        &self,
        bank_name: &str,
        category: Option<MemoryCategory>,
    ) -> Result<usize> {
        let (fact_count, vectors) = self.load_bank_vectors(category).await?;
        if vectors.is_empty() {
            self.conn
                .execute(
                    "DELETE FROM memory_banks WHERE bank_name = ?1",
                    params![bank_name],
                )
                .await
                .map_err(|e| db_error("rebuild_bank", e))?;
            return Ok(0);
        }

        let averaged = average_vectors(&vectors);
        let vector_bytes = HolographicEncoder::serialize(&averaged).map_err(|e| {
            db_message(
                "rebuild_bank",
                format!("failed to serialize bank vector: {e}"),
            )
        })?;
        let normalized_name = normalize_bank_name(bank_name);
        let now = current_timestamp();

        self.conn
            .execute(
                "INSERT INTO memory_banks (
                    bank_name, vector, hrr_algebra, hrr_dim, fact_count, updated_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(bank_name) DO UPDATE SET
                    vector = excluded.vector,
                    hrr_algebra = excluded.hrr_algebra,
                    hrr_dim = excluded.hrr_dim,
                    fact_count = excluded.fact_count,
                    updated_at = excluded.updated_at",
                params![
                    normalized_name,
                    vector_bytes,
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    fact_count as i64,
                    now,
                ],
            )
            .await
            .map_err(|e| db_error("rebuild_bank", e))?;

        Ok(fact_count)
    }

    pub async fn rebuild_all_banks(&self) -> Result<usize> {
        let mut categories = Vec::new();
        let mut rows = self
            .conn
            .query("SELECT DISTINCT category FROM memory_facts", ())
            .await
            .map_err(|e| db_error("rebuild_all_banks", e))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("rebuild_all_banks", e))?
        {
            let category = row
                .get::<String>(0)
                .map_err(|e| db_error("rebuild_all_banks", e))?;
            categories.push(parse_category(&category, "rebuild_all_banks")?);
        }

        let mut rebuilt = 0;
        self.rebuild_bank("all", None).await?;
        rebuilt += 1;
        for category in categories {
            self.rebuild_bank(category.as_str(), Some(category)).await?;
            rebuilt += 1;
        }
        Ok(rebuilt)
    }

    pub async fn rebuild_dirty_banks(&self) -> Result<usize> {
        let mut rows = self
            .conn
            .query(
                "SELECT bank_name FROM memory_bank_dirty ORDER BY bank_name",
                (),
            )
            .await
            .map_err(|e| db_error("rebuild_dirty_banks", e))?;
        let mut bank_names = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("rebuild_dirty_banks", e))?
        {
            bank_names.push(
                row.get::<String>(0)
                    .map_err(|e| db_error("rebuild_dirty_banks", e))?,
            );
        }

        let mut rebuilt = 0;
        for bank_name in bank_names {
            if bank_name == "all" {
                self.rebuild_bank("all", None).await?;
            } else {
                let category = parse_category(&bank_name, "rebuild_dirty_banks")?;
                self.rebuild_bank(category.as_str(), Some(category)).await?;
            }
            self.conn
                .execute(
                    "DELETE FROM memory_bank_dirty WHERE bank_name = ?1",
                    params![bank_name],
                )
                .await
                .map_err(|e| db_error("rebuild_dirty_banks", e))?;
            rebuilt += 1;
        }
        Ok(rebuilt)
    }

    pub(crate) fn conn(&self) -> &Connection {
        self.conn
    }

    async fn get_fact_by_content(&self, content: &str) -> Result<Option<FactRecord>> {
        let mut rows = self
            .conn
            .query(
                "SELECT fact_id FROM memory_facts WHERE content = ?1",
                params![content],
            )
            .await
            .map_err(|e| db_error("get_fact_by_content", e))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("get_fact_by_content", e))?
        else {
            return Ok(None);
        };
        let fact_id = row
            .get::<i64>(0)
            .map_err(|e| db_error("get_fact_by_content", e))?;
        self.get_fact(fact_id).await
    }

    async fn row_to_fact(&self, row: &libsql::Row, operation: &str) -> Result<FactRecord> {
        let fact_id = row.get::<i64>(0).map_err(|e| db_error(operation, e))?;
        let entities = self.load_fact_entities(fact_id).await?;
        fact_from_row(row, operation, entities)
    }

    async fn load_entities_for_facts(&self, fact_ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
        let mut entities: HashMap<i64, Vec<String>> = HashMap::new();
        for chunk in fact_ids.chunks(ENTITY_BATCH_SIZE) {
            let Some(id_list) = sql_i64_list(chunk) else {
                continue;
            };
            let sql = format!(
                "SELECT fe.fact_id, e.name
                 FROM memory_fact_entities fe
                 JOIN memory_entities e ON e.entity_id = fe.entity_id
                 WHERE fe.fact_id IN ({id_list})
                 ORDER BY fe.fact_id, e.name"
            );
            let mut rows = self
                .conn
                .query(sql.as_str(), ())
                .await
                .map_err(|e| db_error("load_entities_for_facts", e))?;
            while let Some(row) = rows
                .next()
                .await
                .map_err(|e| db_error("load_entities_for_facts", e))?
            {
                let fact_id = row
                    .get::<i64>(0)
                    .map_err(|e| db_error("load_entities_for_facts", e))?;
                let entity = row
                    .get::<String>(1)
                    .map_err(|e| db_error("load_entities_for_facts", e))?;
                entities.entry(fact_id).or_default().push(entity);
            }
        }
        Ok(entities)
    }

    async fn replace_fact_entities(&self, fact_id: i64, entities: &[String]) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM memory_fact_entities WHERE fact_id = ?1",
                params![fact_id],
            )
            .await
            .map_err(|e| db_error("replace_fact_entities", e))?;

        for entity in entities {
            let entity_id = self.resolve_entity(entity).await?;
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO memory_fact_entities (fact_id, entity_id)
                     VALUES (?1, ?2)",
                    params![fact_id, entity_id],
                )
                .await
                .map_err(|e| db_error("replace_fact_entities", e))?;
        }
        Ok(())
    }

    async fn resolve_entity(&self, entity: &str) -> Result<i64> {
        let name = normalize_entity(entity);
        let normalized = name.to_ascii_lowercase();
        let mut rows = self
            .conn
            .query(
                "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                params![normalized.as_str()],
            )
            .await
            .map_err(|e| db_error("resolve_entity", e))?;
        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("resolve_entity", e))?
        {
            let entity_id = row
                .get::<i64>(0)
                .map_err(|e| db_error("resolve_entity", e))?;
            return Ok(entity_id);
        }

        self.conn
            .execute(
                "INSERT OR IGNORE INTO memory_entities (
                    name, normalized_name, entity_type, aliases, created_at
                 )
                 VALUES (?1, ?2, 'unknown', '[]', ?3)",
                params![name, normalized.as_str(), current_timestamp(),],
            )
            .await
            .map_err(|e| db_error("resolve_entity", e))?;
        let mut rows = self
            .conn
            .query(
                "SELECT entity_id FROM memory_entities WHERE normalized_name = ?1",
                params![normalized.as_str()],
            )
            .await
            .map_err(|e| db_error("resolve_entity", e))?;
        let row = rows
            .next()
            .await
            .map_err(|e| db_error("resolve_entity", e))?
            .ok_or_else(|| db_message("resolve_entity", "entity insert/read returned no row"))?;
        row.get::<i64>(0).map_err(|e| db_error("resolve_entity", e))
    }

    async fn load_fact_entities(&self, fact_id: i64) -> Result<Vec<String>> {
        let mut rows = self
            .conn
            .query(
                "SELECT e.name
                 FROM memory_entities e
                 JOIN memory_fact_entities fe ON fe.entity_id = e.entity_id
                 WHERE fe.fact_id = ?1
                 ORDER BY e.name",
                params![fact_id],
            )
            .await
            .map_err(|e| db_error("load_fact_entities", e))?;
        let mut entities = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("load_fact_entities", e))?
        {
            entities.push(
                row.get::<String>(0)
                    .map_err(|e| db_error("load_fact_entities", e))?,
            );
        }
        Ok(entities)
    }

    async fn load_bank_vectors(
        &self,
        category: Option<MemoryCategory>,
    ) -> Result<(usize, Vec<Vec<f64>>)> {
        let sql = if category.is_some() {
            "SELECT hrr_vector
             FROM memory_facts
             WHERE category = ?1 AND trust_score >= ?2"
        } else {
            "SELECT hrr_vector
             FROM memory_facts
             WHERE trust_score >= ?1"
        };

        let mut rows = if let Some(category) = category {
            self.conn.query(sql, params![category.as_str(), 0.0]).await
        } else {
            self.conn.query(sql, params![0.0]).await
        }
        .map_err(|e| db_error("load_bank_vectors", e))?;

        let mut fact_count = 0;
        let mut vectors = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("load_bank_vectors", e))?
        {
            fact_count += 1;
            let value = row
                .get::<libsql::Value>(0)
                .map_err(|e| db_error("load_bank_vectors", e))?;
            if let Some(vector) = deserialize_vector_value(value, "load_bank_vectors")? {
                vectors.push(vector);
            }
        }
        Ok((fact_count, vectors))
    }

    async fn last_insert_rowid(&self, operation: &str) -> Result<i64> {
        let mut rows = self
            .conn
            .query("SELECT last_insert_rowid()", ())
            .await
            .map_err(|e| db_error(operation, e))?;
        let row = rows
            .next()
            .await
            .map_err(|e| db_error(operation, e))?
            .ok_or_else(|| db_message(operation, "last_insert_rowid returned no rows"))?;
        row.get::<i64>(0).map_err(|e| db_error(operation, e))
    }

    fn encode_vector(
        &self,
        content: &str,
        entities: &[String],
        operation: &str,
    ) -> Result<Vec<u8>> {
        let vector = self.encoder.encode_fact(content, entities);
        HolographicEncoder::serialize(&vector)
            .map_err(|e| db_message(operation, format!("failed to serialize vector: {e}")))
    }

    async fn update_fact_vector(
        &self,
        fact_id: i64,
        content: &str,
        entities: &[String],
        operation: &str,
    ) -> Result<()> {
        let vector = self.encode_vector(content, entities, operation)?;
        self.conn
            .execute(
                "UPDATE memory_facts
                 SET hrr_vector = ?1,
                     hrr_algebra = ?2,
                     hrr_dim = ?3,
                     updated_at = ?4
                 WHERE fact_id = ?5",
                params![
                    vector,
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    current_timestamp(),
                    fact_id,
                ],
            )
            .await
            .map_err(|e| db_error(operation, e))?;
        Ok(())
    }

    async fn mark_fact_banks_dirty(&self, category: MemoryCategory) -> Result<()> {
        self.mark_bank_dirty("all").await?;
        self.mark_bank_dirty(category.as_str()).await
    }

    async fn mark_bank_dirty(&self, bank_name: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO memory_bank_dirty (bank_name, updated_at)
                 VALUES (?1, ?2)
                 ON CONFLICT(bank_name) DO UPDATE SET updated_at = excluded.updated_at",
                params![bank_name, current_timestamp()],
            )
            .await
            .map_err(|e| db_error("mark_bank_dirty", e))?;
        Ok(())
    }
}

fn merge_entities(content: &str, explicit: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut entities = Vec::new();
    for entity in explicit.iter().cloned().chain(extract_entities(content)) {
        let normalized = normalize_entity(&entity);
        if normalized.is_empty() {
            continue;
        }
        if seen.insert(normalized.to_ascii_lowercase()) {
            entities.push(normalized);
        }
    }
    entities
}

fn to_json_string<T: serde::Serialize>(value: &T, operation: &str) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| db_message(operation, format!("failed to serialize JSON: {e}")))
}

fn parse_json_array(value: &str, operation: &str) -> Result<Vec<String>> {
    serde_json::from_str(value)
        .map_err(|e| db_message(operation, format!("failed to parse JSON array: {e}")))
}

fn parse_category(value: &str, operation: &str) -> Result<MemoryCategory> {
    value
        .parse()
        .map_err(|e| db_message(operation, format!("failed to parse category: {e}")))
}

fn fact_from_row(row: &libsql::Row, operation: &str, entities: Vec<String>) -> Result<FactRecord> {
    let category = parse_category(
        &row.get::<String>(2).map_err(|e| db_error(operation, e))?,
        operation,
    )?;
    let tags = parse_json_array(
        &row.get::<String>(3).map_err(|e| db_error(operation, e))?,
        operation,
    )?;
    let metadata =
        serde_json::from_str(&row.get::<String>(13).map_err(|e| db_error(operation, e))?)
            .map_err(|e| db_message(operation, format!("failed to parse metadata: {e}")))?;

    Ok(FactRecord {
        fact_id: row.get::<i64>(0).map_err(|e| db_error(operation, e))?,
        content: row.get::<String>(1).map_err(|e| db_error(operation, e))?,
        category,
        tags,
        entities,
        trust_score: row.get::<f64>(4).map_err(|e| db_error(operation, e))?,
        source: Some(row.get::<String>(5).map_err(|e| db_error(operation, e))?),
        retrieval_count: row.get::<i64>(6).map_err(|e| db_error(operation, e))?,
        helpful_count: row.get::<i64>(7).map_err(|e| db_error(operation, e))?,
        unhelpful_count: row.get::<i64>(8).map_err(|e| db_error(operation, e))?,
        created_at: row.get::<i64>(9).map_err(|e| db_error(operation, e))?,
        updated_at: row.get::<i64>(10).map_err(|e| db_error(operation, e))?,
        last_retrieved_at: row
            .get::<Option<i64>>(11)
            .map_err(|e| db_error(operation, e))?,
        last_feedback_at: row
            .get::<Option<i64>>(12)
            .map_err(|e| db_error(operation, e))?,
        metadata,
    })
}

fn deserialize_vector_value(value: libsql::Value, operation: &str) -> Result<Option<Vec<f64>>> {
    match value {
        libsql::Value::Blob(bytes) => HolographicEncoder::deserialize(&bytes)
            .map(Some)
            .map_err(|e| db_message(operation, format!("failed to decode vector: {e}"))),
        libsql::Value::Null => Ok(None),
        _ => Err(db_message(
            operation,
            "hrr_vector contained a non-blob value",
        )),
    }
}

fn sql_i64_list(ids: &[i64]) -> Option<String> {
    if ids.is_empty() {
        None
    } else {
        Some(
            ids.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}

fn feedback_action_str(action: FeedbackAction) -> &'static str {
    match action {
        FeedbackAction::Helpful => "helpful",
        FeedbackAction::Unhelpful => "unhelpful",
    }
}

fn normalized_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_LIMIT
    } else {
        limit.min(i64::MAX as usize)
    }
}

fn average_vectors(vectors: &[Vec<f64>]) -> Vec<f64> {
    if vectors.is_empty() {
        return vec![0.0; HolographicEncoder::DIMENSIONS];
    }

    let mut average = vec![0.0; HolographicEncoder::DIMENSIONS];
    let mut count = 0.0;
    for vector in vectors {
        if vector.len() != HolographicEncoder::DIMENSIONS {
            continue;
        }
        count += 1.0;
        for (target, value) in average.iter_mut().zip(vector) {
            *target += value;
        }
    }
    if count > 0.0 {
        for value in &mut average {
            *value /= count;
        }
    }
    average
}

fn normalize_bank_name(bank_name: &str) -> String {
    bank_name
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_")
}

fn db_error(operation: &str, error: impl fmt::Display) -> TokenSaveError {
    TokenSaveError::Database {
        message: error.to_string(),
        operation: operation.to_string(),
    }
}

fn db_message(operation: &str, message: impl Into<String>) -> TokenSaveError {
    TokenSaveError::Database {
        message: message.into(),
        operation: operation.to_string(),
    }
}
