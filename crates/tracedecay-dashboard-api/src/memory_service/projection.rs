//! Vector-point rows, fingerprints, and the cached PCA projection payload.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracedecay_domain::{FactId, PayloadAccessState};

use super::super::DashboardState;
use super::super::memory_analysis::pca_scores;
use super::facts::fact_summary_json;
use crate::read_model::DashboardDomainStateV1;
use crate::snapshot_cache::DerivedSnapshotCacheState;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::{
    FactReadControl, ProjectMemoryDashboardVectorPointV1, ProjectMemoryFactProjectionV1,
    ProjectMemoryStoreRevisionV1,
};

pub(super) const PROJECTION_POINT_CAP: i64 = 2000;

/// Cache provenance of one derived (projection or similarity) read.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MemoryDerivedScanV1 {
    pub cache_scope: String,
    pub cache_state: String,
    /// Vector rows loaded for this response; zero on a cache hit.
    pub vector_rows_read: usize,
}

impl MemoryDerivedScanV1 {
    pub(super) fn store_revision(
        cache_state: DerivedSnapshotCacheState,
        vector_rows_read: usize,
    ) -> Self {
        Self {
            cache_scope: "store_revision".to_owned(),
            cache_state: cache_state.as_str().to_owned(),
            vector_rows_read,
        }
    }
}

/// One eligible fact as the canonical dashboard fact summary projects it.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryProjectedFactV1 {
    pub fact_id: FactId,
    pub payload_access: PayloadAccessState,
    pub trust_score: f64,
    pub retrieval_count: u64,
    pub access_count: u64,
    pub helpful_count: u64,
    pub unhelpful_count: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub projected_as_of: i64,
    pub last_recalled_at: Option<i64>,
    pub content: String,
    pub category: String,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub metadata: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    pub entity_count: u64,
}

/// One projected fact placed in the 2D phase projection.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MemoryProjectionPointV1 {
    #[serde(flatten)]
    pub fact: MemoryProjectedFactV1,
    pub x: f64,
    pub y: f64,
}

/// `pca` only when the decomposition succeeded over at least two equal-length
/// vectors; every other outcome is `none` and is not a semantic map.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryProjectionMethodV1 {
    Pca,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryProjectionCompletenessV1 {
    Complete,
    Bounded,
    Unknown,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MemoryProjectionCoverageV1 {
    pub completeness: MemoryProjectionCompletenessV1,
    pub examined: usize,
    pub limit: i64,
    pub omission_reasons: Vec<String>,
}

/// `GET /api/plugins/holographic/projection`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MemoryProjectionPayloadV1 {
    pub exists: bool,
    pub dim: usize,
    pub limit: i64,
    pub method: MemoryProjectionMethodV1,
    pub points: Vec<MemoryProjectionPointV1>,
    pub coverage: MemoryProjectionCoverageV1,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan: Option<MemoryDerivedScanV1>,
    /// Request lifecycle state when the read ended before a result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DashboardDomainStateV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub error: String,
}

impl MemoryProjectionPayloadV1 {
    pub fn empty(limit: i64, error: impl Into<String>) -> Self {
        Self {
            exists: true,
            dim: 0,
            limit,
            method: MemoryProjectionMethodV1::None,
            points: Vec::new(),
            coverage: MemoryProjectionCoverageV1 {
                completeness: MemoryProjectionCompletenessV1::Unknown,
                examined: 0,
                limit,
                omission_reasons: vec!["read_not_completed".to_owned()],
            },
            scan: None,
            state: None,
            code: None,
            error: error.into(),
        }
    }
}

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

/// One cached PCA projection of a store revision for a query/limit pair.
pub(crate) struct ProjectionComputation {
    dim: usize,
    method: MemoryProjectionMethodV1,
    error: &'static str,
    points: Vec<MemoryProjectionPointV1>,
    examined: usize,
    coverage_complete: bool,
}

pub(crate) type ProjectionCacheRevision = (ProjectMemoryStoreRevisionV1, String, i64);

fn projection_point(meta: &Value, x: f64, y: f64) -> Result<MemoryProjectionPointV1, String> {
    let mut fact = MemoryProjectedFactV1::deserialize(meta)
        .map_err(|error| format!("projection metadata did not match its contract: {error}"))?;
    fact.content = fact.content.chars().take(200).collect();
    Ok(MemoryProjectionPointV1 {
        fact,
        x: (x * 1e6).round() / 1e6,
        y: (y * 1e6).round() / 1e6,
    })
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
            method: MemoryProjectionMethodV1::None,
            error: "",
            points,
            examined: rows.len(),
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
            method: MemoryProjectionMethodV1::Pca,
            error: "",
            points: rows
                .iter()
                .zip(&scores)
                .map(|((meta, _), s)| projection_point(meta, s[0], s[1]))
                .collect::<Result<Vec<_>, _>>()?,
            examined: rows.len(),
            coverage_complete: rows.len() < point_limit,
        }),
        None => Ok(ProjectionComputation {
            dim,
            method: MemoryProjectionMethodV1::None,
            error: "projection failed",
            points: Vec::new(),
            examined: rows.len(),
            coverage_complete: rows.len() < point_limit,
        }),
    }
}

pub async fn projection_payload(
    state: &DashboardState,
    query: &str,
    limit: i64,
    read_control: &FactReadControl,
) -> MemoryProjectionPayloadV1 {
    if read_control.interrupted() {
        return MemoryProjectionPayloadV1::empty(limit, "memory projection interrupted");
    }
    let application = match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => application,
        Err(error) => return MemoryProjectionPayloadV1::empty(limit, error.to_string()),
    };
    let point_limit = match usize::try_from(limit.clamp(1, PROJECTION_POINT_CAP)) {
        Ok(limit) => limit,
        Err(error) => return MemoryProjectionPayloadV1::empty(limit, error.to_string()),
    };
    let store_revision = match application.dashboard_store_revision(read_control).await {
        Ok(revision) => revision,
        Err(error) => return MemoryProjectionPayloadV1::empty(limit, error.to_string()),
    };
    let normalized_query = query.trim().to_owned();
    let revision = (store_revision, normalized_query.clone(), limit);
    // The loader is the only writer, so this is the exact row count read for
    // this response. A hit never polls the closure and therefore reports zero.
    let vector_rows_read = AtomicUsize::new(0);
    let (computed, cache_state) = match state
        .derived_snapshots
        .projection
        .get_or_compute(revision, || async {
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
        Err(error) => return MemoryProjectionPayloadV1::empty(limit, error),
    };
    if read_control.interrupted() {
        return MemoryProjectionPayloadV1::empty(limit, "memory projection interrupted");
    }
    let (completeness, omission_reasons) = if computed.coverage_complete {
        (MemoryProjectionCompletenessV1::Complete, Vec::new())
    } else {
        (
            MemoryProjectionCompletenessV1::Bounded,
            vec!["request_limit_reached".to_owned()],
        )
    };
    MemoryProjectionPayloadV1 {
        dim: computed.dim,
        method: computed.method,
        points: computed.points.clone(),
        coverage: MemoryProjectionCoverageV1 {
            completeness,
            examined: computed.examined,
            limit: limit.clamp(1, PROJECTION_POINT_CAP),
            omission_reasons,
        },
        scan: Some(MemoryDerivedScanV1::store_revision(
            cache_state,
            vector_rows_read.load(Ordering::Relaxed),
        )),
        ..MemoryProjectionPayloadV1::empty(limit, computed.error)
    }
}
