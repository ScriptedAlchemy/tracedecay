//! Query, entity, and contradiction retrieval over stored memory facts.

use std::collections::{HashMap, HashSet};
use std::fmt;

use libsql::{params, Connection};

use super::encoding::HolographicEncoder;
use super::entities::normalize_entity;
use super::store::MemoryStore;
use super::trust::DEFAULT_MIN_TRUST;
use super::types::{
    ContradictionResult, EntityRecord, FactRecord, FactSearchResult, MemoryCategory,
};
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::current_timestamp;

const DEFAULT_LIMIT: usize = 10;
const FTS_SCORE_WEIGHT: f64 = 0.40;
const JACCARD_SCORE_WEIGHT: f64 = 0.30;
const HOLOGRAPHIC_SCORE_WEIGHT: f64 = 0.30;

pub struct FactRetriever<'a> {
    store: MemoryStore<'a>,
    encoder: HolographicEncoder,
}

impl<'a> FactRetriever<'a> {
    pub const fn new(conn: &'a Connection) -> Self {
        Self {
            store: MemoryStore::new(conn),
            encoder: HolographicEncoder::new(),
        }
    }

    pub async fn search(
        &self,
        query: &str,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let min_trust = min_trust.unwrap_or(DEFAULT_MIN_TRUST);
        let limit = normalized_limit(limit);
        let query_tokens = tokenize(query);
        let fts_scores = self
            .fts_candidates(query, category, min_trust, limit.saturating_mul(5))
            .await?;
        let entity_candidate_ids = self
            .entity_candidates(
                query,
                &query_tokens,
                category,
                min_trust,
                limit.saturating_mul(10),
            )
            .await?;
        let mut candidates = self
            .store
            .list_facts(category, Some(min_trust), limit.saturating_mul(10))
            .await?;
        let mut candidate_ids: HashSet<i64> = candidates.iter().map(|fact| fact.fact_id).collect();
        // Collect the union of ids surfaced by FTS and entity matching that the
        // `list_facts` baseline did not already include, then hydrate them with a
        // single batched `get_facts` call instead of one round-trip per id.
        let mut missing_ids: Vec<i64> = Vec::new();
        for fact_id in fts_scores.keys().copied().chain(entity_candidate_ids) {
            if candidate_ids.insert(fact_id) {
                missing_ids.push(fact_id);
            }
        }
        if !missing_ids.is_empty() {
            let mut hydrated = self.store.get_facts(&missing_ids).await?;
            for fact_id in &missing_ids {
                if let Some(fact) = hydrated.remove(fact_id) {
                    candidates.push(fact);
                }
            }
        }

        if !query_tokens.is_empty() {
            let fts_ids: HashSet<i64> = fts_scores.keys().copied().collect();
            candidates.retain(|fact| {
                fts_ids.contains(&fact.fact_id)
                    || token_overlap(&query_tokens, &fact_search_tokens(fact)) > 0
            });
        }

        // Preload every candidate's stored vector in one batched query so the
        // scoring loop never makes a per-fact round-trip. Facts without a stored
        // vector are absent from the map and fall back to on-the-fly encoding.
        let candidate_vectors = self
            .store
            .fact_vectors(
                &candidates
                    .iter()
                    .map(|fact| fact.fact_id)
                    .collect::<Vec<_>>(),
            )
            .await?;

        let mut results = Vec::with_capacity(candidates.len());
        for fact in candidates {
            let fts_score = fts_scores.get(&fact.fact_id).copied().unwrap_or(0.0);
            let jaccard_score = jaccard(&query_tokens, &fact_search_tokens(&fact));
            let holographic_score =
                self.holographic_score_with(query, &fact, candidate_vectors.get(&fact.fact_id));
            let trust_score = fact.trust_score;
            let temporal_decay = temporal_decay_factor(fact.updated_at);
            let score = combined_score(
                fts_score,
                jaccard_score,
                holographic_score,
                trust_score,
                temporal_decay,
            );
            results.push(FactSearchResult {
                fact,
                score,
                fts_score,
                jaccard_score,
                holographic_score,
                trust_score,
                why: Some(format!(
                    "fts={fts_score:.3}, jaccard={jaccard_score:.3}, holographic={holographic_score:.3}, trust={trust_score:.3}, temporal_decay={temporal_decay:.3}"
                )),
            });
        }

        results.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| right.fact.updated_at.cmp(&left.fact.updated_at))
        });
        results.truncate(limit);

        // Access tracking for the facts actually RETURNED to the caller —
        // candidates scanned and dropped above never count, and the other
        // retrieval modes (probe/list/related/reason) deliberately do not
        // bump access_count. Batched single UPDATE, fire-and-forget: a
        // tracking failure must never fail the search itself.
        let returned_ids: Vec<i64> = results.iter().map(|result| result.fact.fact_id).collect();
        let _ = self.store.record_fact_recalls(&returned_ids).await;

        Ok(results)
    }

    pub async fn probe(
        &self,
        entity: &str,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        let fact_ids = self
            .fact_ids_for_entity(entity, category, min_trust, normalized_limit(limit))
            .await?;
        self.results_for_fact_ids(&fact_ids, "entity probe").await
    }

    pub async fn related(&self, entity: &str, limit: usize) -> Result<Vec<EntityRecord>> {
        let normalized = normalize_entity(entity).to_ascii_lowercase();
        let mut rows = self
            .store
            .conn()
            .query(
                "SELECT DISTINCT related.entity_id, related.name, related.normalized_name,
                        related.entity_type, related.created_at
                 FROM memory_entities source
                 JOIN memory_fact_entities source_fe ON source_fe.entity_id = source.entity_id
                 JOIN memory_fact_entities related_fe ON related_fe.fact_id = source_fe.fact_id
                 JOIN memory_entities related ON related.entity_id = related_fe.entity_id
                 WHERE source.normalized_name = ?1
                   AND related.normalized_name != ?1
                 ORDER BY related.name
                 LIMIT ?2",
                params![normalized, normalized_limit(limit) as i64],
            )
            .await
            .map_err(|e| db_error("related", e))?;

        let mut entities = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| db_error("related", e))? {
            let created_at = row.get::<i64>(4).map_err(|e| db_error("related", e))?;
            entities.push(EntityRecord {
                entity_id: row.get::<i64>(0).map_err(|e| db_error("related", e))?,
                name: row.get::<String>(1).map_err(|e| db_error("related", e))?,
                normalized_name: row.get::<String>(2).map_err(|e| db_error("related", e))?,
                entity_type: Some(row.get::<String>(3).map_err(|e| db_error("related", e))?),
                created_at,
                updated_at: created_at,
            });
        }
        Ok(entities)
    }

    pub async fn reason(
        &self,
        entities: &[String],
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<FactSearchResult>> {
        if entities.is_empty() {
            return Ok(Vec::new());
        }

        let normalized: Vec<String> = entities
            .iter()
            .map(|entity| normalize_entity(entity).to_ascii_lowercase())
            .filter(|entity| !entity.is_empty())
            .collect();
        if normalized.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = normalized
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");
        let required_count = normalized.len() as i64;
        let min_trust = min_trust.unwrap_or(DEFAULT_MIN_TRUST);
        let limit_usize = normalized_limit(limit);
        let limit_i64 = limit_usize as i64;
        // Bind the entity names (and the trailing scalars) as anonymous `?`
        // placeholders in positional order rather than interpolating them.
        let mut values: Vec<libsql::Value> = normalized
            .iter()
            .map(|entity| libsql::Value::Text(entity.clone()))
            .collect();
        let sql = if let Some(category) = category {
            values.push(libsql::Value::Text(category.as_str().to_string()));
            values.push(libsql::Value::Real(min_trust));
            values.push(libsql::Value::Integer(required_count));
            values.push(libsql::Value::Integer(limit_i64));
            format!(
                "SELECT f.fact_id
                 FROM memory_facts f
                 JOIN memory_fact_entities fe ON fe.fact_id = f.fact_id
                 JOIN memory_entities e ON e.entity_id = fe.entity_id
                 WHERE e.normalized_name IN ({placeholders})
                   AND f.category = ?
                   AND f.trust_score >= ?
                 GROUP BY f.fact_id
                 HAVING COUNT(DISTINCT e.normalized_name) = ?
                 ORDER BY f.updated_at DESC, f.fact_id DESC
                 LIMIT ?"
            )
        } else {
            values.push(libsql::Value::Real(min_trust));
            values.push(libsql::Value::Integer(required_count));
            values.push(libsql::Value::Integer(limit_i64));
            format!(
                "SELECT f.fact_id
                 FROM memory_facts f
                 JOIN memory_fact_entities fe ON fe.fact_id = f.fact_id
                 JOIN memory_entities e ON e.entity_id = fe.entity_id
                 WHERE e.normalized_name IN ({placeholders})
                   AND f.trust_score >= ?
                 GROUP BY f.fact_id
                 HAVING COUNT(DISTINCT e.normalized_name) = ?
                 ORDER BY f.updated_at DESC, f.fact_id DESC
                 LIMIT ?"
            )
        };
        let mut rows = self
            .store
            .conn()
            .query(&sql, values)
            .await
            .map_err(|e| db_error("reason", e))?;
        let mut fact_ids = Vec::new();
        while let Some(row) = rows.next().await.map_err(|e| db_error("reason", e))? {
            fact_ids.push(row.get::<i64>(0).map_err(|e| db_error("reason", e))?);
        }
        let mut results = self
            .results_for_fact_ids(&fact_ids, "entity reasoning")
            .await?;
        results.truncate(limit_usize);
        Ok(results)
    }

    pub async fn contradict(
        &self,
        category: MemoryCategory,
        threshold: f64,
        limit: usize,
    ) -> Result<Vec<ContradictionResult>> {
        let facts = self
            .store
            .list_facts(Some(category), Some(0.0), usize::MAX)
            .await?;
        let mut results = Vec::new();
        for (index, left) in facts.iter().enumerate() {
            for right in facts.iter().skip(index + 1) {
                if !has_shared_entity(left, right) {
                    continue;
                }
                let left_tokens = fact_search_tokens(left);
                let right_tokens = fact_search_tokens(right);
                let content_similarity = jaccard(&left_tokens, &right_tokens);
                let divergence = 1.0 - content_similarity;
                if divergence >= threshold || polarity_conflicts(&left_tokens, &right_tokens) {
                    let (existing_fact, new_content) = if has_negative_marker(&left_tokens) {
                        (right.clone(), left.content.clone())
                    } else {
                        (left.clone(), right.content.clone())
                    };
                    results.push(ContradictionResult {
                        existing_fact,
                        new_content,
                        score: divergence,
                        why: Some(format!(
                            "shared entities with content divergence={divergence:.3}"
                        )),
                    });
                    if results.len() >= normalized_limit(limit) {
                        return Ok(results);
                    }
                }
            }
        }
        Ok(results)
    }

    async fn fts_candidates(
        &self,
        query: &str,
        category: Option<MemoryCategory>,
        min_trust: f64,
        limit: usize,
    ) -> Result<HashMap<i64, f64>> {
        let Some(fts_query) = build_fts_query(query) else {
            return Ok(HashMap::new());
        };

        let sql = if category.is_some() {
            "SELECT f.fact_id, bm25(memory_facts_fts) AS rank
             FROM memory_facts_fts
             JOIN memory_facts f ON f.rowid = memory_facts_fts.rowid
             WHERE memory_facts_fts MATCH ?1
               AND f.category = ?2
               AND f.trust_score >= ?3
             ORDER BY rank
             LIMIT ?4"
        } else {
            "SELECT f.fact_id, bm25(memory_facts_fts) AS rank
             FROM memory_facts_fts
             JOIN memory_facts f ON f.rowid = memory_facts_fts.rowid
             WHERE memory_facts_fts MATCH ?1
               AND f.trust_score >= ?2
             ORDER BY rank
             LIMIT ?3"
        };

        let mut rows = if let Some(category) = category {
            self.store
                .conn()
                .query(
                    sql,
                    params![
                        fts_query,
                        category.as_str(),
                        min_trust,
                        normalized_limit(limit) as i64
                    ],
                )
                .await
        } else {
            self.store
                .conn()
                .query(
                    sql,
                    params![fts_query, min_trust, normalized_limit(limit) as i64],
                )
                .await
        }
        .map_err(|e| db_error("fts_candidates", e))?;

        let mut scores = HashMap::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("fts_candidates", e))?
        {
            let rank = row
                .get::<f64>(1)
                .map_err(|e| db_error("fts_candidates", e))?;
            scores.insert(
                row.get::<i64>(0)
                    .map_err(|e| db_error("fts_candidates", e))?,
                1.0 / (1.0 + rank.abs()),
            );
        }
        Ok(scores)
    }

    async fn entity_candidates(
        &self,
        query: &str,
        query_tokens: &[String],
        category: Option<MemoryCategory>,
        min_trust: f64,
        limit: usize,
    ) -> Result<Vec<i64>> {
        let mut terms = Vec::new();
        let normalized_query = normalize_entity(query).to_ascii_lowercase();
        if !normalized_query.is_empty() {
            terms.push(normalized_query);
        }
        terms.extend(query_tokens.iter().cloned());
        terms.sort();
        terms.dedup();
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        // Bind each term's exact and LIKE values as anonymous `?` placeholders in
        // positional order. `escape_like` still governs wildcard semantics on the
        // LIKE value, but the value is bound rather than interpolated.
        let mut values: Vec<libsql::Value> = Vec::with_capacity(terms.len() * 2 + 3);
        let predicates = terms
            .iter()
            .map(|term| {
                values.push(libsql::Value::Text(term.clone()));
                values.push(libsql::Value::Text(format!("%{}%", escape_like(term))));
                "(e.normalized_name = ? OR e.normalized_name LIKE ? ESCAPE '\\')".to_string()
            })
            .collect::<Vec<_>>()
            .join(" OR ");

        let sql = if let Some(category) = category {
            values.push(libsql::Value::Text(category.as_str().to_string()));
            values.push(libsql::Value::Real(min_trust));
            values.push(libsql::Value::Integer(normalized_limit(limit) as i64));
            format!(
                "SELECT DISTINCT f.fact_id
                 FROM memory_facts f
                 JOIN memory_fact_entities fe ON fe.fact_id = f.fact_id
                 JOIN memory_entities e ON e.entity_id = fe.entity_id
                 WHERE ({predicates})
                   AND f.category = ?
                   AND f.trust_score >= ?
                 ORDER BY f.updated_at DESC, f.fact_id DESC
                 LIMIT ?"
            )
        } else {
            values.push(libsql::Value::Real(min_trust));
            values.push(libsql::Value::Integer(normalized_limit(limit) as i64));
            format!(
                "SELECT DISTINCT f.fact_id
                 FROM memory_facts f
                 JOIN memory_fact_entities fe ON fe.fact_id = f.fact_id
                 JOIN memory_entities e ON e.entity_id = fe.entity_id
                 WHERE ({predicates})
                   AND f.trust_score >= ?
                 ORDER BY f.updated_at DESC, f.fact_id DESC
                 LIMIT ?"
            )
        };

        let mut rows = self
            .store
            .conn()
            .query(sql.as_str(), values)
            .await
            .map_err(|e| db_error("entity_candidates", e))?;

        let mut fact_ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("entity_candidates", e))?
        {
            fact_ids.push(
                row.get::<i64>(0)
                    .map_err(|e| db_error("entity_candidates", e))?,
            );
        }
        Ok(fact_ids)
    }

    async fn fact_ids_for_entity(
        &self,
        entity: &str,
        category: Option<MemoryCategory>,
        min_trust: Option<f64>,
        limit: usize,
    ) -> Result<Vec<i64>> {
        let normalized = normalize_entity(entity).to_ascii_lowercase();
        if normalized.is_empty() {
            return Ok(Vec::new());
        }

        let sql = if category.is_some() {
            "SELECT fe.fact_id
             FROM memory_entities e
             JOIN memory_fact_entities fe ON fe.entity_id = e.entity_id
             JOIN memory_facts f ON f.fact_id = fe.fact_id
             WHERE e.normalized_name = ?1
               AND f.category = ?2
               AND f.trust_score >= ?3
             ORDER BY f.updated_at DESC
             LIMIT ?4"
        } else {
            "SELECT fe.fact_id
             FROM memory_entities e
             JOIN memory_fact_entities fe ON fe.entity_id = e.entity_id
             JOIN memory_facts f ON f.fact_id = fe.fact_id
             WHERE e.normalized_name = ?1
               AND f.trust_score >= ?2
             ORDER BY f.updated_at DESC
             LIMIT ?3"
        };
        let min_trust = min_trust.unwrap_or(DEFAULT_MIN_TRUST);

        let mut rows = if let Some(category) = category {
            self.store
                .conn()
                .query(
                    sql,
                    params![
                        normalized,
                        category.as_str(),
                        min_trust,
                        normalized_limit(limit) as i64
                    ],
                )
                .await
        } else {
            self.store
                .conn()
                .query(
                    sql,
                    params![normalized, min_trust, normalized_limit(limit) as i64],
                )
                .await
        }
        .map_err(|e| db_error("fact_ids_for_entity", e))?;

        let mut fact_ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| db_error("fact_ids_for_entity", e))?
        {
            fact_ids.push(
                row.get::<i64>(0)
                    .map_err(|e| db_error("fact_ids_for_entity", e))?,
            );
        }
        Ok(fact_ids)
    }

    async fn results_for_fact_ids(
        &self,
        fact_ids: &[i64],
        why: &str,
    ) -> Result<Vec<FactSearchResult>> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        // One batched fetch, then iterate the ORIGINAL `fact_ids` order so the
        // ordering callers rely on (probe/reason) is preserved exactly.
        let facts = self.store.get_facts(fact_ids).await?;
        let mut results = Vec::with_capacity(fact_ids.len());
        for fact_id in fact_ids {
            if let Some(fact) = facts.get(fact_id).cloned() {
                let trust_score = fact.trust_score;
                results.push(FactSearchResult {
                    score: trust_score,
                    fts_score: 0.0,
                    jaccard_score: 0.0,
                    holographic_score: 1.0,
                    trust_score,
                    why: Some(why.to_string()),
                    fact,
                });
            }
        }
        Ok(results)
    }

    /// Holographic similarity between `query` and `fact`, using `stored_vector`
    /// when present and otherwise encoding the fact's vector on the fly. This is
    /// the pure form of the former `holographic_score`: callers preload vectors
    /// in bulk via [`MemoryStore::fact_vectors`] and pass the result in here.
    fn holographic_score_with(
        &self,
        query: &str,
        fact: &FactRecord,
        stored_vector: Option<&Vec<f64>>,
    ) -> f64 {
        let query_entities: Vec<String> = tokenize(query);
        let query_vector = self.encoder.encode_fact(query, &query_entities);
        let similarity = if let Some(vector) = stored_vector {
            self.encoder.similarity(&query_vector, vector)
        } else {
            let fact_vector = self.encoder.encode_fact(&fact.content, &fact.entities);
            self.encoder.similarity(&query_vector, &fact_vector)
        };
        f64::midpoint(similarity, 1.0).clamp(0.0, 1.0)
    }
}

fn build_fts_query(query: &str) -> Option<String> {
    let tokens = tokenize(query);
    if tokens.is_empty() {
        return None;
    }
    Some(
        tokens
            .into_iter()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '/' | ':' | '.') {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            push_token(&mut tokens, &mut current);
        }
    }
    if !current.is_empty() {
        push_token(&mut tokens, &mut current);
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

fn push_token(tokens: &mut Vec<String>, current: &mut String) {
    if current.len() >= 2 {
        tokens.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

fn fact_search_tokens(fact: &FactRecord) -> Vec<String> {
    let mut tokens = tokenize(&fact.content);
    for tag in &fact.tags {
        tokens.extend(tokenize(tag));
    }
    for entity in &fact.entities {
        tokens.extend(tokenize(entity));
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

fn token_overlap(left: &[String], right: &[String]) -> usize {
    let right_set: HashSet<&str> = right.iter().map(String::as_str).collect();
    left.iter()
        .filter(|token| right_set.contains(token.as_str()))
        .count()
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn jaccard(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let left_set: HashSet<&str> = left.iter().map(String::as_str).collect();
    let right_set: HashSet<&str> = right.iter().map(String::as_str).collect();
    let intersection = left_set.intersection(&right_set).count();
    let union = left_set.union(&right_set).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Recall ranking: relevance (FTS + Jaccard + holographic) weighted by trust
/// and temporal decay.
///
/// `access_count` is deliberately NOT an input. Folding access frequency into
/// the ranking would create a rich-get-richer feedback loop: frequently
/// recalled facts rank higher, get recalled even more, and crowd out newer or
/// niche-but-correct facts. Access stats exist for *curation* signals
/// (delete-reluctance for actively used facts), never for retrieval order.
fn combined_score(
    fts: f64,
    jaccard: f64,
    holographic: f64,
    trust: f64,
    temporal_decay: f64,
) -> f64 {
    let relevance = fts.mul_add(
        FTS_SCORE_WEIGHT,
        jaccard.mul_add(JACCARD_SCORE_WEIGHT, holographic * HOLOGRAPHIC_SCORE_WEIGHT),
    );
    relevance * trust * temporal_decay.clamp(0.0, 1.0)
}

fn temporal_decay_factor(updated_at: i64) -> f64 {
    if updated_at <= 0 {
        return 1.0;
    }
    let age_secs = current_timestamp().saturating_sub(updated_at).max(0) as f64;
    let age_days = age_secs / 86_400.0;
    0.5_f64.powf(age_days / 365.0).clamp(0.10, 1.0)
}

fn has_shared_entity(left: &FactRecord, right: &FactRecord) -> bool {
    let right_entities: HashSet<String> = right
        .entities
        .iter()
        .map(|entity| entity.to_ascii_lowercase())
        .collect();
    left.entities
        .iter()
        .any(|entity| right_entities.contains(&entity.to_ascii_lowercase()))
}

fn polarity_conflicts(left: &[String], right: &[String]) -> bool {
    has_negative_marker(left) != has_negative_marker(right)
}

fn has_negative_marker(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        matches!(
            token.as_str(),
            "not" | "no" | "never" | "avoid" | "dont" | "don't"
        )
    })
}

fn normalized_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_LIMIT
    } else {
        limit.min(i64::MAX as usize)
    }
}

fn db_error(operation: &str, error: impl fmt::Display) -> TraceDecayError {
    TraceDecayError::Database {
        message: error.to_string(),
        operation: operation.to_string(),
    }
}
