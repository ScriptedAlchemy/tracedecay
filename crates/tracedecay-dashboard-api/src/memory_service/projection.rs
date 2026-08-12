//! Vector-point rows, fingerprints, and the cached PCA projection payload.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use super::super::DashboardState;
use super::super::memory_analysis::pca_scores;
use super::facts::fact_summary_json;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::{
    FactReadControl, ProjectMemoryDashboardVectorPointV1, ProjectMemoryFactProjectionV1,
};

pub(super) const PROJECTION_POINT_CAP: i64 = 2000;

pub(super) type VectorStateFingerprint = (usize, i64, Option<String>, u64);

pub fn projection_point_cap() -> i64 {
    PROJECTION_POINT_CAP
}

pub(super) fn vector_rows(
    points: Vec<ProjectMemoryDashboardVectorPointV1>,
) -> Result<Vec<(Value, Vec<f64>)>, String> {
    let mut rows = Vec::new();
    for point in points {
        let ProjectMemoryDashboardVectorPointV1 {
            fact,
            vector,
            entity_count,
            ..
        } = point;
        let vector = match (&fact.fact, vector) {
            (ProjectMemoryFactProjectionV1::Available(_), Some(vector)) => vector,
            (ProjectMemoryFactProjectionV1::Unavailable(_), None) => continue,
            (ProjectMemoryFactProjectionV1::Available(_), None) => {
                return Err("available fact omitted its query-time holographic vector".to_owned());
            }
            (ProjectMemoryFactProjectionV1::Unavailable(_), Some(_)) => {
                return Err("unavailable fact exposed a holographic vector".to_owned());
            }
        };
        let mut fact = fact_summary_json(&fact);
        let object = fact
            .as_object_mut()
            .ok_or_else(|| "canonical fact summary was not an object".to_owned())?;
        object.insert("entity_count".into(), json!(entity_count));
        for field in [
            "fact_id",
            "payload_access",
            "trust_score",
            "retrieval_count",
            "created_at",
            "updated_at",
            "content",
            "category",
            "metadata",
            "entity_count",
        ] {
            if !object.contains_key(field) {
                return Err(format!(
                    "canonical vector row omitted authoritative field `{field}`"
                ));
            }
        }
        rows.push((fact, vector));
    }
    Ok(rows)
}

pub(super) fn vector_fingerprint(
    rows: &[(Value, Vec<f64>)],
) -> Result<VectorStateFingerprint, String> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut updated_at = 0_i64;
    let mut max_fact_id = None;
    for (fact, vector) in rows {
        let fact_id = fact
            .get("fact_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "vector row omitted its canonical fact ID".to_owned())?;
        let updated = fact
            .get("updated_at")
            .and_then(Value::as_i64)
            .ok_or_else(|| "vector row omitted its authoritative update time".to_owned())?;
        updated_at = updated_at.max(updated);
        if max_fact_id
            .as_deref()
            .is_none_or(|current| fact_id > current)
        {
            max_fact_id = Some(fact_id.to_owned());
        }
        fact_id.hash(&mut hasher);
        updated.hash(&mut hasher);
        vector.len().hash(&mut hasher);
        for component in vector {
            component.to_bits().hash(&mut hasher);
        }
    }
    Ok((rows.len(), updated_at, max_fact_id, hasher.finish()))
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

fn projection_point(meta: &Value, x: f64, y: f64) -> Result<Value, String> {
    let mut point = meta.clone();
    let object = point
        .as_object_mut()
        .ok_or_else(|| "projection metadata was not an object".to_owned())?;
    object
        .get("fact_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "projection metadata omitted its canonical fact ID".to_owned())?;
    object
        .get("payload_access")
        .ok_or_else(|| "projection metadata omitted its payload-access state".to_owned())?;
    object
        .get("category")
        .and_then(Value::as_str)
        .ok_or_else(|| "projection metadata omitted its authoritative category".to_owned())?;
    object
        .get("trust_score")
        .and_then(Value::as_f64)
        .ok_or_else(|| "projection metadata omitted its authoritative trust score".to_owned())?;
    object
        .get("retrieval_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            "projection metadata omitted its authoritative retrieval count".to_owned()
        })?;
    object
        .get("created_at")
        .and_then(Value::as_i64)
        .ok_or_else(|| "projection metadata omitted its authoritative creation time".to_owned())?;
    object
        .get("updated_at")
        .and_then(Value::as_i64)
        .ok_or_else(|| "projection metadata omitted its authoritative update time".to_owned())?;
    object
        .get("metadata")
        .ok_or_else(|| "projection metadata omitted authoritative fact metadata".to_owned())?;
    object
        .get("entity_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| "projection metadata omitted its authoritative entity count".to_owned())?;
    let content = object
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "projection metadata omitted authoritative fact content".to_owned())?
        .chars()
        .take(200)
        .collect::<String>();
    object.insert("content".into(), json!(content));
    object.insert("x".into(), json!((x * 1e6).round() / 1e6));
    object.insert("y".into(), json!((y * 1e6).round() / 1e6));
    Ok(point)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1, ProvenanceId,
    };

    #[test]
    fn projection_point_preserves_canonical_fact_metadata() {
        let fact_id = FactId::derive(
            &FactIdentityMaterialV1::new(
                FactOwnerV1::Profile,
                FactIdentitySourceV1::Application {
                    operation_id: ProvenanceId::new("dashboard.projection.fact")
                        .expect("fixture provenance must be canonical"),
                },
            )
            .expect("fixture identity material must be canonical"),
        )
        .expect("fixture fact ID must derive");
        let point = projection_point(
            &json!({
                "fact_id": fact_id.as_str(),
                "payload_access": "eligible",
                "content": "canonical fact content",
                "category": "project",
                "trust_score": 0.8,
                "retrieval_count": 3,
                "created_at": 1,
                "updated_at": 2,
                "metadata": {},
                "entity_count": 2,
            }),
            0.25,
            -0.5,
        )
        .expect("complete projection metadata must project");

        assert_eq!(point["fact_id"], fact_id.as_str());
        assert_eq!(point["payload_access"], "eligible");
        assert_eq!(point["entity_count"], 2);
        assert_eq!(point["x"], 0.25);
        assert_eq!(point["y"], -0.5);
    }
}

fn compute_projection(
    key: (String, i64, VectorStateFingerprint),
    rows: Vec<(Value, Vec<f64>)>,
    read_control: FactReadControl,
) -> Result<ProjectionComputation, String> {
    if read_control.interrupted() {
        return Err("memory projection interrupted".to_owned());
    }
    let dim = rows.iter().map(|(_, v)| v.len()).next().unwrap_or(0);
    if rows.iter().any(|(_, vector)| vector.len() != dim) {
        return Err("holographic projection vector dimension mismatch".to_owned());
    }

    if rows.len() < 2 {
        let points = rows
            .first()
            .map(|(meta, _)| projection_point(meta, 0.0, 0.0))
            .transpose()?
            .into_iter()
            .collect();
        return Ok(ProjectionComputation {
            key,
            dim,
            method: "none",
            error: "",
            points,
        });
    }

    let mut features = Vec::with_capacity(rows.len());
    for (_, phases) in &rows {
        if read_control.interrupted() {
            return Err("memory projection interrupted".to_owned());
        }
        features.push(
            phases
                .iter()
                .map(|p| p.cos())
                .chain(phases.iter().map(|p| p.sin()))
                .collect(),
        );
    }
    match pca_scores(&features, &read_control).map_err(|error| error.to_string())? {
        Some(scores) => Ok(ProjectionComputation {
            key,
            dim,
            method: "pca",
            error: "",
            points: rows
                .iter()
                .zip(&scores)
                .map(|((meta, _), s)| projection_point(meta, s[0], s[1]))
                .collect::<Result<Vec<_>, _>>()?,
        }),
        None => Ok(ProjectionComputation {
            key,
            dim,
            method: "none",
            error: "projection failed",
            points: Vec::new(),
        }),
    }
}

pub async fn projection_payload(
    state: &DashboardState,
    query: &str,
    limit: i64,
    read_control: &FactReadControl,
) -> Value {
    let mut obj = Map::new();
    obj.insert("exists".into(), json!(true));
    obj.insert("dim".into(), json!(0));
    obj.insert("limit".into(), json!(limit));
    obj.insert("method".into(), json!("none"));
    obj.insert("points".into(), json!([]));
    obj.insert(
        "coverage".into(),
        json!({
            "completeness": "unknown",
            "examined": 0,
            "limit": limit,
            "omission_reasons": ["read_not_completed"],
        }),
    );
    obj.insert("error".into(), json!(""));

    if read_control.interrupted() {
        obj.insert("error".into(), json!("memory projection interrupted"));
        return Value::Object(obj);
    }
    let application = match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => application,
        Err(error) => {
            obj.insert("error".into(), json!(error.to_string()));
            return Value::Object(obj);
        }
    };
    let point_limit = match usize::try_from(limit.clamp(1, PROJECTION_POINT_CAP)) {
        Ok(limit) => limit,
        Err(error) => {
            obj.insert("error".into(), json!(error.to_string()));
            return Value::Object(obj);
        }
    };
    let points = match application
        .dashboard_vector_points(
            (!query.trim().is_empty()).then(|| query.trim().to_owned()),
            point_limit,
            read_control,
        )
        .await
    {
        Ok(points) => points,
        Err(error) => {
            obj.insert("error".into(), json!(error.to_string()));
            return Value::Object(obj);
        }
    };
    let rows = match vector_rows(points) {
        Ok(rows) => rows,
        Err(error) => {
            obj.insert("error".into(), json!(error));
            return Value::Object(obj);
        }
    };
    let coverage_complete = rows.len() < point_limit;
    obj.insert(
        "coverage".into(),
        json!({
            "completeness": if coverage_complete { "complete" } else { "bounded" },
            "examined": rows.len(),
            "limit": point_limit,
            "omission_reasons": if coverage_complete {
                Vec::<&str>::new()
            } else {
                vec!["request_limit_reached"]
            },
        }),
    );
    let fingerprint = match vector_fingerprint(&rows) {
        Ok(fingerprint) => fingerprint,
        Err(error) => {
            obj.insert("error".into(), json!(error));
            return Value::Object(obj);
        }
    };
    let key = (query.trim().to_string(), limit, fingerprint);

    let cache = PROJECTION_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    if read_control.interrupted() {
        obj.insert("error".into(), json!("memory projection interrupted"));
        return Value::Object(obj);
    }
    if let Some(existing) = guard.get(&state.mem_db_path)
        && existing.key == key
    {
        return projection_response(existing, obj);
    }

    let blocking_control = read_control.clone();
    let computed =
        match tokio::task::spawn_blocking(move || compute_projection(key, rows, blocking_control))
            .await
        {
            Ok(Ok(computed)) => Arc::new(computed),
            Ok(Err(error)) => {
                obj.insert("error".into(), json!(error));
                return Value::Object(obj);
            }
            Err(e) => {
                obj.insert(
                    "error".into(),
                    json!(format!("projection task failed: {e}")),
                );
                return Value::Object(obj);
            }
        };
    if read_control.interrupted() {
        obj.insert("error".into(), json!("memory projection interrupted"));
        return Value::Object(obj);
    }
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
