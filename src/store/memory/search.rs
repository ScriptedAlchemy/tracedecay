//! Compatibility fact search, ranking, cursors, and retrieval recording.

use std::collections::BTreeSet;

use crate::memory::entities::normalize_entity;

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use serde_json::{Value, json};

use tracedecay_domain::{Confidence, FactCategoryV1, FactId, FactOwnerV1, UtcMicros};
use tracedecay_store::{
    CompatibilityFactContradictionPageV1, CompatibilityFactContradictionQueryV1,
    CompatibilityFactContradictionV1, CompatibilityFactListQueryV1, CompatibilityFactProjectionV1,
    CompatibilityFactRetrievalCommandV1, CompatibilityFactSearchCursorV1,
    CompatibilityFactSearchHitV1, CompatibilityFactSearchKindV1, CompatibilityFactSearchPageV1,
    CompatibilityFactSearchQuery, CompatibilityFactSearchScoresV1, CompatibilityFactV1,
    FactCompatibilityResult, FactStoreError, FactStoreResult,
};

use super::crud::{list_compatibility_facts_tx, load_current_fact_tx};
use super::envelope::{
    CompatibilityOperationReceiptV1, compatibility_digest,
    compatibility_lookup_operation_receipt_tx, compatibility_record_operation_receipt_tx,
    compatibility_target_digest,
};
use super::primitives::{
    COMPATIBILITY_READ_OPERATION, COMPATIBILITY_WRITE_OPERATION, OwnerKey,
    compatibility_category_label, compatibility_legacy_timestamp, compatibility_now,
    compatibility_source_store_id, row_i64, row_string, storage_error, storage_message,
};
use super::projection::{
    compatibility_required_mapping_tx, load_compatibility_projections_tx,
    resolve_compatibility_target_tx,
};
use super::scoring::{
    compatibility_fact_tokens, compatibility_holographic_score, compatibility_jaccard,
    compatibility_millionths, compatibility_temporal_decay, compatibility_term_coverage,
    compatibility_tokens,
};
use crate::memory::encoding::HolographicEncoder;

fn compatibility_search_scores(
    query_tokens: &[String],
    encoder: &HolographicEncoder,
    query_vector: &[f64],
    fact: &CompatibilityFactV1,
    now: UtcMicros,
) -> FactStoreResult<(CompatibilityFactSearchScoresV1, String)> {
    let fact_tokens = compatibility_fact_tokens(fact);
    let coverage = compatibility_term_coverage(query_tokens, &fact_tokens);
    let fts = coverage;
    let jaccard = compatibility_jaccard(query_tokens, &fact_tokens);
    let holographic = compatibility_holographic_score(encoder, query_vector, fact);
    let trust = fact.fact().trust().as_f64();
    let temporal_decay = compatibility_temporal_decay(fact.telemetry().updated_at(), now);
    let usage_boost = 1.0 + (0.02 * (fact.telemetry().retrieval_count() as f64).ln_1p()).min(0.5);
    let score =
        (fts * 0.40 + jaccard * 0.30 + holographic * 0.30) * trust * temporal_decay * usage_boost;
    Ok((
        CompatibilityFactSearchScoresV1::new(
            compatibility_millionths(score),
            compatibility_millionths(fts),
            compatibility_millionths(jaccard),
            compatibility_millionths(holographic),
            compatibility_millionths(trust),
        )?,
        format!(
            "fts={fts:.3}, coverage={coverage:.3}, jaccard={jaccard:.3}, holographic={holographic:.3}, trust={trust:.3}, temporal_decay={temporal_decay:.3}, retrieval_count={}",
            fact.telemetry().retrieval_count(),
        ),
    ))
}

async fn compatibility_available_facts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    category: Option<FactCategoryV1>,
    min_trust: Option<Confidence>,
) -> FactCompatibilityResult<Vec<CompatibilityFactV1>> {
    let query = CompatibilityFactListQueryV1::new(owner.clone(), category, min_trust, None, 1_000)?;
    let page = list_compatibility_facts_tx(transaction, &query).await?;
    Ok(page
        .facts()
        .iter()
        .filter_map(|projection| match projection {
            CompatibilityFactProjectionV1::Available(fact) => Some(fact.as_ref().clone()),
            CompatibilityFactProjectionV1::Unavailable(_) => None,
        })
        .collect())
}

async fn compatibility_search_candidates_tx(
    transaction: &Transaction<'_>,
    query: &CompatibilityFactSearchQuery,
    min_trust: Confidence,
) -> FactCompatibilityResult<Vec<CompatibilityFactV1>> {
    let text = query.query().ok_or_else(|| {
        storage_message(
            COMPATIBILITY_READ_OPERATION,
            "compatibility search query is missing",
        )
    })?;
    let mut tokens = compatibility_tokens(text)
        .into_iter()
        .map(|token| token.to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    if tokens.is_empty() {
        return compatibility_available_facts_tx(
            transaction,
            query.owner(),
            query.filter().category(),
            Some(min_trust),
        )
        .await;
    }
    tokens.truncate(8);

    let key = OwnerKey::new(query.owner())?;
    let mut values = vec![
        crate::db::engine::Value::Text(key.kind.to_string()),
        crate::db::engine::Value::Text(key.project_id.clone()),
        crate::db::engine::Value::Text(key.json.clone()),
        crate::db::engine::Value::Real(min_trust.as_f64()),
    ];
    let category_filter = query
        .filter()
        .category()
        .map_or_else(String::new, |category| {
            let index = values.len() + 1;
            values.push(crate::db::engine::Value::Text(
                compatibility_category_label(category).to_owned(),
            ));
            format!("AND json_extract(payloads.payload_json, '$.category') = ?{index}")
        });
    let mut token_filters = Vec::with_capacity(tokens.len());
    for token in tokens {
        let content_index = values.len() + 1;
        values.push(crate::db::engine::Value::Text(format!("%{token}%")));
        let payload_index = values.len() + 1;
        values.push(crate::db::engine::Value::Text(format!("%{token}%")));
        token_filters.push(format!(
            "(lower(payloads.content) LIKE ?{content_index} \
              OR lower(payloads.payload_json) LIKE ?{payload_index})"
        ));
    }
    let limit_index = values.len() + 1;
    let candidate_limit = query.limit().saturating_mul(2).clamp(4, 32);
    values.push(crate::db::engine::Value::Integer(
        i64::try_from(candidate_limit).unwrap_or(256),
    ));
    let sql = format!(
        "SELECT current_facts.fact_id
         FROM memory_v2_current_facts AS current_facts
         JOIN memory_v2_facts AS facts
           ON facts.fact_id = current_facts.fact_id
          AND facts.owner_kind = current_facts.owner_kind
          AND facts.project_id = current_facts.project_id
         JOIN memory_v2_assertion_payloads AS payloads
           ON payloads.assertion_id = current_facts.active_assertion_id
          AND payloads.fact_id = current_facts.fact_id
          AND payloads.owner_kind = current_facts.owner_kind
          AND payloads.project_id = current_facts.project_id
         WHERE current_facts.owner_kind = ?1
           AND current_facts.project_id = ?2
           AND facts.owner_json = ?3
           AND current_facts.active_assertion_id IS NOT NULL
           AND current_facts.trust_score >= ?4
           {category_filter}
           AND ({})
         ORDER BY current_facts.updated_at DESC, current_facts.fact_id ASC
         LIMIT ?{limit_index}",
        token_filters.join(" OR ")
    );
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        fact_ids.push(
            FactId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let facts = load_compatibility_projections_tx(transaction, query.owner(), &fact_ids)
        .await?
        .into_iter()
        .filter_map(|projection| match projection {
            CompatibilityFactProjectionV1::Available(fact) => Some(fact.as_ref().clone()),
            CompatibilityFactProjectionV1::Unavailable(_) => None,
        })
        .collect();
    Ok(facts)
}

fn compatibility_matches_entity(fact: &CompatibilityFactV1, entity: &str) -> bool {
    let normalized = normalize_entity(entity).to_ascii_lowercase();
    !normalized.is_empty()
        && fact.entities().is_some_and(|entities| {
            entities.iter().any(|candidate| {
                normalize_entity(candidate).eq_ignore_ascii_case(normalized.as_str())
            })
        })
}

fn compatibility_matches_all_entities(fact: &CompatibilityFactV1, entities: &[String]) -> bool {
    entities
        .iter()
        .all(|entity| compatibility_matches_entity(fact, entity))
}

async fn compatibility_rank_facts_tx(
    transaction: &Transaction<'_>,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    let min_trust = query
        .filter()
        .min_trust()
        .unwrap_or(Confidence::new(0.3).map_err(FactStoreError::from)?);
    let mut facts = if matches!(query.kind(), CompatibilityFactSearchKindV1::Search) {
        compatibility_search_candidates_tx(transaction, query, min_trust).await?
    } else {
        compatibility_available_facts_tx(
            transaction,
            query.owner(),
            query.filter().category(),
            Some(min_trust),
        )
        .await?
    };
    let now = compatibility_now()?;
    let mut ranked = Vec::with_capacity(facts.len());
    match query.kind() {
        CompatibilityFactSearchKindV1::Search => {
            let text = query.query().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "compatibility search query is missing",
                )
            })?;
            let tokens = compatibility_tokens(text);
            let encoder = HolographicEncoder::new();
            let query_vector = encoder.encode_fact(text, &tokens);
            for fact in facts.drain(..) {
                let (scores, why) =
                    compatibility_search_scores(&tokens, &encoder, &query_vector, &fact, now)?;
                // Mirror the legacy retriever's relevance floor: a non-empty
                // query only returns facts with a real textual signal (FTS/term
                // overlap). Facts surfaced solely by the dense holographic
                // baseline or trust are never relevant matches, so scoring them
                // above zero must not pull unrelated facts into the results (or
                // bump their access counts).
                if !tokens.is_empty()
                    && scores.fts_score_millionths() == 0
                    && scores.jaccard_score_millionths() == 0
                {
                    continue;
                }
                if query
                    .filter()
                    .threshold_millionths()
                    .is_some_and(|threshold| scores.score_millionths() < threshold)
                {
                    continue;
                }
                ranked.push((
                    CompatibilityFactSearchHitV1::new(fact.clone(), scores, Some(why))?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
        CompatibilityFactSearchKindV1::Probe => {
            let entity = query.query().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_READ_OPERATION,
                    "compatibility probe query is missing",
                )
            })?;
            for fact in facts
                .drain(..)
                .filter(|fact| compatibility_matches_entity(fact, entity))
            {
                let trust = compatibility_millionths(fact.fact().trust().as_f64());
                let scores = CompatibilityFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
                ranked.push((
                    CompatibilityFactSearchHitV1::new(
                        fact.clone(),
                        scores,
                        Some("entity probe".to_owned()),
                    )?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
        CompatibilityFactSearchKindV1::Related { entity } => {
            for fact in facts
                .drain(..)
                .filter(|fact| compatibility_matches_entity(fact, &entity))
            {
                let trust = compatibility_millionths(fact.fact().trust().as_f64());
                let scores = CompatibilityFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
                ranked.push((
                    CompatibilityFactSearchHitV1::new(
                        fact.clone(),
                        scores,
                        Some("entity related".to_owned()),
                    )?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
        CompatibilityFactSearchKindV1::Reason { entities } => {
            for fact in facts
                .drain(..)
                .filter(|fact| compatibility_matches_all_entities(fact, &entities))
            {
                let trust = compatibility_millionths(fact.fact().trust().as_f64());
                let scores = CompatibilityFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
                ranked.push((
                    CompatibilityFactSearchHitV1::new(
                        fact.clone(),
                        scores,
                        Some("entity reasoning".to_owned()),
                    )?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
    }
    ranked.sort_by(|(left, left_updated), (right, right_updated)| {
        right
            .score_millionths()
            .cmp(&left.score_millionths())
            .then_with(|| right_updated.cmp(left_updated))
            .then_with(|| left.fact().fact_id().cmp(right.fact().fact_id()))
    });
    if let Some(after) = query.after() {
        ranked.retain(|(hit, updated_at)| {
            hit.score_millionths() < after.score_millionths()
                || (hit.score_millionths() == after.score_millionths()
                    && (*updated_at < after.updated_at()
                        || (*updated_at == after.updated_at()
                            && hit.fact().fact_id() > after.fact_id())))
        });
    }
    let has_more = ranked.len() > query.limit();
    ranked.truncate(query.limit());
    let next_after = if has_more {
        ranked.last().map(|(hit, updated_at)| {
            CompatibilityFactSearchCursorV1::new(
                hit.score_millionths(),
                *updated_at,
                hit.fact().fact_id().clone(),
            )
        })
    } else {
        None
    }
    .transpose()?;
    CompatibilityFactSearchPageV1::new(
        query.owner().clone(),
        ranked.into_iter().map(|(hit, _)| hit).collect(),
        next_after,
    )
    .map_err(Into::into)
}

pub(super) async fn search_compatibility_facts_tx(
    transaction: &Transaction<'_>,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    compatibility_rank_facts_tx(transaction, query).await
}

pub(super) async fn probe_compatibility_facts_tx(
    transaction: &Transaction<'_>,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    compatibility_rank_facts_tx(transaction, query).await
}

pub(super) async fn related_compatibility_facts_tx(
    transaction: &Transaction<'_>,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    let CompatibilityFactSearchKindV1::Related { entity } = query.kind() else {
        return Err(storage_message(
            COMPATIBILITY_READ_OPERATION,
            "compatibility related query has the wrong kind",
        )
        .into());
    };
    let key = OwnerKey::new(query.owner())?;
    let source_store_id = compatibility_source_store_id()?;
    let normalized = normalize_entity(&entity).to_ascii_lowercase();
    let mut entity_rows = transaction
        .query(
            "SELECT DISTINCT entities.entity_id
             FROM memory_entities AS entities
             JOIN memory_fact_entities AS links ON links.entity_id = entities.entity_id
             JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
             WHERE mappings.owner_kind = ?1 AND mappings.project_id = ?2
               AND mappings.owner_json = ?3 AND mappings.source_store_id = ?4
               AND (
                    entities.normalized_name = ?5
                    OR (
                        json_valid(entities.aliases)
                        AND EXISTS(
                            SELECT 1 FROM json_each(entities.aliases) AS aliases
                            WHERE lower(trim(CAST(aliases.value AS TEXT))) = ?5
                        )
                    )
               )
              ORDER BY entities.name ASC, entities.entity_id ASC
              LIMIT 256",
            params![
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                source_store_id.as_str(),
                normalized,
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut source_entity_ids = Vec::new();
    while let Some(row) = entity_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        source_entity_ids.push(row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?);
    }
    drop(entity_rows);
    if source_entity_ids.is_empty() {
        return CompatibilityFactSearchPageV1::new(query.owner().clone(), Vec::new(), None)
            .map_err(Into::into);
    }

    let placeholders = std::iter::repeat_n("?", source_entity_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut values = Vec::with_capacity(source_entity_ids.len() + 4);
    values.push(crate::db::engine::Value::Text(key.kind.to_string()));
    values.push(crate::db::engine::Value::Text(key.project_id.clone()));
    values.push(crate::db::engine::Value::Text(key.json.clone()));
    values.push(crate::db::engine::Value::Text(
        source_store_id.as_str().to_owned(),
    ));
    values.extend(
        source_entity_ids
            .iter()
            .copied()
            .map(crate::db::engine::Value::Integer),
    );
    let sql = format!(
        "SELECT DISTINCT co_entities.entity_id, co_entities.name
         FROM memory_fact_entities AS source_links
         JOIN memory_fact_entities AS co_links ON co_links.fact_id = source_links.fact_id
         JOIN memory_entities AS co_entities ON co_entities.entity_id = co_links.entity_id
         JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = source_links.fact_id
         WHERE mappings.owner_kind = ? AND mappings.project_id = ?
           AND mappings.owner_json = ? AND mappings.source_store_id = ?
           AND source_links.entity_id IN ({placeholders})
           AND co_links.entity_id NOT IN ({placeholders})
         ORDER BY co_entities.name ASC, co_entities.entity_id ASC
         LIMIT ?",
    );
    // The source-id list appears twice. Bind a separate, fixed-width value
    // list so this remains parameterized rather than interpolating identifiers.
    let mut co_values = values.clone();
    co_values.extend(
        source_entity_ids
            .iter()
            .copied()
            .map(crate::db::engine::Value::Integer),
    );
    co_values.push(crate::db::engine::Value::Integer(query.limit() as i64));
    let mut co_rows = transaction
        .query(&sql, co_values)
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
    let mut co_entities = Vec::new();
    while let Some(row) = co_rows
        .next()
        .await
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
    {
        co_entities.push((
            row_i64(&row, 0, COMPATIBILITY_READ_OPERATION)?,
            row_string(&row, 1, COMPATIBILITY_READ_OPERATION)?,
        ));
    }
    drop(co_rows);

    let per_entity_limit = query.limit().saturating_mul(2).max(1);
    let mut encountered = Vec::new();
    let mut seen = BTreeSet::new();
    for (entity_id, _) in co_entities {
        let category = query.filter().category().map(compatibility_category_label);
        let min_trust = query
            .filter()
            .min_trust()
            .unwrap_or(Confidence::new(0.3).map_err(FactStoreError::from)?)
            .as_f64();
        let mut rows = match category {
            Some(category) => transaction
                .query(
                    "SELECT mappings.fact_id
                     FROM memory_fact_entities AS links
                     JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
                     JOIN memory_facts AS legacy_facts ON legacy_facts.fact_id = links.fact_id
                     WHERE links.entity_id = ?1
                       AND mappings.owner_kind = ?2 AND mappings.project_id = ?3
                       AND mappings.owner_json = ?4 AND mappings.source_store_id = ?5
                       AND legacy_facts.category = ?6 AND legacy_facts.trust_score >= ?7
                     ORDER BY legacy_facts.updated_at DESC, mappings.fact_id ASC LIMIT ?8",
                    params![
                        entity_id,
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        source_store_id.as_str(),
                        category,
                        min_trust,
                        per_entity_limit as i64,
                    ],
                )
                .await,
            None => transaction
                .query(
                    "SELECT mappings.fact_id
                     FROM memory_fact_entities AS links
                     JOIN memory_v2_legacy_map AS mappings ON mappings.legacy_fact_id = links.fact_id
                     JOIN memory_facts AS legacy_facts ON legacy_facts.fact_id = links.fact_id
                     WHERE links.entity_id = ?1
                       AND mappings.owner_kind = ?2 AND mappings.project_id = ?3
                       AND mappings.owner_json = ?4 AND mappings.source_store_id = ?5
                       AND legacy_facts.trust_score >= ?6
                     ORDER BY legacy_facts.updated_at DESC, mappings.fact_id ASC LIMIT ?7",
                    params![
                        entity_id,
                        key.kind,
                        key.project_id.as_str(),
                        key.json.as_str(),
                        source_store_id.as_str(),
                        min_trust,
                        per_entity_limit as i64,
                    ],
                )
                .await,
        }
        .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| storage_error(COMPATIBILITY_READ_OPERATION, error))?
        {
            let fact_id = FactId::new(row_string(&row, 0, COMPATIBILITY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?;
            if seen.insert(fact_id.clone()) {
                encountered.push(fact_id);
            }
        }
    }
    let mut ranked = Vec::new();
    for projection in
        load_compatibility_projections_tx(transaction, query.owner(), &encountered).await?
    {
        let CompatibilityFactProjectionV1::Available(fact) = projection else {
            continue;
        };
        let trust = compatibility_millionths(fact.fact().trust().as_f64());
        let scores = CompatibilityFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
        let updated_at = fact.telemetry().updated_at();
        ranked.push((
            CompatibilityFactSearchHitV1::new(
                *fact,
                scores,
                Some("co-occurring entity".to_owned()),
            )?,
            updated_at,
        ));
    }
    ranked.sort_by(|(left, left_updated), (right, right_updated)| {
        right
            .score_millionths()
            .cmp(&left.score_millionths())
            .then_with(|| right_updated.cmp(left_updated))
            .then_with(|| left.fact().fact_id().cmp(right.fact().fact_id()))
    });
    if let Some(after) = query.after() {
        ranked.retain(|(hit, updated_at)| {
            hit.score_millionths() < after.score_millionths()
                || (hit.score_millionths() == after.score_millionths()
                    && (*updated_at < after.updated_at()
                        || (*updated_at == after.updated_at()
                            && hit.fact().fact_id() > after.fact_id())))
        });
    }
    ranked.truncate(query.limit());
    // V1 related-fact traversal is one bounded, name-ordered co-occurrence
    // expansion rather than a cursorable global search. Exposing a cursor
    // here would falsely imply coverage beyond the intentionally capped
    // co-entity frontier.
    let next_after = None;
    CompatibilityFactSearchPageV1::new(
        query.owner().clone(),
        ranked.into_iter().map(|(hit, _)| hit).collect(),
        next_after,
    )
    .map_err(Into::into)
}

pub(super) async fn reason_compatibility_facts_tx(
    transaction: &Transaction<'_>,
    query: &CompatibilityFactSearchQuery,
) -> FactCompatibilityResult<CompatibilityFactSearchPageV1> {
    compatibility_rank_facts_tx(transaction, query).await
}

pub(super) async fn find_compatibility_contradictions_tx(
    transaction: &Transaction<'_>,
    query: &CompatibilityFactContradictionQueryV1,
) -> FactCompatibilityResult<CompatibilityFactContradictionPageV1> {
    let mut facts = compatibility_available_facts_tx(
        transaction,
        query.owner(),
        query.category(),
        Some(Confidence::new(0.0).map_err(FactStoreError::from)?),
    )
    .await?;
    facts.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
    let mut contradictions = Vec::new();
    'outer: for (index, left) in facts.iter().enumerate() {
        for right in facts.iter().skip(index + 1) {
            let Some(left_content) = left.content() else {
                continue;
            };
            let Some(right_content) = right.content() else {
                continue;
            };
            let left_entities = left
                .entities()
                .unwrap_or_default()
                .iter()
                .map(|entity| normalize_entity(entity).to_ascii_lowercase())
                .collect::<BTreeSet<_>>();
            let right_entities = right
                .entities()
                .unwrap_or_default()
                .iter()
                .map(|entity| normalize_entity(entity).to_ascii_lowercase())
                .collect::<BTreeSet<_>>();
            if left_entities.is_disjoint(&right_entities) {
                continue;
            }
            let left_tokens = compatibility_fact_tokens(left);
            let right_tokens = compatibility_fact_tokens(right);
            let similarity = compatibility_jaccard(&left_tokens, &right_tokens);
            let divergence = 1.0 - similarity;
            let left_negative = left_tokens.iter().any(|token| {
                matches!(
                    token.as_str(),
                    "not" | "no" | "never" | "avoid" | "dont" | "don't"
                )
            });
            let right_negative = right_tokens.iter().any(|token| {
                matches!(
                    token.as_str(),
                    "not" | "no" | "never" | "avoid" | "dont" | "don't"
                )
            });
            let score = compatibility_millionths(divergence);
            if score < query.threshold_millionths() && left_negative == right_negative {
                continue;
            }
            let (existing, new_content) = if left_negative {
                (right.clone(), left_content)
            } else {
                (left.clone(), right_content)
            };
            contradictions.push(CompatibilityFactContradictionV1::new(
                existing,
                new_content.to_owned(),
                score,
                Some(format!(
                    "shared entities with content divergence={divergence:.3}"
                )),
            )?);
            if contradictions.len() >= query.limit() {
                break 'outer;
            }
        }
    }
    CompatibilityFactContradictionPageV1::new(query.owner().clone(), contradictions)
        .map_err(Into::into)
}

async fn compatibility_update_retrieval_projection_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    fact_id: &FactId,
    recall: bool,
    timestamp: UtcMicros,
) -> FactStoreResult<()> {
    let key = OwnerKey::new(owner)?;
    let changed = transaction
        .execute(
            "UPDATE memory_v2_current_facts SET
                retrieval_count = retrieval_count + 1,
                access_count = access_count + ?1,
                last_retrieved_at = ?2,
                last_recalled_at = CASE WHEN ?1 = 1 THEN ?2 ELSE last_recalled_at END
             WHERE fact_id = ?3 AND owner_kind = ?4 AND project_id = ?5",
            params![
                i64::from(recall),
                timestamp.0,
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility retrieval target has no current projection",
        ));
    }
    let mapping = compatibility_required_mapping_tx(transaction, owner, fact_id).await?;
    let changed = transaction
        .execute(
            "UPDATE memory_facts SET
                retrieval_count = retrieval_count + 1,
                access_count = access_count + ?1,
                last_retrieved_at = ?2,
                last_recalled_at = CASE WHEN ?1 = 1 THEN ?2 ELSE last_recalled_at END
             WHERE fact_id = ?3",
            params![
                i64::from(recall),
                compatibility_legacy_timestamp(timestamp),
                mapping.legacy_fact_id()
            ],
        )
        .await
        .map_err(|error| storage_error(COMPATIBILITY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility retrieval target is missing from the legacy mirror",
        ));
    }
    Ok(())
}

async fn compatibility_replay_retrieval_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    receipt: &CompatibilityOperationReceiptV1,
) -> FactCompatibilityResult<Vec<CompatibilityFactProjectionV1>> {
    let fact_ids = receipt
        .receipt
        .get("fact_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility retrieval receipt fact ids are missing",
            )
        })?;
    let mut parsed_ids = Vec::with_capacity(fact_ids.len());
    for value in fact_ids {
        parsed_ids.push(
            FactId::new(value.as_str().ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility retrieval receipt fact id is malformed",
                )
            })?)
            .map_err(FactStoreError::from)?,
        );
    }
    let facts = load_compatibility_projections_tx(transaction, owner, &parsed_ids).await?;
    if facts.len() != parsed_ids.len() {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility retrieval replay fact is missing",
        )
        .into());
    }
    Ok(facts)
}

pub(super) async fn record_compatibility_fact_retrieval_tx(
    transaction: &Transaction<'_>,
    request: &CompatibilityFactRetrievalCommandV1,
) -> FactCompatibilityResult<Vec<CompatibilityFactProjectionV1>> {
    let request_digest = compatibility_digest(json!({
        "targets": request
            .targets()
            .iter()
            .map(compatibility_target_digest)
            .collect::<FactStoreResult<Vec<_>>>()?,
        "recall": request.recall(),
    }))?;
    if let Some(receipt) = compatibility_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "retrieval",
        &request_digest,
    )
    .await?
    {
        return compatibility_replay_retrieval_tx(transaction, request.owner(), &receipt).await;
    }
    let mut fact_ids = Vec::with_capacity(request.targets().len());
    let mut seen = BTreeSet::new();
    for target in request.targets() {
        let fact_id = resolve_compatibility_target_tx(transaction, target)
            .await?
            .ok_or_else(|| {
                storage_message(
                    COMPATIBILITY_WRITE_OPERATION,
                    "compatibility retrieval target is missing",
                )
            })?;
        if seen.insert(fact_id.clone()) {
            fact_ids.push(fact_id);
        }
    }
    let owner_key = OwnerKey::new(request.owner())?;
    for fact_id in &fact_ids {
        if load_current_fact_tx(transaction, &owner_key, request.owner(), fact_id)
            .await?
            .is_none()
        {
            return Err(storage_message(
                COMPATIBILITY_WRITE_OPERATION,
                "compatibility retrieval target is unavailable",
            )
            .into());
        }
    }
    let now = compatibility_now()?;
    for fact_id in &fact_ids {
        compatibility_update_retrieval_projection_tx(
            transaction,
            request.owner(),
            fact_id,
            request.recall(),
            now,
        )
        .await?;
    }
    let facts = load_compatibility_projections_tx(transaction, request.owner(), &fact_ids).await?;
    if facts.len() != fact_ids.len() {
        return Err(storage_message(
            COMPATIBILITY_WRITE_OPERATION,
            "compatibility retrieval projection is missing",
        )
        .into());
    }
    let receipt = json!({
        "fact_ids": fact_ids.iter().map(FactId::as_str).collect::<Vec<_>>(),
    });
    compatibility_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "retrieval",
        &request_digest,
        None,
        None,
        &receipt,
        now,
    )
    .await?;
    Ok(facts)
}
