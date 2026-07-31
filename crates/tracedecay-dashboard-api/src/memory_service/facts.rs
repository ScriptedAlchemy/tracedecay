//! Memory fact, entity, and overview payloads for the dashboard memory API.

use serde_json::{Map, Value, json};

use super::super::DashboardState;
use super::projection::PROJECTION_POINT_CAP;
use crate::memory::types::MemoryCategory;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::{
    CompatibilityDashboardEntityV1, CompatibilityDashboardFactSummaryV1,
    CompatibilityDashboardHrrStateV1, CompatibilityDashboardMemoryOverviewV1,
    CompatibilityFactProjectionV1, CompatibilityFactTargetV1,
};

pub fn providers_payload() -> Value {
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

fn legacy_fact_id(projection: &CompatibilityFactProjectionV1) -> Option<i64> {
    match projection {
        CompatibilityFactProjectionV1::Available(fact) => fact.legacy_fact_id(),
        CompatibilityFactProjectionV1::Unavailable(_) => None,
    }
}

pub(super) fn target_legacy_fact_id(target: &CompatibilityFactTargetV1) -> Option<i64> {
    target
        .legacy_query()
        .map(tracedecay_store::LegacyFactQuery::legacy_fact_id)
}

/// Converts only an available, mapped compatibility fact. Unavailable or
/// redacted payload fields stay omitted; dashboard handlers never invent them.
pub(super) fn fact_summary_json(summary: &CompatibilityDashboardFactSummaryV1) -> Option<Value> {
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
    row.insert("has_hrr".into(), json!(i64::from(summary.has_hrr_vector)));
    if let Some(content) = fact.content() {
        row.insert("content".into(), json!(content));
    }
    if let Some(category) = fact.category() {
        row.insert(
            "category".into(),
            json!(MemoryCategory::from(category).as_str()),
        );
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

pub(super) fn fact_matches_query(fact: &Value, query: &str) -> bool {
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

pub(super) async fn dashboard_overview(
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

pub async fn fetch_facts(
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

pub async fn fetch_entities(
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
        // The store emits bucket rows named "trust-<n>" (see
        // dashboard_compatibility_named_counts_tx); a bare-number parse fails
        // on every real row and left this histogram permanently zero.
        let name = row.name.strip_prefix("trust-").unwrap_or(&row.name);
        let Ok(idx) = name.parse::<usize>() else {
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

pub async fn overview_payload(state: &DashboardState) -> Result<Value, String> {
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

pub async fn fact_detail_payload(
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
    Ok(fact.map(|mut fact| {
        let entities: Vec<Value> = detail.entities.iter().map(entity_json).collect();
        if let Some(obj) = fact.as_object_mut() {
            obj.insert("entities".into(), json!(entities));
        }
        json!({
            "fact": fact,
            "error": "",
        })
    }))
}
