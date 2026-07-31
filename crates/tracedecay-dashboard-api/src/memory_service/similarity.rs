//! Cached similarity computation and the similarity pairs payload.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use super::super::DashboardState;
use super::super::memory_analysis::{
    SIMILARITY_FACT_CAP, SIMILARITY_PAIR_FLOOR, SIMILARITY_SCORE_MAX, SIMILARITY_SCORE_MIN,
    SimilarityComputation, build_similarity_computation, score_distribution, score_similar_pairs,
};
use super::projection::{vector_fingerprint, vector_rows};
use crate::tracedecay::facts::memory_application_for_db;

pub fn coerce_similarity_score(value: Option<f64>, default: f64) -> f64 {
    value
        .filter(|score| score.is_finite())
        .unwrap_or(default)
        .clamp(SIMILARITY_SCORE_MIN, SIMILARITY_SCORE_MAX)
}

static SIMILARITY_CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<SimilarityComputation>>>> =
    OnceLock::new();

pub async fn similarity_computation(
    state: &DashboardState,
) -> Result<Arc<SimilarityComputation>, String> {
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let vector_cap = usize::try_from(SIMILARITY_FACT_CAP).map_err(|error| error.to_string())?;
    let rows = vector_rows(
        application
            .dashboard_vector_points_v1(None, vector_cap)
            .await
            .map_err(|error| error.to_string())?,
    );
    let key = vector_fingerprint(&rows);
    let cache = SIMILARITY_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    if let Some(existing) = guard.get(&state.mem_db_path)
        && existing.key == key
    {
        return Ok(existing.clone());
    }

    let computed = tokio::task::spawn_blocking(move || {
        let dim = rows.iter().map(|(_, v)| v.len()).next().unwrap_or(0);
        let decoded: Vec<_> = rows.into_iter().filter(|(_, v)| v.len() == dim).collect();
        let scored = if decoded.len() < 2 || dim == 0 {
            Vec::new()
        } else {
            score_similar_pairs(&decoded, SIMILARITY_PAIR_FLOOR)
        };
        let facts: Vec<Value> = decoded.into_iter().map(|(meta, _)| meta).collect();
        build_similarity_computation(key, dim, facts, scored)
    })
    .await
    .map_err(|e| format!("similarity computation task failed: {e}"))?;

    let arc = Arc::new(computed);
    guard.insert(state.mem_db_path.clone(), arc.clone());
    Ok(arc)
}

pub async fn similarity_payload(
    state: &DashboardState,
    min_similarity: f64,
    pair_cap: usize,
) -> Value {
    let mut obj = Map::new();
    obj.insert("exists".into(), json!(true));
    obj.insert("dim".into(), json!(0));
    obj.insert("count".into(), json!(0));
    obj.insert("limit".into(), json!(pair_cap));
    obj.insert("threshold".into(), json!(min_similarity));
    obj.insert("min_similarity".into(), json!(min_similarity));
    obj.insert("total_pairs".into(), json!(0));
    obj.insert("score_distribution".into(), score_distribution(&[]));
    obj.insert("pairs".into(), json!([]));
    obj.insert("error".into(), json!(""));

    let computation = match similarity_computation(state).await {
        Ok(computation) => computation,
        Err(e) => {
            obj.insert("error".into(), json!(e));
            return Value::Object(obj);
        }
    };
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

    let pairs: Vec<Value> = computation
        .pairs
        .iter()
        .take_while(|pair| pair.similarity >= min_similarity)
        .take(pair_cap)
        .map(|scored_pair| {
            let a = &computation.facts[scored_pair.a];
            let b = &computation.facts[scored_pair.b];
            let a_content = a.get("content").and_then(Value::as_str).unwrap_or("");
            let b_content = b.get("content").and_then(Value::as_str).unwrap_or("");
            let mut pair = json!({
                "a_id": a.get("fact_id").cloned().unwrap_or(json!(0)),
                "b_id": b.get("fact_id").cloned().unwrap_or(json!(0)),
                "a_content": a_content.chars().take(200).collect::<String>(),
                "b_content": b_content.chars().take(200).collect::<String>(),
                "a_category": a.get("category").cloned().unwrap_or(json!("general")),
                "b_category": b.get("category").cloned().unwrap_or(json!("general")),
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
            pair
        })
        .collect();
    obj.insert("pairs".into(), json!(pairs));
    Value::Object(obj)
}
