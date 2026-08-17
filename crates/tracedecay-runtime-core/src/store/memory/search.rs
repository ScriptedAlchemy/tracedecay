//! Canonical fact search, ranking, cursors, and retrieval recording.

use std::collections::BTreeSet;

use crate::memory::entities::normalize_entity;

use crate::db::engine::params;
use crate::db::{Database, DatabaseMemoryTransaction as Transaction};
use serde::{Deserialize, Serialize};

use tracedecay_domain::{Confidence, DomainError, FactId, FactOwnerV1, ProvenanceId, UtcMicros};
use tracedecay_store::{
    FactReadControl, FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_GRAPH_RELATIONS,
    ProjectMemoryFactContradictionPageV1, ProjectMemoryFactContradictionQueryV1,
    ProjectMemoryFactContradictionV1, ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1,
    ProjectMemoryFactRetrievalCommandV1, ProjectMemoryFactRetrievalOutcomeV1,
    ProjectMemoryFactRetrievalReceiptV1, ProjectMemoryFactSearchCursorV1,
    ProjectMemoryFactSearchGraphCoverageV1, ProjectMemoryFactSearchGraphDegradationV1,
    ProjectMemoryFactSearchHitV1, ProjectMemoryFactSearchKindV1, ProjectMemoryFactSearchPageV1,
    ProjectMemoryFactSearchQuery, ProjectMemoryFactSearchScoresV1, ProjectMemoryFactV1,
    ProjectMemoryGraphQueryV1,
};

use super::candidates::{
    SearchCandidates, project_memory_available_facts_tx, project_memory_matches_all_entities,
    project_memory_matches_entity, project_memory_probe_candidates_tx,
    project_memory_reason_candidates_tx, project_memory_related_candidates_tx,
    project_memory_search_candidates_tx,
};
use super::crud::load_mutable_project_memory_fact_tx;
use super::envelope::{
    ProjectMemoryOperationReceiptV1, finish_read_snapshot,
    project_memory_lookup_operation_receipt_tx, project_memory_record_operation_receipt_tx,
};
use super::primitives::{
    OwnerKey, PROJECT_MEMORY_READ_OPERATION, PROJECT_MEMORY_WRITE_OPERATION,
    ensure_project_memory_read_active, project_memory_now, storage_error, storage_message,
};
use super::projection::{
    load_project_memory_projections_controlled_tx, load_project_memory_projections_tx,
};
use super::scoring::{
    project_memory_combined_score, project_memory_fact_tokens, project_memory_fts_component,
    project_memory_holographic_error, project_memory_holographic_score, project_memory_jaccard,
    project_memory_millionths, project_memory_score_millionths, project_memory_temporal_decay,
    project_memory_term_coverage, project_memory_tokens,
};
use crate::memory::encoding::HolographicEncoder;

fn project_memory_search_scores(
    query_tokens: &[String],
    encoder: &HolographicEncoder,
    query_vector: &[f64],
    normalized_bm25: f64,
    fact: &ProjectMemoryFactV1,
    now: UtcMicros,
) -> FactStoreResult<(ProjectMemoryFactSearchScoresV1, String)> {
    let fact_tokens = project_memory_fact_tokens(fact);
    let coverage = project_memory_term_coverage(query_tokens, &fact_tokens);
    let fts = project_memory_fts_component(normalized_bm25, coverage);
    let jaccard = project_memory_jaccard(query_tokens, &fact_tokens);
    let holographic = project_memory_holographic_score(encoder, query_vector, fact)?;
    let trust = fact.trust().as_f64();
    let temporal_decay = project_memory_temporal_decay(fact.telemetry().updated_at(), now);
    let score = project_memory_combined_score(
        fts,
        jaccard,
        holographic,
        trust,
        temporal_decay,
        fact.telemetry().retrieval_count(),
    );
    Ok((
        ProjectMemoryFactSearchScoresV1::new(
            project_memory_score_millionths(score),
            project_memory_millionths(fts),
            project_memory_millionths(jaccard),
            project_memory_millionths(holographic),
            project_memory_millionths(trust),
        )?,
        format!(
            "fts={fts:.3}, coverage={coverage:.3}, jaccard={jaccard:.3}, holographic={holographic:.3}, trust={trust:.3}, temporal_decay={temporal_decay:.3}, retrieval_count={}",
            fact.telemetry().retrieval_count(),
        ),
    ))
}

/// Orders a page highest-score first, breaking ties on most-recently-updated
/// and then ascending fact id, and drops everything at or before `after`.
///
/// The comparator and the cursor predicate are one unit and must stay that
/// way: a cursor is only resumable against the exact order it was cut from, so
/// every paged canonical search shares this single definition.
fn rank_and_seek(
    ranked: &mut Vec<(ProjectMemoryFactSearchHitV1, UtcMicros)>,
    after: Option<&ProjectMemoryFactSearchCursorV1>,
) {
    ranked.sort_by(|(left, left_updated), (right, right_updated)| {
        right
            .score_millionths()
            .cmp(&left.score_millionths())
            .then_with(|| right_updated.cmp(left_updated))
            .then_with(|| left.fact().fact_id().cmp(right.fact().fact_id()))
    });
    if let Some(after) = after {
        // Search scores include wall-clock decay, so the score carried by a
        // prior page can be stale even when the fact snapshot is unchanged.
        // Resume from the cursor fact's position in this ranking when it is
        // still eligible; its identity is the durable continuation anchor.
        let (after_score, after_updated_at) = ranked
            .iter()
            .find(|(hit, updated_at)| {
                hit.fact().fact_id() == after.fact_id() && *updated_at == after.updated_at()
            })
            .map(|(hit, updated_at)| (hit.score_millionths(), *updated_at))
            .unwrap_or((after.score_millionths(), after.updated_at()));
        ranked.retain(|(hit, updated_at)| {
            hit.score_millionths() < after_score
                || (hit.score_millionths() == after_score
                    && (*updated_at < after_updated_at
                        || (*updated_at == after_updated_at
                            && hit.fact().fact_id() > after.fact_id())))
        });
    }
}

async fn project_memory_rank_facts_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    mut candidates: Option<SearchCandidates>,
    graph_coverage: ProjectMemoryFactSearchGraphCoverageV1,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
    ensure_project_memory_read_active(read_control)?;
    let min_trust = query
        .filter()
        .min_trust()
        .unwrap_or(Confidence::new(0.3).map_err(FactStoreError::from)?);
    let fts_scores = candidates
        .as_mut()
        .map(|candidates| std::mem::take(&mut candidates.fts_scores))
        .unwrap_or_default();
    let mut facts = if let Some(candidates) = candidates.take() {
        ensure_project_memory_read_active(read_control)?;
        let projections = load_project_memory_projections_controlled_tx(
            transaction,
            query.owner(),
            &candidates.fact_ids.iter().cloned().collect::<Vec<_>>(),
            read_control,
        )
        .await?;
        ensure_project_memory_read_active(read_control)?;
        projections
            .into_iter()
            .filter_map(|projection| match projection {
                ProjectMemoryFactProjectionV1::Available(fact) => Some(fact.as_ref().clone()),
                ProjectMemoryFactProjectionV1::Unavailable(_) => None,
            })
            .collect()
    } else {
        project_memory_available_facts_tx(
            transaction,
            query.owner(),
            query.filter().category(),
            Some(min_trust),
            read_control,
        )
        .await?
    };
    ensure_project_memory_read_active(read_control)?;
    let now = project_memory_now()?;
    let mut ranked = Vec::with_capacity(facts.len());
    match query.kind() {
        ProjectMemoryFactSearchKindV1::Search => {
            let text = query.query().ok_or_else(|| {
                storage_message(PROJECT_MEMORY_READ_OPERATION, "search query is missing")
            })?;
            let tokens = project_memory_tokens(text);
            let encoder = HolographicEncoder::new();
            let query_vector = encoder
                .encode_fact(text, &tokens)
                .map_err(project_memory_holographic_error)?;
            for fact in facts.drain(..) {
                ensure_project_memory_read_active(read_control)?;
                let (scores, why) = project_memory_search_scores(
                    &tokens,
                    &encoder,
                    &query_vector,
                    fts_scores.get(fact.fact_id()).copied().unwrap_or(0.0),
                    &fact,
                    now,
                )?;
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
                    ProjectMemoryFactSearchHitV1::new(fact.clone(), scores, Some(why))?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
        ProjectMemoryFactSearchKindV1::Probe => {
            let entity = query.query().ok_or_else(|| {
                storage_message(PROJECT_MEMORY_READ_OPERATION, "probe query is missing")
            })?;
            for fact in facts.drain(..) {
                ensure_project_memory_read_active(read_control)?;
                if !project_memory_matches_entity(&fact, entity) {
                    continue;
                }
                let trust = project_memory_millionths(fact.trust().as_f64());
                let scores = ProjectMemoryFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
                ranked.push((
                    ProjectMemoryFactSearchHitV1::new(
                        fact.clone(),
                        scores,
                        Some("entity probe".to_owned()),
                    )?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
        ProjectMemoryFactSearchKindV1::Related { entity } => {
            for fact in facts.drain(..) {
                ensure_project_memory_read_active(read_control)?;
                let trust = project_memory_millionths(fact.trust().as_f64());
                let scores = ProjectMemoryFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
                ranked.push((
                    ProjectMemoryFactSearchHitV1::new(
                        fact.clone(),
                        scores,
                        Some(format!("entity/relation co-occurrence from {entity}")),
                    )?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
        ProjectMemoryFactSearchKindV1::Reason { entities } => {
            for fact in facts.drain(..) {
                ensure_project_memory_read_active(read_control)?;
                if !project_memory_matches_all_entities(&fact, &entities) {
                    continue;
                }
                let trust = project_memory_millionths(fact.trust().as_f64());
                let scores = ProjectMemoryFactSearchScoresV1::new(trust, 0, 0, 1_000_000, trust)?;
                ranked.push((
                    ProjectMemoryFactSearchHitV1::new(
                        fact.clone(),
                        scores,
                        Some("entity reasoning".to_owned()),
                    )?,
                    fact.telemetry().updated_at(),
                ));
            }
        }
    }
    ensure_project_memory_read_active(read_control)?;
    rank_and_seek(&mut ranked, query.after());
    ensure_project_memory_read_active(read_control)?;
    let has_more = ranked.len() > query.limit();
    ranked.truncate(query.limit());
    let next_after = if has_more {
        ranked.last().map(|(hit, updated_at)| {
            ProjectMemoryFactSearchCursorV1::new(
                hit.score_millionths(),
                *updated_at,
                hit.fact().fact_id().clone(),
            )
        })
    } else {
        None
    }
    .transpose()?;
    ProjectMemoryFactSearchPageV1::new(
        query.owner().clone(),
        ranked.into_iter().map(|(hit, _)| hit).collect(),
        next_after,
        graph_coverage,
    )
}

pub(super) async fn probe_project_memory_facts_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
    let min_trust = query
        .filter()
        .min_trust()
        .unwrap_or(Confidence::new(0.3).map_err(FactStoreError::from)?);
    let candidates =
        project_memory_probe_candidates_tx(transaction, query, min_trust, read_control).await?;
    project_memory_rank_facts_tx(
        transaction,
        query,
        Some(candidates),
        ProjectMemoryFactSearchGraphCoverageV1::NotApplicable,
        read_control,
    )
    .await
}

pub(super) async fn reason_project_memory_facts_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactSearchQuery,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
    let min_trust = query
        .filter()
        .min_trust()
        .unwrap_or(Confidence::new(0.3).map_err(FactStoreError::from)?);
    let candidates =
        project_memory_reason_candidates_tx(transaction, query, min_trust, read_control).await?;
    project_memory_rank_facts_tx(
        transaction,
        query,
        Some(candidates),
        ProjectMemoryFactSearchGraphCoverageV1::NotApplicable,
        read_control,
    )
    .await
}

async fn project_memory_candidates_snapshot(
    db: &Database,
    query: &ProjectMemoryFactSearchQuery,
    read_control: &FactReadControl,
) -> FactStoreResult<SearchCandidates> {
    ensure_project_memory_read_active(read_control)?;
    let transaction = db
        .begin_memory_read_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let min_trust = query
        .filter()
        .min_trust()
        .unwrap_or(Confidence::new(0.3).map_err(FactStoreError::from)?);
    let result = match query.kind() {
        ProjectMemoryFactSearchKindV1::Search => {
            project_memory_search_candidates_tx(&transaction, query, min_trust, read_control).await
        }
        ProjectMemoryFactSearchKindV1::Related { .. } => {
            project_memory_related_candidates_tx(&transaction, query, min_trust, read_control).await
        }
        _ => Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "graph-assisted search has an unsupported kind",
        )),
    };
    finish_read_snapshot(transaction, result).await
}

async fn project_memory_graph_assist(
    db: &Database,
    query: &ProjectMemoryFactSearchQuery,
    candidates: &mut SearchCandidates,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactSearchGraphCoverageV1> {
    if db.issue_memory_graph_runtime_operation().is_err() {
        return Ok(ProjectMemoryFactSearchGraphCoverageV1::NotMounted);
    }
    let roots = candidates
        .relation_roots
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    if roots.is_empty() {
        return Ok(ProjectMemoryFactSearchGraphCoverageV1::Complete {
            root_count: 0,
            relation_count: 0,
            expanded_fact_count: 0,
        });
    }
    let graph_query = ProjectMemoryGraphQueryV1::new(
        query.owner().clone(),
        roots.clone(),
        MAX_PROJECT_MEMORY_GRAPH_RELATIONS,
    )?;
    let page = match super::graph::project_memory_graph(db, graph_query, read_control).await {
        Ok(page) => page,
        Err(FactStoreError::GraphCancelled) => return Err(FactStoreError::GraphCancelled),
        Err(error) => match project_memory_graph_degradation(&error) {
            Some(reason) => {
                return Ok(ProjectMemoryFactSearchGraphCoverageV1::Degraded { reason });
            }
            None => return Err(error),
        },
    };
    let before = candidates.fact_ids.len();
    candidates
        .fact_ids
        .extend(page.facts().iter().filter_map(|fact| {
            matches!(fact, ProjectMemoryFactProjectionV1::Available(_))
                .then(|| fact.fact_id().clone())
        }));
    Ok(ProjectMemoryFactSearchGraphCoverageV1::Complete {
        root_count: roots.len(),
        relation_count: page.relations().len(),
        expanded_fact_count: candidates.fact_ids.len().saturating_sub(before),
    })
}

pub(super) fn project_memory_graph_degradation(
    error: &FactStoreError,
) -> Option<ProjectMemoryFactSearchGraphDegradationV1> {
    match error {
        FactStoreError::GraphConflict => Some(ProjectMemoryFactSearchGraphDegradationV1::Conflict),
        FactStoreError::GraphUnavailable => {
            Some(ProjectMemoryFactSearchGraphDegradationV1::Unavailable)
        }
        FactStoreError::GraphBudgetExhausted => {
            Some(ProjectMemoryFactSearchGraphDegradationV1::BudgetExhausted)
        }
        FactStoreError::GraphDeadlineExceeded => {
            Some(ProjectMemoryFactSearchGraphDegradationV1::DeadlineExceeded)
        }
        _ => None,
    }
}

async fn project_memory_rank_snapshot(
    db: &Database,
    query: &ProjectMemoryFactSearchQuery,
    candidates: SearchCandidates,
    graph_coverage: ProjectMemoryFactSearchGraphCoverageV1,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
    ensure_project_memory_read_active(read_control)?;
    let transaction = db
        .begin_memory_read_transaction(PROJECT_MEMORY_READ_OPERATION)
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_READ_OPERATION, error))?;
    let result = project_memory_rank_facts_tx(
        &transaction,
        query,
        Some(candidates),
        graph_coverage,
        read_control,
    )
    .await;
    finish_read_snapshot(transaction, result).await
}

pub(super) async fn search_project_memory_facts(
    db: &Database,
    query: &ProjectMemoryFactSearchQuery,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
    ensure_project_memory_read_active(read_control)?;
    let mut candidates = project_memory_candidates_snapshot(db, query, read_control).await?;
    ensure_project_memory_read_active(read_control)?;
    let graph_coverage =
        project_memory_graph_assist(db, query, &mut candidates, read_control).await?;
    ensure_project_memory_read_active(read_control)?;
    let page =
        project_memory_rank_snapshot(db, query, candidates, graph_coverage, read_control).await?;
    ensure_project_memory_read_active(read_control)?;
    Ok(page)
}

pub(super) async fn related_project_memory_facts(
    db: &Database,
    query: &ProjectMemoryFactSearchQuery,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactSearchPageV1> {
    let ProjectMemoryFactSearchKindV1::Related { .. } = query.kind() else {
        return Err(storage_message(
            PROJECT_MEMORY_READ_OPERATION,
            "related query has the wrong kind",
        ));
    };
    ensure_project_memory_read_active(read_control)?;
    let mut candidates = project_memory_candidates_snapshot(db, query, read_control).await?;
    ensure_project_memory_read_active(read_control)?;
    let graph_coverage =
        project_memory_graph_assist(db, query, &mut candidates, read_control).await?;
    ensure_project_memory_read_active(read_control)?;
    let page =
        project_memory_rank_snapshot(db, query, candidates, graph_coverage, read_control).await?;
    ensure_project_memory_read_active(read_control)?;
    Ok(page)
}

pub(super) async fn find_project_memory_contradictions_tx(
    transaction: &Transaction<'_>,
    query: &ProjectMemoryFactContradictionQueryV1,
    read_control: &FactReadControl,
) -> FactStoreResult<ProjectMemoryFactContradictionPageV1> {
    ensure_project_memory_read_active(read_control)?;
    let mut facts = project_memory_available_facts_tx(
        transaction,
        query.owner(),
        query.category(),
        Some(Confidence::new(0.0).map_err(FactStoreError::from)?),
        read_control,
    )
    .await?;
    ensure_project_memory_read_active(read_control)?;
    facts.sort_by(|left, right| left.fact_id().cmp(right.fact_id()));
    let mut contradictions = Vec::new();
    'outer: for (index, left) in facts.iter().enumerate() {
        for right in facts.iter().skip(index + 1) {
            ensure_project_memory_read_active(read_control)?;
            let left_entities = left
                .entities()
                .iter()
                .map(|entity| normalize_entity(entity).to_ascii_lowercase())
                .collect::<BTreeSet<_>>();
            let right_entities = right
                .entities()
                .iter()
                .map(|entity| normalize_entity(entity).to_ascii_lowercase())
                .collect::<BTreeSet<_>>();
            if left_entities.is_disjoint(&right_entities) {
                continue;
            }
            let left_tokens = project_memory_fact_tokens(left);
            let right_tokens = project_memory_fact_tokens(right);
            let similarity = project_memory_jaccard(&left_tokens, &right_tokens);
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
            let score = project_memory_millionths(divergence);
            if score < query.threshold_millionths() && left_negative == right_negative {
                continue;
            }
            let (existing, new_content) = if left_negative {
                (right.clone(), left.content())
            } else {
                (left.clone(), right.content())
            };
            contradictions.push(ProjectMemoryFactContradictionV1::new(
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
    ProjectMemoryFactContradictionPageV1::new(query.owner().clone(), contradictions)
        .map_err(Into::into)
}

async fn project_memory_update_retrieval_projection_tx(
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
             WHERE fact_id = ?3
               AND owner_kind = ?4
               AND project_id = ?5
               AND payload_access = 'eligible'
               AND active_assertion_id IS NOT NULL",
            params![
                i64::from(recall),
                timestamp.0,
                fact_id.as_str(),
                key.kind,
                key.project_id.as_str(),
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    if changed != 1 {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "retrieval target has no current projection",
        ));
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProjectMemoryFactRetrievalReceiptWire {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    input_digest: String,
    fact_ids: Vec<FactId>,
    recall: bool,
}

impl From<&ProjectMemoryFactRetrievalReceiptV1> for ProjectMemoryFactRetrievalReceiptWire {
    fn from(receipt: &ProjectMemoryFactRetrievalReceiptV1) -> Self {
        Self {
            owner: receipt.owner().clone(),
            operation_id: receipt.operation_id().clone(),
            input_digest: receipt.input_digest().to_owned(),
            fact_ids: receipt
                .fact_ids()
                .iter()
                .map(|fact_id| fact_id.fact_id().clone())
                .collect(),
            recall: receipt.recall(),
        }
    }
}

fn invalid_project_memory_retrieval_receipt() -> FactStoreError {
    FactStoreError::Contract(DomainError::NonCanonical {
        field: "project memory fact retrieval operation receipt",
    })
}

async fn project_memory_replay_retrieval_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactRetrievalCommandV1,
    input_digest: &str,
    operation_receipt: &ProjectMemoryOperationReceiptV1,
) -> FactStoreResult<ProjectMemoryFactRetrievalOutcomeV1> {
    if operation_receipt.fact_id.is_some() || operation_receipt.event_id.is_some() {
        return Err(invalid_project_memory_retrieval_receipt());
    }
    let persisted = serde_json::from_value::<ProjectMemoryFactRetrievalReceiptWire>(
        operation_receipt.receipt.clone(),
    )
    .map_err(|_| invalid_project_memory_retrieval_receipt())?;
    if &persisted.owner != request.owner()
        || &persisted.operation_id != request.operation_id()
        || persisted.input_digest != input_digest
        || persisted.recall != request.recall()
    {
        return Err(invalid_project_memory_retrieval_receipt());
    }
    let fact_ids = persisted
        .fact_ids
        .into_iter()
        .map(|fact_id| ProjectMemoryFactIdV1::new(persisted.owner.clone(), fact_id))
        .collect::<FactStoreResult<Vec<_>>>()?;
    if fact_ids != request.targets() {
        return Err(invalid_project_memory_retrieval_receipt());
    }
    let receipt = ProjectMemoryFactRetrievalReceiptV1::from_replay(
        persisted.owner,
        persisted.operation_id,
        persisted.input_digest,
        fact_ids,
        persisted.recall,
    )?;
    let projection_ids = receipt
        .fact_ids()
        .iter()
        .map(|fact_id| fact_id.fact_id().clone())
        .collect::<Vec<_>>();
    let projections =
        load_project_memory_projections_tx(transaction, receipt.owner(), &projection_ids).await?;
    ProjectMemoryFactRetrievalOutcomeV1::new(receipt, projections)
}

pub(super) async fn record_project_memory_fact_retrieval_tx(
    transaction: &Transaction<'_>,
    request: &ProjectMemoryFactRetrievalCommandV1,
) -> FactStoreResult<ProjectMemoryFactRetrievalOutcomeV1> {
    let request_digest = request.input_digest()?;
    if let Some(receipt) = project_memory_lookup_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "retrieval",
        &request_digest,
    )
    .await?
    {
        return project_memory_replay_retrieval_tx(transaction, request, &request_digest, &receipt)
            .await;
    }

    for target in request.targets() {
        load_mutable_project_memory_fact_tx(transaction, target).await?;
    }
    let fact_ids = request
        .targets()
        .iter()
        .map(|target| target.fact_id().clone())
        .collect::<Vec<_>>();

    let now = project_memory_now()?;
    for fact_id in &fact_ids {
        project_memory_update_retrieval_projection_tx(
            transaction,
            request.owner(),
            fact_id,
            request.recall(),
            now,
        )
        .await?;
    }
    let facts = load_project_memory_projections_tx(transaction, request.owner(), &fact_ids).await?;
    if facts.len() != fact_ids.len() {
        return Err(storage_message(
            PROJECT_MEMORY_WRITE_OPERATION,
            "retrieval projection is missing",
        )
        .into());
    }
    let receipt = ProjectMemoryFactRetrievalReceiptV1::recorded(
        request.owner().clone(),
        request.operation_id().clone(),
        request_digest.clone(),
        request.targets().to_vec(),
        request.recall(),
    )?;
    let outcome = ProjectMemoryFactRetrievalOutcomeV1::new(receipt, facts)?;
    let persisted_receipt = serde_json::to_value(ProjectMemoryFactRetrievalReceiptWire::from(
        outcome.receipt(),
    ))
    .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    project_memory_record_operation_receipt_tx(
        transaction,
        request.owner(),
        request.operation_id(),
        "retrieval",
        &request_digest,
        None,
        None,
        &persisted_receipt,
        now,
    )
    .await?;
    Ok(outcome)
}
