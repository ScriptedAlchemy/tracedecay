//! Bounded canonical `SQLite` candidate discovery for project-memory retrieval.

use std::collections::{BTreeMap, BTreeSet};

use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::Value;
use crate::memory::entities::normalize_entity;

use tracedecay_domain::{Confidence, FactCategoryV1, FactId, FactOwnerV1};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactStoreResult, ProjectMemoryFactListQueryV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchQuery,
    ProjectMemoryFactV1,
};

use super::crud::list_project_memory_facts_controlled_tx;
use super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, ensure_project_memory_read_active,
    project_memory_category_label, row_f64, row_string, storage_error, storage_message,
};
use super::projection::load_project_memory_projections_controlled_tx;
use super::scoring::{project_memory_normalize_fts5_ranks, project_memory_tokens};

const SEARCH_CANDIDATE_ARM_LIMIT: i64 = 1_000;

#[derive(Debug, Default)]
pub(super) struct SearchCandidates {
    pub(super) fact_ids: BTreeSet<FactId>,
    pub(super) relation_roots: BTreeSet<FactId>,
    pub(super) fts_scores: BTreeMap<FactId, f64>,
}

pub(super) async fn project_memory_available_facts_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    category: Option<FactCategoryV1>,
    min_trust: Option<Confidence>,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<ProjectMemoryFactV1>> {
    let query = ProjectMemoryFactListQueryV1::new(owner.clone(), category, min_trust, None, 1_000)?;
    let page = list_project_memory_facts_controlled_tx(transaction, &query, read_control).await?;
    Ok(page
        .facts()
        .iter()
        .filter_map(|projection| match projection {
            ProjectMemoryFactProjectionV1::Available(fact) => Some(fact.as_ref().clone()),
            ProjectMemoryFactProjectionV1::Unavailable(_) => None,
        })
        .collect())
}

fn project_memory_fts_query(tokens: &[String]) -> Option<String> {
    (!tokens.is_empty()).then(|| {
        tokens
            .iter()
            .map(|token| {
                let quoted = format!("\"{}\"", token.replace('"', "\"\""));
                if token.chars().count() >= 4 {
                    format!("{quoted}*")
                } else {
                    quoted
                }
            })
            .collect::<Vec<_>>()
            .join(" OR ")
    })
}

fn project_memory_escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn project_memory_candidate_values(
    query: &ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
) -> FactStoreResult<Vec<Value>> {
    let key = OwnerKey::new(query.owner())?;
    Ok(vec![
        Value::Text(key.kind.to_owned()),
        Value::Text(key.project_id),
        Value::Text(key.json),
        Value::Real(min_trust.as_f64()),
    ])
}

fn project_memory_category_filter(
    query: &ProjectMemoryFactSearchQuery,
    values: &mut Vec<Value>,
) -> String {
    query
        .filter()
        .category()
        .map_or_else(String::new, |category| {
            let index = values.len() + 1;
            values.push(Value::Text(
                project_memory_category_label(category).to_owned(),
            ));
            format!("AND json_extract(payloads.payload_json, '$.category') = ?{index}")
        })
}

async fn project_memory_fts_candidates_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<(FactId, f64)>> {
    ensure_project_memory_read_active(read_control)?;
    let text = query
        .query()
        .ok_or_else(|| storage_message(PROJECT_MEMORY_READ_OPERATION, "search query is missing"))?;
    let Some(fts_query) = project_memory_fts_query(&project_memory_tokens(text)) else {
        return Ok(Vec::new());
    };
    let mut values = project_memory_candidate_values(query, min_trust)?;
    let fts_index = values.len() + 1;
    values.push(Value::Text(fts_query));
    let category_filter = project_memory_category_filter(query, &mut values);
    let limit_index = values.len() + 1;
    values.push(Value::Integer(SEARCH_CANDIDATE_ARM_LIMIT));
    let sql = format!(
        "SELECT current_facts.fact_id, bm25(memory_v2_assertion_payloads_fts) AS rank
         FROM memory_v2_assertion_payloads_fts
         JOIN memory_v2_assertion_payloads AS payloads
           ON payloads.rowid = memory_v2_assertion_payloads_fts.rowid
         JOIN memory_v2_current_facts AS current_facts
           ON current_facts.active_assertion_id = payloads.assertion_id
          AND current_facts.fact_id = payloads.fact_id
          AND current_facts.owner_kind = payloads.owner_kind
          AND current_facts.project_id = payloads.project_id
         JOIN memory_v2_facts AS facts
           ON facts.fact_id = current_facts.fact_id
          AND facts.owner_kind = current_facts.owner_kind
          AND facts.project_id = current_facts.project_id
         WHERE current_facts.owner_kind = ?1
           AND current_facts.project_id = ?2
           AND facts.owner_json = ?3
           AND current_facts.payload_access = 'eligible'
           AND current_facts.active_assertion_id IS NOT NULL
           AND current_facts.trust_score >= ?4
           AND memory_v2_assertion_payloads_fts MATCH ?{fts_index}
           {category_filter}
         ORDER BY rank ASC, current_facts.updated_at DESC, current_facts.fact_id ASC
         LIMIT ?{limit_index}"
    );
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    ensure_project_memory_read_active(read_control)?;
    let mut ranked = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_project_memory_read_active(read_control)?;
        ranked.push((
            FactId::new(row_string(&row, 0, PROJECT_MEMORY_READ_OPERATION)?)
                .map_err(FactStoreError::from)?,
            row_f64(&row, 1, PROJECT_MEMORY_READ_OPERATION)?,
        ));
    }
    drop(rows);
    ensure_project_memory_read_active(read_control)?;
    Ok(ranked)
}

async fn project_memory_entity_candidates_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<FactId>> {
    ensure_project_memory_read_active(read_control)?;
    let text = query
        .query()
        .ok_or_else(|| storage_message(PROJECT_MEMORY_READ_OPERATION, "search query is missing"))?;
    let mut terms = project_memory_tokens(text);
    let normalized = normalize_entity(text).to_ascii_lowercase();
    if !normalized.is_empty() {
        terms.push(normalized);
    }
    terms.sort();
    terms.dedup();
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut values = project_memory_candidate_values(query, min_trust)?;
    let mut predicates = Vec::with_capacity(terms.len());
    for term in terms {
        let exact_index = values.len() + 1;
        values.push(Value::Text(term.clone()));
        let like_index = values.len() + 1;
        values.push(Value::Text(format!(
            "%{}%",
            project_memory_escape_like(&term)
        )));
        predicates.push(format!(
            "(lower(trim(CAST(entities.value AS TEXT))) = ?{exact_index} \
              OR lower(trim(CAST(entities.value AS TEXT))) LIKE ?{like_index} ESCAPE '\\')"
        ));
    }
    let category_filter = project_memory_category_filter(query, &mut values);
    let limit_index = values.len() + 1;
    values.push(Value::Integer(SEARCH_CANDIDATE_ARM_LIMIT));
    let sql = format!(
        "SELECT DISTINCT current_facts.fact_id, current_facts.updated_at
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
         JOIN json_each(payloads.payload_json, '$.entities') AS entities
         WHERE current_facts.owner_kind = ?1
           AND current_facts.project_id = ?2
           AND facts.owner_json = ?3
           AND current_facts.payload_access = 'eligible'
           AND current_facts.active_assertion_id IS NOT NULL
           AND current_facts.trust_score >= ?4
           {category_filter}
           AND ({})
         ORDER BY current_facts.updated_at DESC, current_facts.fact_id ASC
         LIMIT ?{limit_index}",
        predicates.join(" OR ")
    );
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    ensure_project_memory_read_active(read_control)?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_project_memory_read_active(read_control)?;
        fact_ids.push(FactId::new(row_string(
            &row,
            0,
            PROJECT_MEMORY_READ_OPERATION,
        )?)?);
    }
    drop(rows);
    ensure_project_memory_read_active(read_control)?;
    Ok(fact_ids)
}

async fn project_memory_newest_candidates_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<FactId>> {
    ensure_project_memory_read_active(read_control)?;
    let mut values = project_memory_candidate_values(query, min_trust)?;
    let category_filter = project_memory_category_filter(query, &mut values);
    let limit_index = values.len() + 1;
    values.push(Value::Integer(SEARCH_CANDIDATE_ARM_LIMIT));
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
           AND current_facts.payload_access = 'eligible'
           AND current_facts.active_assertion_id IS NOT NULL
           AND current_facts.trust_score >= ?4
           {category_filter}
         ORDER BY current_facts.updated_at DESC, current_facts.fact_id ASC
         LIMIT ?{limit_index}"
    );
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    ensure_project_memory_read_active(read_control)?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_project_memory_read_active(read_control)?;
        fact_ids.push(FactId::new(row_string(
            &row,
            0,
            PROJECT_MEMORY_READ_OPERATION,
        )?)?);
    }
    drop(rows);
    ensure_project_memory_read_active(read_control)?;
    Ok(fact_ids)
}

async fn project_memory_exact_entity_candidates_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
    entities: &[String],
    require_all: bool,
    read_control: &FactReadControl,
) -> FactStoreResult<Vec<FactId>> {
    ensure_project_memory_read_active(read_control)?;
    let mut normalized = entities
        .iter()
        .map(|entity| normalize_entity(entity).to_ascii_lowercase())
        .filter(|entity| !entity.is_empty())
        .collect::<Vec<_>>();
    normalized.sort_unstable();
    normalized.dedup();
    if normalized.is_empty() {
        return Ok(Vec::new());
    }
    let mut values = project_memory_candidate_values(query, min_trust)?;
    let mut predicates = Vec::with_capacity(normalized.len());
    for entity in normalized {
        let index = values.len() + 1;
        values.push(Value::Text(entity));
        predicates.push(format!(
            "EXISTS (
                SELECT 1
                FROM json_each(payloads.payload_json, '$.entities') AS entity
                WHERE lower(trim(CAST(entity.value AS TEXT))) = ?{index}
             )"
        ));
    }
    let category_filter = project_memory_category_filter(query, &mut values);
    let limit_index = values.len() + 1;
    values.push(Value::Integer(SEARCH_CANDIDATE_ARM_LIMIT));
    let conjunction = if require_all { " AND " } else { " OR " };
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
           AND current_facts.payload_access = 'eligible'
           AND current_facts.active_assertion_id IS NOT NULL
           AND current_facts.trust_score >= ?4
           {category_filter}
           AND ({})
         ORDER BY current_facts.updated_at DESC, current_facts.fact_id ASC
         LIMIT ?{limit_index}",
        predicates.join(conjunction)
    );
    let mut rows = transaction
        .query(&sql, values)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    ensure_project_memory_read_active(read_control)?;
    let mut fact_ids = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?
    {
        ensure_project_memory_read_active(read_control)?;
        fact_ids.push(FactId::new(row_string(
            &row,
            0,
            PROJECT_MEMORY_READ_OPERATION,
        )?)?);
    }
    drop(rows);
    ensure_project_memory_read_active(read_control)?;
    Ok(fact_ids)
}

pub(super) async fn project_memory_search_candidates_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
    read_control: &FactReadControl,
) -> FactStoreResult<SearchCandidates> {
    let fts_ranked =
        project_memory_fts_candidates_tx(transaction, query, min_trust, read_control).await?;
    let fts_scores = project_memory_normalize_fts5_ranks(fts_ranked);
    let entity_ids =
        project_memory_entity_candidates_tx(transaction, query, min_trust, read_control).await?;
    let newest_ids =
        project_memory_newest_candidates_tx(transaction, query, min_trust, read_control).await?;
    let relation_roots = fts_scores
        .keys()
        .cloned()
        .chain(entity_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let fact_ids = relation_roots.iter().cloned().chain(newest_ids).collect();
    Ok(SearchCandidates {
        fact_ids,
        relation_roots,
        fts_scores,
    })
}

pub(super) async fn project_memory_probe_candidates_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
    read_control: &FactReadControl,
) -> FactStoreResult<SearchCandidates> {
    let entity = query
        .query()
        .ok_or_else(|| storage_message(PROJECT_MEMORY_READ_OPERATION, "probe query is missing"))?;
    let fact_ids = project_memory_exact_entity_candidates_tx(
        transaction,
        query,
        min_trust,
        &[entity.to_owned()],
        true,
        read_control,
    )
    .await?
    .into_iter()
    .collect();
    Ok(SearchCandidates {
        fact_ids,
        ..SearchCandidates::default()
    })
}

pub(super) async fn project_memory_reason_candidates_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
    read_control: &FactReadControl,
) -> FactStoreResult<SearchCandidates> {
    let ProjectMemoryFactSearchKindV1::Reason { entities } = query.kind() else {
        return Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "reason query has the wrong kind",
        ));
    };
    let fact_ids = project_memory_exact_entity_candidates_tx(
        transaction,
        query,
        min_trust,
        &entities,
        true,
        read_control,
    )
    .await?
    .into_iter()
    .collect();
    Ok(SearchCandidates {
        fact_ids,
        ..SearchCandidates::default()
    })
}

pub(super) fn project_memory_matches_entity(fact: &ProjectMemoryFactV1, entity: &str) -> bool {
    let normalized = normalize_entity(entity).to_ascii_lowercase();
    !normalized.is_empty()
        && fact
            .entities()
            .iter()
            .any(|candidate| normalize_entity(candidate).eq_ignore_ascii_case(normalized.as_str()))
}

pub(super) fn project_memory_matches_all_entities(
    fact: &ProjectMemoryFactV1,
    entities: &[String],
) -> bool {
    entities
        .iter()
        .all(|entity| project_memory_matches_entity(fact, entity))
}

pub(super) async fn project_memory_related_candidates_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    min_trust: Confidence,
    read_control: &FactReadControl,
) -> FactStoreResult<SearchCandidates> {
    let ProjectMemoryFactSearchKindV1::Related { entity } = query.kind() else {
        return Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "related query has the wrong kind",
        ));
    };
    let source_ids = project_memory_exact_entity_candidates_tx(
        transaction,
        query,
        min_trust,
        std::slice::from_ref(&entity),
        true,
        read_control,
    )
    .await?;
    ensure_project_memory_read_active(read_control)?;
    let source_facts = load_project_memory_projections_controlled_tx(
        transaction,
        query.owner(),
        &source_ids,
        read_control,
    )
    .await?;
    ensure_project_memory_read_active(read_control)?;
    let source_facts = source_facts
        .into_iter()
        .filter_map(|projection| match projection {
            ProjectMemoryFactProjectionV1::Available(fact) => Some(*fact),
            ProjectMemoryFactProjectionV1::Unavailable(_) => None,
        })
        .collect::<Vec<_>>();
    let relation_roots = source_facts
        .iter()
        .map(|fact| fact.fact_id().clone())
        .collect::<BTreeSet<_>>();
    let normalized_source = normalize_entity(&entity).to_ascii_lowercase();
    let co_entities = source_facts
        .iter()
        .flat_map(tracedecay_store::ProjectMemoryFactV1::entities)
        .map(|candidate| normalize_entity(candidate).to_ascii_lowercase())
        .filter(|candidate| !candidate.is_empty() && candidate != &normalized_source)
        .collect::<BTreeSet<_>>();
    let fact_ids = project_memory_exact_entity_candidates_tx(
        transaction,
        query,
        min_trust,
        &co_entities.into_iter().collect::<Vec<_>>(),
        false,
        read_control,
    )
    .await?
    .into_iter()
    .collect();
    Ok(SearchCandidates {
        fact_ids,
        relation_roots,
        fts_scores: BTreeMap::new(),
    })
}
