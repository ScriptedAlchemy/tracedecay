//! Health, test risk, sessions, gini, dependency depth, DSM, and test map
//! tool handlers.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::{Value, json};

use crate::errors::{Result, TraceDecayError};
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
    max_chain: usize,
    ideal_chain: usize,
    modularity_components: usize,
    dead_count: usize,
    total_fns: usize,
    skip_coverage_count: usize,
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
        max_chain: depth_result.max_depth,
        ideal_chain: depth_result.ideal_depth,
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

    // Build named_values per metric+scope
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

async fn observation_authority_audit(
    registry: Option<&crate::global_db::RegisteredGlobalDb>,
) -> (Option<bool>, Option<String>) {
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
                Ok(()) => (Some(true), None),
                Err(error) => (Some(false), Some(error.to_string())),
            }
        }
        None => (
            None,
            Some("authoritative global registry is unavailable".to_string()),
        ),
    }
}

/// Registered-runtime implementation of session-ingest health over a
/// registered read snapshot: same sessions/parse-offset queries and the same
/// filesystem backlog accounting.
async fn session_ingest_health_for_provider(
    conn: &impl crate::db::engine::QueryExecutor,
    provider: Option<&str>,
) -> crate::global_db::SessionIngestHealth {
    let mut health = crate::global_db::SessionIngestHealth::default();
    let rows = if let Some(provider) = provider {
        conn.query(
            "SELECT DISTINCT transcript_path FROM sessions
             WHERE provider = ?1
               AND transcript_path IS NOT NULL
               AND transcript_path != ''
             LIMIT 1000",
            crate::db::engine::params![provider],
        )
        .await
    } else {
        conn.query(
            "SELECT DISTINCT transcript_path FROM sessions
             WHERE transcript_path IS NOT NULL AND transcript_path != ''
             LIMIT 1000",
            (),
        )
        .await
    };
    let Ok(mut rows) = rows else {
        return health;
    };
    let mut paths = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(path) = row.get::<String>(0) {
            paths.push(path);
        }
    }
    for path in paths {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        health.tracked_transcripts += 1;
        let (byte_offset, mtime) = parse_offset_for_path(conn, &path).await.unwrap_or_default();
        if mtime > 0 {
            let mtime = mtime as i64;
            health.last_ingest_unix = Some(
                health
                    .last_ingest_unix
                    .map_or(mtime, |prev| prev.max(mtime)),
            );
        }
        let pending = meta.len().saturating_sub(byte_offset);
        if pending > 0 {
            health.pending_transcripts += 1;
            health.pending_bytes = health.pending_bytes.saturating_add(pending);
            health.max_transcript_pending_bytes = health.max_transcript_pending_bytes.max(pending);
        }
    }
    health
}

/// Reads one transcript parse offset through the two-column projection; a
/// column subset stays valid whether or not the current `file_id` column
/// exists.
async fn parse_offset_for_path(
    conn: &impl crate::db::engine::QueryExecutor,
    path: &str,
) -> Option<(u64, u64)> {
    let mut rows = conn
        .query(
            "SELECT byte_offset, mtime FROM parse_offsets WHERE file_path = ?1",
            crate::db::engine::params![path],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    let byte_offset = row.get::<i64>(0).ok()?;
    let mtime = row.get::<i64>(1).ok()?;
    Some((u64::try_from(byte_offset).ok()?, u64::try_from(mtime).ok()?))
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
pub(super) async fn handle_runtime(
    cg: &TraceDecay,
    args: Value,
    registry: Option<&crate::global_db::RegisteredGlobalDb>,
    project_session_db: Option<&crate::global_db::RegisteredGlobalDb>,
) -> Result<ToolResult> {
    let snap = crate::runtime_telemetry::collect(cg).await?;
    let mut value = serde_json::to_value(&snap).unwrap_or_else(|_| json!({}));
    let authority_audit = args
        .get("authority_audit")
        .and_then(Value::as_bool)
        .unwrap_or(false);
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
        if let Some((authority_audit_ok, authority_audit_error)) = authority
            && let Some(database) = value.get_mut("database").and_then(Value::as_object_mut)
        {
            database.insert("authority_audit_ok".to_string(), json!(authority_audit_ok));
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
            Some(db) => match db.read_snapshot().await {
                Ok(snapshot) => {
                    value["cursor_session_ingest"] = serde_json::to_value(
                        session_ingest_health_for_provider(&snapshot, Some("cursor")).await,
                    )
                    .unwrap_or_else(|_| json!({}));
                    value["cursor_session_placeholder_paths"] =
                        json!(literal_workspace_placeholder_transcript_paths(&snapshot, 10).await);
                }
                Err(_) => {
                    value["cursor_session_ingest"] = json!({
                        "status": "unavailable",
                        "message": "project session store snapshot is unavailable",
                    });
                    value["cursor_session_placeholder_paths"] = json!([]);
                }
            },
            None => {
                value["cursor_session_ingest"] = json!({
                    "status": "unavailable",
                    "message": "daemon project session authority is unavailable",
                });
            }
        }
    }
    if let Some(semantic) =
        crate::application::semantic_runtime::project_semantic_application_status(cg.project_root())
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

fn session_baseline_snapshot(snap: &HealthSnapshot) -> Value {
    json!({
        "quality_signal": snap.quality_signal,
        "files_analyzed": snap.files_analyzed,
        "dimensions": {
            "acyclicity": snap.acyclicity,
            "depth": snap.depth,
            "equality": snap.equality,
            "redundancy": snap.redundancy,
            "modularity": snap.modularity,
            "coverage_discipline": snap.coverage_discipline,
        },
        "timestamp": crate::tracedecay::current_timestamp(),
    })
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
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let path_prefix = effective_path(&args, scope_prefix);
    let snap = compute_health_snapshot(cg, path_prefix).await?;

    let baseline = session_baseline_snapshot(&snap);

    // Write baseline to the active project store.
    let tracedecay_dir = &cg.store_layout().data_root;
    std::fs::create_dir_all(tracedecay_dir).map_err(|e| {
        crate::errors::TraceDecayError::Config {
            message: format!("failed to create active store data root: {e}"),
        }
    })?;
    let baseline_path = tracedecay_dir.join("session_baseline.json");
    std::fs::write(
        &baseline_path,
        serde_json::to_string_pretty(&baseline).unwrap_or_default(),
    )
    .map_err(|e| crate::errors::TraceDecayError::Config {
        message: format!("failed to write session baseline: {e}"),
    })?;

    let output = json!({
        "status": "baseline_saved",
        "quality_signal": snap.quality_signal,
        "files_analyzed": snap.files_analyzed,
    });
    Ok(session_tool_result(cg, &args, &output))
}

/// Handles `tracedecay_session_end` tool calls.
pub(super) async fn handle_session_end(
    cg: &TraceDecay,
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

    // Read baseline
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

    let signal_before = baseline["quality_signal"].as_u64().unwrap_or(0) as u32;
    let dims_before = &baseline["dimensions"];

    // Recompute current health
    let path_prefix = effective_path(&args, scope_prefix);
    let snap = compute_health_snapshot(cg, path_prefix).await?;

    // Remove the baseline file
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
