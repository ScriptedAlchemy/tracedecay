//! Memory fact, entity, and overview payloads for the dashboard memory API.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value, json};

use super::super::DashboardState;
use super::super::read_model::DashboardCoverageCompletenessV1;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_application::memory::{FactSearchGraphCoverageV1, FactSearchGraphDegradationV1};
use tracedecay_domain::{FactId, PayloadAccessState};
use tracedecay_store::{
    FactReadControl, ProjectMemoryDashboardEntityV1, ProjectMemoryDashboardFactSummaryV1,
    ProjectMemoryDashboardMemoryOverviewV1, ProjectMemoryFactProjectionV1,
    ProjectMemoryFactSearchGraphCoverageV1, ProjectMemoryFactSearchKindV1,
    ProjectMemoryFactSearchQuery,
};

pub const MEMORY_FACT_LIMIT_MINIMUM: i64 = 1;
pub const MEMORY_FACT_LIMIT_MAXIMUM: i64 = 100;

pub fn providers_payload() -> Value {
    json!({
        "memory_provider": "tracedecay",
        "memory_options": [
            {
                "name": "tracedecay",
                "description": "TraceDecay canonical project fact store with query-time holographic retrieval."
            }
        ],
        "context_engine": "tracedecay",
        "context_options": [],
        "plugin_context_engine": null,
        "curator_tools": { "enabled": false, "count": 0, "available": 0, "tools": [] },
    })
}

/// Converts one canonical fact projection without fabricating payload fields.
/// Unavailable facts remain visible to operators through their exact
/// payload-access state, but never receive content, category, or metadata.
pub(super) fn fact_summary_json(summary: &ProjectMemoryDashboardFactSummaryV1) -> Value {
    match &summary.fact {
        ProjectMemoryFactProjectionV1::Available(fact) => {
            let telemetry = fact.telemetry();
            let mut row = Map::new();
            row.insert("fact_id".into(), json!(fact.fact_id().as_str()));
            row.insert("payload_access".into(), json!(PayloadAccessState::Eligible));
            row.insert("trust_score".into(), json!(fact.trust().as_f64()));
            row.insert("retrieval_count".into(), json!(telemetry.retrieval_count()));
            row.insert("access_count".into(), json!(telemetry.access_count()));
            row.insert("helpful_count".into(), json!(telemetry.helpful_count()));
            row.insert("unhelpful_count".into(), json!(telemetry.unhelpful_count()));
            row.insert("created_at".into(), json!(telemetry.created_at().0));
            row.insert("updated_at".into(), json!(telemetry.updated_at().0));
            row.insert("projected_as_of".into(), json!(fact.projected_as_of().0));
            row.insert(
                "last_recalled_at".into(),
                json!(telemetry.last_recalled_at().map(|value| value.0)),
            );
            row.insert("content".into(), json!(fact.content()));
            row.insert("category".into(), json!(fact.category()));
            row.insert("tags".into(), json!(fact.tags()));
            row.insert("entities".into(), json!(fact.entities()));
            row.insert("metadata".into(), fact.metadata().clone());
            if let Some(source_label) = fact.source_label() {
                row.insert("source_label".into(), json!(source_label));
            }
            Value::Object(row)
        }
        ProjectMemoryFactProjectionV1::Unavailable(fact) => json!({
            "fact_id": fact.fact_id().as_str(),
            "payload_access": fact.payload_access(),
            "projected_as_of": fact.status().projected_as_of().0,
        }),
    }
}

fn entity_json(entity: &ProjectMemoryDashboardEntityV1) -> Value {
    json!({
        "entity_id": entity.target.entity(),
        "name": entity.name,
        "fact_count": entity.fact_count,
    })
}

pub(super) async fn dashboard_overview(
    state: &DashboardState,
    fact_limit: usize,
    graph_limit: usize,
    read_control: &FactReadControl,
) -> Result<ProjectMemoryDashboardMemoryOverviewV1, String> {
    memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?
        .dashboard_overview(fact_limit, graph_limit, read_control)
        .await
        .map_err(|error| error.to_string())
}

pub struct FactRowsPayload {
    pub rows: Vec<Value>,
    pub coverage: MemoryFactsCoverageV1,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryFactsCoverageV1 {
    pub completeness: DashboardCoverageCompletenessV1,
    #[schemars(range(min = MEMORY_FACT_LIMIT_MINIMUM, max = MEMORY_FACT_LIMIT_MAXIMUM))]
    pub limit: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<FactSearchGraphCoverageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub examined: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligible: Option<u64>,
}

pub struct EntityRowsPayload {
    pub rows: Vec<Value>,
    pub bounded: bool,
}

fn public_graph_coverage(
    coverage: ProjectMemoryFactSearchGraphCoverageV1,
) -> FactSearchGraphCoverageV1 {
    match coverage {
        ProjectMemoryFactSearchGraphCoverageV1::NotApplicable => {
            FactSearchGraphCoverageV1::NotApplicable
        }
        ProjectMemoryFactSearchGraphCoverageV1::NotMounted => {
            FactSearchGraphCoverageV1::NotMounted
        }
        ProjectMemoryFactSearchGraphCoverageV1::Complete {
            root_count,
            relation_count,
            expanded_fact_count,
        } => FactSearchGraphCoverageV1::Complete {
            root_count,
            relation_count,
            expanded_fact_count,
        },
        ProjectMemoryFactSearchGraphCoverageV1::Degraded { reason } => {
            FactSearchGraphCoverageV1::Degraded {
                reason: match reason {
                    tracedecay_store::ProjectMemoryFactSearchGraphDegradationV1::Conflict => {
                        FactSearchGraphDegradationV1::Conflict
                    }
                    tracedecay_store::ProjectMemoryFactSearchGraphDegradationV1::Unavailable => {
                        FactSearchGraphDegradationV1::Unavailable
                    }
                    tracedecay_store::ProjectMemoryFactSearchGraphDegradationV1::BudgetExhausted => {
                        FactSearchGraphDegradationV1::BudgetExhausted
                    }
                    tracedecay_store::ProjectMemoryFactSearchGraphDegradationV1::DeadlineExceeded => {
                        FactSearchGraphDegradationV1::DeadlineExceeded
                    }
                },
            }
        }
    }
}

pub async fn fetch_facts(
    state: &DashboardState,
    query: &str,
    limit: i64,
    read_control: &FactReadControl,
) -> Result<FactRowsPayload, String> {
    if read_control.interrupted() {
        return Err("memory fact read was cancelled".to_owned());
    }
    let limit = usize::try_from(limit.clamp(MEMORY_FACT_LIMIT_MINIMUM, MEMORY_FACT_LIMIT_MAXIMUM))
        .map_err(|error| error.to_string())?;
    if !query.trim().is_empty() {
        let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
            .map_err(|error| error.to_string())?;
        let page = application
            .search_project_memory_facts(
                ProjectMemoryFactSearchQuery::new(
                    state.memory_owner.clone(),
                    ProjectMemoryFactSearchKindV1::Search,
                    Some(query.trim().to_owned()),
                    None,
                    limit,
                )
                .map_err(|error| error.to_string())?,
                read_control,
            )
            .await
            .map_err(|error| error.to_string())?;
        let rows = page
            .hits()
            .iter()
            .map(|hit| {
                let mut row = fact_summary_json(&ProjectMemoryDashboardFactSummaryV1 {
                    fact: ProjectMemoryFactProjectionV1::Available(Box::new(hit.fact().clone())),
                });
                if let Some(object) = row.as_object_mut() {
                    object.insert("score_millionths".into(), json!(hit.score_millionths()));
                    if let Some(why) = hit.why() {
                        object.insert("why".into(), json!(why));
                    }
                }
                row
            })
            .collect::<Vec<_>>();
        let graph_coverage = public_graph_coverage(page.graph_coverage());
        return Ok(FactRowsPayload {
            coverage: MemoryFactsCoverageV1 {
                completeness: if page.next_after().is_some() {
                    DashboardCoverageCompletenessV1::Partial
                } else {
                    DashboardCoverageCompletenessV1::Complete
                },
                limit,
                graph: Some(graph_coverage),
                examined: None,
                eligible: None,
            },
            rows,
        });
    }
    let overview = dashboard_overview(state, limit, 1, read_control).await?;
    if read_control.interrupted() {
        return Err("memory fact read was cancelled".to_owned());
    }
    let rows = overview
        .facts
        .iter()
        .map(fact_summary_json)
        .take(limit)
        .collect();
    Ok(FactRowsPayload {
        coverage: MemoryFactsCoverageV1 {
            completeness: if overview.fact_count > overview.facts.len() as u64 {
                DashboardCoverageCompletenessV1::Partial
            } else {
                DashboardCoverageCompletenessV1::Complete
            },
            limit,
            graph: None,
            examined: Some(overview.facts.len()),
            eligible: Some(overview.fact_count),
        },
        rows,
    })
}

pub async fn fetch_entities(
    state: &DashboardState,
    limit: i64,
    read_control: &FactReadControl,
) -> Result<EntityRowsPayload, String> {
    let limit = usize::try_from(limit.max(1)).map_err(|error| error.to_string())?;
    let overview = dashboard_overview(state, 1, limit.min(1000), read_control).await?;
    let rows: Vec<Value> = overview
        .entities
        .iter()
        .map(entity_json)
        .take(limit)
        .collect();
    Ok(EntityRowsPayload {
        bounded: overview.entity_count > rows.len() as u64,
        rows,
    })
}

fn trust_histogram(overview: &ProjectMemoryDashboardMemoryOverviewV1) -> Vec<Value> {
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
        // The canonical dashboard authority emits bucket rows as `trust-<n>`.
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

pub async fn overview_payload(
    state: &DashboardState,
    read_control: &FactReadControl,
) -> Result<Value, String> {
    let overview = dashboard_overview(state, 100, 1000, read_control).await?;
    let categories: Vec<Value> = overview
        .categories
        .iter()
        .map(|row| json!({ "category": row.name, "count": row.count }))
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
        "categories": categories,
        "trust_histogram": trust_histogram(&overview),
        "growth": growth,
    }))
}

pub async fn fact_detail_payload(
    state: &DashboardState,
    fact_id: FactId,
    read_control: &FactReadControl,
) -> Result<Option<Value>, String> {
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| error.to_string())?;
    let Some(detail) = application
        .dashboard_fact_detail(fact_id, read_control)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut fact = fact_summary_json(&ProjectMemoryDashboardFactSummaryV1 { fact: detail.fact });
    let entities: Vec<Value> = detail.entities.iter().map(entity_json).collect();
    if let Some(obj) = fact.as_object_mut() {
        obj.insert("linked_entities".into(), json!(entities));
    }
    Ok(Some(json!({
        "fact": fact,
        "error": "",
    })))
}
