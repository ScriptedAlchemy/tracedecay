//! Canonical project-memory scoring primitives.

use std::collections::{BTreeMap, BTreeSet};

use crate::memory::encoding::{HolographicEncoder, HolographicEncodingError};

use tracedecay_domain::{FactId, UtcMicros};
use tracedecay_store::{
    FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_SEARCH_SCORE_MILLIONTHS,
    ProjectMemoryFactV1,
};

const FTS_SCORE_WEIGHT: f64 = 0.40;
const JACCARD_SCORE_WEIGHT: f64 = 0.30;
const HOLOGRAPHIC_SCORE_WEIGHT: f64 = 0.30;
const RETRIEVAL_REINFORCEMENT_WEIGHT: f64 = 0.02;
const RETRIEVAL_REINFORCEMENT_CAP: f64 = 0.50;

pub(super) fn project_memory_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '/' | ':' | '.') {
            current.push(character.to_ascii_lowercase());
        } else if !current.is_empty() {
            if current.len() >= 2 {
                tokens.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= 2 {
        tokens.push(current);
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

pub(super) fn project_memory_fact_tokens(fact: &ProjectMemoryFactV1) -> Vec<String> {
    let mut tokens = project_memory_tokens(fact.content());
    for tag in fact.tags() {
        tokens.extend(project_memory_tokens(tag));
    }
    for entity in fact.entities() {
        tokens.extend(project_memory_tokens(entity));
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

pub(super) fn project_memory_term_coverage(query: &[String], fact: &[String]) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    let matched = query
        .iter()
        .filter(|query_token| {
            fact.iter().any(|fact_token| {
                fact_token == *query_token
                    || (query_token.len() >= 4 && fact_token.starts_with(query_token.as_str()))
            })
        })
        .count();
    matched as f64 / query.len() as f64
}

pub(super) fn project_memory_jaccard(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let left = left.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let right = right.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let union = left.union(&right).count();
    if union == 0 {
        0.0
    } else {
        left.intersection(&right).count() as f64 / union as f64
    }
}

pub(super) fn project_memory_holographic_score(
    encoder: &HolographicEncoder,
    query_vector: &[f64],
    fact: &ProjectMemoryFactV1,
) -> FactStoreResult<f64> {
    let fact_vector = encoder
        .encode_fact(fact.content(), fact.entities())
        .map_err(project_memory_holographic_error)?;
    Ok(project_memory_holographic_midpoint(
        encoder
            .similarity(query_vector, &fact_vector)
            .map_err(project_memory_holographic_error)?,
    ))
}

fn project_memory_holographic_midpoint(similarity: f64) -> f64 {
    f64::midpoint(similarity, 1.0).clamp(0.0, 1.0)
}

pub(super) fn project_memory_normalize_fts5_ranks(
    ranked: Vec<(FactId, f64)>,
) -> BTreeMap<FactId, f64> {
    let max_relevance = ranked
        .iter()
        .map(|(_, rank)| project_memory_fts5_rank_relevance(*rank))
        .fold(0.0_f64, f64::max);
    if max_relevance <= f64::EPSILON {
        return ranked
            .into_iter()
            .map(|(fact_id, _)| (fact_id, 0.0))
            .collect();
    }
    ranked
        .into_iter()
        .map(|(fact_id, rank)| {
            (
                fact_id,
                (project_memory_fts5_rank_relevance(rank) / max_relevance).clamp(0.0, 1.0),
            )
        })
        .collect()
}

pub(super) fn project_memory_fts_component(normalized_bm25: f64, coverage: f64) -> f64 {
    normalized_bm25.clamp(0.0, 1.0) * (0.5 + 0.5 * coverage.clamp(0.0, 1.0))
}

pub(super) fn project_memory_combined_score(
    fts: f64,
    jaccard: f64,
    holographic: f64,
    trust: f64,
    temporal_decay: f64,
    retrieval_count: u64,
) -> f64 {
    let relevance = fts.mul_add(
        FTS_SCORE_WEIGHT,
        jaccard.mul_add(JACCARD_SCORE_WEIGHT, holographic * HOLOGRAPHIC_SCORE_WEIGHT),
    );
    let usage_boost = 1.0
        + (RETRIEVAL_REINFORCEMENT_WEIGHT * (retrieval_count as f64).ln_1p())
            .min(RETRIEVAL_REINFORCEMENT_CAP);
    relevance * trust.clamp(0.0, 1.0) * temporal_decay.clamp(0.0, 1.0) * usage_boost
}

pub(super) fn project_memory_millionths(value: f64) -> u32 {
    (value.clamp(0.0, 1.0) * 1_000_000.0).round() as u32
}

pub(super) fn project_memory_score_millionths(value: f64) -> u32 {
    (value.clamp(
        0.0,
        f64::from(MAX_PROJECT_MEMORY_SEARCH_SCORE_MILLIONTHS) / 1_000_000.0,
    ) * 1_000_000.0)
        .round() as u32
}

pub(super) fn project_memory_temporal_decay(updated_at: UtcMicros, now: UtcMicros) -> f64 {
    if updated_at.0 <= 0 {
        return 1.0;
    }
    let age_micros = now.0.saturating_sub(updated_at.0).max(0) as f64;
    let age_days = age_micros / 86_400_000_000.0;
    0.5_f64.powf(age_days / 365.0).clamp(0.10, 1.0)
}

fn project_memory_fts5_rank_relevance(rank: f64) -> f64 {
    if rank.is_finite() {
        (-rank).max(0.0)
    } else {
        0.0
    }
}

pub(super) fn project_memory_holographic_error(error: HolographicEncodingError) -> FactStoreError {
    match error {
        HolographicEncodingError::DimensionMismatch { expected, actual } => {
            FactStoreError::HolographicDimensionMismatch { expected, actual }
        }
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::{
        FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1, ProvenanceId, UtcMicros,
    };

    use super::{
        project_memory_combined_score, project_memory_fts_component,
        project_memory_holographic_midpoint, project_memory_jaccard,
        project_memory_normalize_fts5_ranks, project_memory_score_millionths,
        project_memory_temporal_decay, project_memory_tokens,
    };

    fn fact_id(label: &str) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                FactOwnerV1::Profile,
                FactIdentitySourceV1::Application {
                    operation_id: ProvenanceId::new(format!("fixture.scoring.{label}"))
                        .expect("fixture operation id"),
                },
            )
            .expect("fixture identity material"),
        )
        .expect("fixture fact id")
    }

    #[test]
    fn shipped_bm25_coverage_and_retrieval_modifiers_remain_exact() {
        let first = fact_id("bm25-first");
        let second = fact_id("bm25-second");
        let scores = project_memory_normalize_fts5_ranks(vec![
            (first.clone(), -0.000_002),
            (second.clone(), -0.000_001),
        ]);
        assert_eq!(scores[&first], 1.0);
        assert_eq!(scores[&second], 0.5);
        assert_eq!(project_memory_fts_component(scores[&first], 0.5), 0.75);

        let unboosted = project_memory_combined_score(0.75, 0.4, 0.6, 0.8, 0.9, 0);
        let expected_relevance = 0.75_f64.mul_add(0.40, 0.4_f64.mul_add(0.30, 0.6 * 0.30));
        assert!((unboosted - expected_relevance * 0.8 * 0.9).abs() < 1e-12);
        let saturated = project_memory_combined_score(0.75, 0.4, 0.6, 0.8, 0.9, u64::MAX);
        assert!(saturated <= unboosted * 1.5 + 1e-12);
    }

    #[test]
    fn aggregate_score_retains_the_shipped_one_point_five_ceiling() {
        let score = project_memory_combined_score(1.0, 1.0, 1.0, 1.0, 1.0, u64::MAX);
        assert_eq!(project_memory_score_millionths(score), 1_500_000);
    }

    #[test]
    fn jaccard_and_fhrr_midpoint_components_are_exact() {
        let query = project_memory_tokens("sqlite graph memory");
        let fact = project_memory_tokens("sqlite graph retrieval");
        assert!((project_memory_jaccard(&query, &fact) - 0.5).abs() < f64::EPSILON);
        assert_eq!(project_memory_holographic_midpoint(-1.0), 0.0);
        assert_eq!(project_memory_holographic_midpoint(0.0), 0.5);
        assert_eq!(project_memory_holographic_midpoint(1.0), 1.0);
    }

    #[test]
    fn nonpositive_and_future_timestamps_do_not_decay() {
        let now = UtcMicros(1_000_000);
        assert_eq!(project_memory_temporal_decay(UtcMicros(0), now), 1.0);
        assert_eq!(project_memory_temporal_decay(UtcMicros(-1), now), 1.0);
        assert_eq!(
            project_memory_temporal_decay(UtcMicros(2_000_000), now),
            1.0
        );
    }
}
