use std::collections::{HashMap, HashSet};
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

const PROJECTION_POINT_CAP: i64 = 2000;

pub(crate) fn projection_point_cap() -> i64 {
    PROJECTION_POINT_CAP
}

pub(crate) fn providers_stub() -> Value {
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
    json!({
        "provider": "tracedecay",
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
    })
}

pub(crate) fn curation_activity_payload(limit: i64) -> Value {
    json!({ "events": [], "count": 0, "limit": limit, "error": "" })
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
    let (actions, hygiene_candidates, counts, total) = build_delete_plan(state).await?;

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
        return Ok(report);
    }

    let mut applied = 0i64;
    let mut skipped = 0i64;
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
