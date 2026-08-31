//! Canonical project-memory scoring primitives.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use crate::memory::encoding::{
    HolographicEncoder, HolographicEncodingError, HolographicQueryVector,
};

use tracedecay_domain::{FactAssertionId, FactId, FactOwnerV1, UtcMicros};
use tracedecay_store::{
    FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_SEARCH_SCORE_MILLIONTHS,
    ProjectMemoryFactV1,
};

const FTS_SCORE_WEIGHT: f64 = 0.40;
const JACCARD_SCORE_WEIGHT: f64 = 0.30;
const HOLOGRAPHIC_SCORE_WEIGHT: f64 = 0.30;
const RETRIEVAL_REINFORCEMENT_WEIGHT: f64 = 0.02;
const RETRIEVAL_REINFORCEMENT_CAP: f64 = 0.50;

/// Tokenize project-memory text for fact-search scoring and FTS queries.
///
/// Keeps path-like punctuation (`_`, `/`, `:`, `.`) so identifiers such as
/// `crate::foo`, `src/lib.rs`, and `foo.bar` stay one token. Does not strip
/// English stopwords: a query term like "the" or "and" must still match.
/// Output is a sorted unique `Vec` so Jaccard can walk two-pointer without
/// building sets.
///
/// Not the dashboard/write-time similarity tokenizer in
/// `memory::similarity`: that one splits on `/` `:` `.`, keeps hyphens and
/// apostrophes, and drops stopwords for near-duplicate classification.
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
    // Callers pass the sorted unique slices from `project_memory_tokens` /
    // `project_memory_fact_tokens`, so a two-pointer walk is exact Jaccard
    // without building sets.
    let mut left_index = 0;
    let mut right_index = 0;
    let mut intersection = 0usize;
    while left_index < left.len() && right_index < right.len() {
        match left[left_index].as_str().cmp(right[right_index].as_str()) {
            Ordering::Less => left_index += 1,
            Ordering::Greater => right_index += 1,
            Ordering::Equal => {
                intersection += 1;
                left_index += 1;
                right_index += 1;
            }
        }
    }
    let union = left.len() + right.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Bounds the resident fact-vector cache: 512 entries at 2048 `f64`
/// coefficients each is 8 MiB, sized to cover every candidate arm of one
/// search (each arm is capped at 1,000 ids with heavy overlap in practice)
/// without letting a long-lived daemon accumulate vectors for unbounded
/// historical facts.
const FACT_VECTOR_CACHE_CAPACITY: usize = 512;

#[derive(Clone, Eq, Hash, PartialEq)]
struct FactVectorKey {
    owner: FactOwnerV1,
    fact_id: FactId,
    assertion_id: FactAssertionId,
}

#[derive(Default)]
struct FactVectorCache {
    vectors: HashMap<FactVectorKey, Arc<Vec<f64>>>,
    order: VecDeque<FactVectorKey>,
}

/// Cached FHRR encodings of canonical fact payloads, shared across searches.
///
/// An assertion's payload is immutable (`memory_v2_payloads_no_update`
/// rejects updates), and the encoded vector is a deterministic function of
/// that payload's content and entities, so an entry keyed by the exact
/// (owner, fact, active assertion) triple can never serve a stale vector:
/// any payload change activates a new assertion id and misses this cache.
/// Without it, ranking re-derives every candidate fact's encoding on every
/// search — ~512 SHA-256 digests per distinct token at 2048 dimensions —
/// which dominates recall latency once a store holds more than a handful of
/// facts.
static FACT_VECTOR_CACHE: LazyLock<Mutex<FactVectorCache>> =
    LazyLock::new(|| Mutex::new(FactVectorCache::default()));

/// Returns the FHRR encoding for a canonical fact projection, computing and
/// retaining it on first use for the fact's current active assertion.
pub(super) fn project_memory_fact_vector(
    encoder: &HolographicEncoder,
    fact: &ProjectMemoryFactV1,
) -> Result<Arc<Vec<f64>>, HolographicEncodingError> {
    let key = FactVectorKey {
        owner: fact.owner().clone(),
        fact_id: fact.fact_id().clone(),
        assertion_id: fact.active_assertion_id().clone(),
    };
    {
        let cache = FACT_VECTOR_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(vector) = cache.vectors.get(&key) {
            return Ok(Arc::clone(vector));
        }
    }
    let vector = Arc::new(encoder.encode_fact(fact.content(), fact.entities())?);
    let mut cache = FACT_VECTOR_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache
        .vectors
        .insert(key.clone(), Arc::clone(&vector))
        .is_none()
    {
        cache.order.push_back(key);
        while cache.order.len() > FACT_VECTOR_CACHE_CAPACITY {
            let Some(evicted) = cache.order.pop_front() else {
                break;
            };
            cache.vectors.remove(&evicted);
        }
    }
    Ok(vector)
}

pub(super) fn project_memory_holographic_score(
    encoder: &HolographicEncoder,
    query: &HolographicQueryVector,
    fact: &ProjectMemoryFactV1,
) -> FactStoreResult<f64> {
    let fact_vector =
        project_memory_fact_vector(encoder, fact).map_err(project_memory_holographic_error)?;
    Ok(project_memory_holographic_midpoint(
        encoder
            .query_similarity(query, &fact_vector)
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
    use std::sync::Arc;

    use serde_json::json;
    use tracedecay_domain::{
        ComponentVersion, Confidence, FactAssertionId, FactCategoryV1, FactEventId, FactId,
        FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1, FactPayloadV1,
        PayloadReferenceV1, ProvenanceId, RetentionClass, SanitizationReceiptId,
        SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
        UtcMicros,
    };
    use tracedecay_store::{
        ProjectMemoryFactSnapshotV1, ProjectMemoryFactTelemetryV1, ProjectMemoryFactV1,
    };

    use super::{
        project_memory_combined_score, project_memory_fact_vector, project_memory_fts_component,
        project_memory_holographic_midpoint, project_memory_jaccard,
        project_memory_normalize_fts5_ranks, project_memory_score_millionths,
        project_memory_temporal_decay, project_memory_tokens,
    };
    use crate::memory::encoding::HolographicEncoder;

    fn domain_id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("fixture id")
    }

    fn projected_fact(operation: &str, assertion: &str) -> ProjectMemoryFactV1 {
        let owner = FactOwnerV1::Profile;
        let source = FactIdentitySourceV1::Application {
            operation_id: domain_id::<ProvenanceId>(operation),
        };
        let derived = FactId::derive(
            &FactIdentityMaterialV1::new(owner.clone(), source.clone())
                .expect("fixture identity material"),
        )
        .expect("fixture fact id");
        let content = "the daemon caches fact vectors";
        let material = json!({
            "content": content,
            "category": "project",
            "tags": ["memory"],
            "entities": ["TraceDecay"],
            "metadata": {},
        });
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                domain_id::<SanitizationReceiptId>("receipt.fact.scoring.fixture"),
                domain_id::<ComponentVersion>("sanitizer.fixture.v1"),
            )
            .expect("fixture receipt reference"),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&material).expect("fixture payload reference")),
        )
        .expect("fixture receipt");
        let payload = FactPayloadV1::new(
            content.to_owned(),
            FactCategoryV1::Project,
            vec!["memory".to_owned()],
            vec!["TraceDecay".to_owned()],
            json!({}),
            None,
            receipt,
            RetentionClass::new("durable.fact").expect("fixture retention"),
        )
        .expect("fixture payload");
        ProjectMemoryFactV1::new(
            derived,
            owner,
            payload,
            Confidence::new(0.5).expect("fixture trust"),
            ProjectMemoryFactSnapshotV1::new(
                domain_id::<FactAssertionId>(assertion),
                domain_id::<FactEventId>("event.scoring-cache"),
                UtcMicros(1),
            ),
            source,
            ProjectMemoryFactTelemetryV1::new(
                0,
                0,
                0,
                0,
                UtcMicros(1),
                UtcMicros(1),
                None,
                None,
                None,
            )
            .expect("fixture telemetry"),
        )
        .expect("fixture fact")
    }

    #[test]
    fn fact_vector_cache_hits_the_active_assertion_and_misses_a_new_one() {
        let encoder = HolographicEncoder::new();
        let fact = projected_fact("operation.scoring-cache", "assertion.scoring-cache-one");
        let first = project_memory_fact_vector(&encoder, &fact).expect("first encoding");
        let cached = project_memory_fact_vector(&encoder, &fact).expect("cached encoding");
        // The exact (owner, fact, assertion) triple returns the retained
        // vector without re-deriving it.
        assert!(Arc::ptr_eq(&first, &cached));
        let fresh = encoder
            .encode_fact(fact.content(), fact.entities())
            .expect("fresh encoding");
        assert_eq!(*first, fresh);

        // Activating a new assertion misses the cache and re-encodes, so a
        // payload change can never serve a stale vector.
        let reasserted = projected_fact("operation.scoring-cache", "assertion.scoring-cache-two");
        assert_eq!(reasserted.fact_id(), fact.fact_id());
        let second =
            project_memory_fact_vector(&encoder, &reasserted).expect("re-encoded assertion");
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(*second, fresh);
    }

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

    fn assert_f64_bits_eq(actual: f64, expected: f64) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }

    #[test]
    fn shipped_bm25_coverage_and_retrieval_modifiers_remain_exact() {
        let first = fact_id("bm25-first");
        let second = fact_id("bm25-second");
        let scores = project_memory_normalize_fts5_ranks(vec![
            (first.clone(), -0.000_002),
            (second.clone(), -0.000_001),
        ]);
        assert_f64_bits_eq(scores[&first], 1.0);
        assert_f64_bits_eq(scores[&second], 0.5);
        assert_f64_bits_eq(project_memory_fts_component(scores[&first], 0.5), 0.75);

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
        assert_f64_bits_eq(project_memory_holographic_midpoint(-1.0), 0.0);
        assert_f64_bits_eq(project_memory_holographic_midpoint(0.0), 0.5);
        assert_f64_bits_eq(project_memory_holographic_midpoint(1.0), 1.0);
    }

    #[test]
    fn nonpositive_and_future_timestamps_do_not_decay() {
        let now = UtcMicros(1_000_000);
        assert_f64_bits_eq(project_memory_temporal_decay(UtcMicros(0), now), 1.0);
        assert_f64_bits_eq(project_memory_temporal_decay(UtcMicros(-1), now), 1.0);
        assert_f64_bits_eq(
            project_memory_temporal_decay(UtcMicros(2_000_000), now),
            1.0,
        );
    }
}
