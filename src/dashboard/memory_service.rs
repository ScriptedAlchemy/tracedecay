use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use super::DashboardState;
use super::memory_analysis::{
    SIMILARITY_DEFAULT_THRESHOLD, SIMILARITY_FACT_CAP, SIMILARITY_PAIR_FLOOR, SIMILARITY_SCORE_MAX,
    SIMILARITY_SCORE_MIN, SimilarityComputation, build_similarity_computation, pca_scores,
    propose_dedup_actions, propose_hygiene_candidates, score_distribution, score_similar_pairs,
};
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_domain::FactCategoryV1;
use tracedecay_store::{
    CompatibilityDashboardEntityV1, CompatibilityDashboardFactSummaryV1,
    CompatibilityDashboardHrrStateV1, CompatibilityDashboardMemoryOverviewV1,
    CompatibilityDashboardOplogDetailsV1, CompatibilityDashboardVectorPointV1,
    CompatibilityFactProjectionV1, CompatibilityFactTargetV1,
};

const PROJECTION_POINT_CAP: i64 = 2000;
type VectorStateFingerprint = (i64, i64, i64, u64);

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

const fn fact_category_label(category: FactCategoryV1) -> &'static str {
    match category {
        FactCategoryV1::General => "general",
        FactCategoryV1::UserPref => "user_pref",
        FactCategoryV1::Project => "project",
        FactCategoryV1::Tool => "tool",
        FactCategoryV1::Decision => "decision",
        FactCategoryV1::CodeArea => "code_area",
    }
}

fn legacy_fact_id(projection: &CompatibilityFactProjectionV1) -> Option<i64> {
    match projection {
        CompatibilityFactProjectionV1::Available(fact) => fact.legacy_fact_id(),
        CompatibilityFactProjectionV1::Unavailable(_) => None,
    }
}

fn target_legacy_fact_id(target: &CompatibilityFactTargetV1) -> Option<i64> {
    target
        .legacy_query()
        .map(tracedecay_store::LegacyFactQuery::legacy_fact_id)
}

/// Converts only an available, mapped compatibility fact. Unavailable or
/// redacted payload fields stay omitted; dashboard handlers never invent them.
fn fact_summary_json(summary: &CompatibilityDashboardFactSummaryV1) -> Option<Value> {
    let CompatibilityFactProjectionV1::Available(fact) = &summary.fact else {
        return None;
    };
    let fact_id = fact.legacy_fact_id()?;
    let telemetry = fact.telemetry();
    let mut row = Map::new();
    row.insert("fact_id".into(), json!(fact_id));
    row.insert("trust_score".into(), json!(fact.fact().trust().as_f64()));
    row.insert("retrieval_count".into(), json!(telemetry.retrieval_count()));
    row.insert("access_count".into(), json!(telemetry.access_count()));
    row.insert("helpful_count".into(), json!(telemetry.helpful_count()));
    row.insert("unhelpful_count".into(), json!(telemetry.unhelpful_count()));
    row.insert("created_at".into(), json!(telemetry.created_at().0));
    row.insert("updated_at".into(), json!(telemetry.updated_at().0));
    row.insert(
        "last_recalled_at".into(),
        json!(telemetry.last_recalled_at().map(|value| value.0)),
    );
    row.insert(
        "has_hrr".into(),
        json!(if summary.has_hrr_vector { 1_i64 } else { 0_i64 }),
    );
    if let Some(content) = fact.content() {
        row.insert("content".into(), json!(content));
    }
    if let Some(category) = fact.category() {
        row.insert("category".into(), json!(fact_category_label(category)));
    }
    if let Some(tags) = fact.tags() {
        row.insert("tags".into(), json!(tags));
    }
    if let Some(metadata) = fact.metadata() {
        row.insert("metadata".into(), metadata.clone());
    }
    Some(Value::Object(row))
}

fn entity_json(entity: &CompatibilityDashboardEntityV1) -> Value {
    json!({
        "entity_id": entity.target.legacy_entity_id(),
        "name": entity.name,
        "entity_type": entity.entity_type,
        "aliases": entity.aliases,
        "created_at": entity.created_at.0,
        "fact_count": entity.fact_count,
    })
}

fn fact_matches_query(fact: &Value, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let query = query.to_ascii_lowercase();
    fact.get("content")
        .and_then(Value::as_str)
        .is_some_and(|content| content.to_ascii_lowercase().contains(&query))
        || fact
            .get("tags")
            .and_then(Value::as_array)
            .is_some_and(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .any(|tag| tag.to_ascii_lowercase().contains(&query))
            })
}

async fn dashboard_overview(
    state: &DashboardState,
    fact_limit: usize,
    graph_limit: usize,
) -> Result<CompatibilityDashboardMemoryOverviewV1, String> {
    memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?
        .dashboard_overview_v1(fact_limit, graph_limit)
        .await
        .map_err(|error| error.to_string())
}

fn vector_rows(points: Vec<CompatibilityDashboardVectorPointV1>) -> Vec<(Value, Vec<f64>)> {
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

fn vector_fingerprint(rows: &[(Value, Vec<f64>)]) -> VectorStateFingerprint {
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

pub(crate) async fn fetch_facts(
    state: &DashboardState,
    query: &str,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let limit = usize::try_from(limit.max(1)).map_err(|error| error.to_string())?;
    let overview = dashboard_overview(state, limit.min(100), 1).await?;
    Ok(overview
        .facts
        .iter()
        .filter_map(fact_summary_json)
        .filter(|fact| fact_matches_query(fact, query))
        .take(limit)
        .collect())
}

pub(crate) async fn fetch_entities(
    state: &DashboardState,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let limit = usize::try_from(limit.max(1)).map_err(|error| error.to_string())?;
    let overview = dashboard_overview(state, 1, limit.min(1000)).await?;
    Ok(overview
        .entities
        .iter()
        .map(entity_json)
        .take(limit)
        .collect())
}

fn trust_histogram(overview: &CompatibilityDashboardMemoryOverviewV1) -> Vec<Value> {
    let mut buckets: Vec<Value> = (0..10)
        .map(|i| {
            json!({
                "bucket": i,
                "label": format!("{:.1}\u{2013}{:.1}", f64::from(i) / 10.0, f64::from(i + 1) / 10.0),
                "count": 0,
            })
        })
        .collect();
    for row in &overview.trust_histogram {
        let Ok(idx) = row.name.parse::<usize>() else {
            continue;
        };
        let Some(bucket) = buckets.get_mut(idx.min(9)) else {
            continue;
        };
        if let Some(count) = bucket.get_mut("count") {
            *count = json!(count.as_u64().unwrap_or(0).saturating_add(row.count));
        }
    }
    buckets
}

pub(crate) async fn overview_payload(state: &DashboardState) -> Result<Value, String> {
    let overview = dashboard_overview(state, 100, 1000).await?;
    let hrr_coverage: Vec<Value> = overview
        .hrr_coverage
        .iter()
        .map(|coverage| {
            let state = match &coverage.state {
                CompatibilityDashboardHrrStateV1::Ready => "ready",
                CompatibilityDashboardHrrStateV1::MissingVectors => "missing_vectors",
                CompatibilityDashboardHrrStateV1::MissingBank => "missing_bank",
                CompatibilityDashboardHrrStateV1::StaleBank => "stale_bank",
            };
            json!({
                "category": coverage.category,
                "facts": coverage.fact_count,
                "hrr_vectors": coverage.hrr_vector_count,
                "coverage": f64::from(coverage.coverage_basis_points) / 10_000.0,
                "bank_name": coverage.bank_name,
                "bank_fact_count": coverage.bank_fact_count,
                "dim": coverage.dimension,
                "updated_at": coverage.updated_at.map(|value| value.0),
                "status": state,
            })
        })
        .collect();
    let categories: Vec<Value> = overview
        .categories
        .iter()
        .map(|row| json!({ "category": row.name, "count": row.count }))
        .collect();
    let entity_types: Vec<Value> = overview
        .entity_types
        .iter()
        .map(|row| json!({ "entity_type": row.name, "count": row.count }))
        .collect();
    let memory_banks: Vec<Value> = overview
        .memory_banks
        .iter()
        .map(|bank| {
            json!({
                "bank_name": bank.name,
                "dim": bank.dimension,
                "fact_count": bank.fact_count,
                "bundled_fact_count": bank.bundled_fact_count,
                "updated_at": bank.updated_at.map(|value| value.0),
            })
        })
        .collect();
    let growth: Vec<Value> = overview
        .growth
        .iter()
        .map(|point| {
            json!({
                "date": point.period,
                "facts": point.fact_count,
                "cumulative_facts": point.cumulative_fact_count,
            })
        })
        .collect();

    Ok(json!({
        "facts": overview.fact_count,
        "entities": overview.entity_count,
        "banks": overview.bank_count,
        "categories": categories,
        "entity_types": entity_types,
        "hrr_coverage": hrr_coverage,
        "memory_banks": memory_banks,
        "trust_histogram": trust_histogram(&overview),
        "growth": growth,
    }))
}

pub(crate) async fn graph_payload(
    state: &DashboardState,
    query: &str,
    limit: i64,
) -> Result<Value, String> {
    let limit = usize::try_from(limit.max(1)).map_err(|error| error.to_string())?;
    let overview = dashboard_overview(state, 100, limit.min(1000)).await?;
    let fact_rows: Vec<Value> = overview
        .facts
        .iter()
        .filter_map(fact_summary_json)
        .filter(|fact| fact_matches_query(fact, query))
        .take(limit)
        .collect();

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

    let entity_by_id: HashMap<i64, &CompatibilityDashboardEntityV1> = overview
        .entities
        .iter()
        .map(|entity| (entity.target.legacy_entity_id(), entity))
        .collect();
    let fact_ids: HashSet<i64> = fact_ids.into_iter().collect();
    for link in &overview.fact_entity_links {
        let Some(fact_id) = target_legacy_fact_id(&link.fact) else {
            continue;
        };
        if !fact_ids.contains(&fact_id) {
            continue;
        }
        let entity_id = link.entity.legacy_entity_id();
        let Some(entity) = entity_by_id.get(&entity_id) else {
            continue;
        };
        let entity_node = format!("entity:{entity_id}");
        let fact_node = format!("fact:{fact_id}");
        nodes.entry(entity_node.clone()).or_insert_with(|| {
            json!({
                "id": entity_node,
                "kind": "entity",
                "label": entity.name,
                "entity_id": entity_id,
                "entity_type": entity.entity_type,
            })
        });
        edges.push(json!({ "source": fact_node, "target": entity_node, "kind": "mentions" }));
    }

    for bank in &overview.memory_banks {
        let bank_name = bank.name.as_str();
        let category = bank_name.to_owned();
        let bank_node_id = format!("bank:{bank_name}");
        let category_node_id = format!("category:{category}");
        if let Some(existing) = nodes.get_mut(&bank_node_id) {
            if let Some(obj) = existing.as_object_mut() {
                obj.insert("dim".into(), json!(bank.dimension));
                obj.insert("fact_count".into(), json!(bank.fact_count));
                obj.insert(
                    "updated_at".into(),
                    json!(bank.updated_at.map(|value| value.0)),
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
                    "dim": bank.dimension,
                    "fact_count": bank.fact_count,
                    "updated_at": bank.updated_at.map(|value| value.0),
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
        if let Some(node) = nodes.get_mut(&format!("category:{category}"))
            && let Some(obj) = node.as_object_mut()
        {
            obj.insert("fact_count".into(), count.clone());
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
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let Some(detail) = application
        .dashboard_fact_detail_v1(fact_id)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let vector_state = application
        .dashboard_vector_points_v1(None, PROJECTION_POINT_CAP as usize)
        .await
        .ok()
        .map(|points| {
            points.into_iter().find_map(|point| {
                (legacy_fact_id(&point.fact.fact) == Some(fact_id))
                    .then_some(point.vector.is_some())
            })
        });
    let mut fact = fact_summary_json(&CompatibilityDashboardFactSummaryV1 {
        fact: detail.fact,
        has_hrr_vector: vector_state.flatten().unwrap_or(false),
    });
    if vector_state.is_none()
        && let Some(fact) = fact.as_mut().and_then(Value::as_object_mut)
    {
        fact.remove("has_hrr");
    }
    Ok(fact.map(|fact| {
        json!({
            "fact": fact,
            "entities": detail.entities.iter().map(entity_json).collect::<Vec<_>>(),
            "error": "",
        })
    }))
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

static SIMILARITY_CACHE: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<SimilarityComputation>>>> =
    OnceLock::new();

pub(crate) async fn similarity_computation(
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
        },
        "config": {
            "enabled": true,
            "interval_hours": null,
            "min_idle_hours": null,
            "mode": "similarity_dedup",
            "dry_run_first": false,
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

pub(crate) async fn build_delete_plan(
    state: &DashboardState,
) -> Result<(Vec<Value>, Value, Map<String, Value>, i64), String> {
    let total = i64::try_from(dashboard_overview(state, 1, 1).await?.fact_count)
        .map_err(|error| error.to_string())?;
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
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let context = crate::application::memory::MemoryOperationContext::generated(
        &state.memory_owner,
        "dashboard-delete",
        None,
    )
    .map_err(|error| error.to_string())?;
    application
        .remove_fact_v1(fact_id, context)
        .await
        .map_err(|error| error.to_string())
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

    let merged_content = op
        .get("merged_content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let application = match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => application,
        Err(error) => {
            return (
                json!({
                    "op": "merge",
                    "winner_id": winner_id,
                    "status": "error",
                    "error": error.to_string(),
                }),
                false,
            );
        }
    };
    let context = match crate::application::memory::MemoryOperationContext::generated(
        &state.memory_owner,
        "dashboard-merge",
        None,
    ) {
        Ok(context) => context,
        Err(error) => {
            return (
                json!({
                    "op": "merge",
                    "winner_id": winner_id,
                    "status": "error",
                    "error": error.to_string(),
                }),
                false,
            );
        }
    };
    match application
        .dashboard_merge_fact_ids_v1(winner_id, parsed_loser_ids, merged_content, context)
        .await
    {
        Ok(outcome) => (
            json!({
                "op": "merge",
                "winner_id": winner_id,
                "content_updated": outcome.content_updated(),
                "deleted_loser_ids": outcome.deleted_losers().iter().filter_map(|fact| fact.legacy_fact_id()).collect::<Vec<_>>(),
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
                "error": format!("{e:?}"),
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
    let bounded_limit = usize::try_from(limit.clamp(1, 300)).unwrap_or(300);
    let result = match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => application
            .dashboard_oplog_v1(bounded_limit)
            .await
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    match result {
        Ok(entries) => {
            let events: Vec<Value> = entries
                .iter()
                .map(|entry| {
                    let detail = match &entry.details {
                        CompatibilityDashboardOplogDetailsV1::Available { summary } => {
                            json!({ "summary": summary })
                        }
                        CompatibilityDashboardOplogDetailsV1::Redacted => {
                            json!({ "redacted": true })
                        }
                        CompatibilityDashboardOplogDetailsV1::Unknown => {
                            json!({ "availability": "unknown" })
                        }
                    };
                    json!({
                        "id": entry.id,
                        "ts": entry.occurred_at.0,
                        "op": entry.operation,
                        "fact_id": entry.fact.as_ref().and_then(target_legacy_fact_id),
                        "detail": detail,
                    })
                })
                .collect();
            let count = events.len();
            json!({ "events": events, "count": count, "limit": limit, "error": "" })
        }
        Err(error) => json!({ "events": [], "count": 0, "limit": limit, "error": error }),
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
