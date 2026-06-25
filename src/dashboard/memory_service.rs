use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use serde_json::{json, Map, Value};

use super::memory_analysis::{
    build_similarity_computation, pca_scores, propose_dedup_actions, propose_hygiene_candidates,
    score_distribution, score_similar_pairs, SimilarityComputation, SIMILARITY_DEFAULT_THRESHOLD,
    SIMILARITY_FACT_CAP, SIMILARITY_PAIR_FLOOR, SIMILARITY_SCORE_MAX, SIMILARITY_SCORE_MIN,
};
use super::memory_queries::{self, VectorStateFingerprint};
use super::{CuratePreviewEntry, DashboardState};
use crate::memory::store::MemoryStore;
use crate::sessions::lcm::{LcmGrepSort, LcmScope};

const PROJECTION_POINT_CAP: i64 = 2000;

pub(crate) fn projection_point_cap() -> i64 {
    PROJECTION_POINT_CAP
}

pub(crate) fn providers_payload() -> Value {
    json!({
        "memory_provider": "tracedecay",
        "memory_options": [
            {
                "name": "tracedecay",
                "description": "TraceDecay holographic memory store (resolved project memory_facts)."
            }
        ],
        "context_engine": "tracedecay",
        "context_options": [],
        "plugin_context_engine": null,
        "curator_tools": { "enabled": false, "count": 0, "available": 0, "tools": [] },
    })
}

pub(crate) fn coerce_similarity_score(value: Option<f64>, default: f64) -> f64 {
    value
        .filter(|score| score.is_finite())
        .unwrap_or(default)
        .clamp(SIMILARITY_SCORE_MIN, SIMILARITY_SCORE_MAX)
}

pub(crate) async fn fetch_facts(
    state: &DashboardState,
    query: &str,
    limit: i64,
) -> Result<Vec<Value>, String> {
    memory_queries::fact_rows(state, query, limit).await
}

pub(crate) async fn fetch_entities(
    state: &DashboardState,
    limit: i64,
) -> Result<Vec<Value>, String> {
    memory_queries::entity_rows(state, limit).await
}

async fn trust_histogram(state: &DashboardState) -> Vec<Value> {
    let Ok(rows) = memory_queries::trust_histogram_rows(state).await else {
        return Vec::new();
    };
    if rows.is_empty() {
        return Vec::new();
    }

    let mut buckets: Vec<Value> = (0..10)
        .map(|i| {
            json!({
                "bucket": i,
                "label": format!("{:.1}\u{2013}{:.1}", f64::from(i) / 10.0, f64::from(i + 1) / 10.0),
                "count": 0,
            })
        })
        .collect();
    for row in rows {
        let idx = row
            .get("bucket")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 9) as usize;
        let added = row.get("count").and_then(Value::as_i64).unwrap_or(0);
        if let Some(count) = buckets[idx].get_mut("count") {
            *count = json!(count.as_i64().unwrap_or(0) + added);
        }
    }
    buckets
}

pub(crate) async fn overview_payload(state: &DashboardState) -> Result<Value, String> {
    let facts_count =
        super::util::query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_facts", ()).await;
    let banks_count =
        super::util::query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_banks", ()).await;

    let categories = memory_queries::overview_categories(state).await?;
    let category_rows = memory_queries::overview_category_rows(state).await?;

    let bank_rows = memory_queries::overview_bank_rows(state)
        .await
        .unwrap_or_default();
    let banks_by_name: Map<String, Value> = bank_rows
        .iter()
        .filter_map(|row| {
            let name = row.get("bank_name")?.as_str()?.to_string();
            Some((name, row.clone()))
        })
        .collect();

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

    let entity_types = memory_queries::overview_entity_types(state).await?;
    let entities_count: i64 = entity_types
        .iter()
        .filter_map(|row| row.get("count").and_then(Value::as_i64))
        .sum();

    let memory_banks = memory_queries::live_memory_banks(state).await?;
    let growth = memory_queries::growth_rows(state).await.unwrap_or_default();

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

pub(crate) async fn graph_payload(
    state: &DashboardState,
    query: &str,
    limit: i64,
) -> Result<Value, String> {
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

    for row in memory_queries::graph_entity_rows(state, &fact_ids).await? {
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

    for row in memory_queries::graph_bank_rows(state)
        .await
        .unwrap_or_default()
    {
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

pub(crate) async fn fact_detail_payload(
    state: &DashboardState,
    fact_id: i64,
) -> Result<Option<Value>, String> {
    let Some(mut fact) = memory_queries::fact_detail_row(state, fact_id).await? else {
        return Ok(None);
    };
    let entities = memory_queries::fact_entities(state, fact_id)
        .await
        .unwrap_or_default();
    if let Some(obj) = fact.as_object_mut() {
        obj.insert("entities".into(), json!(entities));
    }
    Ok(Some(json!({ "fact": fact, "error": "" })))
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

pub(crate) async fn projection_payload(state: &DashboardState, query: &str, limit: i64) -> Value {
    let mut obj = Map::new();
    obj.insert("exists".into(), json!(true));
    obj.insert("dim".into(), json!(0));
    obj.insert("limit".into(), json!(limit));
    obj.insert("method".into(), json!("none"));
    obj.insert("points".into(), json!([]));
    obj.insert("error".into(), json!(""));

    let fingerprint = match memory_queries::vector_state_fingerprint(state).await {
        Ok(fingerprint) => fingerprint,
        Err(e) => {
            obj.insert("error".into(), json!(e));
            return Value::Object(obj);
        }
    };
    let key = (query.trim().to_string(), limit, fingerprint);

    let cache = PROJECTION_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    if let Some(existing) = guard.get(&state.mem_db_path) {
        if existing.key == key {
            return projection_response(existing, obj);
        }
    }

    let rows = match memory_queries::vector_facts(state, query, limit).await {
        Ok(rows) => rows,
        Err(e) => {
            obj.insert("error".into(), json!(e));
            return Value::Object(obj);
        }
    };
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

static SIMILARITY_CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<SimilarityComputation>>>> =
    OnceLock::new();

pub(crate) async fn similarity_computation(
    state: &DashboardState,
) -> Result<Arc<SimilarityComputation>, String> {
    let key = memory_queries::vector_state_fingerprint(state).await?;
    let cache = SIMILARITY_CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut guard = cache.lock().await;
    if let Some(existing) = guard.get(&state.mem_db_path) {
        if existing.key == key {
            return Ok(existing.clone());
        }
    }

    let rows = memory_queries::vector_facts(state, "", SIMILARITY_FACT_CAP).await?;
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

pub(crate) async fn similarity_payload(
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

fn curation_apply_snapshot(index: usize, event: &Value) -> Value {
    let id = format!("curate-apply-{}", index + 1);
    json!({
        "id": id,
        "name": id,
        "path": format!("curation://{id}"),
        "ts": event.get("ts").cloned().unwrap_or(Value::Null),
        "summary": event.get("message").cloned().unwrap_or(Value::Null),
        "provider": "tracedecay",
        "mode": "similarity_dedup",
    })
}

pub(crate) async fn curation_status_payload(state: &DashboardState) -> Value {
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
    let activity = state.curation_activity.read().await;
    let apply_finishes: Vec<&Value> = activity
        .iter()
        .filter(|event| {
            event.get("phase").and_then(Value::as_str) == Some("finish")
                && event.get("dry_run").and_then(Value::as_bool) == Some(false)
        })
        .collect();
    let run_count = apply_finishes.len() as i64;
    let latest_run = apply_finishes.last().copied();
    let last_run_at = latest_run
        .and_then(|event| event.get("ts"))
        .cloned()
        .unwrap_or(Value::Null);
    let last_run_summary = latest_run
        .and_then(|event| event.get("message"))
        .cloned()
        .unwrap_or(Value::Null);
    let last_run_id = if run_count > 0 {
        json!(format!("curate-apply-{run_count}"))
    } else {
        Value::Null
    };
    let snapshots: Vec<Value> = apply_finishes
        .iter()
        .rev()
        .take(10)
        .rev()
        .enumerate()
        .map(|(index, event)| curation_apply_snapshot(index, event))
        .collect();
    json!({
        "provider": "tracedecay",
        "state": {
            "paused": false,
            "last_run_at": last_run_at,
            "run_count": run_count,
            "last_run_summary": last_run_summary,
            "last_run_id": last_run_id,
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
        "snapshots": snapshots,
    })
}

pub(crate) async fn push_curation_activity(
    state: &DashboardState,
    phase: &str,
    message: impl Into<String>,
    dry_run: bool,
) {
    push_curation_activity_with_level(state, phase, message, dry_run, "info").await;
}

pub(crate) async fn push_curation_activity_with_level(
    state: &DashboardState,
    phase: &str,
    message: impl Into<String>,
    dry_run: bool,
    level: &str,
) {
    let mut events = state.curation_activity.write().await;
    events.push(json!({
        "ts": crate::timeutil::now_iso_utc(),
        "phase": phase,
        "message": message.into(),
        "level": level,
        "dry_run": dry_run,
    }));
    if events.len() > 300 {
        let overflow = events.len() - 300;
        events.drain(0..overflow);
    }
}

pub(crate) async fn curation_activity_payload(state: &DashboardState, limit: i64) -> Value {
    let events = state.curation_activity.read().await;
    let limit = limit.max(0) as usize;
    let start = events.len().saturating_sub(limit);
    let visible: Vec<Value> = events[start..].to_vec();
    let count = visible.len();
    json!({ "events": visible, "count": count, "limit": limit, "error": "" })
}

pub(crate) async fn curation_preview_payload(state: &DashboardState) -> Value {
    let preview = state.curate_preview.read().await;
    match preview.as_ref() {
        None => json!({
            "report": null,
            "saved_at": null,
            "stale": false,
            "stale_reason": "",
            "error": "",
        }),
        Some(entry) => {
            let report = entry.report.clone();
            let saved_at = entry.saved_at.clone();
            let memory_fingerprint_at_save = entry.memory_fingerprint_at_save;
            drop(preview);
            let current_fingerprint = memory_queries::curation_preview_fingerprint(state)
                .await
                .unwrap_or((-1, -1, -1, -1));
            let stale = current_fingerprint != memory_fingerprint_at_save;
            let stale_reason = if stale {
                "Memory store changed since this preview was generated."
            } else {
                ""
            };
            json!({
                "report": report,
                "saved_at": saved_at,
                "stale": stale,
                "stale_reason": stale_reason,
                "error": "",
            })
        }
    }
}

pub(crate) async fn curation_agent_plan_payload(
    state: &DashboardState,
    max_clusters: usize,
    min_confidence: f64,
) -> Result<Value, String> {
    Box::pin(curation_agent_plan_payload_with_run_id(
        state,
        max_clusters,
        min_confidence,
        None,
    ))
    .await
}

pub(crate) async fn curation_agent_plan_payload_with_run_id(
    state: &DashboardState,
    max_clusters: usize,
    min_confidence: f64,
    run_id: Option<String>,
) -> Result<Value, String> {
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        run_memory_curator_with_backend, MemoryCuratorAutomationOptions,
    };

    push_curation_activity(
        state,
        "queued",
        "Queued standalone memory-curator agent plan",
        true,
    )
    .await;
    let run_context = match dashboard_automation_run_context(state).await {
        Ok(context) => context,
        Err(err) => {
            push_curation_activity_with_level(
                state,
                "failure",
                format!("Could not prepare memory-curator backend context: {err}"),
                true,
                "error",
            )
            .await;
            push_curation_activity(
                state,
                "finish",
                "Finished standalone memory-curator agent plan with setup failure",
                true,
            )
            .await;
            return Err(err);
        }
    };

    push_curation_activity(
        state,
        "evidence",
        format!(
            "Collecting memory-curator evidence with up to {max_clusters} cluster(s) at confidence floor {min_confidence:.2}"
        ),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "backend",
        "Running standalone memory-curator backend review",
        true,
    )
    .await;
    let run = match run_memory_curator_with_backend(
        &run_context.cg,
        &run_context.config,
        &run_context.backend,
        MemoryCuratorAutomationOptions {
            trigger: AutomationTrigger::Dashboard,
            run_id,
            max_clusters,
            min_confidence,
        },
    )
    .await
    {
        Ok(run) => run,
        Err(err) => {
            push_curation_activity_with_level(
                state,
                "failure",
                format!("Memory-curator backend review failed: {err}"),
                true,
                "error",
            )
            .await;
            push_curation_activity(
                state,
                "finish",
                "Finished standalone memory-curator agent plan with backend failure",
                true,
            )
            .await;
            return Err(err.to_string());
        }
    };
    if run.ledger_record.fallback_status.as_deref() == Some("backend_failed_noop") {
        push_curation_activity_with_level(
            state,
            "failure",
            "Memory-curator backend was unavailable; recorded a no-op fallback run",
            true,
            "warning",
        )
        .await;
        push_curation_activity(
            state,
            "report",
            format!(
                "Agent plan {}: backend unavailable; no changes proposed",
                run.ledger_record.status.as_str()
            ),
            true,
        )
        .await;
        push_curation_activity(
            state,
            "finish",
            "Finished standalone memory-curator agent plan with no-op fallback",
            true,
        )
        .await;
        return Ok(automation_run_payload(
            &run.run_id,
            &run.report,
            &run.ledger_record,
            run.backend_response.as_ref(),
        ));
    }
    push_curation_activity(
        state,
        "validation",
        format!(
            "Validated backend proposal: {} accepted op(s), {} rejected op(s)",
            run.ledger_record.accepted_count, run.ledger_record.rejected_count
        ),
        true,
    )
    .await;
    if run.ledger_record.rejected_count > 0 {
        push_curation_activity_with_level(
            state,
            "rejection",
            format!(
                "Rejected {} backend-proposed op(s) during evidence validation",
                run.ledger_record.rejected_count
            ),
            true,
            "warning",
        )
        .await;
    }
    let apply_policy = run
        .report
        .get("automation_apply_policy")
        .cloned()
        .unwrap_or(Value::Null);
    let apply_decision = apply_policy
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mutates_store = apply_policy
        .get("mutates_store")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    push_curation_activity(
        state,
        "apply",
        format!(
            "Memory-curator apply policy: {apply_decision}; store mutation {}",
            if mutates_store {
                "performed"
            } else {
                "not performed"
            }
        ),
        !mutates_store,
    )
    .await;
    push_curation_activity(
        state,
        "report",
        format!(
            "Agent plan {}: {} accepted op(s), {} rejected op(s)",
            run.ledger_record.status.as_str(),
            run.ledger_record.accepted_count,
            run.ledger_record.rejected_count
        ),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "finish",
        format!(
            "Finished standalone memory-curator agent plan: {}",
            run.ledger_record.status.as_str()
        ),
        true,
    )
    .await;

    Ok(automation_run_payload(
        &run.run_id,
        &run.report,
        &run.ledger_record,
        run.backend_response.as_ref(),
    ))
}

pub(crate) struct SessionReflectionRunRequest {
    pub provider: Option<String>,
    pub query: Option<String>,
    pub evidence_limit: Option<usize>,
    pub storage_scope: Option<String>,
    pub hermes_home: Option<PathBuf>,
    pub scope: Option<LcmScope>,
    pub session_id: Option<String>,
    pub include_summaries: Option<bool>,
    pub sort: Option<LcmGrepSort>,
    pub source: Option<String>,
    pub role: Option<String>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

pub(crate) async fn session_reflection_run_payload_with_run_id(
    state: &DashboardState,
    request: SessionReflectionRunRequest,
    run_id: Option<String>,
) -> Result<Value, String> {
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{
        run_session_reflector_with_backend, SessionReflectorAutomationOptions,
    };

    push_dashboard_automation_activity_start(
        state,
        "session-reflector",
        "Collecting session-reflector evidence from LCM search",
        "Preparing standalone session-reflector backend review",
    )
    .await;
    let run_context = match dashboard_automation_run_context(state).await {
        Ok(context) => context,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "session-reflector",
                format!("Could not prepare session-reflector backend context: {err}"),
                "setup failure",
            )
            .await;
            return Err(err);
        }
    };
    let mut options = SessionReflectorAutomationOptions {
        trigger: AutomationTrigger::Dashboard,
        run_id,
        ..SessionReflectorAutomationOptions::default()
    };
    if let Some(provider) = request.provider {
        options.provider = provider;
    }
    if let Some(query) = request.query {
        options.query = query;
    }
    if let Some(evidence_limit) = request.evidence_limit {
        options.evidence_limit = evidence_limit;
    }
    if let Some(storage_scope) = request.storage_scope {
        options.storage_scope = storage_scope;
    }
    if let Some(hermes_home) = request.hermes_home {
        options.hermes_home = Some(hermes_home);
    }
    if let Some(scope) = request.scope {
        options.scope = scope;
    }
    if let Some(session_id) = request.session_id {
        options.session_id = Some(session_id);
    }
    if let Some(include_summaries) = request.include_summaries {
        options.include_summaries = include_summaries;
    }
    if let Some(sort) = request.sort {
        options.sort = sort;
    }
    if let Some(source) = request.source {
        options.source = Some(source);
    }
    if let Some(role) = request.role {
        options.role = Some(role);
    }
    options.start_time = request.start_time;
    options.end_time = request.end_time;
    let run = match run_session_reflector_with_backend(
        &run_context.cg,
        &run_context.config,
        &run_context.backend,
        options,
    )
    .await
    {
        Ok(run) => run,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "session-reflector",
                format!("Session-reflector backend review failed: {err}"),
                "backend failure",
            )
            .await;
            return Err(err.to_string());
        }
    };
    push_dashboard_automation_activity_result(state, "session-reflector", &run.ledger_record).await;

    Ok(automation_run_payload(
        &run.run_id,
        &run.report,
        &run.ledger_record,
        run.backend_response.as_ref(),
    ))
}

pub(crate) async fn skill_writing_run_payload_with_run_id(
    state: &DashboardState,
    provider: Option<String>,
    query: Option<String>,
    evidence_limit: Option<usize>,
    run_id: Option<String>,
) -> Result<Value, String> {
    use crate::automation::run_ledger::AutomationTrigger;
    use crate::automation::runner::{run_skill_writer_with_backend, SkillWriterAutomationOptions};

    push_dashboard_automation_activity_start(
        state,
        "skill-writer",
        "Collecting skill-writer evidence from LCM, managed skills, and usage telemetry",
        "Preparing standalone skill-writer backend review",
    )
    .await;
    let run_context = match dashboard_automation_run_context(state).await {
        Ok(context) => context,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "skill-writer",
                format!("Could not prepare skill-writer backend context: {err}"),
                "setup failure",
            )
            .await;
            return Err(err);
        }
    };
    let mut options = SkillWriterAutomationOptions {
        trigger: AutomationTrigger::Dashboard,
        run_id,
        profile_root: None,
        ..SkillWriterAutomationOptions::default()
    };
    if let Some(provider) = provider {
        options.provider = provider;
    }
    if let Some(query) = query {
        options.query = query;
    }
    if let Some(evidence_limit) = evidence_limit {
        options.evidence_limit = evidence_limit;
    }
    let run = match run_skill_writer_with_backend(
        &run_context.cg,
        &run_context.config,
        &run_context.backend,
        options,
    )
    .await
    {
        Ok(run) => run,
        Err(err) => {
            push_dashboard_automation_activity_failure(
                state,
                "skill-writer",
                format!("Skill-writer backend review failed: {err}"),
                "backend failure",
            )
            .await;
            return Err(err.to_string());
        }
    };
    push_dashboard_automation_activity_result(state, "skill-writer", &run.ledger_record).await;

    Ok(automation_run_payload(
        &run.run_id,
        &run.report,
        &run.ledger_record,
        run.backend_response.as_ref(),
    ))
}

async fn push_dashboard_automation_activity_start(
    state: &DashboardState,
    task_label: &str,
    evidence_message: &'static str,
    backend_message: &'static str,
) {
    push_curation_activity(
        state,
        "queued",
        format!("Queued dashboard {task_label} automation run"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "evidence",
        format!("{evidence_message} for dashboard {task_label} automation run"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "backend",
        format!("{backend_message} for dashboard {task_label} automation run"),
        true,
    )
    .await;
}

async fn push_dashboard_automation_activity_failure(
    state: &DashboardState,
    task_label: &str,
    message: impl Into<String>,
    finish_reason: &str,
) {
    push_curation_activity_with_level(state, "failure", message, true, "error").await;
    push_curation_activity(
        state,
        "finish",
        format!("Finished dashboard {task_label} automation run with {finish_reason}"),
        true,
    )
    .await;
}

async fn push_dashboard_automation_activity_result(
    state: &DashboardState,
    task_label: &str,
    record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
) {
    if record.status == crate::automation::run_ledger::AutomationRunStatus::Skipped {
        let reason = record.error.as_deref().unwrap_or("skipped");
        push_curation_activity(
            state,
            "validation",
            format!("Skipped dashboard {task_label} automation run: {reason}"),
            true,
        )
        .await;
        push_curation_activity(
            state,
            "apply",
            format!("No mutations applied for dashboard {task_label} automation run: {reason}"),
            true,
        )
        .await;
        push_curation_activity(
            state,
            "report",
            format!("Dashboard {task_label} automation run skipped: {reason}"),
            true,
        )
        .await;
        push_curation_activity(
            state,
            "finish",
            format!("Finished skipped dashboard {task_label} automation run: {reason}"),
            true,
        )
        .await;
        return;
    }

    push_curation_activity(
        state,
        "validation",
        format!(
            "Validated dashboard {task_label} proposal: {} accepted item(s), {} rejected item(s)",
            record.accepted_count, record.rejected_count
        ),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "apply",
        format!("Dashboard {task_label} run kept mutations gated behind approval controls"),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "report",
        format!(
            "Dashboard {task_label} automation run {}: {} accepted item(s), {} rejected item(s)",
            record.status.as_str(),
            record.accepted_count,
            record.rejected_count
        ),
        true,
    )
    .await;
    push_curation_activity(
        state,
        "finish",
        format!(
            "Finished dashboard {task_label} automation run: {}",
            record.status.as_str()
        ),
        true,
    )
    .await;
}

struct DashboardAutomationRunContext {
    cg: crate::tracedecay::TraceDecay,
    config: crate::automation::config::AutomationConfig,
    backend: crate::automation::backend::CodexAppServerBackend,
}

async fn dashboard_automation_run_context(
    state: &DashboardState,
) -> Result<DashboardAutomationRunContext, String> {
    use crate::automation::backend::CodexAppServerBackend;
    use crate::automation::config::{effective_config, load_project_config, AutomationBackend};
    use crate::tracedecay::TraceDecay;

    let cg = TraceDecay::open(&state.project_root)
        .await
        .map_err(|e| e.to_string())?;
    let global = crate::user_config::UserConfig::load().automation;
    let project = load_project_config(&state.dashboard_root)
        .await
        .map_err(|e| e.to_string())?;
    let config = effective_config(&global, project.as_ref()).map_err(|e| e.to_string())?;
    if config.enabled && config.backend == AutomationBackend::ExternalCommand {
        return Err("automation backend external_command is not implemented yet".to_string());
    }
    let backend = CodexAppServerBackend::from_automation_config(&config);

    Ok(DashboardAutomationRunContext {
        cg,
        config,
        backend,
    })
}

fn automation_run_payload(
    run_id: &str,
    report: &Value,
    ledger_record: &crate::automation::run_ledger::AutomationRunLedgerRecord,
    backend_response: Option<&crate::automation::backend::AgentTaskResponse>,
) -> Value {
    json!({
        "run_id": run_id,
        "dry_run": true,
        "status": ledger_record.status,
        "report": report,
        "ledger_record": ledger_record,
        "backend_response": backend_response,
    })
}

pub(crate) async fn build_delete_plan(
    state: &DashboardState,
) -> Result<(Vec<Value>, Value, Map<String, Value>, i64), String> {
    let total =
        super::util::query_i64(&state.mem_conn, "SELECT COUNT(*) FROM memory_facts", ()).await;
    let computation = similarity_computation(state).await?;

    let actions = if computation.facts.len() < 2 || computation.dim == 0 {
        Vec::new()
    } else {
        let planner_len = computation
            .pairs
            .iter()
            .take_while(|pair| pair.similarity >= SIMILARITY_DEFAULT_THRESHOLD)
            .count();
        propose_dedup_actions(&computation.facts, &computation.pairs[..planner_len])
    };

    let dedup_loser_ids: HashSet<i64> = actions
        .iter()
        .filter_map(|action| action.get("fact_id").and_then(Value::as_i64))
        .collect();
    let hygiene_facts = fetch_facts(state, "", total).await?;
    let hygiene_candidates = propose_hygiene_candidates(
        &hygiene_facts,
        &computation.facts,
        &computation.supersession_pairs,
        &dedup_loser_ids,
    );

    let mut counts = Map::new();
    if !actions.is_empty() {
        counts.insert("delete".to_string(), json!(actions.len()));
    }
    Ok((actions, hygiene_candidates, counts, total))
}

pub(crate) async fn delete_fact(state: &DashboardState, fact_id: i64) -> Result<bool, String> {
    let store = MemoryStore::new(&state.mem_conn);
    store.remove_fact(fact_id).await.map_err(|e| e.to_string())
}

pub(crate) async fn curate_payload(state: &DashboardState, dry_run: bool) -> Result<Value, String> {
    push_curation_activity(
        state,
        "queued",
        if dry_run {
            "Queued similarity-dedup curation preview"
        } else {
            "Queued similarity-dedup curation apply"
        },
        dry_run,
    )
    .await;
    push_curation_activity(
        state,
        "start",
        if dry_run {
            "Starting similarity-dedup curation preview"
        } else {
            "Starting similarity-dedup curation apply"
        },
        dry_run,
    )
    .await;
    push_curation_activity(
        state,
        "evidence",
        "Collecting similarity and hygiene evidence",
        dry_run,
    )
    .await;
    push_curation_activity(
        state,
        "backend",
        "Running deterministic similarity-dedup planner",
        dry_run,
    )
    .await;
    let (actions, hygiene_candidates, counts, total) = match build_delete_plan(state).await {
        Ok(plan) => plan,
        Err(err) => {
            push_curation_activity_with_level(
                state,
                "failure",
                format!("Curation evidence collection failed: {err}"),
                dry_run,
                "error",
            )
            .await;
            return Err(err);
        }
    };
    push_curation_activity(
        state,
        "validation",
        format!(
            "Validated deterministic curation plan: {} delete action(s), {} hygiene candidate(s)",
            actions.len(),
            hygiene_candidates.as_array().map_or(0, Vec::len)
        ),
        dry_run,
    )
    .await;

    let report = json!({
        "ran": true,
        "dry_run": dry_run,
        "actions": actions,
        "hygiene_candidates": hygiene_candidates,
        "counts": counts,
        "applied_counts": if dry_run { Value::Null } else { json!(counts.clone()) },
        "llm_calls": 0,
        "coverage": {
            "scanned": total,
            "active_total": total,
            "due_remaining": 0,
        },
        "provider": "tracedecay",
        "mode": "similarity_dedup",
    });

    if dry_run {
        let saved_at = crate::timeutil::now_iso_utc();
        let memory_fingerprint_at_save = memory_queries::curation_preview_fingerprint(state)
            .await
            .unwrap_or((total, 0, 0, 0));
        let entry = CuratePreviewEntry {
            report: report.clone(),
            saved_at,
            active_facts_at_save: total,
            memory_fingerprint_at_save,
        };
        super::curate_preview_store::save(&state.dashboard_root, &entry).await;
        *state.curate_preview.write().await = Some(entry);
        push_curation_activity(
            state,
            "report",
            format!(
                "Preview report ready: {} delete action(s), {} active fact(s) scanned",
                actions.len(),
                total
            ),
            true,
        )
        .await;
        push_curation_activity(
            state,
            "finish",
            format!(
                "Preview completed: {} delete action(s), {} active fact(s) scanned",
                actions.len(),
                total
            ),
            true,
        )
        .await;
        return Ok(report);
    }

    let mut applied = 0i64;
    let mut skipped = 0i64;
    push_curation_activity(
        state,
        "report",
        format!(
            "Apply report ready: {} delete action(s), {} active fact(s) scanned",
            actions.len(),
            total
        ),
        false,
    )
    .await;
    push_curation_activity(
        state,
        "apply",
        format!(
            "Applying {} deterministic curation action(s)",
            actions.len()
        ),
        false,
    )
    .await;
    if let Some(action_list) = report.get("actions").and_then(Value::as_array) {
        for action in action_list {
            let Some(fact_id) = action.get("fact_id").and_then(Value::as_i64) else {
                skipped += 1;
                continue;
            };
            match delete_fact(state, fact_id).await {
                Ok(true) => applied += 1,
                Ok(false) | Err(_) => skipped += 1,
            }
        }
    }

    *state.curate_preview.write().await = None;
    super::curate_preview_store::clear(&state.dashboard_root).await;

    let _ = MemoryStore::new(&state.mem_conn)
        .record_oplog(
            "curate_apply",
            None,
            &json!({ "mode": "similarity_dedup", "deleted": applied, "skipped": skipped }),
        )
        .await;

    let mut applied_counts = Map::new();
    if applied > 0 {
        applied_counts.insert("delete".to_string(), json!(applied));
    }
    if skipped > 0 {
        push_curation_activity_with_level(
            state,
            "rejection",
            format!("{skipped} deterministic curation action(s) were skipped during apply"),
            false,
            "warning",
        )
        .await;
    }
    push_curation_activity(
        state,
        "finish",
        format!("Apply completed: {applied} fact(s) deleted, {skipped} action(s) skipped"),
        false,
    )
    .await;
    Ok(json!({
        "ran": true,
        "dry_run": false,
        "actions": report["actions"],
        "hygiene_candidates": report["hygiene_candidates"],
        "counts": report["counts"],
        "applied_counts": applied_counts,
        "skipped_actions": skipped,
        "llm_calls": 0,
        "coverage": report["coverage"],
        "provider": "tracedecay",
        "mode": "similarity_dedup",
    }))
}

pub(crate) async fn apply_delete_op(state: &DashboardState, op: &Value) -> (Value, bool) {
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

pub(crate) async fn apply_merge_op(state: &DashboardState, op: &Value) -> (Value, bool) {
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
    let mut parsed_loser_ids = Vec::with_capacity(loser_ids.len());
    for (index, value) in loser_ids.iter().enumerate() {
        let Some(loser_id) = value.as_i64() else {
            return (
                json!({
                    "op": "merge",
                    "winner_id": winner_id,
                    "status": "error",
                    "error": format!("loser_ids[{index}] must be an integer"),
                }),
                false,
            );
        };
        parsed_loser_ids.push(loser_id);
    }

    let store = MemoryStore::new(&state.mem_conn);
    let merged_content = op
        .get("merged_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    match store
        .merge_facts(winner_id, parsed_loser_ids, merged_content)
        .await
    {
        Ok((content_updated, deleted)) => (
            json!({
                "op": "merge",
                "winner_id": winner_id,
                "content_updated": content_updated,
                "deleted_loser_ids": deleted,
                "failed_losers": [],
                "status": "merged",
            }),
            true,
        ),
        Err(e) => (
            json!({
                "op": "merge",
                "winner_id": winner_id,
                "content_updated": false,
                "deleted_loser_ids": [],
                "failed_losers": [],
                "status": "error",
                "error": e.to_string(),
            }),
            false,
        ),
    }
}

pub(crate) async fn curate_apply_payload(state: &DashboardState, ops: &[Value]) -> Value {
    push_curation_activity(
        state,
        "queued",
        format!("Queued explicit apply for {} curation op(s)", ops.len()),
        false,
    )
    .await;
    push_curation_activity(
        state,
        "apply",
        format!("Applying {} explicit curation op(s)", ops.len()),
        false,
    )
    .await;
    let mut results: Vec<Value> = Vec::with_capacity(ops.len());
    let mut deleted = 0i64;
    let mut merged = 0i64;
    let mut errors = 0i64;

    for op in ops {
        let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
        let (result, ok) = match kind {
            "delete" => apply_delete_op(state, op).await,
            "merge" => apply_merge_op(state, op).await,
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

    push_curation_activity(
        state,
        "validation",
        format!(
            "Validated explicit apply results: {deleted} delete op(s), {merged} merge op(s), {errors} error(s)"
        ),
        false,
    )
    .await;
    if errors > 0 {
        push_curation_activity_with_level(
            state,
            "rejection",
            format!("{errors} explicit curation op(s) were rejected or failed"),
            false,
            "warning",
        )
        .await;
    }
    if deleted > 0 || merged > 0 {
        *state.curate_preview.write().await = None;
        super::curate_preview_store::clear(&state.dashboard_root).await;
        let _ = MemoryStore::new(&state.mem_conn)
            .record_oplog(
                "curate_apply",
                None,
                &json!({ "mode": "ops", "deleted": deleted, "merged": merged, "errors": errors }),
            )
            .await;
    }
    push_curation_activity(
        state,
        "report",
        format!(
            "Explicit apply report ready: {deleted} delete op(s), {merged} merge op(s), {errors} error(s)"
        ),
        false,
    )
    .await;
    if errors > 0 && deleted == 0 && merged == 0 {
        push_curation_activity_with_level(
            state,
            "failure",
            format!("All {errors} explicit curation op(s) failed validation or apply"),
            false,
            "error",
        )
        .await;
    }
    push_curation_activity(
        state,
        "finish",
        format!(
            "Explicit apply completed: {deleted} delete op(s), {merged} merge op(s), {errors} op(s) errored"
        ),
        false,
    )
    .await;

    json!({
        "results": results,
        "counts": { "deleted": deleted, "merged": merged, "errors": errors },
    })
}

pub(crate) async fn oplog_payload(state: &DashboardState, limit: i64) -> Value {
    match memory_queries::oplog_rows(state, limit).await {
        Ok(rows) => {
            let events: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    let detail = row
                        .get("detail_json")
                        .and_then(Value::as_str)
                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                        .unwrap_or_else(|| json!({}));
                    json!({
                        "id": row.get("id").cloned().unwrap_or(Value::Null),
                        "ts": row.get("ts").cloned().unwrap_or(Value::Null),
                        "op": row.get("op").cloned().unwrap_or(Value::Null),
                        "fact_id": row.get("fact_id").cloned().unwrap_or(Value::Null),
                        "detail": detail,
                    })
                })
                .collect();
            let count = events.len();
            json!({ "events": events, "count": count, "limit": limit, "error": "" })
        }
        Err(e) => json!({ "events": [], "count": 0, "limit": limit, "error": e }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn curation_apply_snapshot_keeps_dashboard_history_contract() {
        let event = json!({
            "ts": "2026-06-23T00:00:00Z",
            "phase": "finish",
            "message": "Apply completed: 2 fact(s) deleted, 0 action(s) skipped",
            "dry_run": false,
        });

        let snapshot = curation_apply_snapshot(0, &event);

        assert_eq!(snapshot["id"], "curate-apply-1");
        assert_eq!(snapshot["name"], "curate-apply-1");
        assert_eq!(snapshot["path"], "curation://curate-apply-1");
        assert_eq!(snapshot["ts"], "2026-06-23T00:00:00Z");
        assert_eq!(
            snapshot["summary"],
            "Apply completed: 2 fact(s) deleted, 0 action(s) skipped"
        );
        assert_eq!(snapshot["provider"], "tracedecay");
        assert_eq!(snapshot["mode"], "similarity_dedup");
    }
}
