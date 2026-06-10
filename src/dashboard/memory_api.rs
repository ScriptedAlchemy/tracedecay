//! Holographic-memory dashboard API, backed by tokensave's memory store.
//!
//! Port of `plugins/memory/holographic_plus/dashboard/plugin_api.py` (Hermes)
//! onto the project database tables `memory_facts`, `memory_entities`,
//! `memory_fact_entities`, and `memory_banks`. Payload shapes mirror the
//! original routes so the ported UI bundle works unchanged.
//!
//! Differences from the Hermes backend, by design:
//! - Curation is implemented as similarity-based deduplication (no LLM).
//!   `POST /curate` proposes hard-DELETING the lower-trust fact in each
//!   `likely_duplicate` pair; `dry_run=false` applies those deletions.
//! - `POST /curate/apply` is a generic curation-ops endpoint (`delete` /
//!   `merge`) that external planners (e.g. an LLM-backed Hermes wrapper)
//!   can call with their own proposed operations.
//! - There is no fact archive: deletion is permanent (the original
//!   `holographic_plus` soft-archived facts; tokensave does not).
//! - Banks are named after their category directly (no `cat:` prefix).

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::memory_analysis::{
    lexical_overlap, pca_scores, propose_dedup_actions, score_distribution, score_similar_pairs,
    similarity_classification, SimilarityComputation, SIMILARITY_DEFAULT_THRESHOLD,
    SIMILARITY_FACT_CAP, SIMILARITY_PAIR_FLOOR, SIMILARITY_SCORE_MAX, SIMILARITY_SCORE_MIN,
};
use super::util::{coerce_limit, http_detail, like_pattern, query_i64, query_rows};
use super::{CuratePreviewEntry, DashboardState};
use crate::memory::encoding::HolographicEncoder;
use crate::memory::store::MemoryStore;
use crate::memory::types::UpdateFactRequest;

#[derive(Deserialize)]
pub(crate) struct OverviewParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
    graph_limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct ProjectionParams {
    #[serde(default)]
    q: String,
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct SimilarityParams {
    threshold: Option<f64>,
    min_similarity: Option<f64>,
    limit: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct LimitParams {
    limit: Option<i64>,
}

const PROJECTION_POINT_CAP: i64 = 2000;
const SIMILARITY_PAIR_CAP: i64 = 2000;

fn providers_stub() -> Value {
    json!({
        "memory_provider": "tokensave",
        "memory_options": [
            {
                "name": "tokensave",
                "description": "TokenSave holographic memory store (project-local memory_facts)."
            }
        ],
        "context_engine": "tokensave",
        "context_options": [],
        "plugin_context_engine": null,
        "curator_tools": { "enabled": false, "count": 0, "available": 0, "tools": [] },
    })
}

fn coerce_similarity_score(value: Option<f64>, default: f64) -> f64 {
    value
        .filter(|score| score.is_finite())
        .unwrap_or(default)
        .clamp(SIMILARITY_SCORE_MIN, SIMILARITY_SCORE_MAX)
}

async fn fetch_facts(
    state: &DashboardState,
    query: &str,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let q = query.trim();
    if q.is_empty() {
        query_rows(
            &state.mem_conn,
            "SELECT fact_id, content, category, tags, trust_score,
                    retrieval_count, helpful_count, created_at, updated_at,
                    hrr_vector IS NOT NULL AS has_hrr
             FROM memory_facts
             ORDER BY trust_score DESC, updated_at DESC
             LIMIT ?1",
            libsql::params![limit],
        )
        .await
    } else {
        let like = like_pattern(q);
        query_rows(
            &state.mem_conn,
            "SELECT fact_id, content, category, tags, trust_score,
                    retrieval_count, helpful_count, created_at, updated_at,
                    hrr_vector IS NOT NULL AS has_hrr
             FROM memory_facts
             WHERE content LIKE ?1 ESCAPE '\\' OR tags LIKE ?1 ESCAPE '\\'
             ORDER BY trust_score DESC, updated_at DESC
             LIMIT ?2",
            libsql::params![like, limit],
        )
        .await
    }
}

async fn fetch_entities(state: &DashboardState, limit: i64) -> Result<Vec<Value>, String> {
    query_rows(
        &state.mem_conn,
        "SELECT e.entity_id, e.name, e.entity_type, e.aliases, e.created_at,
                COUNT(fe.fact_id) AS fact_count
         FROM memory_entities e
         LEFT JOIN memory_fact_entities fe ON fe.entity_id = e.entity_id
         GROUP BY e.entity_id
         ORDER BY fact_count DESC, e.name ASC
         LIMIT ?1",
        libsql::params![limit],
    )
    .await
}

async fn trust_histogram(state: &DashboardState) -> Vec<Value> {
    let mut buckets: Vec<Value> = (0..10)
        .map(|i| {
            json!({
                "bucket": i,
                "label": format!("{:.1}\u{2013}{:.1}", f64::from(i) / 10.0, f64::from(i + 1) / 10.0),
                "count": 0,
            })
        })
        .collect();
    if let Ok(rows) = query_rows(
        &state.mem_conn,
        "SELECT trust_score FROM memory_facts WHERE trust_score IS NOT NULL",
        (),
    )
    .await
    {
        for row in rows {
            let score = row
                .get("trust_score")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let idx = ((score * 10.0) as usize).min(9);
            if let Some(count) = buckets[idx].get_mut("count") {
                *count = json!(count.as_i64().unwrap_or(0) + 1);
            }
        }
    }
    buckets
}

async fn overview_payload(state: &DashboardState) -> Result<Value, String> {
    let facts_count = query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_facts", ()).await;
    let banks_count = query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_banks", ()).await;

    let categories = query_rows(
        &state.mem_conn,
        "SELECT category, COUNT(*) AS count, AVG(trust_score) AS avg_trust
         FROM memory_facts
         GROUP BY category
         ORDER BY count DESC, category ASC",
        (),
    )
    .await?;

    let category_rows = query_rows(
        &state.mem_conn,
        "SELECT category,
                COUNT(*) AS facts,
                SUM(CASE WHEN hrr_vector IS NOT NULL THEN 1 ELSE 0 END) AS hrr_vectors
         FROM memory_facts
         GROUP BY category
         ORDER BY facts DESC, category ASC",
        (),
    )
    .await?;

    let bank_rows = query_rows(
        &state.mem_conn,
        "SELECT bank_name, fact_count, hrr_dim AS dim, updated_at FROM memory_banks",
        (),
    )
    .await
    .unwrap_or_default();
    let banks_by_name: Map<String, Value> = bank_rows
        .iter()
        .filter_map(|row| {
            let name = row.get("bank_name")?.as_str()?.to_string();
            Some((name, row.clone()))
        })
        .collect();

    // Per-category HRR coverage: tokensave names category banks after the
    // category itself (the Hermes store used a `cat:` prefix).
    let mut hrr_coverage = Vec::new();
    for row in &category_rows {
        let category = row
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("general")
            .to_string();
        let facts = row.get("facts").and_then(Value::as_i64).unwrap_or(0);
        let hrr_vectors = row.get("hrr_vectors").and_then(Value::as_i64).unwrap_or(0);
        let bank = banks_by_name.get(&category);
        let bank_fact_count = bank
            .and_then(|b| b.get("fact_count"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let status = if hrr_vectors == 0 {
            "missing_vectors"
        } else if bank.is_none() {
            "missing_bank"
        } else if bank_fact_count != hrr_vectors {
            "stale_bank"
        } else {
            "ready"
        };
        let coverage = if facts > 0 {
            (hrr_vectors as f64 / facts as f64 * 10_000.0).round() / 10_000.0
        } else {
            0.0
        };
        hrr_coverage.push(json!({
            "category": category,
            "facts": facts,
            "hrr_vectors": hrr_vectors,
            "coverage": coverage,
            "bank_name": category,
            "bank_fact_count": bank_fact_count,
            "dim": bank.and_then(|b| b.get("dim")).cloned().unwrap_or(Value::Null),
            "updated_at": bank.and_then(|b| b.get("updated_at")).cloned().unwrap_or(Value::Null),
            "status": status,
        }));
    }

    let entity_types = query_rows(
        &state.mem_conn,
        "SELECT e.entity_type, COUNT(DISTINCT e.entity_id) AS count
         FROM memory_entities e
         JOIN memory_fact_entities fe ON fe.entity_id = e.entity_id
         GROUP BY e.entity_type
         ORDER BY count DESC, e.entity_type ASC",
        (),
    )
    .await?;
    let entities_count: i64 = entity_types
        .iter()
        .filter_map(|row| row.get("count").and_then(Value::as_i64))
        .sum();

    let memory_banks = query_rows(
        &state.mem_conn,
        "SELECT bank_id, bank_name, hrr_dim AS dim, fact_count, updated_at
         FROM memory_banks
         ORDER BY updated_at DESC
         LIMIT 50",
        (),
    )
    .await?;

    let growth = query_rows(
        &state.mem_conn,
        "WITH bounds AS (
             SELECT date(MAX(created_at), 'unixepoch') AS max_date
             FROM memory_facts
             WHERE created_at > 0
         ),
         daily AS (
             SELECT date(f.created_at, 'unixepoch') AS date, COUNT(*) AS facts
             FROM memory_facts f, bounds
             WHERE f.created_at > 0
               AND bounds.max_date IS NOT NULL
               AND date(f.created_at, 'unixepoch') >= date(bounds.max_date, '-180 days')
             GROUP BY date
         )
         SELECT daily.date,
                daily.facts,
                (
                    SELECT COUNT(*)
                    FROM memory_facts prior
                    WHERE prior.created_at > 0
                      AND date(prior.created_at, 'unixepoch') <= daily.date
                ) AS cumulative_facts
         FROM daily
         ORDER BY daily.date ASC",
        (),
    )
    .await
    .unwrap_or_default();

    Ok(json!({
        "facts": facts_count,
        "entities": entities_count,
        "banks": banks_count,
        "categories": categories,
        "entity_types": entity_types,
        "hrr_coverage": hrr_coverage,
        "memory_banks": memory_banks,
        "trust_histogram": trust_histogram(state).await,
        "growth": growth,
    }))
}

async fn graph_payload(state: &DashboardState, query: &str, limit: i64) -> Result<Value, String> {
    let fact_rows = fetch_facts(state, query, limit).await?;

    let mut nodes: Map<String, Value> = Map::new();
    let mut edges: Vec<Value> = Vec::new();
    let mut fact_ids: Vec<i64> = Vec::new();
    let mut category_counts: Map<String, Value> = Map::new();

    for fact in &fact_rows {
        let fact_id = fact.get("fact_id").and_then(Value::as_i64).unwrap_or(0);
        let category = fact
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("general")
            .to_string();
        let has_hrr = fact.get("has_hrr").and_then(Value::as_i64).unwrap_or(0) != 0;
        fact_ids.push(fact_id);

        let fact_node = format!("fact:{fact_id}");
        let category_node = format!("category:{category}");
        let bank_node = format!("bank:{category}");

        nodes.entry(fact_node.clone()).or_insert_with(|| {
            json!({
                "id": fact_node,
                "kind": "fact",
                "label": format!("#{fact_id}"),
                "fact_id": fact_id,
                "category": category,
                "content": fact.get("content").cloned().unwrap_or(Value::Null),
                "trust_score": fact.get("trust_score").cloned().unwrap_or(Value::Null),
                "retrieval_count": fact.get("retrieval_count").cloned().unwrap_or(Value::Null),
                "helpful_count": fact.get("helpful_count").cloned().unwrap_or(Value::Null),
                "has_hrr": has_hrr,
            })
        });
        nodes.entry(category_node.clone()).or_insert_with(|| {
            json!({ "id": category_node, "kind": "category", "label": category, "category": category })
        });
        edges.push(json!({ "source": category_node, "target": fact_node, "kind": "contains" }));
        if has_hrr {
            nodes.entry(bank_node.clone()).or_insert_with(|| {
                json!({ "id": bank_node, "kind": "bank", "label": category, "category": category })
            });
            edges.push(json!({ "source": bank_node, "target": fact_node, "kind": "bundles" }));
        }

        let count = category_counts
            .get(&category)
            .and_then(Value::as_i64)
            .unwrap_or(0);
        category_counts.insert(category, json!(count + 1));
    }

    if !fact_ids.is_empty() {
        let placeholders = fact_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT fe.fact_id, e.entity_id, e.name, e.entity_type
             FROM memory_fact_entities fe
             JOIN memory_entities e ON e.entity_id = fe.entity_id
             WHERE fe.fact_id IN ({placeholders})
             ORDER BY e.name ASC"
        );
        let params: Vec<libsql::Value> = fact_ids
            .iter()
            .map(|id| libsql::Value::Integer(*id))
            .collect();
        for row in query_rows(&state.mem_conn, &sql, params).await? {
            let entity_id = row.get("entity_id").and_then(Value::as_i64).unwrap_or(0);
            let fact_id = row.get("fact_id").and_then(Value::as_i64).unwrap_or(0);
            let entity_node = format!("entity:{entity_id}");
            let fact_node = format!("fact:{fact_id}");
            nodes.entry(entity_node.clone()).or_insert_with(|| {
                json!({
                    "id": entity_node,
                    "kind": "entity",
                    "label": row.get("name").cloned().unwrap_or(Value::Null),
                    "entity_id": entity_id,
                    "entity_type": row.get("entity_type").cloned().unwrap_or(Value::Null),
                })
            });
            edges.push(json!({ "source": fact_node, "target": entity_node, "kind": "mentions" }));
        }
    }

    let bank_rows = query_rows(
        &state.mem_conn,
        "SELECT bank_name, hrr_dim AS dim, fact_count, updated_at
         FROM memory_banks
         ORDER BY fact_count DESC, bank_name ASC
         LIMIT 50",
        (),
    )
    .await
    .unwrap_or_default();
    for row in bank_rows {
        let Some(bank_name) = row.get("bank_name").and_then(Value::as_str) else {
            continue;
        };
        let category = bank_name.to_string();
        let bank_node_id = format!("bank:{bank_name}");
        let category_node_id = format!("category:{category}");
        if let Some(existing) = nodes.get_mut(&bank_node_id) {
            if let Some(obj) = existing.as_object_mut() {
                obj.insert("dim".into(), row.get("dim").cloned().unwrap_or(Value::Null));
                obj.insert(
                    "fact_count".into(),
                    row.get("fact_count").cloned().unwrap_or(Value::Null),
                );
                obj.insert(
                    "updated_at".into(),
                    row.get("updated_at").cloned().unwrap_or(Value::Null),
                );
            }
        } else if nodes.contains_key(&category_node_id) {
            nodes.insert(
                bank_node_id.clone(),
                json!({
                    "id": bank_node_id,
                    "kind": "bank",
                    "label": bank_name,
                    "category": category,
                    "dim": row.get("dim").cloned().unwrap_or(Value::Null),
                    "fact_count": row.get("fact_count").cloned().unwrap_or(Value::Null),
                    "updated_at": row.get("updated_at").cloned().unwrap_or(Value::Null),
                }),
            );
        }
        if nodes.contains_key(&category_node_id) && nodes.contains_key(&bank_node_id) {
            edges.push(
                json!({ "source": category_node_id, "target": bank_node_id, "kind": "bank" }),
            );
        }
    }

    for (category, count) in &category_counts {
        if let Some(node) = nodes.get_mut(&format!("category:{category}")) {
            if let Some(obj) = node.as_object_mut() {
                obj.insert("fact_count".into(), count.clone());
            }
        }
    }

    Ok(json!({
        "nodes": nodes.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        "edges": edges,
    }))
}

/// `GET /api/plugins/holographic/` — overview + facts + entities + graph.
pub(crate) async fn overview(
    State(state): State<DashboardState>,
    Query(params): Query<OverviewParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 25, 100);
    let graph_limit = coerce_limit(params.graph_limit.or(Some(limit)), limit, 1000);

    let mut obj = Map::new();
    obj.insert("path".into(), json!(state.mem_db_path));
    obj.insert("exists".into(), json!(true));
    obj.insert("overview".into(), Value::Null);
    obj.insert("facts".into(), json!([]));
    obj.insert("entities".into(), json!([]));
    obj.insert("graph".into(), json!({ "nodes": [], "edges": [] }));
    obj.insert("error".into(), json!(""));
    match overview_payload(&state).await {
        Ok(payload) => {
            obj.insert("overview".into(), payload);
        }
        Err(e) => {
            obj.insert("error".into(), json!(e));
        }
    }
    if let Ok(facts) = fetch_facts(&state, &params.q, limit).await {
        obj.insert("facts".into(), json!(facts));
    }
    if let Ok(entities) = fetch_entities(&state, limit).await {
        obj.insert("entities".into(), json!(entities));
    }
    if let Ok(graph) = graph_payload(&state, &params.q, graph_limit).await {
        obj.insert("graph".into(), graph);
    }
    let holographic = Value::Object(obj);

    Json(json!({
        "providers": providers_stub(),
        "query": params.q,
        "limit": limit,
        "holographic": holographic,
    }))
}

/// Facts that have an HRR vector, with the decoded phase vector.
async fn vector_facts(
    state: &DashboardState,
    query: &str,
    limit: i64,
) -> Result<Vec<(Value, Vec<f64>)>, String> {
    let q = query.trim();
    let (sql, params): (String, Vec<libsql::Value>) = if q.is_empty() {
        (
            "SELECT f.fact_id, f.content, f.category, f.trust_score, f.retrieval_count,
                    f.hrr_vector, b.bank_id, b.bank_name, COUNT(fe.entity_id) AS entity_count
             FROM memory_facts f
             LEFT JOIN memory_banks b ON b.bank_name = f.category
             LEFT JOIN memory_fact_entities fe ON fe.fact_id = f.fact_id
             WHERE f.hrr_vector IS NOT NULL
             GROUP BY f.fact_id
             ORDER BY f.trust_score DESC, f.updated_at DESC
             LIMIT ?1"
                .to_string(),
            vec![libsql::Value::Integer(limit)],
        )
    } else {
        (
            "SELECT f.fact_id, f.content, f.category, f.trust_score, f.retrieval_count,
                    f.hrr_vector, b.bank_id, b.bank_name, COUNT(fe.entity_id) AS entity_count
             FROM memory_facts f
             LEFT JOIN memory_banks b ON b.bank_name = f.category
             LEFT JOIN memory_fact_entities fe ON fe.fact_id = f.fact_id
             WHERE f.hrr_vector IS NOT NULL
               AND (f.content LIKE ?1 ESCAPE '\\' OR f.tags LIKE ?1 ESCAPE '\\')
             GROUP BY f.fact_id
             ORDER BY f.trust_score DESC, f.updated_at DESC
             LIMIT ?2"
                .to_string(),
            vec![
                libsql::Value::Text(like_pattern(q)),
                libsql::Value::Integer(limit),
            ],
        )
    };

    let mut rows = state
        .mem_conn
        .query(&sql, params)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| e.to_string())? {
        let fact_id: i64 = row.get(0).unwrap_or(0);
        let content: String = row.get(1).unwrap_or_default();
        let category: String = row.get(2).unwrap_or_else(|_| "general".to_string());
        let trust_score: f64 = row.get(3).unwrap_or(0.0);
        let retrieval_count: i64 = row.get(4).unwrap_or(0);
        let Ok(libsql::Value::Blob(bytes)) = row.get_value(5) else {
            continue;
        };
        let Ok(vector) = HolographicEncoder::deserialize(&bytes) else {
            continue;
        };
        // Skip empty or corrupted blobs: NaN/inf phases would propagate
        // through PCA and similarity math and serialize as null coordinates.
        if vector.is_empty() || vector.iter().any(|v| !v.is_finite()) {
            continue;
        }
        let bank_id = match row.get_value(6) {
            Ok(libsql::Value::Integer(id)) => json!(id),
            _ => Value::Null,
        };
        let bank_name = match row.get_value(7) {
            Ok(libsql::Value::Text(name)) => json!(name),
            _ => Value::Null,
        };
        let entity_count: i64 = row.get(8).unwrap_or(0);
        out.push((
            json!({
                "fact_id": fact_id,
                "content": content,
                "category": category,
                "trust_score": trust_score,
                "retrieval_count": retrieval_count,
                "bank_id": bank_id,
                "bank_name": bank_name,
                "entity_count": entity_count,
                "connection_count": entity_count,
            }),
            vector,
        ));
    }
    Ok(out)
}

/// `GET /api/plugins/holographic/projection` — 2D PCA of phase vectors,
/// embedded as `[cos(p), sin(p)]` so wrapped phases compare correctly.
pub(crate) async fn projection(
    State(state): State<DashboardState>,
    Query(params): Query<ProjectionParams>,
) -> Json<Value> {
    let limit = coerce_limit(params.limit, 25, PROJECTION_POINT_CAP);
    let mut obj = Map::new();
    obj.insert("exists".into(), json!(true));
    obj.insert("dim".into(), json!(0));
    obj.insert("limit".into(), json!(limit));
    obj.insert("method".into(), json!("none"));
    obj.insert("points".into(), json!([]));
    obj.insert("error".into(), json!(""));

    let rows = match vector_facts(&state, &params.q, limit).await {
        Ok(rows) => rows,
        Err(e) => {
            obj.insert("error".into(), json!(e));
            return Json(Value::Object(obj));
        }
    };
    let dim = rows.iter().map(|(_, v)| v.len()).next().unwrap_or(0);
    let rows: Vec<_> = rows.into_iter().filter(|(_, v)| v.len() == dim).collect();
    obj.insert("dim".into(), json!(dim));

    let point = |meta: &Value, x: f64, y: f64| {
        json!({
            "fact_id": meta.get("fact_id").cloned().unwrap_or(json!(0)),
            "x": (x * 1e6).round() / 1e6,
            "y": (y * 1e6).round() / 1e6,
            "category": meta.get("category").cloned().unwrap_or(json!("general")),
            "content": meta.get("content").and_then(Value::as_str).map(|s| s.chars().take(200).collect::<String>()).unwrap_or_default(),
            "trust_score": meta.get("trust_score").cloned().unwrap_or(json!(0.0)),
            "retrieval_count": meta.get("retrieval_count").cloned().unwrap_or(json!(0)),
            "bank_id": meta.get("bank_id").cloned().unwrap_or(Value::Null),
            "bank_name": meta.get("bank_name").cloned().unwrap_or(Value::Null),
            "entity_count": meta.get("entity_count").cloned().unwrap_or(json!(0)),
            "connection_count": meta.get("connection_count").cloned().unwrap_or(json!(0)),
        })
    };

    if rows.len() < 2 {
        if let Some((meta, _)) = rows.first() {
            obj.insert("points".into(), json!([point(meta, 0.0, 0.0)]));
        }
        return Json(Value::Object(obj));
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
        Some(scores) => {
            obj.insert("method".into(), json!("pca"));
            let points: Vec<Value> = rows
                .iter()
                .zip(&scores)
                .map(|((meta, _), s)| point(meta, s[0], s[1]))
                .collect();
            obj.insert("points".into(), json!(points));
        }
        None => {
            obj.insert("error".into(), json!("projection failed"));
        }
    }
    Json(Value::Object(obj))
}

/// Fingerprint of the vectored-fact state, used as the similarity-cache key.
/// Deletes change count/sum, inserts change count, content edits bump
/// `updated_at` — any of which invalidates the cached pair computation.
async fn vector_state_fingerprint(state: &DashboardState) -> Result<(i64, i64, i64), String> {
    let mut rows = state
        .mem_conn
        .query(
            "SELECT COUNT(*), COALESCE(MAX(updated_at), 0), COALESCE(SUM(fact_id), 0)
             FROM memory_facts
             WHERE hrr_vector IS NOT NULL",
            (),
        )
        .await
        .map_err(|e| e.to_string())?;
    let Some(row) = rows.next().await.map_err(|e| e.to_string())? else {
        return Ok((0, 0, 0));
    };
    Ok((
        row.get(0).unwrap_or(0),
        row.get(1).unwrap_or(0),
        row.get(2).unwrap_or(0),
    ))
}

/// Process-wide cache of the last pairwise-similarity computation per project
/// DB. The O(n²·d) scoring runs on the blocking pool (never inline on the
/// async runtime) and is keyed by [`vector_state_fingerprint`], so repeated
/// threshold tweaks from the UI slider reuse the same computed pair set.
static SIMILARITY_CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<SimilarityComputation>>>> =
    OnceLock::new();

async fn similarity_computation(
    state: &DashboardState,
) -> Result<Arc<SimilarityComputation>, String> {
    let key = vector_state_fingerprint(state).await?;
    let cache = SIMILARITY_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    // The lock is held across the computation so concurrent requests do not
    // burn the blocking pool computing the same pair set twice.
    let mut guard = cache.lock().await;
    if let Some(existing) = guard.get(&state.mem_db_path) {
        if existing.key == key {
            return Ok(existing.clone());
        }
    }

    let rows = vector_facts(state, "", SIMILARITY_FACT_CAP).await?;
    let computed = tokio::task::spawn_blocking(move || {
        let dim = rows.iter().map(|(_, v)| v.len()).next().unwrap_or(0);
        let decoded: Vec<_> = rows.into_iter().filter(|(_, v)| v.len() == dim).collect();
        let scored = if decoded.len() < 2 || dim == 0 {
            Vec::new()
        } else {
            score_similar_pairs(&decoded, SIMILARITY_PAIR_FLOOR)
        };
        let facts: Vec<Value> = decoded.into_iter().map(|(meta, _)| meta).collect();
        SimilarityComputation {
            key,
            dim,
            facts,
            scored,
        }
    })
    .await
    .map_err(|e| format!("similarity computation task failed: {e}"))?;

    let arc = Arc::new(computed);
    guard.insert(state.mem_db_path.clone(), arc.clone());
    Ok(arc)
}

/// `GET /api/plugins/holographic/similarity` — pairwise phase-cosine
/// similarity (`mean(cos(p_i − p_j))`) over all vectored facts.
pub(crate) async fn similarity(
    State(state): State<DashboardState>,
    Query(params): Query<SimilarityParams>,
) -> Json<Value> {
    let threshold = coerce_similarity_score(params.threshold, SIMILARITY_DEFAULT_THRESHOLD);
    let min_similarity = coerce_similarity_score(params.min_similarity, threshold);
    let pair_cap = coerce_limit(params.limit, 25, SIMILARITY_PAIR_CAP) as usize;
    let mut obj = Map::new();
    obj.insert("exists".into(), json!(true));
    obj.insert("dim".into(), json!(0));
    obj.insert("count".into(), json!(0));
    obj.insert("limit".into(), json!(pair_cap));
    obj.insert("threshold".into(), json!(threshold));
    obj.insert("min_similarity".into(), json!(min_similarity));
    obj.insert("total_pairs".into(), json!(0));
    obj.insert("score_distribution".into(), score_distribution(&[]));
    obj.insert("pairs".into(), json!([]));
    obj.insert("error".into(), json!(""));

    let computation = match similarity_computation(&state).await {
        Ok(computation) => computation,
        Err(e) => {
            obj.insert("error".into(), json!(e));
            return Json(Value::Object(obj));
        }
    };
    obj.insert("dim".into(), json!(computation.dim));
    obj.insert("count".into(), json!(computation.facts.len()));
    obj.insert("total_pairs".into(), json!(computation.scored.len()));
    obj.insert(
        "score_distribution".into(),
        score_distribution(&computation.scored),
    );
    if computation.facts.len() < 2 || computation.dim == 0 {
        return Json(Value::Object(obj));
    }

    // The cached pair set is pre-sorted descending across all finite scores;
    // per-request filtering is a cheap prefix scan.
    let scored: Vec<(f64, usize, usize)> = computation
        .scored
        .iter()
        .take_while(|(sim, _, _)| *sim >= min_similarity)
        .take(pair_cap)
        .copied()
        .collect();

    let pairs: Vec<Value> = scored
        .into_iter()
        .map(|(sim, i, j)| {
            let a = &computation.facts[i];
            let b = &computation.facts[j];
            let a_content = a.get("content").and_then(Value::as_str).unwrap_or("");
            let b_content = b.get("content").and_then(Value::as_str).unwrap_or("");
            let (overlap, token_overlap, overlap_coefficient) =
                lexical_overlap(a_content, b_content);
            let mut pair = json!({
                "a_id": a.get("fact_id").cloned().unwrap_or(json!(0)),
                "b_id": b.get("fact_id").cloned().unwrap_or(json!(0)),
                "a_content": a_content.chars().take(200).collect::<String>(),
                "b_content": b_content.chars().take(200).collect::<String>(),
                "a_category": a.get("category").cloned().unwrap_or(json!("general")),
                "b_category": b.get("category").cloned().unwrap_or(json!("general")),
                "similarity": sim,
                "classification": similarity_classification(sim, token_overlap, overlap_coefficient),
            });
            if let (Some(obj), Some(extra)) = (pair.as_object_mut(), overlap.as_object()) {
                for (k, v) in extra {
                    obj.insert(k.clone(), v.clone());
                }
            }
            pair
        })
        .collect();
    obj.insert("pairs".into(), json!(pairs));
    Json(Value::Object(obj))
}

/// `GET /api/plugins/holographic/curation/status` — similarity-dedup curator status.
pub(crate) async fn curation_status(State(state): State<DashboardState>) -> Json<Value> {
    let preview = state.curate_preview.read().await;
    let (last_preview_at, last_preview_summary) = match preview.as_ref() {
        Some(entry) => (
            Value::String(entry.saved_at.clone()),
            Value::String(format!(
                "{} duplicate fact(s) flagged for deletion",
                entry
                    .report
                    .get("counts")
                    .and_then(|c| c.get("delete"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
            )),
        ),
        None => (Value::Null, Value::Null),
    };
    Json(json!({
        "provider": "tokensave",
        "state": {
            "paused": false,
            "last_run_at": null,
            "run_count": 0,
            "last_run_summary": null,
            "last_run_id": null,
            "last_preview_at": last_preview_at,
            "last_preview_summary": last_preview_summary,
            "last_preview_run_id": null,
        },
        "config": {
            "enabled": true,
            "interval_hours": null,
            "min_idle_hours": null,
            "mode": "similarity_dedup",
            "dry_run_first": true,
        },
        "snapshots": [],
    }))
}

/// `GET /api/plugins/holographic/curation/activity` — no live event stream.
pub(crate) async fn curation_activity(Query(params): Query<LimitParams>) -> Json<Value> {
    let limit = coerce_limit(params.limit, 100, 300);
    Json(json!({ "events": [], "count": 0, "limit": limit, "error": "" }))
}

/// `GET /api/plugins/holographic/curation/preview` — returns the last saved
/// dry-run preview, or null if none has been run this server session.
pub(crate) async fn curation_preview(State(state): State<DashboardState>) -> Json<Value> {
    let preview = state.curate_preview.read().await;
    match preview.as_ref() {
        None => Json(json!({
            "report": null,
            "saved_at": null,
            "stale": false,
            "stale_reason": "",
            "error": "",
        })),
        Some(entry) => {
            let report = entry.report.clone();
            let saved_at = entry.saved_at.clone();
            let active_facts_at_save = entry.active_facts_at_save;
            drop(preview);
            let current_active =
                query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_facts", ()).await;
            let stale = current_active != active_facts_at_save;
            let stale_reason = if stale {
                "Memory store changed since this preview was generated."
            } else {
                ""
            };
            Json(json!({
                "report": report,
                "saved_at": saved_at,
                "stale": stale,
                "stale_reason": stale_reason,
                "error": "",
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// Similarity-based deduplication curation (hard-delete semantics)
// ---------------------------------------------------------------------------

/// Build a deduplication plan from the cached similarity computation.
///
/// Returns (actions, counts, total) where actions is the list of proposed
/// `delete` operations for `likely_duplicate` pairs.
async fn build_delete_plan(
    state: &DashboardState,
) -> Result<(Vec<Value>, Map<String, Value>, i64), String> {
    let total = query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_facts", ()).await;
    let computation = similarity_computation(state).await?;
    if computation.facts.len() < 2 || computation.dim == 0 {
        return Ok((Vec::new(), Map::new(), total));
    }

    let planner_pairs: Vec<(f64, usize, usize)> = computation
        .scored
        .iter()
        .take_while(|(sim, _, _)| *sim >= SIMILARITY_DEFAULT_THRESHOLD)
        .copied()
        .collect();
    let actions = propose_dedup_actions(&computation.facts, &planner_pairs);

    let mut counts = Map::new();
    if !actions.is_empty() {
        counts.insert("delete".to_string(), json!(actions.len()));
    }
    Ok((actions, counts, total))
}

/// Hard-deletes one fact through the canonical store path (transactional
/// delete + FK-cascaded entity links + FTS trigger + bank dirty-marking).
async fn delete_fact(state: &DashboardState, fact_id: i64) -> Result<bool, String> {
    let store = MemoryStore::new(&state.mem_conn);
    store.remove_fact(fact_id).await.map_err(|e| e.to_string())
}

#[derive(Deserialize, Default)]
pub(crate) struct CurateBody {
    #[serde(default = "default_dry_run")]
    dry_run: bool,
}

fn default_dry_run() -> bool {
    true
}

/// `POST /api/plugins/holographic/curate` — similarity-based deduplication
/// curation. `dry_run=true` (default) returns the proposed plan without
/// mutating; `dry_run=false` applies the plan by hard-DELETING duplicate
/// losers (no archive — deletion is permanent).
pub(crate) async fn curate(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<CurateBody>>,
) -> (StatusCode, Json<Value>) {
    let dry_run = body.is_none_or(|b| b.dry_run);

    let (actions, counts, total) = match build_delete_plan(&state).await {
        Ok(result) => result,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(http_detail(&format!("Curation analysis failed: {e}"))),
            );
        }
    };

    let report = json!({
        "ran": true,
        "dry_run": dry_run,
        "actions": actions,
        "counts": counts,
        "applied_counts": if dry_run { Value::Null } else { json!(counts.clone()) },
        "llm_calls": 0,
        "coverage": {
            "scanned": total,
            "active_total": total,
            "due_remaining": 0,
        },
        "provider": "tokensave",
        "mode": "similarity_dedup",
    });

    if dry_run {
        let saved_at = chrono_now_iso();
        let entry = CuratePreviewEntry {
            report: report.clone(),
            saved_at,
            active_facts_at_save: total,
        };
        *state.curate_preview.write().await = Some(entry);
        return (StatusCode::OK, Json(report));
    }

    // Apply: hard-delete each proposed loser fact via the canonical store path.
    let mut applied = 0i64;
    let mut skipped = 0i64;
    if let Value::Array(ref action_list) = report["actions"] {
        for action in action_list {
            let Some(fact_id) = action.get("fact_id").and_then(Value::as_i64) else {
                skipped += 1;
                continue;
            };
            match delete_fact(&state, fact_id).await {
                Ok(true) => applied += 1,
                Ok(false) | Err(_) => skipped += 1,
            }
        }
    }

    // Clear the saved preview since we've now applied changes.
    *state.curate_preview.write().await = None;

    let mut applied_counts = Map::new();
    if applied > 0 {
        applied_counts.insert("delete".to_string(), json!(applied));
    }
    let applied_report = json!({
        "ran": true,
        "dry_run": false,
        "actions": report["actions"],
        "counts": report["counts"],
        "applied_counts": applied_counts,
        "skipped_actions": skipped,
        "llm_calls": 0,
        "coverage": report["coverage"],
        "provider": "tokensave",
        "mode": "similarity_dedup",
    });

    (StatusCode::OK, Json(applied_report))
}

// ---------------------------------------------------------------------------
// Generic curation-ops apply API
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct CurateApplyBody {
    ops: Vec<Value>,
}

/// Applies one `delete` op; returns the per-op result object.
async fn apply_delete_op(state: &DashboardState, op: &Value) -> (Value, bool) {
    let Some(fact_id) = op.get("fact_id").and_then(Value::as_i64) else {
        return (
            json!({ "op": "delete", "status": "error", "error": "missing or invalid fact_id" }),
            false,
        );
    };
    let reason = op.get("reason").and_then(Value::as_str).unwrap_or("");
    match delete_fact(state, fact_id).await {
        Ok(true) => (
            json!({ "op": "delete", "fact_id": fact_id, "reason": reason, "status": "deleted" }),
            true,
        ),
        Ok(false) => (
            json!({
                "op": "delete",
                "fact_id": fact_id,
                "status": "error",
                "error": format!("fact {fact_id} not found"),
            }),
            false,
        ),
        Err(e) => (
            json!({
                "op": "delete",
                "fact_id": fact_id,
                "status": "error",
                "error": e,
            }),
            false,
        ),
    }
}

/// Applies one `merge` op: optionally rewrites the winner's content, then
/// hard-deletes the losers. Returns the per-op result object.
async fn apply_merge_op(state: &DashboardState, op: &Value) -> (Value, bool) {
    let Some(winner_id) = op.get("winner_id").and_then(Value::as_i64) else {
        return (
            json!({ "op": "merge", "status": "error", "error": "missing or invalid winner_id" }),
            false,
        );
    };
    let Some(loser_ids) = op.get("loser_ids").and_then(Value::as_array) else {
        return (
            json!({
                "op": "merge",
                "winner_id": winner_id,
                "status": "error",
                "error": "missing or invalid loser_ids",
            }),
            false,
        );
    };
    let loser_ids: Vec<i64> = loser_ids.iter().filter_map(Value::as_i64).collect();

    let store = MemoryStore::new(&state.mem_conn);

    // Validate the winner exists before touching anything.
    match store.get_fact(winner_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                json!({
                    "op": "merge",
                    "winner_id": winner_id,
                    "status": "error",
                    "error": format!("winner fact {winner_id} not found"),
                }),
                false,
            );
        }
        Err(e) => {
            return (
                json!({
                    "op": "merge",
                    "winner_id": winner_id,
                    "status": "error",
                    "error": e.to_string(),
                }),
                false,
            );
        }
    }

    // Optionally rewrite the winner's content (re-encodes HRR + entities).
    let merged_content = op
        .get("merged_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mut content_updated = false;
    if let Some(content) = merged_content {
        let request = UpdateFactRequest {
            fact_id: winner_id,
            content: Some(content.to_string()),
            category: None,
            tags: None,
            entities: None,
            trust: None,
            source: None,
            metadata: None,
        };
        if let Err(e) = store.update_fact(request).await {
            return (
                json!({
                    "op": "merge",
                    "winner_id": winner_id,
                    "status": "error",
                    "error": format!("failed to update winner content: {e}"),
                }),
                false,
            );
        }
        content_updated = true;
    }

    // Hard-delete each loser (the winner itself is never deletable here).
    let mut deleted: Vec<i64> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for loser_id in loser_ids {
        if loser_id == winner_id {
            failed.push(json!({ "fact_id": loser_id, "error": "loser equals winner" }));
            continue;
        }
        match delete_fact(state, loser_id).await {
            Ok(true) => deleted.push(loser_id),
            Ok(false) => {
                failed.push(json!({ "fact_id": loser_id, "error": "not found" }));
            }
            Err(e) => {
                failed.push(json!({ "fact_id": loser_id, "error": e }));
            }
        }
    }

    let ok = failed.is_empty();
    (
        json!({
            "op": "merge",
            "winner_id": winner_id,
            "content_updated": content_updated,
            "deleted_loser_ids": deleted,
            "failed_losers": failed,
            "status": if ok { "merged" } else { "error" },
        }),
        ok,
    )
}

/// `POST /api/plugins/holographic/curate/apply` — generic curation-ops apply
/// endpoint. Body: `{"ops": [...]}` where each op is one of:
///
/// - `{"op": "delete", "fact_id": <id>, "reason": <string?>}` — hard-deletes
///   the fact (entity links cascade, FTS rows drop via trigger).
/// - `{"op": "merge", "winner_id": <id>, "loser_ids": [<id>...],
///   "merged_content": <string?>}` — optionally rewrites the winner's content
///   with `merged_content`, then hard-deletes the losers.
///
/// Per-op failures are reported in `results` (status stays 200); the request
/// only fails wholesale on a malformed body. External planners (e.g. the
/// LLM-backed Hermes wrapper) build against this contract.
pub(crate) async fn curate_apply(
    State(state): State<DashboardState>,
    body: Option<axum::extract::Json<CurateApplyBody>>,
) -> (StatusCode, Json<Value>) {
    let Some(axum::extract::Json(body)) = body else {
        return (
            StatusCode::BAD_REQUEST,
            Json(http_detail("Request body must be JSON: {\"ops\": [...]}")),
        );
    };

    let mut results: Vec<Value> = Vec::with_capacity(body.ops.len());
    let mut deleted = 0i64;
    let mut merged = 0i64;
    let mut errors = 0i64;

    for op in &body.ops {
        let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
        let (result, ok) = match kind {
            "delete" => apply_delete_op(&state, op).await,
            "merge" => apply_merge_op(&state, op).await,
            other => (
                json!({
                    "op": other,
                    "status": "error",
                    "error": format!("unsupported op '{other}' (expected 'delete' or 'merge')"),
                }),
                false,
            ),
        };
        if ok {
            match kind {
                "delete" => deleted += 1,
                "merge" => merged += 1,
                _ => {}
            }
        } else {
            errors += 1;
        }
        results.push(result);
    }

    // Mutations invalidate any saved similarity-dedup preview.
    if deleted > 0 || merged > 0 {
        *state.curate_preview.write().await = None;
    }

    (
        StatusCode::OK,
        Json(json!({
            "results": results,
            "counts": { "deleted": deleted, "merged": merged, "errors": errors },
        })),
    )
}

/// Returns the current UTC time as an ISO 8601 string.
fn chrono_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Format as yyyy-mm-ddThh:mm:ssZ using manual epoch arithmetic.
    let mut rem = secs;
    let sec = rem % 60;
    rem /= 60;
    let min = rem % 60;
    rem /= 60;
    let hour = rem % 24;
    rem /= 24;
    // Days since 1970-01-01 → Gregorian calendar.
    let (year, month, day) = days_to_ymd(rem);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // Gregorian calendar algorithm (Fliegel & Van Flandern, truncated).
    let jd = days + 2_440_588; // Julian Day Number for 1970-01-01 = 2440588
    let l_a = jd + 68_569;
    let n = 4 * l_a / 146_097;
    let l_b = l_a - (146_097 * n).div_ceil(4);
    let ii = 4_000 * (l_b + 1) / 1_461_001;
    let l_c = l_b - 1_461 * ii / 4 + 31;
    let jj = 80 * l_c / 2_447;
    let d = (l_c - 2_447 * jj / 80) as u32;
    let l_d = jj / 11;
    let m = (jj + 2 - 12 * l_d) as u32;
    let y = (100 * (n - 49) + ii + l_d) as u32;
    (y, m, d)
}
