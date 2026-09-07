//! Vector-point rows, fingerprints, and the cached PCA projection payload.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use super::super::DashboardState;
use super::super::memory_analysis::pca_scores;
use super::facts::fact_summary_json;
use crate::snapshot_cache::{DerivedSnapshotCache, DerivedSnapshotCacheState};
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::{
    FactReadControl, ProjectMemoryDashboardVectorPointV1, ProjectMemoryFactProjectionV1,
    ProjectMemoryStoreRevisionV1,
};

pub(super) const PROJECTION_POINT_CAP: i64 = 2000;

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

struct ProjectionComputation {
    dim: usize,
    method: &'static str,
    error: &'static str,
    points: Vec<Value>,
    examined: usize,
    point_limit: usize,
    coverage_complete: bool,
}

type ProjectionCacheRevision = (ProjectMemoryStoreRevisionV1, String, i64);

static PROJECTION_CACHE: OnceLock<
    DerivedSnapshotCache<String, ProjectionCacheRevision, ProjectionComputation>,
> = OnceLock::new();

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

fn compute_projection(
    rows: Vec<(Value, Vec<f64>)>,
    point_limit: usize,
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
            dim,
            method: "none",
            error: "",
            points,
            examined: rows.len(),
            point_limit,
            coverage_complete: rows.len() < point_limit,
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
            dim,
            method: "pca",
            error: "",
            points: rows
                .iter()
                .zip(&scores)
                .map(|((meta, _), s)| projection_point(meta, s[0], s[1]))
                .collect::<Result<Vec<_>, _>>()?,
            examined: rows.len(),
            point_limit,
            coverage_complete: rows.len() < point_limit,
        }),
        None => Ok(ProjectionComputation {
            dim,
            method: "none",
            error: "projection failed",
            points: Vec::new(),
            examined: rows.len(),
            point_limit,
            coverage_complete: rows.len() < point_limit,
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
    let store_revision = match application.dashboard_store_revision(read_control).await {
        Ok(revision) => revision,
        Err(error) => {
            obj.insert("error".into(), json!(error.to_string()));
            return Value::Object(obj);
        }
    };
    let normalized_query = query.trim().to_owned();
    let revision = (store_revision, normalized_query.clone(), limit);
    let cache = PROJECTION_CACHE.get_or_init(DerivedSnapshotCache::new);
    // The loader is the only writer, so this is the exact row count read for
    // this response. A hit never polls the closure and therefore reports zero.
    let vector_rows_read = AtomicUsize::new(0);
    let (computed, cache_state) = match cache
        .get_or_compute(state.mem_db_path.clone(), revision, || async {
            let snapshot = application
                .dashboard_vector_snapshot(
                    (!normalized_query.is_empty()).then(|| normalized_query.clone()),
                    point_limit,
                    read_control,
                )
                .await
                .map_err(|error| error.to_string())?;
            vector_rows_read.store(snapshot.points().len(), Ordering::Relaxed);
            let observed_revision = (snapshot.store_revision(), normalized_query, limit);
            let rows = vector_rows(snapshot.into_points())?;
            let blocking_control = read_control.clone();
            let computed = tokio::task::spawn_blocking(move || {
                hotpath::measure_block!("dashboard_api.memory.projection_compute", {
                    compute_projection(rows, point_limit, blocking_control)
                })
            })
            .await
            .map_err(|error| format!("projection task failed: {error}"))??;
            Ok::<_, String>((observed_revision, Arc::new(computed)))
        })
        .await
    {
        Ok(cached) => cached,
        Err(error) => {
            obj.insert("error".into(), json!(error));
            return Value::Object(obj);
        }
    };
    if read_control.interrupted() {
        obj.insert("error".into(), json!("memory projection interrupted"));
        return Value::Object(obj);
    }
    projection_response(
        &computed,
        cache_state,
        vector_rows_read.load(Ordering::Relaxed),
        obj,
    )
}

fn projection_response(
    computation: &ProjectionComputation,
    cache_state: DerivedSnapshotCacheState,
    vector_rows_read: usize,
    mut obj: Map<String, Value>,
) -> Value {
    obj.insert(
        "coverage".into(),
        json!({
            "completeness": if computation.coverage_complete { "complete" } else { "bounded" },
            "examined": computation.examined,
            "limit": computation.point_limit,
            "omission_reasons": if computation.coverage_complete {
                Vec::<&str>::new()
            } else {
                vec!["request_limit_reached"]
            },
        }),
    );
    obj.insert(
        "scan".into(),
        json!({
            "cache_scope": "store_revision",
            "cache_state": cache_state.as_str(),
            "vector_rows_read": vector_rows_read,
        }),
    );
    obj.insert("dim".into(), json!(computation.dim));
    obj.insert("method".into(), json!(computation.method));
    obj.insert("points".into(), json!(computation.points));
    obj.insert("error".into(), json!(computation.error));
    Value::Object(obj)
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
