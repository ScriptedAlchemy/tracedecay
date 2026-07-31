//! Health, test risk, sessions, gini, dependency depth, DSM, and test map
//! tool handlers.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_application::retrieval::{
    HealthDeltaCoverageV1, HealthDeltaCurrentnessV1, HealthDeltaPointV1, HealthDeltaResult,
    HealthDeltaScopeV1, HealthDimensionDeltaV1, HealthDimensionPointV1,
};
use tracedecay_application::{
    ObservabilityApplicationV1, ObservabilityHorizonV1, ObservabilityQueryV1,
};
use tracedecay_domain::{
    CoverageStateV1, HealthDimensionObservedV1, HealthSnapshotObservedV1, ManifestDigest,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityRetentionClassV1,
    ObservabilityTerminalResultV1, UtcMicros, canonical_sha256,
};

use crate::application::observability::RegisteredObservabilityPortV1;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::graph::health::{
    HealthDimensions, acyclicity_score, compute_composite_health, dependency_depth, depth_score,
    dsm_clusters, gini_coefficient, gini_label, modularity_score,
};

/// Coarse human label for a modularity score in [0,1].
fn modularity_label(score: f64) -> &'static str {
    if score >= 0.75 {
        "high"
    } else if score >= 0.5 {
        "moderate"
    } else {
        "low"
    }
}
use crate::graph::queries::GraphQueryManager;
use crate::tracedecay::TraceDecay;
use crate::types::NodeKind;

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::support::{effective_path, unique_file_paths};

struct HealthSnapshot {
    quality_signal: u32,
    files_analyzed: usize,
    acyclicity: f64,
    depth: f64,
    equality: f64,
    redundancy: f64,
    modularity: f64,
    coverage_discipline: f64,
    /// Raw signals retained for `details=true` (#82).
    gini: f64,
    edges_in_cycles: usize,
    total_edges: usize,
    max_chain: usize,
    ideal_chain: usize,
    complexity_files: usize,
    modularity_components: usize,
    dead_count: usize,
    total_fns: usize,
    skip_coverage_count: usize,
}

const HEALTH_DELTA_SCHEMA_VERSION: u32 = 1;
const HEALTH_DELTA_CURSOR_PREFIX: &str = "health-delta.v1.";

fn health_delta_now() -> UtcMicros {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_micros().min(i64::MAX as u128) as i64
        });
    UtcMicros(micros)
}

fn health_score_ppm(value: f64) -> u64 {
    (value.clamp(0.0, 1.0) * 1_000_000.0).round() as u64
}

fn health_delta_dimensions(snapshot: &HealthSnapshot) -> BTreeMap<String, HealthDimensionPointV1> {
    session_dimension_values(snapshot)
        .into_iter()
        .map(|(name, score)| {
            let denominator = match name {
                "acyclicity" => Some(snapshot.total_edges as u64),
                "depth" => Some(snapshot.max_chain as u64),
                "equality" => Some(snapshot.complexity_files as u64),
                "modularity" => Some(snapshot.modularity_components as u64),
                "redundancy" | "coverage_discipline" => Some(snapshot.total_fns as u64),
                _ => None,
            }
            .filter(|value| *value > 0);
            (
                name.to_owned(),
                HealthDimensionPointV1 {
                    score_ppm: health_score_ppm(score),
                    denominator,
                },
            )
        })
        .collect()
}

fn health_delta_scope(cg: &TraceDecay, path_prefix: Option<&str>) -> Result<HealthDeltaScopeV1> {
    let project_id = cg.store_layout().identity.project_id.clone();
    let path_prefix = path_prefix
        .map(|raw| {
            let trimmed = raw.trim_matches('/');
            if trimmed.is_empty()
                || trimmed.len() > 4_096
                || raw.starts_with('/')
                || raw.contains('\\')
                || raw.chars().any(char::is_control)
                || trimmed
                    .split('/')
                    .any(|component| component.is_empty() || matches!(component, "." | ".."))
            {
                return Err(TraceDecayError::Config {
                    message: "health-delta path_prefix must be one canonical project-relative path"
                        .to_owned(),
                });
            }
            Ok(trimmed.to_owned())
        })
        .transpose()?;
    let scope_digest = canonical_sha256(&(
        "tracedecay.health-delta.scope.v1",
        project_id.as_deref(),
        path_prefix.as_deref(),
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to bind health-delta scope: {error}"),
    })?;
    Ok(HealthDeltaScopeV1 {
        project_id,
        scope_digest,
        path_prefix,
    })
}

fn health_delta_watermark(
    scope: &HealthDeltaScopeV1,
    observed_at: UtcMicros,
    quality_signal: u32,
    files_analyzed: u64,
    function_denominator: u64,
    dimensions: &BTreeMap<String, HealthDimensionPointV1>,
) -> Result<ManifestDigest> {
    canonical_sha256(&(
        "tracedecay.health-delta.watermark.v1",
        scope,
        observed_at,
        quality_signal,
        files_analyzed,
        function_denominator,
        dimensions,
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("failed to seal health-delta watermark: {error}"),
    })
}

fn health_delta_cursor(watermark: &ManifestDigest) -> String {
    format!(
        "{HEALTH_DELTA_CURSOR_PREFIX}{}",
        watermark
            .as_str()
            .strip_prefix("sha256:")
            .unwrap_or_default()
    )
}

fn health_delta_digest_from_cursor(cursor: &str) -> Result<&str> {
    let digest = cursor
        .strip_prefix(HEALTH_DELTA_CURSOR_PREFIX)
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| TraceDecayError::Config {
            message: "invalid health-delta cursor".to_owned(),
        })?;
    Ok(digest)
}

async fn persist_health_delta_point(
    db: &RegisteredGlobalDb,
    scope: &HealthDeltaScopeV1,
    point: &HealthDeltaPointV1,
) -> Result<String> {
    let cursor = health_delta_cursor(&point.watermark);
    let payload = HealthSnapshotObservedV1 {
        scope_digest: scope.scope_digest.as_str().to_owned(),
        quality_signal: point.quality_signal,
        files_analyzed: point.files_analyzed,
        function_denominator: point.function_denominator,
        dimensions: point
            .dimensions
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    HealthDimensionObservedV1 {
                        score_ppm: value.score_ppm,
                        denominator: value.denominator,
                    },
                )
            })
            .collect(),
    };
    let coverage = if point.files_analyzed > 0
        && point.function_denominator > 0
        && point
            .dimensions
            .values()
            .all(|dimension| dimension.denominator.is_some())
    {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    let observed_at = point.observed_at.0;
    let envelope = ObservabilityEnvelopeV1 {
        event_id: cursor.clone(),
        event_kind: "health.snapshot.observed.v1".to_owned(),
        schema_revision: HEALTH_DELTA_SCHEMA_VERSION,
        idempotency_key: cursor.clone(),
        trace_id: format!("health-delta:{}", scope.scope_digest.as_str()),
        scope_ref: scope.scope_digest.as_str().to_owned(),
        capability: "health_delta".to_owned(),
        operation: "observe".to_owned(),
        event_time_micros: observed_at,
        observation_time_micros: observed_at,
        valid_from_micros: Some(observed_at),
        valid_until_micros: None,
        quantity: Some(f64::from(point.quality_signal)),
        unit: Some("quality_signal".to_owned()),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "health-delta-projector.v1".to_owned(),
        configuration_revision: "effective-project-configuration.v1".to_owned(),
        policy_revision: "local-health-observation.v1".to_owned(),
        watermark: point.watermark.as_str().to_owned(),
        coverage,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: format!("health-delta-{}", std::process::id()),
        producer_sequence: observed_at.max(0) as u64,
        payload: ObservabilityPayloadV1::HealthSnapshot(payload),
    };
    let port = RegisteredObservabilityPortV1::new(db);
    ObservabilityApplicationV1::new(port, port)
        .record(envelope)
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to retain health-delta observation: {error}"),
        })?;
    Ok(cursor)
}

async fn load_health_delta_point(
    db: &RegisteredGlobalDb,
    scope: &HealthDeltaScopeV1,
    cursor: &str,
) -> Result<HealthDeltaPointV1> {
    health_delta_digest_from_cursor(cursor)?;
    let port = RegisteredObservabilityPortV1::new(db);
    let page = ObservabilityApplicationV1::new(port, port)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.scope_digest.as_str().to_owned(),
            event_kinds: vec!["health.snapshot.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 10_000,
        })
        .await
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to read health-delta observations: {error}"),
        })?;
    let envelope = page
        .events
        .into_iter()
        .find(|event| event.idempotency_key == cursor)
        .ok_or_else(|| TraceDecayError::Config {
            message: "health-delta cursor is unknown or expired".to_owned(),
        })?;
    let ObservabilityPayloadV1::HealthSnapshot(payload) = envelope.payload else {
        return Err(TraceDecayError::Config {
            message: "health-delta cursor snapshot is invalid".to_owned(),
        });
    };
    let stored = HealthDeltaPointV1 {
        watermark: ManifestDigest::new(envelope.watermark).map_err(|_| {
            TraceDecayError::Config {
                message: "health-delta cursor snapshot is invalid".to_owned(),
            }
        })?,
        observed_at: UtcMicros(envelope.event_time_micros),
        quality_signal: payload.quality_signal,
        files_analyzed: payload.files_analyzed,
        function_denominator: payload.function_denominator,
        dimensions: payload
            .dimensions
            .into_iter()
            .map(|(name, value)| {
                (
                    name,
                    HealthDimensionPointV1 {
                        score_ppm: value.score_ppm,
                        denominator: value.denominator,
                    },
                )
            })
            .collect(),
    };
    let recomputed = health_delta_watermark(
        scope,
        stored.observed_at,
        stored.quality_signal,
        stored.files_analyzed,
        stored.function_denominator,
        &stored.dimensions,
    )?;
    if payload.scope_digest != scope.scope_digest.as_str()
        || stored.watermark != recomputed
        || health_delta_cursor(&recomputed) != cursor
    {
        return Err(TraceDecayError::Config {
            message: "health-delta cursor snapshot failed identity validation".to_owned(),
        });
    }
    Ok(stored)
}

fn health_dimension_deltas(
    before: &HealthDeltaPointV1,
    after: &HealthDeltaPointV1,
) -> BTreeMap<String, HealthDimensionDeltaV1> {
    after
        .dimensions
        .iter()
        .filter_map(|(name, after_value)| {
            let before_value = before.dimensions.get(name)?;
            let delta_ppm = after_value.score_ppm as i64 - before_value.score_ppm as i64;
            Some((
                name.clone(),
                HealthDimensionDeltaV1 {
                    before_ppm: before_value.score_ppm,
                    after_ppm: after_value.score_ppm,
                    delta_ppm,
                    before_denominator: before_value.denominator,
                    after_denominator: after_value.denominator,
                    status: if delta_ppm > 1_000 {
                        "improved"
                    } else if delta_ppm < -1_000 {
                        "degraded"
                    } else {
                        "unchanged"
                    }
                    .to_owned(),
                },
            ))
        })
        .collect()
}

pub(crate) async fn compute_health_delta_result(
    cg: &TraceDecay,
    db: &RegisteredGlobalDb,
    before_cursor: Option<&str>,
    path_prefix: Option<&str>,
) -> Result<HealthDeltaResult> {
    let scope = health_delta_scope(cg, path_prefix)?;
    let pinned_before = if let Some(cursor) = before_cursor {
        let stored = load_health_delta_point(db, &scope, cursor).await?;
        Some((stored, cursor.to_owned()))
    } else {
        None
    };
    let snapshot = compute_health_snapshot(cg, scope.path_prefix.as_deref()).await?;
    let observed_at = health_delta_now();
    let dimensions = health_delta_dimensions(&snapshot);
    let watermark = health_delta_watermark(
        &scope,
        observed_at,
        snapshot.quality_signal,
        snapshot.files_analyzed as u64,
        snapshot.total_fns as u64,
        &dimensions,
    )?;
    let after = HealthDeltaPointV1 {
        watermark,
        observed_at,
        quality_signal: snapshot.quality_signal,
        files_analyzed: snapshot.files_analyzed as u64,
        function_denominator: snapshot.total_fns as u64,
        dimensions,
    };
    let after_cursor = persist_health_delta_point(db, &scope, &after).await?;
    let (before, before_cursor) =
        pinned_before.unwrap_or_else(|| (after.clone(), after_cursor.clone()));
    let delta = i64::from(after.quality_signal) - i64::from(before.quality_signal);
    let branch = cg.branch_diagnostics();
    let eligible = before.files_analyzed.saturating_add(after.files_analyzed);
    let denominator = (eligible > 0).then_some(eligible);
    Ok(HealthDeltaResult {
        schema_version: HEALTH_DELTA_SCHEMA_VERSION,
        scope,
        before: before.clone(),
        after: after.clone(),
        before_cursor,
        after_cursor,
        pass: denominator.is_some() && delta >= 0,
        delta,
        dimensions: health_dimension_deltas(&before, &after),
        coverage: HealthDeltaCoverageV1 {
            eligible: denominator,
            visited: denominator,
            denominator,
            completeness: if denominator.is_some() {
                "complete"
            } else {
                "unknown"
            }
            .to_owned(),
        },
        currentness: HealthDeltaCurrentnessV1 {
            state: if branch.serving_db_exists && !branch.is_fallback {
                "current"
            } else {
                "degraded"
            }
            .to_owned(),
            observed_at,
        },
    })
}

/// Computes all 5 health dimensions and the composite signal for a given scope.
async fn compute_health_snapshot(
    cg: &TraceDecay,
    path_prefix: Option<&str>,
) -> Result<HealthSnapshot> {
    let adj = GraphQueryManager::new(cg.db())
        .build_file_adjacency(path_prefix)
        .await?;
    let files_analyzed = adj.len();
    let total_edges = adj.values().map(HashSet::len).sum();

    let (acyclicity, edges_in_cycles) = acyclicity_score(&adj);
    let depth_result = dependency_depth(&adj, 1);
    let depth = depth_score(depth_result.max_depth, depth_result.ideal_depth);

    let all_nodes = cg.get_all_nodes().await?;
    let nodes: Vec<_> = all_nodes
        .iter()
        .filter(|n| crate::path_scope::path_matches_scope(&n.file_path, path_prefix))
        .collect();

    let mut per_file_complexity: HashMap<String, f64> = HashMap::new();
    for n in &nodes {
        let c = f64::from(n.branches) * 2.0
            + f64::from(n.loops) * 2.0
            + f64::from(n.max_nesting) * 3.0
            + f64::from(n.end_line.saturating_sub(n.start_line) + 1);
        *per_file_complexity
            .entry(n.file_path.clone())
            .or_insert(0.0) += c;
    }
    let complexity_values: Vec<f64> = per_file_complexity.values().copied().collect();
    let complexity_files = complexity_values.len();
    let gini = gini_coefficient(&complexity_values);
    let equality = (1.0 - gini).clamp(0.0, 1.0);

    let dead = cg
        .find_dead_code(&[NodeKind::Function, NodeKind::Method], false)
        .await?;
    let dead_in_scope = dead
        .iter()
        .filter(|n| crate::path_scope::path_matches_scope(&n.file_path, path_prefix));
    let dead_count = dead_in_scope.count();
    let total_fns = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .count();
    let redundancy = if total_fns == 0 {
        1.0
    } else {
        (1.0 - dead_count as f64 / total_fns as f64).clamp(0.0, 1.0)
    };

    let (modularity, modularity_components) = modularity_score(&adj);

    // coverage_discipline: penalise overuse of skip-test-coverage annotations.
    let skip_coverage = cg.get_skip_test_coverage_node_ids().await?;
    let skipped_in_scope = nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Function | NodeKind::Method) && skip_coverage.contains(&n.id)
        })
        .count();
    let coverage_discipline = if total_fns == 0 {
        1.0
    } else {
        (1.0 - skipped_in_scope as f64 / total_fns as f64).clamp(0.0, 1.0)
    };

    let dims = HealthDimensions {
        acyclicity,
        depth,
        equality,
        redundancy,
        modularity,
        coverage_discipline,
    };
    let quality_signal = compute_composite_health(&dims);

    Ok(HealthSnapshot {
        quality_signal,
        files_analyzed,
        acyclicity,
        depth,
        equality,
        redundancy,
        modularity,
        coverage_discipline,
        gini,
        edges_in_cycles,
        total_edges,
        max_chain: depth_result.max_depth,
        ideal_chain: depth_result.ideal_depth,
        complexity_files,
        modularity_components,
        dead_count,
        total_fns,
        skip_coverage_count: skipped_in_scope,
    })
}

/// Handles `tracedecay_gini` tool calls.
pub(super) async fn handle_gini(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let metric = args
        .get("metric")
        .and_then(|v| v.as_str())
        .unwrap_or("complexity");
    let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("file");
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);
    let path_prefix = effective_path(&args, scope_prefix);

    let all_nodes = cg.get_all_nodes().await?;
    let all_edges = if metric == "fan_in" || metric == "fan_out" {
        cg.get_all_edges().await?
    } else {
        vec![]
    };

    // Apply path filter
    let nodes: Vec<_> = all_nodes
        .into_iter()
        .filter(|n| crate::path_scope::path_matches_scope(&n.file_path, path_prefix))
        .collect();

    let named_values: Vec<(String, f64)> = match (metric, scope) {
        ("complexity", "file") => {
            let mut per_file: HashMap<String, f64> = HashMap::new();
            for n in &nodes {
                let c = f64::from(n.branches + n.loops + n.returns + n.max_nesting);
                *per_file.entry(n.file_path.clone()).or_insert(0.0) += c;
            }
            per_file.into_iter().collect()
        }
        ("lines", "file") => {
            let mut per_file: HashMap<String, f64> = HashMap::new();
            for n in &nodes {
                let lines = f64::from(n.end_line.saturating_sub(n.start_line) + 1);
                *per_file.entry(n.file_path.clone()).or_insert(0.0) += lines;
            }
            per_file.into_iter().collect()
        }
        ("fan_in", "file") => {
            let node_to_file: HashMap<String, String> = nodes
                .iter()
                .map(|n| (n.id.clone(), n.file_path.clone()))
                .collect();
            let mut per_file: HashMap<String, f64> = HashMap::new();
            // Initialize all files
            for n in &nodes {
                per_file.entry(n.file_path.clone()).or_insert(0.0);
            }
            for e in &all_edges {
                if let (Some(src_file), Some(tgt_file)) =
                    (node_to_file.get(&e.source), node_to_file.get(&e.target))
                    && src_file != tgt_file
                {
                    *per_file.entry(tgt_file.clone()).or_insert(0.0) += 1.0;
                }
            }
            per_file.into_iter().collect()
        }
        ("fan_out", "file") => {
            let node_to_file: HashMap<String, String> = nodes
                .iter()
                .map(|n| (n.id.clone(), n.file_path.clone()))
                .collect();
            let mut per_file: HashMap<String, f64> = HashMap::new();
            for n in &nodes {
                per_file.entry(n.file_path.clone()).or_insert(0.0);
            }
            for e in &all_edges {
                if let (Some(src_file), Some(tgt_file)) =
                    (node_to_file.get(&e.source), node_to_file.get(&e.target))
                    && src_file != tgt_file
                {
                    *per_file.entry(src_file.clone()).or_insert(0.0) += 1.0;
                }
            }
            per_file.into_iter().collect()
        }
        ("members", _) => {
            // Count members of each Class/Struct via parent_id (v9+).
            let class_nodes: HashSet<String> = nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::Class | NodeKind::Struct))
                .map(|n| n.id.clone())
                .collect();
            let mut per_class: HashMap<String, (String, f64)> = nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::Class | NodeKind::Struct))
                .map(|n| (n.id.clone(), (n.name.clone(), 0.0)))
                .collect();
            for n in &nodes {
                if let Some(parent) = n.parent_id.as_deref()
                    && class_nodes.contains(parent)
                    && let Some(entry) = per_class.get_mut(parent)
                {
                    entry.1 += 1.0;
                }
            }
            per_class.into_values().collect()
        }
        (_, "symbol") => {
            // Per-function/method complexity
            nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
                .map(|n| {
                    let c = f64::from(n.branches + n.loops + n.returns + n.max_nesting);
                    (format!("{}:{}", n.file_path, n.name), c)
                })
                .collect()
        }
        _ => {
            // Default: file-level complexity
            let mut per_file: HashMap<String, f64> = HashMap::new();
            for n in &nodes {
                let c = f64::from(n.branches + n.loops + n.returns + n.max_nesting);
                *per_file.entry(n.file_path.clone()).or_insert(0.0) += c;
            }
            per_file.into_iter().collect()
        }
    };

    let values: Vec<f64> = named_values.iter().map(|(_, v)| *v).collect();
    let gini = gini_coefficient(&values);
    let interpretation = gini_label(gini);

    // Sort descending, take top limit as outliers with percentiles
    let total_items = named_values.len();
    let mut sorted = named_values;
    sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    sorted.truncate(limit);

    let max_val = sorted.first().map_or(0.0, |(_, v)| *v);
    let outliers: Vec<Value> = sorted
        .iter()
        .map(|(name, val)| {
            let pct = if max_val > 0.0 {
                (val / max_val * 100.0).round()
            } else {
                0.0
            };
            json!({
                "name": name,
                "value": val,
                "pct_of_max": pct,
            })
        })
        .collect();

    let output = json!({
        "gini": (gini * 10000.0).round() / 10000.0,
        "interpretation": interpretation,
        "total_items": total_items,
        "metric": metric,
        "scope": scope,
        "outliers": outliers,
    });

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
}

/// Handles `tracedecay_dependency_depth` tool calls.
pub(super) async fn handle_dependency_depth(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);
    let path_prefix = effective_path(&args, scope_prefix);

    let adj = GraphQueryManager::new(cg.db())
        .build_file_adjacency(path_prefix)
        .await?;

    let result = dependency_depth(&adj, limit);
    let score = depth_score(result.max_depth, result.ideal_depth);

    let chains: Vec<Value> = result
        .chains
        .iter()
        .map(|ch| {
            json!({
                "file": ch.file,
                "depth": ch.depth,
                "chain": ch.chain,
            })
        })
        .collect();

    let output = json!({
        "max_depth": result.max_depth,
        "ideal_depth": result.ideal_depth,
        "depth_score": (score * 10000.0).round() / 10000.0,
        "chains": chains,
    });

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
}

/// Handles `tracedecay_health` tool calls.
pub(super) async fn handle_health(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);
    let details = args
        .get("details")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let snap = compute_health_snapshot(cg, path_prefix).await?;

    let output = if details {
        let r4 = |x: f64| (x * 10000.0).round() / 10000.0;
        json!({
            "quality_signal": snap.quality_signal,
            "files_analyzed": snap.files_analyzed,
            "dimensions": {
                "acyclicity": {
                    "score": r4(snap.acyclicity),
                    "edges_in_cycles": snap.edges_in_cycles,
                    "source": "1 - edges_in_nontrivial_SCCs / total_edges",
                },
                "depth": {
                    "score": r4(snap.depth),
                    "max_chain": snap.max_chain,
                    "ideal_chain": snap.ideal_chain,
                    "source": "min(1, ideal_chain / max_chain), ideal = ceil(log2(file_count))",
                },
                "equality": {
                    "score": r4(snap.equality),
                    "gini": r4(snap.gini),
                    "interpretation": gini_label(snap.gini),
                    "source": "1 - gini(per_file_complexity)",
                },
                "redundancy": {
                    "score": r4(snap.redundancy),
                    "dead_count": snap.dead_count,
                    "total_fns": snap.total_fns,
                    "source": "1 - dead_fns / total_fns",
                },
                "modularity": {
                    "score": r4(snap.modularity),
                    "interpretation": modularity_label(snap.modularity),
                    "components_after_hub_removal": snap.modularity_components,
                    "source": "1 - 1/components_after_hub_removal",
                },
                "coverage_discipline": {
                    "score": r4(snap.coverage_discipline),
                    "skip_test_coverage_count": snap.skip_coverage_count,
                    "total_fns": snap.total_fns,
                    "source": "1 - skip_test_coverage_annotations / total_fns",
                },
            },
            "weights": {
                "note": "quality_signal is geometric mean × 10000",
            },
        })
    } else {
        json!({
            "quality_signal": snap.quality_signal,
            "files_analyzed": snap.files_analyzed,
        })
    };

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
}

/// Bound for the session-temporal doctor probe so a wedged sessions DB cannot
/// monopolize a `tracedecay_runtime` request indefinitely.
const SESSION_TEMPORAL_HEALTH_BUDGET: Duration = Duration::from_secs(8);

async fn session_temporal_health_value(
    project_session_db: Option<&crate::global_db::RegisteredGlobalDb>,
) -> Value {
    match project_session_db {
        Some(db) => match tokio::time::timeout(
            SESSION_TEMPORAL_HEALTH_BUDGET,
            db.session_temporal_doctor_health(),
        )
        .await
        {
            Ok(health) => serde_json::to_value(health).unwrap_or_else(|_| {
                json!({
                    "status": "unavailable",
                    "findings": [],
                    "message": "session temporal health serialization failed",
                })
            }),
            Err(_) => json!({
                "status": "timed_out",
                "findings": [],
                "message": "session temporal health exceeded deadline",
            }),
        },
        None => json!({
            "status": "unavailable",
            "findings": [],
        }),
    }
}

/// Runs the exhaustive observation-authority audit for the routed project
/// owner.
///
/// Returns `(ok, typed reason, observed detail)`. `ok` is tri-state: `Some(true)`
/// only when the audit ran and passed, `Some(false)` when it ran and failed, and
/// `None` when it could not run at all. The typed reason uses the vocabulary
/// Doctor already understands (`authority_invariant_failed`,
/// `authority_store_unavailable`) so the CLI can classify without parsing the
/// free-form detail.
async fn observation_authority_audit(
    registry: Option<&crate::global_db::RegisteredGlobalDb>,
) -> (Option<bool>, Option<&'static str>, Option<String>) {
    match registry {
        Some(registry) => {
            let audit = match registry.read_snapshot().await {
                Ok(snapshot) => {
                    crate::global_db::schema_stages::validate_observation_authority_connection(
                        &snapshot,
                    )
                    .await
                }
                Err(error) => Err(TraceDecayError::Database {
                    operation: "begin observation authority audit".to_string(),
                    message: error.to_string(),
                }),
            };
            match audit {
                Ok(()) => (Some(true), None, None),
                Err(error) => (
                    Some(false),
                    Some("authority_invariant_failed"),
                    Some(error.to_string()),
                ),
            }
        }
        // This handler is only reached with a routed project owner, so a missing
        // handle means the registry could not be attached here; the daemon core
        // route is the producer that can distinguish a store that is absent on
        // disk (`authority_store_missing`).
        None => (
            None,
            Some("authority_store_unavailable"),
            Some("authoritative global registry is unavailable".to_string()),
        ),
    }
}

/// Registered-runtime implementation of literal workspace-placeholder paths
/// over a registered read snapshot.
async fn literal_workspace_placeholder_transcript_paths(
    conn: &impl crate::db::engine::QueryExecutor,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let Ok(mut rows) = conn
        .query(
            "SELECT DISTINCT transcript_path FROM sessions
             WHERE transcript_path IS NOT NULL
               AND transcript_path != ''
               AND (transcript_path LIKE '%${workspaceFolder}%'
                    OR transcript_path LIKE '%$workspaceFolder%')
             ORDER BY transcript_path
             LIMIT ?1",
            crate::db::engine::params![i64::try_from(limit).unwrap_or(i64::MAX)],
        )
        .await
    else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(path) = row.get::<String>(0) {
            paths.push(path);
        }
    }
    paths
}

/// Handles `tracedecay_runtime` tool calls.
///
/// Issue #80 — surface process and database telemetry so users hitting
/// unexpected CPU/RAM pressure can attach a structured snapshot to a
/// bug report. The MCP wrapper just delegates to `runtime_telemetry`.
async fn attach_doctor_report(
    value: &mut Value,
    reader: Option<&crate::dashboard::DoctorReportReader>,
) {
    value["doctor_report"] = match reader {
        Some(reader) => match reader().await {
            Ok(admitted) => json!({
                "kind": "observed",
                "report": admitted.report,
                "table_growth_evidence": admitted.table_growth_evidence,
            }),
            Err(_) => json!({
                "kind": "unknown",
                "table_growth_evidence": [],
            }),
        },
        None => json!({
            "kind": "unsupported",
            "table_growth_evidence": [],
        }),
    };
}

pub(super) async fn handle_runtime(
    cg: &TraceDecay,
    args: Value,
    registry: Option<&crate::global_db::RegisteredGlobalDb>,
    project_session_db: Option<&crate::global_db::RegisteredGlobalDb>,
    doctor_report_reader: Option<&crate::dashboard::DoctorReportReader>,
) -> Result<ToolResult> {
    let authority_audit = args
        .get("authority_audit")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let snap = crate::runtime_telemetry::collect_with_integrity(cg, authority_audit).await?;
    let mut value = serde_json::to_value(&snap).unwrap_or_else(|_| json!({}));
    // Doctor historically keys temporal health off `authority_audit`. Keep that
    // coupling, and also allow an explicit independent opt-in.
    let include_session_temporal_health = authority_audit
        || args
            .get("session_temporal_health")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if authority_audit || include_session_temporal_health {
        let (authority, temporal) = tokio::join!(
            async {
                if authority_audit {
                    Some(observation_authority_audit(registry).await)
                } else {
                    None
                }
            },
            async {
                if include_session_temporal_health {
                    Some(session_temporal_health_value(project_session_db).await)
                } else {
                    None
                }
            }
        );
        if let Some((authority_audit_ok, authority_audit_reason, authority_audit_error)) = authority
            && let Some(database) = value.get_mut("database").and_then(Value::as_object_mut)
        {
            database.insert("authority_audit_ok".to_string(), json!(authority_audit_ok));
            database.insert(
                "authority_audit_reason".to_string(),
                json!(authority_audit_reason),
            );
            database.insert(
                "authority_audit_error".to_string(),
                json!(authority_audit_error),
            );
        }
        if let Some(temporal) = temporal {
            value["session_temporal_health"] = temporal;
        }
    }
    if args
        .get("session_ingest_health")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        match project_session_db {
            Some(db) => {
                value["cursor_session_ingest"] = match db.cursor_session_ingest_health().await {
                    Ok(health) => serde_json::to_value(health).unwrap_or_else(|error| {
                        json!({
                            "status": "unavailable",
                            "reason": "session_ingest_serialization_failed",
                            "message": error.to_string(),
                        })
                    }),
                    Err(error) => json!({
                        "status": "unavailable",
                        "reason": "session_ingest_query_failed",
                        "message": error,
                    }),
                };
                match db.read_snapshot().await {
                    Ok(snapshot) => {
                        value["cursor_session_placeholder_paths"] = json!(
                            literal_workspace_placeholder_transcript_paths(&snapshot, 10).await
                        );
                    }
                    Err(_) => {
                        value["cursor_session_placeholder_paths"] = json!([]);
                    }
                }
            }
            None => {
                value["cursor_session_ingest"] = json!({
                    "status": "unavailable",
                    "reason": "session_store_unavailable",
                    "message": "daemon project session authority is unavailable",
                });
            }
        }
    }
    if args
        .get("doctor_report")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        attach_doctor_report(&mut value, doctor_report_reader).await;
    }
    let semantic_configuration = cg
        .configuration_runtime()
        .client()
        .current()
        .await
        .ok()
        .and_then(|pinned| {
            crate::application::semantic_runtime::SemanticConfigurationPinV1::from_current(
                &crate::application::configuration::ConfigurationCurrentStateV1 {
                    revision_id: pinned.revision_id,
                    snapshot: pinned.snapshot,
                },
            )
            .ok()
        });
    if let Some(semantic) =
        crate::application::semantic_runtime::project_semantic_application_status(
            cg.project_root(),
            semantic_configuration,
        )
    {
        value["semantic_runtime"] = serde_json::to_value(&semantic).unwrap_or_else(|_| json!({}));
    }
    let text = render::finalize(Some(cg.project_root()), &args, &value, || {
        render::generic_md(&value)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
}

/// Handles `tracedecay_dsm` tool calls.
pub(super) async fn handle_dsm(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);
    let shape = args
        .get("shape")
        .and_then(|v| v.as_str())
        .unwrap_or("stats");
    let max_files = args
        .get("max_files")
        .and_then(serde_json::Value::as_u64)
        .map_or(30, |v| v.min(200) as usize);

    let adj = GraphQueryManager::new(cg.db())
        .build_file_adjacency(path_prefix)
        .await?;

    let file_count = adj.len();
    let edge_count: usize = adj.values().map(std::collections::HashSet::len).sum();
    let density = if file_count > 1 {
        edge_count as f64 / (file_count * (file_count - 1)) as f64
    } else {
        0.0
    };

    let cluster_rows = dsm_clusters(&adj);
    let mut clusters: Vec<Value> = cluster_rows
        .iter()
        .map(|cluster| {
            json!({
                "directory": cluster.directory,
                "file_count": cluster.file_count,
                "internal_edges": cluster.internal_edges,
                "outgoing_edges": cluster.outgoing_edges,
                "incoming_edges": cluster.incoming_edges,
                "boundary_edges": cluster.boundary_edges(),
            })
        })
        .collect();
    let largest_cluster = clusters
        .iter()
        .filter_map(|cluster| cluster["file_count"].as_u64())
        .max()
        .unwrap_or(0);
    let stats = json!({
        "files": file_count,
        "edges": edge_count,
        "density": (density * 10000.0).round() / 10000.0,
        "clusters": cluster_rows.len(),
        "largest_cluster": largest_cluster,
    });
    let output = match shape {
        "clusters" => json!({
            "shape": "clusters",
            "stats": stats,
            "clusters": clusters,
        }),
        "matrix" => json!({
            "shape": "matrix",
            "stats": stats,
            "clusters": clusters.into_iter().take(10).collect::<Vec<_>>(),
            "matrix": dsm_matrix(&adj, max_files),
        }),
        _ => {
            clusters.truncate(10);
            json!({
                "shape": "stats",
                "stats": stats,
                "clusters": clusters,
            })
        }
    };

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render_dsm_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
}

fn dsm_matrix(adj: &HashMap<String, HashSet<String>>, max_files: usize) -> Value {
    let mut file_edge_counts: Vec<(String, usize)> = adj
        .iter()
        .map(|(f, targets)| {
            let out = targets.len();
            let inc = adj.values().filter(|t| t.contains(f)).count();
            (f.clone(), out + inc)
        })
        .collect();
    file_edge_counts.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    file_edge_counts.truncate(max_files);

    let selected: Vec<String> = file_edge_counts.into_iter().map(|(f, _)| f).collect();
    let short_names: Vec<String> = selected
        .iter()
        .map(|f| {
            f.rfind('/')
                .map_or_else(|| f.clone(), |i| f[i + 1..].to_string())
        })
        .collect();

    let n = selected.len();
    let mut matrix: Vec<Vec<u8>> = vec![vec![0u8; n]; n];
    for (i, src) in selected.iter().enumerate() {
        if let Some(targets) = adj.get(src) {
            for (j, tgt) in selected.iter().enumerate() {
                if i != j && targets.contains(tgt) {
                    matrix[i][j] = 1;
                }
            }
        }
    }

    json!({
        "files": short_names,
        "matrix": matrix,
        "note": format!("Top {} files by edge count shown", n),
    })
}

fn render_dsm_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Design Structure Matrix");
    md.field("shape", render::field_str(value, "shape"));
    if let Some(stats) = value.get("stats") {
        md.field("files", &render::field_i64(stats, "files").to_string());
        md.field("edges", &render::field_i64(stats, "edges").to_string());
        md.field("density", render::field_str(stats, "density"));
        md.field(
            "clusters",
            &render::field_i64(stats, "clusters").to_string(),
        );
        md.field(
            "largest_cluster",
            &render::field_i64(stats, "largest_cluster").to_string(),
        );
    }

    if let Some(clusters) = value.get("clusters").and_then(Value::as_array) {
        md.blank().heading(3, "Top Clusters");
        if clusters.is_empty() {
            md.empty_note("No dependency clusters found.");
        } else {
            for cluster in clusters.iter().take(10) {
                let dir = render::field_str(cluster, "directory");
                let files = render::field_i64(cluster, "file_count");
                let internal = render::field_i64(cluster, "internal_edges");
                let outgoing = render::field_i64(cluster, "outgoing_edges");
                let incoming = render::field_i64(cluster, "incoming_edges");
                let boundary = render::field_i64(cluster, "boundary_edges");
                md.bullet(&format!(
                    "{dir}: {files} files; {internal} internal; {boundary} boundary ({outgoing} out, {incoming} in)"
                ));
            }
        }
    }

    if let Some(matrix) = value.get("matrix") {
        md.blank().heading(3, "Matrix");
        if let Some(note) = matrix.get("note").and_then(Value::as_str) {
            md.field("note", note);
        }
        md.code(
            "json",
            &serde_json::to_string(matrix).unwrap_or_else(|_| "{}".to_string()),
        );
    }

    md.render()
}

/// Handles `tracedecay_test_risk` tool calls.
pub(super) async fn handle_test_risk(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(20, |v| v.min(200) as usize);
    let path_prefix = effective_path(&args, scope_prefix);
    let include_tested = args
        .get("include_tested")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let report =
        crate::graph::health::test_risk::analyze_test_risk(cg, path_prefix, include_tested, limit)
            .await?;
    let output = serde_json::to_value(report).map_err(|err| TraceDecayError::Config {
        message: format!("failed to serialize test risk report: {err}"),
    })?;

    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    ))
}

/// Handles `tracedecay_test_map` tool calls.
pub(super) async fn handle_test_map(
    cg: &TraceDecay,
    args: Value,
    _scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let source_nodes = if let Some(file) = args.get("file").and_then(|v| v.as_str()) {
        cg.get_nodes_by_file(file).await?
    } else if let Some(node_id) = args
        .get("node_id")
        .or(args.get("id"))
        .and_then(|v| v.as_str())
    {
        cg.get_node(node_id).await?.into_iter().collect()
    } else {
        return Err(TraceDecayError::Config {
            message: "provide either 'file' or 'node_id'".to_string(),
        });
    };

    let mut coverage_map: Vec<Value> = Vec::new();
    let mut uncovered: Vec<Value> = Vec::new();
    let mut all_test_files: HashSet<String> = HashSet::new();

    for node in &source_nodes {
        if !node.kind.is_callable_kind() {
            continue;
        }

        let callers = cg.get_callers(&node.id, 3).await?;
        // Batch-check which callers have #[test] annotations (inline test modules).
        let caller_ids: Vec<String> = callers.iter().map(|(n, _)| n.id.clone()).collect();
        let test_annotated = cg.get_test_annotated_node_ids(&caller_ids).await?;
        let test_callers: Vec<Value> = callers
            .iter()
            .filter(|(n, _)| {
                crate::tracedecay::is_test_file(&n.file_path) || test_annotated.contains(&n.id)
            })
            .map(|(n, _)| {
                all_test_files.insert(n.file_path.clone());
                json!({
                    "test_name": n.name,
                    "test_file": n.file_path,
                    "test_line": n.start_line,
                })
            })
            .collect();

        if test_callers.is_empty() {
            uncovered.push(json!({
                "id": node.id,
                "name": node.name,
                "file": node.file_path,
                "line": node.start_line,
            }));
        } else {
            coverage_map.push(json!({
                "source_name": node.name,
                "source_id": node.id,
                "source_file": node.file_path,
                "source_line": node.start_line,
                "tests": test_callers,
            }));
        }
    }

    let mut test_file_list: Vec<String> = all_test_files.into_iter().collect();
    test_file_list.sort();

    let output = json!({
        "covered_symbols": coverage_map.len(),
        "uncovered_symbols": uncovered.len(),
        "test_files": test_file_list,
        "coverage": coverage_map,
        "uncovered": uncovered,
    });

    let touched_files = unique_file_paths(source_nodes.iter().map(|n| n.file_path.as_str()));
    let text = render::finalize(Some(cg.project_root()), &args, &output, || {
        render::generic_md(&output)
    });
    Ok(ToolResult::new(
        json!({"content": [{"type": "text", "text": text}]}),
        touched_files,
    ))
}

fn session_dimension_values(snap: &HealthSnapshot) -> [(&'static str, f64); 6] {
    [
        ("acyclicity", snap.acyclicity),
        ("depth", snap.depth),
        ("equality", snap.equality),
        ("redundancy", snap.redundancy),
        ("modularity", snap.modularity),
        ("coverage_discipline", snap.coverage_discipline),
    ]
}

fn session_dimension_deltas(
    dims_before: &Value,
    snap: &HealthSnapshot,
) -> (serde_json::Map<String, Value>, Vec<String>) {
    let mut dimensions = serde_json::Map::new();
    let mut degraded_dimensions: Vec<String> = vec![];

    for (name, after_val) in session_dimension_values(snap) {
        let before_val = dims_before[name].as_f64().unwrap_or(0.0);
        let dim_delta = after_val - before_val;
        let status = if dim_delta > 0.001 {
            "improved"
        } else if dim_delta < -0.001 {
            degraded_dimensions.push(name.to_string());
            "degraded"
        } else {
            "unchanged"
        };
        dimensions.insert(
            name.to_string(),
            json!({
                "before": (before_val * 10000.0).round() / 10000.0,
                "after": (after_val * 10000.0).round() / 10000.0,
                "delta": (dim_delta * 10000.0).round() / 10000.0,
                "status": status,
            }),
        );
    }

    (dimensions, degraded_dimensions)
}

fn session_tool_result(cg: &TraceDecay, args: &Value, output: &Value) -> ToolResult {
    let text = render::finalize(Some(cg.project_root()), args, output, || {
        render::generic_md(output)
    });
    ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    )
}

/// Handles `tracedecay_session_start` tool calls.
pub(super) async fn handle_session_start(
    cg: &TraceDecay,
    db: &RegisteredGlobalDb,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);
    let delta = compute_health_delta_result(cg, db, None, path_prefix).await?;

    let tracedecay_dir = &cg.store_layout().data_root;
    std::fs::create_dir_all(tracedecay_dir).map_err(|e| {
        crate::errors::TraceDecayError::Config {
            message: format!("failed to create active store data root: {e}"),
        }
    })?;
    let baseline_path = tracedecay_dir.join("session_baseline.json");
    std::fs::write(
        &baseline_path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 2,
            "health_delta_cursor": delta.after_cursor,
        }))
        .unwrap_or_default(),
    )
    .map_err(|e| crate::errors::TraceDecayError::Config {
        message: format!("failed to write session baseline: {e}"),
    })?;

    let output = json!({
        "status": "baseline_saved",
        "quality_signal": delta.after.quality_signal,
        "files_analyzed": delta.after.files_analyzed,
        "health_delta_cursor": delta.after_cursor,
        "deprecated": true,
        "replacement": "tracedecay_health_delta",
    });
    Ok(session_tool_result(cg, &args, &output))
}

/// Handles `tracedecay_session_end` tool calls.
pub(super) async fn handle_session_end(
    cg: &TraceDecay,
    db: &RegisteredGlobalDb,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let tracedecay_dir = &cg.store_layout().data_root;
    let baseline_path = tracedecay_dir.join("session_baseline.json");

    // Check if baseline exists
    if !baseline_path.exists() {
        let output = json!({
            "status": "no_baseline",
            "message": "No session baseline found. Call tracedecay_session_start first.",
        });
        return Ok(session_tool_result(cg, &args, &output));
    }

    let baseline_raw = std::fs::read_to_string(&baseline_path).map_err(|e| {
        crate::errors::TraceDecayError::Config {
            message: format!("failed to read session baseline: {e}"),
        }
    })?;
    let baseline: Value = serde_json::from_str(&baseline_raw).map_err(|e| {
        crate::errors::TraceDecayError::Config {
            message: format!("failed to parse session baseline: {e}"),
        }
    })?;
    if let Some(cursor) = baseline.get("health_delta_cursor").and_then(Value::as_str) {
        let path_prefix = effective_path(&args, scope_prefix);
        let result = compute_health_delta_result(cg, db, Some(cursor), path_prefix).await?;
        let _ = std::fs::remove_file(&baseline_path);
        let degraded_dimensions = result
            .dimensions
            .iter()
            .filter(|(_, value)| value.status == "degraded")
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let dimensions = result
            .dimensions
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    json!({
                        "before": value.before_ppm as f64 / 1_000_000.0,
                        "after": value.after_ppm as f64 / 1_000_000.0,
                        "delta": value.delta_ppm as f64 / 1_000_000.0,
                        "status": value.status,
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let output = json!({
            "pass": result.pass,
            "signal_before": result.before.quality_signal,
            "signal_after": result.after.quality_signal,
            "delta": result.delta,
            "files_analyzed": result.after.files_analyzed,
            "degraded_dimensions": degraded_dimensions,
            "dimensions": dimensions,
            "before_watermark": result.before.watermark,
            "after_watermark": result.after.watermark,
            "coverage": result.coverage,
            "currentness": result.currentness,
            "deprecated": true,
            "replacement": "tracedecay_health_delta",
        });
        return Ok(session_tool_result(cg, &args, &output));
    }

    let signal_before = baseline["quality_signal"].as_u64().unwrap_or(0) as u32;
    let dims_before = &baseline["dimensions"];

    // Recompute current health
    let path_prefix = effective_path(&args, scope_prefix);
    let snap = compute_health_snapshot(cg, path_prefix).await?;

    let _ = std::fs::remove_file(&baseline_path);

    let signal_after = snap.quality_signal;
    let delta = i64::from(signal_after) - i64::from(signal_before);
    let pass = signal_after >= signal_before;

    // Compute per-dimension deltas
    let (dimensions, degraded_dimensions) = session_dimension_deltas(dims_before, &snap);

    let output = json!({
        "pass": pass,
        "signal_before": signal_before,
        "signal_after": signal_after,
        "delta": delta,
        "files_analyzed": snap.files_analyzed,
        "degraded_dimensions": degraded_dimensions,
        "dimensions": dimensions,
    });
    Ok(session_tool_result(cg, &args, &output))
}

#[cfg(test)]
mod health_delta_tests {
    use super::*;

    #[tokio::test]
    async fn requested_doctor_report_is_typed_unavailable_without_reader() {
        let mut value = json!({});

        attach_doctor_report(&mut value, None).await;

        assert_eq!(
            value["doctor_report"],
            json!({
                "kind": "unsupported",
                "table_growth_evidence": [],
            })
        );
    }

    #[tokio::test]
    async fn pinned_health_delta_is_exact_scoped_and_cursor_stable() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let dir = tempfile::tempdir().expect("temporary project");
        std::fs::create_dir_all(dir.path().join("src")).expect("source directory");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn pinned_health() -> bool { true }\n",
        )
        .expect("source fixture");
        let runtime = crate::application::host_admission::HostAdmissionTestRuntimeV1::project(
            crate::storage::default_profile_root().expect("profile root"),
            dir.path(),
            tracedecay_domain::ProjectId::new("project.health-delta").expect("project id"),
        )
        .await
        .expect("registered test runtime");
        let graph = runtime
            .initialize_project_graph_for_test(
                dir.path(),
                crate::tracedecay::TraceDecayOpenOptions::default(),
            )
            .await
            .expect("initialize graph");
        graph.index_all().await.expect("index fixture");
        let observations =
            crate::global_db::tests::harness::RegisteredGlobalDbHarness::open("health-delta").await;
        let db = observations.registered.as_ref();

        let before = compute_health_delta_result(&graph, db, None, Some("src"))
            .await
            .expect("pin before");
        let after = compute_health_delta_result(
            &graph,
            db,
            Some(before.after_cursor.as_str()),
            Some("src"),
        )
        .await
        .expect("compare pinned state");

        assert_eq!(after.before, before.after);
        assert_eq!(after.before_cursor, before.after_cursor);
        assert_eq!(
            health_delta_cursor(&after.before.watermark),
            after.before_cursor
        );
        assert_eq!(
            health_delta_cursor(&after.after.watermark),
            after.after_cursor
        );
        assert_eq!(after.delta, 0);
        assert!(after.pass);
        assert_eq!(after.coverage.eligible, after.coverage.denominator);
        assert_eq!(after.coverage.visited, after.coverage.denominator);
        assert_eq!(after.coverage.completeness, "complete");
        assert_eq!(after.scope.path_prefix.as_deref(), Some("src"));
        assert!(
            after
                .dimensions
                .values()
                .all(|dimension| dimension.status == "unchanged")
        );

        let wrong_scope = compute_health_delta_result(
            &graph,
            db,
            Some(before.after_cursor.as_str()),
            Some("tests"),
        )
        .await
        .expect_err("scope-bound cursor");
        assert!(wrong_scope.to_string().contains("unknown or expired"));
        assert!(!graph.store_layout().data_root.join("health_delta").exists());

        let malformed = format!("{HEALTH_DELTA_CURSOR_PREFIX}{}", "0".repeat(64));
        let rejected = compute_health_delta_result(&graph, db, Some(&malformed), Some("src"))
            .await
            .expect_err("unknown pinned point");
        assert!(rejected.to_string().contains("unknown or expired"));
        graph.checkpoint().await.expect("checkpoint");
        graph.close();
    }
}
