//! Memory graph payload (fact/category/bank/entity nodes and edges).

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};
use tracedecay_domain::FactId;

use super::super::DashboardState;
use super::facts::{
    dashboard_overview, fact_matches_query, fact_summary_json, target_legacy_fact_id,
};
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::{
    MAX_PROJECT_MEMORY_GRAPH_RELATIONS, ProjectMemoryDashboardEntityV1,
    ProjectMemoryFactProjectionV1, ProjectMemoryFactTargetV1, ProjectMemoryGraphQueryV1,
    ProjectMemoryGraphRelationKindV1, ProjectMemoryGraphTargetV1,
};

/// Resolves a fact target to its legacy numeric id, accepting either a legacy
/// query target or a canonical target (mapped through `canonical_to_legacy`).
fn link_legacy_fact_id(
    target: &ProjectMemoryFactTargetV1,
    canonical_to_legacy: &HashMap<String, i64>,
) -> Option<i64> {
    if let Some(legacy) = target_legacy_fact_id(target) {
        return Some(legacy);
    }
    let canonical = target.canonical_fact_id()?;
    canonical_to_legacy.get(canonical.as_str()).copied()
}

pub async fn graph_payload(
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

    let entity_by_id: HashMap<i64, &ProjectMemoryDashboardEntityV1> = overview
        .entities
        .iter()
        .map(|entity| (entity.target.legacy_entity_id(), entity))
        .collect();
    // Fact-entity links carry canonical fact targets; map them back to the
    // legacy numeric ids the graph nodes are keyed on.
    let canonical_to_legacy: HashMap<String, i64> = overview
        .facts
        .iter()
        .filter_map(|summary| match &summary.fact {
            ProjectMemoryFactProjectionV1::Available(fact) => {
                Some((fact.fact_id().as_str().to_owned(), fact.legacy_fact_id()?))
            }
            ProjectMemoryFactProjectionV1::Unavailable(_) => None,
        })
        .collect();
    let fact_ids: HashSet<i64> = fact_ids.into_iter().collect();
    let graph_roots = canonical_to_legacy
        .iter()
        .filter(|(_, legacy)| fact_ids.contains(legacy))
        .map(|(canonical, _)| FactId::new(canonical.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let memory_graph = if graph_roots.is_empty() {
        None
    } else {
        Some(
            memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
                .map_err(|error| error.to_string())?
                .project_memory_graph(
                    ProjectMemoryGraphQueryV1::new(
                        state.memory_owner.clone(),
                        graph_roots,
                        MAX_PROJECT_MEMORY_GRAPH_RELATIONS,
                    )
                    .map_err(|error| error.to_string())?,
                )
                .await
                .map_err(|error| error.to_string())?,
        )
    };
    for link in &overview.fact_entity_links {
        let Some(fact_id) = link_legacy_fact_id(&link.fact, &canonical_to_legacy) else {
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

    for relation in memory_graph
        .as_ref()
        .into_iter()
        .flat_map(|page| page.relations())
    {
        if relation.kind() == ProjectMemoryGraphRelationKindV1::Mentions {
            continue;
        }
        let target_id = |target: &ProjectMemoryGraphTargetV1| -> Option<(String, Value)> {
            match target {
                ProjectMemoryGraphTargetV1::Fact(fact) => {
                    let legacy = canonical_to_legacy.get(fact.fact_id().as_str())?;
                    fact_ids.contains(legacy).then(|| {
                        let id = format!("fact:{legacy}");
                        (id.clone(), json!({ "id": id, "kind": "fact", "label": format!("#{legacy}"), "fact_id": legacy }))
                    })
                }
                ProjectMemoryGraphTargetV1::Entity(entity) => {
                    let legacy = entity.legacy_entity_id();
                    let id = format!("entity:{legacy}");
                    Some((
                        id.clone(),
                        json!({ "id": id, "kind": "entity", "label": format!("Entity #{legacy}"), "entity_id": legacy }),
                    ))
                }
                ProjectMemoryGraphTargetV1::Assertion { assertion_id, .. } => {
                    let id = format!("assertion:{}", assertion_id.as_str());
                    Some((
                        id.clone(),
                        json!({ "id": id, "kind": "assertion", "label": "Assertion", "assertion_id": assertion_id.as_str() }),
                    ))
                }
                ProjectMemoryGraphTargetV1::RetrievalAnchor { anchor_id, .. } => {
                    let id = format!("anchor:{}", anchor_id.as_str());
                    Some((
                        id.clone(),
                        json!({ "id": id, "kind": "retrieval_anchor", "label": "Evidence anchor", "anchor_id": anchor_id.as_str() }),
                    ))
                }
            }
        };
        let Some((source, source_node)) = target_id(relation.source()) else {
            continue;
        };
        let Some((target, target_node)) = target_id(relation.target()) else {
            continue;
        };
        nodes.entry(source.clone()).or_insert(source_node);
        nodes.entry(target.clone()).or_insert(target_node);
        let kind = match relation.kind() {
            ProjectMemoryGraphRelationKindV1::Supports => "supports",
            ProjectMemoryGraphRelationKindV1::Contradicts => "contradicts",
            ProjectMemoryGraphRelationKindV1::Supersedes => "supersedes",
            ProjectMemoryGraphRelationKindV1::DerivedFrom => "derived_from",
            ProjectMemoryGraphRelationKindV1::ActiveAssertion => "active_assertion",
            ProjectMemoryGraphRelationKindV1::EvidenceAnchor => "evidence_anchor",
            ProjectMemoryGraphRelationKindV1::Mentions => continue,
        };
        edges.push(json!({ "source": source, "target": target, "kind": kind }));
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
