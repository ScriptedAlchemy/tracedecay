//! Persistence layer for memory facts, entities, vectors, and feedback.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

use libsql::{Connection, params};

use super::diff::{
    NEAR_DUPLICATE_THRESHOLD, classify_add_diff, combined_similarity, normalized_equivalent,
    vector_similarity,
};
use super::encoding::HolographicEncoder;
use super::entities::{extract_entities, normalize_entity, normalize_entity_alias};
use super::hygiene::detect_secret_like;
use super::similarity::content_tokens;
use super::trust::{DEFAULT_MIN_TRUST, apply_feedback, clamp_trust};
use super::types::{
    AddFactDiff, AddFactDiffKind, AddFactOutcome, AddFactRequest, EntityGroomingResult, FactRecord,
    FactRelationKind, FactRelationRecord, FeedbackAction, FeedbackRequest, FeedbackResult,
    MemoryCategory, MemoryGroomingOperation, MemoryGroomingReport, TrustHistoryEntry,
    UpdateFactRequest,
};
use crate::errors::{Result, TraceDecayError};
use crate::sync::content_hash;
use crate::tracedecay::current_timestamp;

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
    ) -> Result<AddFactOutcome> {
        self.with_immediate_tx("add_fact", self.add_fact_inner(request, default_trust))
            .await
    }

    async fn add_fact_inner(
        &self,
        request: AddFactRequest,
        default_trust: f64,
    ) -> Result<AddFactOutcome> {
        let content = request.content.trim().to_string();
        if content.is_empty() {
            return Err(db_message("add_fact", "fact content cannot be empty"));
        }

        // Write-time hygiene gate: conservative, rule-based secret detection.
        // Secret-like content is REJECTED (never stored); only a content hash
        // is recorded in the oplog.
        if let Some(reason) = detect_secret_like(&content) {
            self.log_oplog(
                "reject_secret_like",
                None,
                &serde_json::json!({ "content_hash": content_hash(&content), "reason": reason }),
            )
            .await?;
            return Ok(AddFactOutcome {
                fact: None,
                diff: AddFactDiff {
                    diff: AddFactDiffKind::RejectedSecretLike,
                    closest_fact_id: None,
                    similarity: None,
                    reason: Some(format!("content matched secret-likeness rule: {reason}")),
                },
            });
        }

        let now = current_timestamp();
        let entities = merge_entities(&content, &request.entities);
        let tags_json = to_json_string(&request.tags, "add_fact")?;
        let metadata_json = to_json_string(&request.metadata, "add_fact")?;
        let phase_vector = self.encoder.encode_fact(&content, &entities);
        let vector = serialize_vector(&phase_vector, "add_fact")?;
        let source = request
            .source
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| MEMORY_SOURCE_DEFAULT.to_string());

        // Exact duplicate (content UNIQUE) and FTS near-duplicate candidates
        // are evaluated BEFORE the insert so a content-normalized equivalent
        // can conservatively skip the insert entirely.
        let pre_existing = self.get_fact_by_content(&content).await?;
        let diff = if let Some(existing) = &pre_existing {
            AddFactDiff {
                diff: AddFactDiffKind::NearDuplicate,
                closest_fact_id: Some(existing.fact_id),
                similarity: Some(1.0),
                reason: Some(format!(
                    "exact content match with fact #{}; merged entities instead of inserting",
                    existing.fact_id
                )),
            }
        } else {
            let diff = self.near_duplicate_diff(&content, &phase_vector).await?;
            // A >0.9 near-duplicate may skip the insert ONLY when the content
            // is normalized-equivalent (case/whitespace) to the closest fact.
            // Anything weaker still inserts and merely reports.
            if diff.diff == AddFactDiffKind::NearDuplicate {
                if let Some(closest_id) = diff.closest_fact_id {
                    if let Some(closest) = self.get_fact(closest_id).await? {
                        if normalized_equivalent(&content, &closest.content) {
                            let fact = self
                                .merge_duplicate_add(closest, &entities, &request.metadata)
                                .await?;
                            self.mark_fact_banks_dirty(fact.category).await?;
                            return Ok(AddFactOutcome {
                                fact: Some(fact),
                                diff: AddFactDiff {
                                    reason: Some(format!(
                                        "content-normalized equivalent of fact #{closest_id}; insert skipped"
                                    )),
                                    ..diff
                                },
                            });
                        }
                    }
                }
            }
            diff
        };

        self.conn
            .execute(
                "INSERT OR IGNORE INTO memory_facts (
                    content, category, tags, trust_score, created_at,
                    updated_at, source, metadata, hrr_vector, hrr_algebra, hrr_dim, hrr_precision
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    content.as_str(),
                    request.category.as_str(),
                    tags_json,
                    clamp_trust(request.trust.unwrap_or(default_trust)),
                    now,
                    now,
                    source.as_str(),
                    metadata_json,
                    vector,
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    HolographicEncoder::HRR_PRECISION,
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
        let fact = self
            .merge_duplicate_add(existing, &entities, &request.metadata)
            .await?;
        self.mark_fact_banks_dirty(fact.category).await?;
        if pre_existing.is_none() {
            self.log_oplog(
                "add",
                Some(fact.fact_id),
                &serde_json::json!({
                    "category": fact.category.as_str(),
                    "source": source,
                    "diff": diff.diff.as_str(),
                }),
            )
            .await?;
        }
        Ok(AddFactOutcome {
            fact: Some(fact),
            diff,
        })
    }

    async fn merge_duplicate_add(
        &self,
        existing: FactRecord,
        entities: &[String],
        metadata: &serde_json::Value,
    ) -> Result<FactRecord> {
        let mut merged_entities = existing.entities.clone();
        let original_entities = merged_entities.clone();
        for entity in entities {
            if !merged_entities
                .iter()
                .any(|stored| stored.eq_ignore_ascii_case(entity))
            {
                merged_entities.push(entity.clone());
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

        let mut merged_metadata = existing.metadata.clone();
        if merge_metadata_object(&mut merged_metadata, metadata) {
            let metadata_json = to_json_string(&merged_metadata, "add_fact")?;
            self.conn
                .execute(
                    "UPDATE memory_facts
                     SET metadata = ?1, updated_at = ?2
                     WHERE fact_id = ?3",
                    params![metadata_json, current_timestamp(), existing.fact_id],
                )
                .await
                .map_err(|e| db_error("add_fact", e))?;
        }

        self.get_fact(existing.fact_id).await?.ok_or_else(|| {
            db_message(
                "add_fact",
                "inserted fact was not found when reading it back",
            )
        })
    }

    /// Scores the new content against its FTS candidates and classifies the
    /// strongest match. Deterministic lexical + phase-cosine scoring only —
    /// this is a write-time REPORT, never an automatic action.
    async fn near_duplicate_diff(
        &self,
        content: &str,
        phase_vector: &[f64],
    ) -> Result<AddFactDiff> {
        const CANDIDATE_LIMIT: i64 = 8;
        const MAX_QUERY_TOKENS: usize = 24;
        /// Report floor: weaker matches are ordinary adds and not worth a
        /// closest-fact pointer.
        const REPORT_FLOOR: f64 = 0.5;

        let tokens: Vec<String> = content_tokens(content)
            .into_iter()
            .take(MAX_QUERY_TOKENS)
            .collect();
        if tokens.is_empty() {
            return Ok(AddFactDiff::plain_add());
        }
        let fts_query = tokens
            .iter()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");

        let mut rows = self
            .conn
            .query(
                "SELECT f.fact_id, f.content, f.hrr_vector
                 FROM memory_facts_fts
                 JOIN memory_facts f ON f.rowid = memory_facts_fts.rowid
                 WHERE memory_facts_fts MATCH ?1
                 ORDER BY bm25(memory_facts_fts)
                 LIMIT ?2",
                params![fts_query, CANDIDATE_LIMIT],
            )
            .await
            .map_err(|e| db_error("near_duplicate_diff", e))?;

        let mut best: Option<(i64, String, f64)> = None;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("near_duplicate_diff", e))?
        {
            let fact_id = row
                .get::<i64>(0)
                .map_err(|e| db_error("near_duplicate_diff", e))?;
            let candidate_content = row
                .get::<String>(1)
                .map_err(|e| db_error("near_duplicate_diff", e))?;
            let stored_vector = row
                .get::<libsql::Value>(2)
                .ok()
                .and_then(|value| deserialize_vector_value(value, "near_duplicate_diff").ok())
                .flatten();
            let cosine = stored_vector
                .as_deref()
                .map(|vector| vector_similarity(phase_vector, vector));
            let similarity = combined_similarity(content, &candidate_content, cosine);
            if best
                .as_ref()
                .is_none_or(|(_, _, best_sim)| similarity > *best_sim)
            {
                best = Some((fact_id, candidate_content, similarity));
            }
        }

        let Some((closest_id, closest_content, similarity)) = best else {
            return Ok(AddFactDiff::plain_add());
        };
        if similarity < REPORT_FLOOR {
            return Ok(AddFactDiff::plain_add());
        }
        let kind = classify_add_diff(similarity, content, &closest_content);
        let rounded = (similarity * 10_000.0).round() / 10_000.0;
        let reason = match kind {
            AddFactDiffKind::NearDuplicate => Some(format!(
                "similarity {rounded:.4} to fact #{closest_id} exceeds the near-duplicate threshold ({NEAR_DUPLICATE_THRESHOLD}); stored anyway — review for consolidation"
            )),
            AddFactDiffKind::PossibleConflict => Some(format!(
                "similarity {rounded:.4} to fact #{closest_id} with a negation/state-change cue; possible supersession — review which fact is current"
            )),
            AddFactDiffKind::Add | AddFactDiffKind::RejectedSecretLike => None,
        };
        Ok(AddFactDiff {
            diff: match kind {
                AddFactDiffKind::Add | AddFactDiffKind::RejectedSecretLike => AddFactDiffKind::Add,
                other => other,
            },
            closest_fact_id: Some(closest_id),
            similarity: Some(rounded),
            reason,
        })
    }

    pub async fn update_fact(&self, request: UpdateFactRequest) -> Result<FactRecord> {
        if let Some(content) = request.content.as_ref().map(|value| value.trim()) {
            if !content.is_empty() {
                if let Some(reason) = detect_secret_like(content) {
                    self.get_fact(request.fact_id).await?.ok_or_else(|| {
                        db_message(
                            "update_fact",
                            format!("fact {} does not exist", request.fact_id),
                        )
                    })?;
                    self.log_oplog(
                        "reject_secret_like",
                        Some(request.fact_id),
                        &serde_json::json!({
                            "content_hash": content_hash(content),
                            "reason": reason
                        }),
                    )
                    .await?;
                    return Err(db_message(
                        "update_fact",
                        format!(
                            "rejected_secret_like: content matched secret-likeness rule: {reason}"
                        ),
                    ));
                }
            }
        }
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

        let content_was_supplied = request.content.is_some();
        let content = request.content.map_or_else(
            || existing.content.clone(),
            |value| value.trim().to_string(),
        );
        if content.is_empty() {
            return Err(db_message("update_fact", "fact content cannot be empty"));
        }
        if content_was_supplied {
            if let Some(reason) = detect_secret_like(&content) {
                return Err(db_message(
                    "update_fact",
                    format!("rejected_secret_like: content matched secret-likeness rule: {reason}"),
                ));
            }
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
                     hrr_precision = ?10,
                     updated_at = ?11
                 WHERE fact_id = ?12",
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
                    HolographicEncoder::HRR_PRECISION,
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
        self.log_oplog(
            "update",
            Some(updated.fact_id),
            &serde_json::json!({
                "category": updated.category.as_str(),
                "content_changed": updated.content != existing.content,
            }),
        )
        .await?;
        Ok(updated)
    }

    pub async fn merge_facts(
        &self,
        winner_id: i64,
        loser_ids: Vec<i64>,
        merged_content: Option<String>,
    ) -> Result<(bool, Vec<i64>)> {
        self.with_immediate_tx(
            "merge_facts",
            self.merge_facts_inner(winner_id, loser_ids, merged_content),
        )
        .await
    }

    async fn merge_facts_inner(
        &self,
        winner_id: i64,
        loser_ids: Vec<i64>,
        merged_content: Option<String>,
    ) -> Result<(bool, Vec<i64>)> {
        self.get_fact(winner_id).await?.ok_or_else(|| {
            db_message("merge_facts", format!("winner fact {winner_id} not found"))
        })?;

        let mut seen = BTreeSet::new();
        for loser_id in &loser_ids {
            if *loser_id == winner_id {
                return Err(db_message(
                    "merge_facts",
                    format!("loser fact {loser_id} equals winner"),
                ));
            }
            if !seen.insert(*loser_id) {
                return Err(db_message(
                    "merge_facts",
                    format!("duplicate loser fact {loser_id}"),
                ));
            }
            if self.get_fact(*loser_id).await?.is_none() {
                return Err(db_message(
                    "merge_facts",
                    format!("loser fact {loser_id} not found"),
                ));
            }
        }

        let mut content_updated = false;
        if let Some(content) = merged_content {
            self.update_fact_inner(UpdateFactRequest {
                fact_id: winner_id,
                content: Some(content),
                category: None,
                tags: None,
                entities: None,
                trust: None,
                source: None,
                metadata: None,
            })
            .await?;
            content_updated = true;
        }

        let mut deleted = Vec::with_capacity(loser_ids.len());
        self.rewire_fact_relations_inner(winner_id, &loser_ids)
            .await?;
        for loser_id in loser_ids {
            if self.remove_fact_inner(loser_id).await? {
                deleted.push(loser_id);
            }
        }
        Ok((content_updated, deleted))
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
                // Deletes log a content hash, never the content itself.
                self.log_oplog(
                    "remove",
                    Some(fact_id),
                    &serde_json::json!({
                        "category": fact.category.as_str(),
                        "content_hash": content_hash(&fact.content),
                    }),
                )
                .await?;
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
                        metadata, access_count, last_recalled_at
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

    /// Appends one row to `memory_oplog`. `detail` must never carry fact
    /// content beyond what the operation needs — deletes record a
    /// `content_hash`, not the content (hard-delete stance preserved).
    async fn log_oplog(
        &self,
        op: &str,
        fact_id: Option<i64>,
        detail: &serde_json::Value,
    ) -> Result<()> {
        let detail_json = to_json_string(detail, "log_oplog")?;
        self.conn
            .execute(
                "INSERT INTO memory_oplog (ts, op, fact_id, detail_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![current_timestamp(), op, fact_id, detail_json],
            )
            .await
            .map_err(|e| db_error("log_oplog", e))?;
        Ok(())
    }

    /// Public oplog hook for mutation flows that live outside this store
    /// (e.g. dashboard curation apply).
    pub async fn record_oplog(
        &self,
        op: &str,
        fact_id: Option<i64>,
        detail: &serde_json::Value,
    ) -> Result<()> {
        self.log_oplog(op, fact_id, detail).await
    }

    pub async fn record_feedback_event(&self, request: FeedbackRequest) -> Result<FeedbackResult> {
        self.with_immediate_tx(
            "record_feedback_event",
            self.record_feedback_event_inner(request),
        )
        .await
    }

    pub async fn fact_trust_history(&self, fact_id: i64) -> Result<Vec<TrustHistoryEntry>> {
        let mut rows = self
            .conn
            .query(
                "SELECT created_at, action, old_trust, new_trust, trust_delta, source, note
                 FROM memory_feedback_events
                 WHERE fact_id = ?1
                 ORDER BY created_at ASC, event_id ASC",
                params![fact_id],
            )
            .await
            .map_err(|e| db_error("fact_trust_history", e))?;

        let mut history = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("fact_trust_history", e))?
        {
            let action = parse_feedback_action(
                &row.get::<String>(1)
                    .map_err(|e| db_error("fact_trust_history", e))?,
                "fact_trust_history",
            )?;
            history.push(TrustHistoryEntry {
                timestamp: row
                    .get::<i64>(0)
                    .map_err(|e| db_error("fact_trust_history", e))?,
                action,
                old_trust: row
                    .get::<f64>(2)
                    .map_err(|e| db_error("fact_trust_history", e))?,
                new_trust: row
                    .get::<f64>(3)
                    .map_err(|e| db_error("fact_trust_history", e))?,
                delta: row
                    .get::<f64>(4)
                    .map_err(|e| db_error("fact_trust_history", e))?,
                source: row
                    .get::<String>(5)
                    .map_err(|e| db_error("fact_trust_history", e))?,
                note: row
                    .get::<Option<String>>(6)
                    .map_err(|e| db_error("fact_trust_history", e))?,
            });
        }
        Ok(history)
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
        self.log_oplog(
            "feedback",
            Some(request.fact_id),
            &serde_json::json!({ "action": action, "trust_delta": delta }),
        )
        .await?;
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
                    OR hrr_precision != ?3
                    OR length(hrr_vector) != ?4
                 ORDER BY updated_at DESC
                 LIMIT ?5",
                params![
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    HolographicEncoder::HRR_PRECISION,
                    HolographicEncoder::SERIALIZED_F32_BYTES as i64,
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
                // hrr_* are derived fields; recomputing them must not touch updated_at,
                // which retrieval uses for temporal decay and tie-breaking. Bumping it here
                // would let a read-only memory_status repair silently promote stale facts.
                self.conn
                    .execute(
                        "UPDATE memory_facts
                         SET hrr_vector = ?1,
                             hrr_algebra = ?2,
                             hrr_dim = ?3,
                             hrr_precision = ?4
                         WHERE fact_id = ?5",
                        params![
                            vector,
                            HRR_ALGEBRA,
                            HolographicEncoder::DIMENSIONS as i64,
                            HolographicEncoder::HRR_PRECISION,
                            *fact_id,
                        ],
                    )
                    .await
                    .map_err(|e| db_error("compute_missing_vectors", e))?;
                self.mark_fact_banks_dirty(fact.category).await?;
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
                "SELECT bank_name, updated_at FROM memory_bank_dirty ORDER BY bank_name",
                (),
            )
            .await
            .map_err(|e| db_error("rebuild_dirty_banks", e))?;
        let mut dirty_banks = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("rebuild_dirty_banks", e))?
        {
            dirty_banks.push((
                row.get::<String>(0)
                    .map_err(|e| db_error("rebuild_dirty_banks", e))?,
                row.get::<i64>(1)
                    .map_err(|e| db_error("rebuild_dirty_banks", e))?,
            ));
        }

        let mut rebuilt = 0;
        for (bank_name, dirty_updated_at) in dirty_banks {
            if bank_name == "all" {
                self.rebuild_bank("all", None).await?;
            } else {
                let category = parse_category(&bank_name, "rebuild_dirty_banks")?;
                self.rebuild_bank(category.as_str(), Some(category)).await?;
            }
            self.conn
                .execute(
                    "DELETE FROM memory_bank_dirty
                     WHERE bank_name = ?1 AND updated_at = ?2",
                    params![bank_name, dirty_updated_at],
                )
                .await
                .map_err(|e| db_error("rebuild_dirty_banks", e))?;
            rebuilt += 1;
        }
        Ok(rebuilt)
    }

    pub async fn upsert_fact_relation(
        &self,
        source_fact_id: i64,
        target_fact_id: i64,
        relation: FactRelationKind,
        confidence: f64,
        source: &str,
        metadata: serde_json::Value,
    ) -> Result<FactRelationRecord> {
        self.with_immediate_tx(
            "upsert_fact_relation",
            self.upsert_fact_relation_inner(
                source_fact_id,
                target_fact_id,
                relation,
                confidence,
                source,
                metadata,
            ),
        )
        .await
    }

    async fn upsert_fact_relation_inner(
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
        if self.get_fact(source_fact_id).await?.is_none()
            || self.get_fact(target_fact_id).await?.is_none()
        {
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
        let sql = if fact_id.is_some() {
            "SELECT source_fact_id, target_fact_id, relation, confidence, source,
                    metadata, created_at, updated_at
             FROM memory_fact_relations
             WHERE source_fact_id = ?1 OR target_fact_id = ?1
             ORDER BY source_fact_id, target_fact_id, relation"
        } else {
            "SELECT source_fact_id, target_fact_id, relation, confidence, source,
                    metadata, created_at, updated_at
             FROM memory_fact_relations
             ORDER BY source_fact_id, target_fact_id, relation"
        };
        let mut rows = if let Some(fact_id) = fact_id {
            self.conn.query(sql, params![fact_id]).await
        } else {
            self.conn.query(sql, ()).await
        }
        .map_err(|e| db_error("list_fact_relations", e))?;
        let mut relations = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("list_fact_relations", e))?
        {
            relations.push(relation_from_row(&row, "list_fact_relations")?);
        }
        Ok(relations)
    }

    pub async fn related_fact_ids(&self, fact_ids: &[i64], limit: usize) -> Result<Vec<i64>> {
        if fact_ids.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let fact_ids = &fact_ids[..fact_ids.len().min(128)];
        let placeholders = std::iter::repeat_n("?", fact_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut values = Vec::with_capacity(fact_ids.len() * 3 + 1);
        for _ in 0..3 {
            values.extend(fact_ids.iter().copied().map(libsql::Value::Integer));
        }
        values.push(libsql::Value::Integer(limit.min(256) as i64));
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

    pub async fn remove_fact_relation(
        &self,
        source_fact_id: i64,
        target_fact_id: i64,
        relation: FactRelationKind,
    ) -> Result<bool> {
        let changed = self
            .conn
            .execute(
                "DELETE FROM memory_fact_relations
                 WHERE source_fact_id = ?1 AND target_fact_id = ?2 AND relation = ?3",
                params![source_fact_id, target_fact_id, relation.as_str()],
            )
            .await
            .map_err(|e| db_error("remove_fact_relation", e))?;
        Ok(changed > 0)
    }

    pub async fn normalize_fact_tags(&self, fact_id: i64, tags: &[String]) -> Result<Vec<String>> {
        self.with_immediate_tx(
            "normalize_fact_tags",
            self.normalize_fact_tags_inner(fact_id, tags),
        )
        .await
    }

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

    pub async fn update_entity_aliases(
        &self,
        entity_id: i64,
        aliases: &[String],
    ) -> Result<Vec<String>> {
        self.with_immediate_tx(
            "update_entity_aliases",
            self.update_entity_aliases_inner(entity_id, aliases),
        )
        .await
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
        self.with_immediate_tx(
            "merge_entities",
            self.merge_entities_inner(winner_entity_id, loser_entity_ids),
        )
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
            let mut links = self
                .conn
                .query(
                    "SELECT fact_id FROM memory_fact_entities WHERE entity_id = ?1",
                    params![entity_id],
                )
                .await
                .map_err(|e| db_error("merge_entities", e))?;
            while let Some(link) = links
                .next()
                .await
                .map_err(|e| db_error("merge_entities", e))?
            {
                fact_ids.insert(
                    link.get::<i64>(0)
                        .map_err(|e| db_error("merge_entities", e))?,
                );
            }
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

    pub async fn repair_fact_vector(&self, fact_id: i64) -> Result<bool> {
        self.with_immediate_tx("repair_fact_vector", self.repair_fact_vector_inner(fact_id))
            .await
    }

    async fn repair_fact_vector_inner(&self, fact_id: i64) -> Result<bool> {
        let Some(fact) = self.get_fact(fact_id).await? else {
            return Err(db_message(
                "repair_fact_vector",
                format!("fact {fact_id} not found"),
            ));
        };
        self.update_fact_vector(fact_id, &fact.content, &fact.entities, "repair_fact_vector")
            .await?;
        self.mark_fact_banks_dirty(fact.category).await?;
        Ok(true)
    }

    /// Applies a fully prevalidated, bounded grooming batch atomically, then
    /// repairs derived vectors and dirty banks using the store's configured
    /// encoder contract before commit.
    pub async fn apply_grooming_batch(
        &self,
        operations: &[MemoryGroomingOperation],
        min_confidence: f64,
    ) -> Result<MemoryGroomingReport> {
        let mut report = self
            .with_immediate_tx(
                "apply_grooming_batch",
                self.apply_grooming_batch_inner(operations, min_confidence),
            )
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
            for fact_id in evidence {
                if self.get_fact(*fact_id).await?.is_none() {
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
                    if let Some(other) = proposed_relations.insert(key, *relation) {
                        if relations_conflict(other, *relation) {
                            return Err(db_message(
                                "apply_grooming_batch",
                                "batch proposes contradictory relation kinds for the same facts",
                            ));
                        }
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
        values.push(libsql::Value::Integer(entity_id));
        values.extend(fact_ids.iter().copied().map(libsql::Value::Integer));
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

    async fn get_fact_relation(
        &self,
        source_fact_id: i64,
        target_fact_id: i64,
        relation: FactRelationKind,
    ) -> Result<Option<FactRelationRecord>> {
        let mut rows = self
            .conn
            .query(
                "SELECT source_fact_id, target_fact_id, relation, confidence, source,
                        metadata, created_at, updated_at
                 FROM memory_fact_relations
                 WHERE source_fact_id = ?1 AND target_fact_id = ?2 AND relation = ?3",
                params![source_fact_id, target_fact_id, relation.as_str()],
            )
            .await
            .map_err(|e| db_error("get_fact_relation", e))?;
        rows.next()
            .await
            .map_err(|e| db_error("get_fact_relation", e))?
            .map(|row| relation_from_row(&row, "get_fact_relation"))
            .transpose()
    }

    async fn rewire_fact_relations_inner(&self, winner_id: i64, loser_ids: &[i64]) -> Result<()> {
        if loser_ids.is_empty() {
            return Ok(());
        }
        let loser_set: BTreeSet<i64> = loser_ids.iter().copied().collect();
        let relations = self.list_fact_relations(None).await?;
        self.conn
            .execute(
                &format!(
                    "DELETE FROM memory_fact_relations WHERE source_fact_id IN ({0}) OR target_fact_id IN ({0})",
                    std::iter::repeat_n("?", loser_ids.len()).collect::<Vec<_>>().join(",")
                ),
                loser_ids.iter().copied().map(libsql::Value::Integer).collect::<Vec<_>>(),
            )
            .await
            .map_err(|e| db_error("merge_facts", e))?;
        for relation in relations.into_iter().filter(|relation| {
            loser_set.contains(&relation.source_fact_id)
                || loser_set.contains(&relation.target_fact_id)
        }) {
            let source_fact_id = if loser_set.contains(&relation.source_fact_id) {
                winner_id
            } else {
                relation.source_fact_id
            };
            let target_fact_id = if loser_set.contains(&relation.target_fact_id) {
                winner_id
            } else {
                relation.target_fact_id
            };
            if source_fact_id != target_fact_id {
                self.upsert_fact_relation_inner(
                    source_fact_id,
                    target_fact_id,
                    relation.relation,
                    relation.confidence,
                    &relation.source,
                    relation.metadata,
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn mark_entity_fact_banks_dirty(&self, entity_id: i64, invalidate: bool) -> Result<()> {
        let mut rows = self
            .conn
            .query(
                "SELECT fact_id FROM memory_fact_entities WHERE entity_id = ?1",
                params![entity_id],
            )
            .await
            .map_err(|e| db_error("mark_entity_fact_banks_dirty", e))?;
        let mut fact_ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("mark_entity_fact_banks_dirty", e))?
        {
            fact_ids.push(
                row.get::<i64>(0)
                    .map_err(|e| db_error("mark_entity_fact_banks_dirty", e))?,
            );
        }
        for fact_id in fact_ids {
            if invalidate {
                self.invalidate_fact_vector_and_mark_dirty(fact_id).await?;
            } else if let Some(fact) = self.get_fact(fact_id).await? {
                self.mark_fact_banks_dirty(fact.category).await?;
            }
        }
        Ok(())
    }

    async fn invalidate_fact_vector_and_mark_dirty(&self, fact_id: i64) -> Result<()> {
        if let Some(fact) = self.get_fact(fact_id).await? {
            self.conn
                .execute(
                    "UPDATE memory_facts SET hrr_vector = NULL WHERE fact_id = ?1",
                    params![fact_id],
                )
                .await
                .map_err(|e| db_error("invalidate_fact_vector", e))?;
            self.mark_fact_banks_dirty(fact.category).await?;
        }
        Ok(())
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
                    name, normalized_name, entity_type, aliases, created_at, updated_at
                 )
                 VALUES (?1, ?2, 'unknown', '[]', ?3, ?3)",
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
        serialize_vector(&vector, operation)
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
                     hrr_precision = ?4,
                     updated_at = ?5
                 WHERE fact_id = ?6",
                params![
                    vector,
                    HRR_ALGEBRA,
                    HolographicEncoder::DIMENSIONS as i64,
                    HolographicEncoder::HRR_PRECISION,
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
        // `updated_at` doubles as an optimistic-concurrency token: `rebuild_dirty_banks` only
        // clears a marker whose value still matches the row it snapshotted. Since
        // `current_timestamp()` is second-resolution, a re-dirty within the same second as the
        // snapshot would reuse that value and be silently cleared, dropping the change. Force the
        // token strictly forward on every mark so same-second re-dirties are always preserved.
        self.conn
            .execute(
                "INSERT INTO memory_bank_dirty (bank_name, updated_at)
                 VALUES (?1, ?2)
                 ON CONFLICT(bank_name) DO UPDATE SET
                     updated_at = max(excluded.updated_at, memory_bank_dirty.updated_at + 1)",
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

fn merge_metadata_object(existing: &mut serde_json::Value, incoming: &serde_json::Value) -> bool {
    let Some(incoming) = incoming.as_object() else {
        return false;
    };
    if incoming.is_empty() {
        return false;
    }

    if existing.is_null() {
        *existing = serde_json::Value::Object(incoming.clone());
        return true;
    }
    let Some(existing) = existing.as_object_mut() else {
        return false;
    };

    let mut changed = false;
    for (key, value) in incoming {
        if existing.get(key) != Some(value) {
            existing.insert(key.clone(), value.clone());
            changed = true;
        }
    }
    changed
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
        access_count: row.get::<i64>(14).map_err(|e| db_error(operation, e))?,
        helpful_count: row.get::<i64>(7).map_err(|e| db_error(operation, e))?,
        unhelpful_count: row.get::<i64>(8).map_err(|e| db_error(operation, e))?,
        created_at: row.get::<i64>(9).map_err(|e| db_error(operation, e))?,
        updated_at: row.get::<i64>(10).map_err(|e| db_error(operation, e))?,
        last_retrieved_at: row
            .get::<Option<i64>>(11)
            .map_err(|e| db_error(operation, e))?,
        last_recalled_at: row
            .get::<Option<i64>>(15)
            .map_err(|e| db_error(operation, e))?,
        last_feedback_at: row
            .get::<Option<i64>>(12)
            .map_err(|e| db_error(operation, e))?,
        metadata,
    })
}

fn relation_from_row(row: &libsql::Row, operation: &str) -> Result<FactRelationRecord> {
    let relation = row
        .get::<String>(2)
        .map_err(|e| db_error(operation, e))?
        .parse::<FactRelationKind>()
        .map_err(|e| db_message(operation, e))?;
    let metadata = serde_json::from_str(&row.get::<String>(5).map_err(|e| db_error(operation, e))?)
        .map_err(|e| db_message(operation, format!("failed to parse relation metadata: {e}")))?;
    Ok(FactRelationRecord {
        source_fact_id: row.get::<i64>(0).map_err(|e| db_error(operation, e))?,
        target_fact_id: row.get::<i64>(1).map_err(|e| db_error(operation, e))?,
        relation,
        confidence: row.get::<f64>(3).map_err(|e| db_error(operation, e))?,
        source: row.get::<String>(4).map_err(|e| db_error(operation, e))?,
        metadata,
        created_at: row.get::<i64>(6).map_err(|e| db_error(operation, e))?,
        updated_at: row.get::<i64>(7).map_err(|e| db_error(operation, e))?,
    })
}

fn relations_conflict(left: FactRelationKind, right: FactRelationKind) -> bool {
    matches!(
        (left, right),
        (FactRelationKind::Supports, FactRelationKind::Contradicts)
            | (FactRelationKind::Contradicts, FactRelationKind::Supports)
    )
}

fn serialize_vector(vector: &[f64], operation: &str) -> Result<Vec<u8>> {
    HolographicEncoder::serialize(vector)
        .map_err(|e| db_message(operation, format!("failed to serialize vector: {e}")))
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

fn parse_feedback_action(value: &str, operation: &str) -> Result<FeedbackAction> {
    match value {
        "helpful" => Ok(FeedbackAction::Helpful),
        "unhelpful" => Ok(FeedbackAction::Unhelpful),
        other => Err(db_message(
            operation,
            format!("failed to parse feedback action: {other}"),
        )),
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

fn db_error(operation: &str, error: impl fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: error.to_string(),
        operation: operation.to_string(),
    }
}

fn db_message(operation: &str, message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        message: message.into(),
        operation: operation.to_string(),
    }
}
