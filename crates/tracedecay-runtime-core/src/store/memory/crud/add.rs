//! Bounded add-time duplicate and conflict classification.

use super::super::primitives::{
    OwnerKey, PROJECT_MEMORY_WRITE_OPERATION, row_string, storage_error,
};
use super::super::projection::load_project_memory_projection_tx;
use super::super::scoring::{
    project_memory_jaccard, project_memory_millionths, project_memory_tokens,
};
use crate::db::DatabaseMemoryTransaction as Transaction;
use crate::db::engine::params;
use crate::memory::diff::{
    ADD_COMPARISON_REPORT_FLOOR_MILLIONTHS, NEAR_DUPLICATE_SCORE_MILLIONTHS,
    POSSIBLE_CONFLICT_SCORE_MILLIONTHS, contains_negation_cue, normalized_equivalent,
};
use crate::memory::encoding::{HolographicEncoder, HolographicEncodingError};
use tracedecay_domain::{FactId, FactOwnerV1};
use tracedecay_store::{
    FactStoreError, FactStoreResult, ProjectMemoryFactProjectionV1, ProjectMemoryFactV1,
};

const ADD_CANDIDATE_LIMIT: usize = 8;

pub(super) enum ProjectMemoryAddClassification {
    NormalizedDuplicate(ProjectMemoryFactV1),
    SemanticNearDuplicate {
        closest_fact_id: FactId,
        similarity_millionths: u32,
    },
    PossibleConflict {
        closest_fact_id: FactId,
        similarity_millionths: u32,
    },
}

fn holographic_store_error(error: HolographicEncodingError) -> FactStoreError {
    match error {
        HolographicEncodingError::DimensionMismatch { expected, actual } => {
            FactStoreError::HolographicDimensionMismatch { expected, actual }
        }
    }
}

fn classification_similarity(
    encoder: &HolographicEncoder,
    proposed_tokens: &[String],
    proposed_vector: &[f64],
    candidate: &ProjectMemoryFactV1,
) -> FactStoreResult<u32> {
    let candidate_tokens = project_memory_tokens(candidate.content());
    let mut similarity = project_memory_jaccard(proposed_tokens, &candidate_tokens);
    let candidate_vector = encoder
        .encode_fact(candidate.content(), candidate.entities())
        .map_err(holographic_store_error)?;
    let holographic = encoder
        .similarity(proposed_vector, &candidate_vector)
        .map_err(holographic_store_error)?;
    if holographic >= 0.85 && holographic > similarity {
        similarity = holographic;
    }
    Ok(project_memory_millionths(similarity))
}

async fn candidates_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposed_fact_id: &FactId,
    content: &str,
) -> FactStoreResult<Vec<ProjectMemoryFactV1>> {
    let match_query = project_memory_tokens(content)
        .into_iter()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ");
    if match_query.is_empty() {
        return Ok(Vec::new());
    }
    let key = OwnerKey::new(owner)?;
    let mut rows = transaction
        .query(
            "SELECT current_facts.fact_id
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
             WHERE memory_v2_assertion_payloads_fts MATCH ?1
               AND current_facts.owner_kind = ?2
               AND current_facts.project_id = ?3
               AND facts.owner_json = ?4
               AND current_facts.payload_access = 'eligible'
               AND current_facts.fact_id <> ?5
             ORDER BY bm25(memory_v2_assertion_payloads_fts) ASC,
                      current_facts.fact_id ASC
             LIMIT ?6",
            params![
                match_query,
                key.kind,
                key.project_id.as_str(),
                key.json.as_str(),
                proposed_fact_id.as_str(),
                8_i64,
            ],
        )
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?;
    let mut fact_ids = Vec::with_capacity(ADD_CANDIDATE_LIMIT);
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| storage_error(PROJECT_MEMORY_WRITE_OPERATION, error))?
    {
        fact_ids.push(
            FactId::new(row_string(&row, 0, PROJECT_MEMORY_WRITE_OPERATION)?)
                .map_err(FactStoreError::from)?,
        );
    }
    drop(rows);
    let mut candidates = Vec::with_capacity(fact_ids.len());
    for fact_id in fact_ids {
        match load_project_memory_projection_tx(transaction, owner, &fact_id).await? {
            Some(ProjectMemoryFactProjectionV1::Available(fact)) => candidates.push(*fact),
            Some(ProjectMemoryFactProjectionV1::Unavailable(_)) => {
                return Err(FactStoreError::FactUnavailable { fact_id }.into());
            }
            None => return Err(FactStoreError::FactNotFound { fact_id }.into()),
        }
    }
    Ok(candidates)
}

pub(super) async fn classify_project_memory_add_tx(
    transaction: &Transaction<'_>,
    owner: &FactOwnerV1,
    proposed_fact_id: &FactId,
    content: &str,
    entities: &[String],
) -> FactStoreResult<Option<ProjectMemoryAddClassification>> {
    let candidates = candidates_tx(transaction, owner, proposed_fact_id, content).await?;
    if let Some(candidate) = candidates
        .iter()
        .find(|candidate| normalized_equivalent(content, candidate.content()))
    {
        return Ok(Some(ProjectMemoryAddClassification::NormalizedDuplicate(
            candidate.clone(),
        )));
    }
    let encoder = HolographicEncoder::new();
    let proposed_tokens = project_memory_tokens(content);
    let proposed_vector = encoder
        .encode_fact(content, entities)
        .map_err(holographic_store_error)?;
    let mut closest: Option<(&ProjectMemoryFactV1, u32)> = None;
    for candidate in &candidates {
        let similarity =
            classification_similarity(&encoder, &proposed_tokens, &proposed_vector, candidate)?;
        if similarity < ADD_COMPARISON_REPORT_FLOOR_MILLIONTHS {
            continue;
        }
        if closest.is_none_or(|(previous, previous_similarity)| {
            similarity > previous_similarity
                || (similarity == previous_similarity && candidate.fact_id() < previous.fact_id())
        }) {
            closest = Some((candidate, similarity));
        }
    }
    let Some((closest, similarity_millionths)) = closest else {
        return Ok(None);
    };
    if similarity_millionths >= POSSIBLE_CONFLICT_SCORE_MILLIONTHS
        && (contains_negation_cue(content) || contains_negation_cue(closest.content()))
    {
        return Ok(Some(ProjectMemoryAddClassification::PossibleConflict {
            closest_fact_id: closest.fact_id().clone(),
            similarity_millionths,
        }));
    }
    Ok(
        (similarity_millionths > NEAR_DUPLICATE_SCORE_MILLIONTHS).then(|| {
            ProjectMemoryAddClassification::SemanticNearDuplicate {
                closest_fact_id: closest.fact_id().clone(),
                similarity_millionths,
            }
        }),
    )
}
