//! Fact CRUD operations for `MemoryStore`.

use std::collections::BTreeSet;

use crate::db::engine::{Value, params};
use crate::errors::Result;
use crate::memory::diff::{
    NEAR_DUPLICATE_THRESHOLD, classify_add_diff, combined_similarity, normalized_equivalent,
    vector_similarity,
};
use crate::memory::encoding::HolographicEncoder;
use crate::memory::hygiene::detect_secret_like;
use crate::memory::similarity::content_tokens;
use crate::memory::trust::clamp_trust;
use crate::memory::types::{
    AddFactDiff, AddFactDiffKind, AddFactOutcome, AddFactRequest, FactRecord, UpdateFactRequest,
};
use crate::sync::content_hash;
use crate::tracedecay::current_timestamp;

use super::{
    HRR_ALGEBRA, MEMORY_SOURCE_DEFAULT, MemoryStore, db_error, db_message,
    deserialize_vector_value, merge_entities, merge_metadata_object, serialize_vector,
    to_json_string,
};

impl MemoryStore<'_> {
    pub async fn add_fact(
        &self,
        request: AddFactRequest,
        default_trust: f64,
    ) -> Result<AddFactOutcome> {
        self.with_immediate_tx("add_fact", move |store| {
            Box::pin(store.add_fact_inner(request, default_trust))
        })
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
            if diff.diff == AddFactDiffKind::NearDuplicate
                && let Some(closest_id) = diff.closest_fact_id
                && let Some(closest) = self.get_fact(closest_id).await?
                && normalized_equivalent(&content, &closest.content)
            {
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
                .get::<Value>(2)
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
        if let Some(content) = request.content.as_ref().map(|value| value.trim())
            && !content.is_empty()
            && let Some(reason) = detect_secret_like(content)
        {
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
                format!("rejected_secret_like: content matched secret-likeness rule: {reason}"),
            ));
        }
        self.with_immediate_tx("update_fact", move |store| {
            Box::pin(store.update_fact_inner(request))
        })
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
        if content_was_supplied && let Some(reason) = detect_secret_like(&content) {
            return Err(db_message(
                "update_fact",
                format!("rejected_secret_like: content matched secret-likeness rule: {reason}"),
            ));
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
        self.with_immediate_tx("merge_facts", move |store| {
            Box::pin(store.merge_facts_inner(winner_id, loser_ids, merged_content))
        })
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
        let existing_losers = self.facts_exist(&loser_ids).await?;
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
            if !existing_losers.contains(loser_id) {
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
        self.with_immediate_tx("remove_fact", move |store| {
            Box::pin(store.remove_fact_inner(fact_id))
        })
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
        if changed > 0
            && let Some(fact) = existing
        {
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
        Ok(changed > 0)
    }
}
