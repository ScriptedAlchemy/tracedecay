//! Cached similarity computation and the similarity pairs payload.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use tracedecay_store::FactReadControl;

use super::super::DashboardState;
use super::super::memory_analysis::{
    MemoryAnalysisError, MemoryScoreDistributionV1, SIMILARITY_FACT_CAP, SIMILARITY_PAIR_FLOOR,
    SIMILARITY_SCORE_MAX, SIMILARITY_SCORE_MIN, SimilarityComputation,
    build_similarity_computation, empty_score_distribution, score_similar_pairs,
};
use super::projection::{MemoryDerivedScanV1, vector_rows};
use crate::read_model::DashboardDomainStateV1;
use crate::snapshot_cache::DerivedSnapshotCacheState;
use crate::tracedecay::facts::memory_application_for_db;

/// One scored fact pair above the requested similarity floor.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MemorySimilarityPairV1 {
    pub a_id: String,
    pub b_id: String,
    pub a_content: String,
    pub b_content: String,
    pub a_category: String,
    pub b_category: String,
    pub similarity: f64,
    pub classification: String,
}

/// `GET /api/plugins/holographic/similarity`.
///
/// `count` is the number of vectored facts scored, `total_pairs` the finite
/// pairs scored before the floor and cap, and `pairs` what survived both.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MemorySimilarityPayloadV1 {
    pub exists: bool,
    pub dim: usize,
    pub count: usize,
    pub limit: usize,
    pub min_similarity: f64,
    pub total_pairs: i64,
    pub score_distribution: MemoryScoreDistributionV1,
    pub pairs: Vec<MemorySimilarityPairV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan: Option<MemoryDerivedScanV1>,
    /// Request lifecycle state when the read ended before a result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DashboardDomainStateV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub error: String,
}

impl MemorySimilarityPayloadV1 {
    pub fn empty(pair_cap: usize, min_similarity: f64, error: impl Into<String>) -> Self {
        Self {
            exists: true,
            dim: 0,
            count: 0,
            limit: pair_cap,
            min_similarity,
            total_pairs: 0,
            score_distribution: empty_score_distribution(),
            pairs: Vec::new(),
            scan: None,
            state: None,
            code: None,
            error: error.into(),
        }
    }
}

pub fn coerce_similarity_score(value: Option<f64>, default: f64) -> f64 {
    value
        .filter(|score| score.is_finite())
        .unwrap_or(default)
        .clamp(SIMILARITY_SCORE_MIN, SIMILARITY_SCORE_MAX)
}

async fn similarity_computation(
    state: &DashboardState,
    read_control: &FactReadControl,
) -> Result<(Arc<SimilarityComputation>, DerivedSnapshotCacheState, usize), String> {
    if read_control.interrupted() {
        return Err("memory similarity interrupted".to_owned());
    }
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let vector_cap = usize::try_from(SIMILARITY_FACT_CAP).map_err(|error| error.to_string())?;
    let store_revision = application
        .dashboard_store_revision(read_control)
        .await
        .map_err(|error| error.to_string())?;
    // The loader is the only writer, so this is the exact row count read for
    // this response. A hit never polls the closure and therefore reports zero.
    let vector_rows_read = AtomicUsize::new(0);
    let (computation, cache_state) = state
        .derived_snapshots
        .similarity
        .get_or_compute(store_revision, || async {
            let snapshot = application
                .dashboard_vector_snapshot(None, vector_cap, read_control)
                .await
                .map_err(|error| error.to_string())?;
            vector_rows_read.store(snapshot.points().len(), Ordering::Relaxed);
            let observed_revision = snapshot.store_revision();
            let rows = vector_rows(snapshot.into_points())?;
            let blocking_control = read_control.clone();
            let computed = tokio::task::spawn_blocking(
                move || -> Result<SimilarityComputation, MemoryAnalysisError> {
                    hotpath::measure_block!("dashboard_api.memory.similarity_compute", {
                        let dim = rows.iter().map(|(_, v)| v.len()).next().unwrap_or(0);
                        let decoded = rows;
                        let scored = if decoded.len() < 2 {
                            Vec::new()
                        } else {
                            score_similar_pairs(&decoded, SIMILARITY_PAIR_FLOOR, &blocking_control)?
                        };
                        let facts = decoded.into_iter().map(|(meta, _)| meta).collect();
                        build_similarity_computation(dim, facts, scored, &blocking_control)
                    })
                },
            )
            .await
            .map_err(|error| format!("similarity computation task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            Ok::<_, String>((observed_revision, Arc::new(computed)))
        })
        .await?;

    if read_control.interrupted() {
        return Err("memory similarity interrupted".to_owned());
    }
    Ok((
        computation,
        cache_state,
        vector_rows_read.load(Ordering::Relaxed),
    ))
}

pub async fn similarity_payload(
    state: &DashboardState,
    min_similarity: f64,
    pair_cap: usize,
    read_control: &FactReadControl,
) -> MemorySimilarityPayloadV1 {
    let (computation, cache_state, vector_rows_read) =
        match similarity_computation(state, read_control).await {
            Ok(cached) => cached,
            Err(error) => return MemorySimilarityPayloadV1::empty(pair_cap, min_similarity, error),
        };
    let mut payload = MemorySimilarityPayloadV1 {
        dim: computation.dim,
        count: computation.facts.len(),
        total_pairs: computation.total_pairs,
        score_distribution: computation.distribution.clone(),
        scan: Some(MemoryDerivedScanV1::store_revision(
            cache_state,
            vector_rows_read,
        )),
        ..MemorySimilarityPayloadV1::empty(pair_cap, min_similarity, "")
    };
    if computation.facts.len() < 2 || computation.dim == 0 {
        return payload;
    }

    let pairs =
        computation
            .pairs
            .iter()
            .take_while(|pair| pair.similarity >= min_similarity)
            .take(pair_cap)
            .map(|scored_pair| {
                if read_control.interrupted() {
                    return Err("memory similarity interrupted".to_owned());
                }
                let a = &computation.facts[scored_pair.a];
                let b = &computation.facts[scored_pair.b];
                let a_id = a.get("fact_id").and_then(Value::as_str).ok_or_else(|| {
                    "similarity left fact omitted its canonical fact ID".to_owned()
                })?;
                let b_id = b.get("fact_id").and_then(Value::as_str).ok_or_else(|| {
                    "similarity right fact omitted its canonical fact ID".to_owned()
                })?;
                let a_content = a.get("content").and_then(Value::as_str).ok_or_else(|| {
                    "similarity left fact omitted authoritative content".to_owned()
                })?;
                let b_content = b.get("content").and_then(Value::as_str).ok_or_else(|| {
                    "similarity right fact omitted authoritative content".to_owned()
                })?;
                let a_category = a
                    .get("category")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "similarity left fact omitted its category".to_owned())?;
                let b_category = b
                    .get("category")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "similarity right fact omitted its category".to_owned())?;
                Ok::<_, String>(MemorySimilarityPairV1 {
                    a_id: a_id.to_owned(),
                    b_id: b_id.to_owned(),
                    a_content: a_content.chars().take(200).collect(),
                    b_content: b_content.chars().take(200).collect(),
                    a_category: a_category.to_owned(),
                    b_category: b_category.to_owned(),
                    similarity: scored_pair.similarity,
                    classification: scored_pair.classification.to_owned(),
                })
            })
            .collect::<Result<Vec<_>, _>>();
    match pairs {
        Ok(_) if read_control.interrupted() => {
            payload.error = "memory similarity interrupted".to_owned();
        }
        Ok(pairs) => payload.pairs = pairs,
        Err(error) => payload.error = error,
    }
    payload
}
