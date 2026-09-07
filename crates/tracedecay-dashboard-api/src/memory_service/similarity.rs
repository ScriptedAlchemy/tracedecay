//! Cached similarity computation and the similarity pairs payload.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};
use tracedecay_store::FactReadControl;

use super::super::DashboardState;
use super::super::memory_analysis::{
    MemoryAnalysisError, SIMILARITY_FACT_CAP, SIMILARITY_PAIR_FLOOR, SIMILARITY_SCORE_MAX,
    SIMILARITY_SCORE_MIN, SimilarityComputation, build_similarity_computation,
    empty_score_distribution, score_similar_pairs,
};
use super::projection::vector_rows;
use crate::snapshot_cache::{DerivedSnapshotCache, DerivedSnapshotCacheState};
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::ProjectMemoryStoreRevisionV1;

pub fn coerce_similarity_score(value: Option<f64>, default: f64) -> f64 {
    value
        .filter(|score| score.is_finite())
        .unwrap_or(default)
        .clamp(SIMILARITY_SCORE_MIN, SIMILARITY_SCORE_MAX)
}

static SIMILARITY_CACHE: OnceLock<
    DerivedSnapshotCache<String, ProjectMemoryStoreRevisionV1, SimilarityComputation>,
> = OnceLock::new();

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
    let cache = SIMILARITY_CACHE.get_or_init(DerivedSnapshotCache::new);
    // The loader is the only writer, so this is the exact row count read for
    // this response. A hit never polls the closure and therefore reports zero.
    let vector_rows_read = AtomicUsize::new(0);
    let (computation, cache_state) = cache
        .get_or_compute(state.mem_db_path.clone(), store_revision, || async {
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
                        let facts: Vec<Value> = decoded.into_iter().map(|(meta, _)| meta).collect();
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
) -> Value {
    let mut obj = Map::new();
    obj.insert("exists".into(), json!(true));
    obj.insert("dim".into(), json!(0));
    obj.insert("count".into(), json!(0));
    obj.insert("limit".into(), json!(pair_cap));
    obj.insert("min_similarity".into(), json!(min_similarity));
    obj.insert("total_pairs".into(), json!(0));
    obj.insert("score_distribution".into(), empty_score_distribution());
    obj.insert("pairs".into(), json!([]));
    obj.insert("error".into(), json!(""));

    let (computation, cache_state, vector_rows_read) =
        match similarity_computation(state, read_control).await {
            Ok(cached) => cached,
            Err(e) => {
                obj.insert("error".into(), json!(e));
                return Value::Object(obj);
            }
        };
    obj.insert(
        "scan".into(),
        json!({
            "cache_scope": "store_revision",
            "cache_state": cache_state.as_str(),
            "vector_rows_read": vector_rows_read,
        }),
    );
    obj.insert("dim".into(), json!(computation.dim));
    obj.insert("count".into(), json!(computation.facts.len()));
    obj.insert("total_pairs".into(), json!(computation.total_pairs));
    obj.insert(
        "score_distribution".into(),
        computation.distribution.clone(),
    );
    if computation.facts.len() < 2 || computation.dim == 0 {
        return Value::Object(obj);
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
                    .cloned()
                    .ok_or_else(|| "similarity left fact omitted its category".to_owned())?;
                let b_category = b
                    .get("category")
                    .cloned()
                    .ok_or_else(|| "similarity right fact omitted its category".to_owned())?;
                let mut pair = json!({
                    "a_id": a_id,
                    "b_id": b_id,
                    "a_content": a_content.chars().take(200).collect::<String>(),
                    "b_content": b_content.chars().take(200).collect::<String>(),
                    "a_category": a_category,
                    "b_category": b_category,
                    "similarity": scored_pair.similarity,
                    "classification": scored_pair.classification,
                });
                if let (Some(obj), Some(extra)) =
                    (pair.as_object_mut(), scored_pair.overlap.as_object())
                {
                    for (k, v) in extra {
                        obj.insert(k.clone(), v.clone());
                    }
                }
                Ok::<Value, String>(pair)
            })
            .collect::<Result<Vec<_>, _>>();
    let pairs = match pairs {
        Ok(pairs) => pairs,
        Err(error) => {
            obj.insert("error".into(), json!(error));
            return Value::Object(obj);
        }
    };
    if read_control.interrupted() {
        obj.insert("pairs".into(), json!([]));
        obj.insert("error".into(), json!("memory similarity interrupted"));
        return Value::Object(obj);
    }
    obj.insert("pairs".into(), json!(pairs));
    Value::Object(obj)
}
