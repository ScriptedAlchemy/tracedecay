//! Vector-point rows, fingerprints, and the cached PCA projection payload.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use super::super::DashboardState;
use super::super::memory_analysis::pca_scores;
use super::facts::fact_summary_json;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::ProjectMemoryDashboardVectorPointV1;

pub(super) const PROJECTION_POINT_CAP: i64 = 2000;

pub(super) type VectorStateFingerprint = (i64, i64, i64, u64);

pub fn projection_point_cap() -> i64 {
    PROJECTION_POINT_CAP
}

pub(super) fn vector_rows(
    points: Vec<ProjectMemoryDashboardVectorPointV1>,
) -> Vec<(Value, Vec<f64>)> {
    points
        .into_iter()
        .filter_map(|point| {
            let vector = point.vector?;
            let mut fact = fact_summary_json(&point.fact)?;
            let object = fact.as_object_mut()?;
            object.insert("bank_id".into(), Value::Null);
            object.insert("bank_name".into(), json!(point.bank_name));
            object.insert("entity_count".into(), json!(point.entity_count));
            object.insert("connection_count".into(), json!(point.connection_count));
            Some((fact, vector))
        })
        .collect()
}

pub(super) fn vector_fingerprint(rows: &[(Value, Vec<f64>)]) -> VectorStateFingerprint {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut updated_at = 0_i64;
    let mut max_fact_id = 0_i64;
    for (fact, vector) in rows {
        let fact_id = fact
            .get("fact_id")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let updated = fact
            .get("updated_at")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        updated_at = updated_at.max(updated);
        max_fact_id = max_fact_id.max(fact_id);
        fact_id.hash(&mut hasher);
        updated.hash(&mut hasher);
        vector.len().hash(&mut hasher);
        for component in vector {
            component.to_bits().hash(&mut hasher);
        }
    }
    (rows.len() as i64, updated_at, max_fact_id, hasher.finish())
}

struct ProjectionComputation {
    key: (String, i64, VectorStateFingerprint),
    dim: usize,
    method: &'static str,
    error: &'static str,
    points: Vec<Value>,
}

static PROJECTION_CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<ProjectionComputation>>>> =
    OnceLock::new();

fn projection_point(meta: &Value, x: f64, y: f64) -> Value {
    json!({
        "fact_id": meta.get("fact_id").cloned().unwrap_or(json!(0)),
        "x": (x * 1e6).round() / 1e6,
        "y": (y * 1e6).round() / 1e6,
        "category": meta.get("category").cloned().unwrap_or(json!("general")),
        "content": meta.get("content").and_then(Value::as_str).map(|s| s.chars().take(200).collect::<String>()).unwrap_or_default(),
        "trust_score": meta.get("trust_score").cloned().unwrap_or(json!(0.0)),
        "retrieval_count": meta.get("retrieval_count").cloned().unwrap_or(json!(0)),
        "created_at": meta.get("created_at").cloned().unwrap_or(json!(0)),
        "updated_at": meta.get("updated_at").cloned().unwrap_or(json!(0)),
        "metadata": meta.get("metadata").cloned().unwrap_or(Value::Null),
        "bank_id": meta.get("bank_id").cloned().unwrap_or(Value::Null),
        "bank_name": meta.get("bank_name").cloned().unwrap_or(Value::Null),
        "entity_count": meta.get("entity_count").cloned().unwrap_or(json!(0)),
        "connection_count": meta.get("connection_count").cloned().unwrap_or(json!(0)),
    })
}

fn compute_projection(
    key: (String, i64, VectorStateFingerprint),
    rows: Vec<(Value, Vec<f64>)>,
) -> ProjectionComputation {
    let dim = rows.iter().map(|(_, v)| v.len()).next().unwrap_or(0);
    let rows: Vec<_> = rows.into_iter().filter(|(_, v)| v.len() == dim).collect();

    if rows.len() < 2 {
        let points = rows
            .first()
            .map(|(meta, _)| vec![projection_point(meta, 0.0, 0.0)])
            .unwrap_or_default();
        return ProjectionComputation {
            key,
            dim,
            method: "none",
            error: "",
            points,
        };
    }

    let features: Vec<Vec<f64>> = rows
        .iter()
        .map(|(_, phases)| {
            phases
                .iter()
                .map(|p| p.cos())
                .chain(phases.iter().map(|p| p.sin()))
                .collect()
        })
        .collect();
    match pca_scores(&features) {
        Some(scores) => ProjectionComputation {
            key,
            dim,
            method: "pca",
            error: "",
            points: rows
                .iter()
                .zip(&scores)
                .map(|((meta, _), s)| projection_point(meta, s[0], s[1]))
                .collect(),
        },
        None => ProjectionComputation {
            key,
            dim,
            method: "none",
            error: "projection failed",
            points: Vec::new(),
        },
    }
}

pub async fn projection_payload(state: &DashboardState, query: &str, limit: i64) -> Value {
    let mut obj = Map::new();
    obj.insert("exists".into(), json!(true));
    obj.insert("dim".into(), json!(0));
    obj.insert("limit".into(), json!(limit));
    obj.insert("method".into(), json!("none"));
    obj.insert("points".into(), json!([]));
    obj.insert("error".into(), json!(""));

    let application = match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => application,
        Err(error) => {
            obj.insert("error".into(), json!(error.to_string()));
            return Value::Object(obj);
        }
    };
    let points = match application
        .dashboard_vector_points_v1(
            (!query.trim().is_empty()).then(|| query.trim().to_owned()),
            usize::try_from(limit.clamp(1, PROJECTION_POINT_CAP))
                .unwrap_or(PROJECTION_POINT_CAP as usize),
        )
        .await
    {
        Ok(points) => points,
        Err(error) => {
            obj.insert("error".into(), json!(error.to_string()));
            return Value::Object(obj);
        }
    };
    let rows = vector_rows(points);
    let fingerprint = vector_fingerprint(&rows);
    let key = (query.trim().to_string(), limit, fingerprint);

    let cache = PROJECTION_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    if let Some(existing) = guard.get(&state.mem_db_path)
        && existing.key == key
    {
        return projection_response(existing, obj);
    }

    let computed = match tokio::task::spawn_blocking(move || compute_projection(key, rows)).await {
        Ok(computed) => Arc::new(computed),
        Err(e) => {
            obj.insert(
                "error".into(),
                json!(format!("projection task failed: {e}")),
            );
            return Value::Object(obj);
        }
    };
    guard.insert(state.mem_db_path.clone(), computed.clone());
    projection_response(&computed, obj)
}

fn projection_response(computation: &ProjectionComputation, mut obj: Map<String, Value>) -> Value {
    obj.insert("dim".into(), json!(computation.dim));
    obj.insert("method".into(), json!(computation.method));
    obj.insert("points".into(), json!(computation.points));
    obj.insert("error".into(), json!(computation.error));
    Value::Object(obj)
}
