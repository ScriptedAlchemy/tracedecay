//! Canonical project-memory graph payload.
//!
//! Grafeo is the verified relation authority. Canonical fact payloads are
//! hydrated by the fact store through `ProjectMemoryGraphPageV1`; this module
//! never reconstructs topology from dashboard summary rows.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;
use tracedecay_domain::{
    FactAssertionId, FactCategoryV1, FactId, PayloadAccessState, ProjectMemoryGraphRelationKindV1,
    RetrievalAnchorId,
};
use tracedecay_store::{
    FactReadControl, FactStoreError, MAX_PROJECT_MEMORY_GRAPH_RELATIONS,
    ProjectMemoryDashboardEntityV1, ProjectMemoryFactProjectionV1, ProjectMemoryGraphPageV1,
    ProjectMemoryGraphQueryV1, ProjectMemoryGraphTargetV1,
};
use tracedecay_usecases::memory::MemoryApplicationError;

use super::super::read_model::{DashboardCoverageV1, DashboardDomainStateV1};
use super::super::{DashboardHttpRequestControlV1, DashboardState};
use super::facts::dashboard_overview;
use crate::tracedecay::facts::memory_application_for_db;

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryGraphNodeV1 {
    Fact {
        id: String,
        label: String,
        fact_id: FactId,
        payload_access: PayloadAccessState,
        projected_as_of: i64,
        content: Option<String>,
        category: Option<String>,
        trust_score: Option<f64>,
        retrieval_count: Option<u64>,
        helpful_count: Option<u64>,
    },
    Entity {
        id: String,
        label: String,
        entity_id: String,
    },
    Assertion {
        id: String,
        label: String,
        assertion_id: FactAssertionId,
        fact_id: FactId,
    },
    RetrievalAnchor {
        id: String,
        label: String,
        anchor_id: RetrievalAnchorId,
    },
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryGraphEdgeV1 {
    source: String,
    target: String,
    kind: ProjectMemoryGraphRelationKindV1,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryGraphPayloadV1 {
    pub nodes: Vec<MemoryGraphNodeV1>,
    pub edges: Vec<MemoryGraphEdgeV1>,
    pub coverage: DashboardCoverageV1,
    pub fact_universe_count: u64,
    pub fact_candidates_examined: usize,
    pub unavailable_fact_candidates: usize,
    pub root_count: usize,
    pub relation_limit: usize,
    pub relation_count: usize,
}

#[derive(Debug)]
pub enum DashboardGraphReadError {
    Conflicting(String),
    Unavailable(String),
    ResetRequired(String),
    Cancelled(String),
    BudgetExhausted(String),
    TimedOut(String),
    Internal(String),
}

impl DashboardGraphReadError {
    pub const fn state(&self) -> DashboardDomainStateV1 {
        match self {
            Self::Conflicting(_) => DashboardDomainStateV1::Conflicting,
            Self::Unavailable(_) => DashboardDomainStateV1::Offline,
            Self::Cancelled(_) => DashboardDomainStateV1::Cancelled,
            Self::ResetRequired(_) | Self::BudgetExhausted(_) | Self::Internal(_) => {
                DashboardDomainStateV1::Error
            }
            Self::TimedOut(_) => DashboardDomainStateV1::TimedOut,
        }
    }

    pub const fn code(&self) -> &'static str {
        match self {
            Self::Conflicting(_) => "graph_conflict",
            Self::Unavailable(_) => "graph_unavailable",
            Self::ResetRequired(_) => "graph_reset_required",
            Self::Cancelled(_) => "graph_cancelled",
            Self::BudgetExhausted(_) => "graph_budget_exhausted",
            Self::TimedOut(_) => "graph_deadline_exceeded",
            Self::Internal(_) => "graph_error",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Conflicting(message)
            | Self::Unavailable(message)
            | Self::ResetRequired(message)
            | Self::Cancelled(message)
            | Self::BudgetExhausted(message)
            | Self::TimedOut(message)
            | Self::Internal(message) => message,
        }
    }
}

fn graph_authority_error(
    error: MemoryApplicationError,
    request_timed_out: bool,
    request_cancelled: bool,
) -> DashboardGraphReadError {
    let message = error.to_string();
    match error {
        MemoryApplicationError::Store(FactStoreError::GraphDeadlineExceeded) => {
            DashboardGraphReadError::TimedOut(message)
        }
        MemoryApplicationError::Store(FactStoreError::GraphResetRequired { reason, .. }) => {
            DashboardGraphReadError::ResetRequired(reason)
        }
        _ if request_timed_out => DashboardGraphReadError::TimedOut(message),
        _ if request_cancelled => DashboardGraphReadError::Cancelled(message),
        MemoryApplicationError::Store(FactStoreError::GraphConflict) => {
            DashboardGraphReadError::Conflicting(message)
        }
        MemoryApplicationError::Store(FactStoreError::GraphUnavailable) => {
            DashboardGraphReadError::Unavailable(message)
        }
        MemoryApplicationError::Store(FactStoreError::GraphCancelled) => {
            DashboardGraphReadError::Cancelled(message)
        }
        MemoryApplicationError::Store(FactStoreError::GraphBudgetExhausted) => {
            DashboardGraphReadError::BudgetExhausted(message)
        }
        _ => DashboardGraphReadError::Internal(message),
    }
}

fn request_terminal_error(
    control: &DashboardHttpRequestControlV1,
    message: impl Into<String>,
) -> Option<DashboardGraphReadError> {
    let message = message.into();
    if control
        .deadline()
        .is_elapsed_at(crate::application::context::application_observed_at())
    {
        Some(DashboardGraphReadError::TimedOut(message))
    } else if control.cancellation().is_cancelled() {
        Some(DashboardGraphReadError::Cancelled(message))
    } else {
        None
    }
}

fn category_name(category: FactCategoryV1) -> &'static str {
    match category {
        FactCategoryV1::General => "general",
        FactCategoryV1::UserPref => "user_pref",
        FactCategoryV1::Project => "project",
        FactCategoryV1::Tool => "tool",
        FactCategoryV1::Decision => "decision",
        FactCategoryV1::CodeArea => "code_area",
    }
}

fn fact_matches_query(fact: &ProjectMemoryFactProjectionV1, query: &str) -> bool {
    let ProjectMemoryFactProjectionV1::Available(fact) = fact else {
        return false;
    };
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let query = query.to_ascii_lowercase();
    fact.content().to_ascii_lowercase().contains(&query)
        || fact
            .tags()
            .iter()
            .any(|tag| tag.to_ascii_lowercase().contains(&query))
        || fact
            .entities()
            .iter()
            .any(|entity| entity.to_ascii_lowercase().contains(&query))
}

fn fact_node_id(fact_id: &FactId) -> String {
    format!("fact:{}", fact_id.as_str())
}

fn fact_node(fact: &ProjectMemoryFactProjectionV1) -> MemoryGraphNodeV1 {
    let id = fact_node_id(fact.fact_id());
    match fact {
        ProjectMemoryFactProjectionV1::Available(fact) => {
            let telemetry = fact.telemetry();
            MemoryGraphNodeV1::Fact {
                id,
                label: fact.content().to_owned(),
                fact_id: fact.fact_id().clone(),
                payload_access: PayloadAccessState::Eligible,
                projected_as_of: fact.projected_as_of().0,
                content: Some(fact.content().to_owned()),
                category: Some(category_name(fact.category()).to_owned()),
                trust_score: Some(fact.trust().as_f64()),
                retrieval_count: Some(telemetry.retrieval_count()),
                helpful_count: Some(telemetry.helpful_count()),
            }
        }
        ProjectMemoryFactProjectionV1::Unavailable(fact) => MemoryGraphNodeV1::Fact {
            id,
            label: fact.fact_id().as_str().to_owned(),
            fact_id: fact.fact_id().clone(),
            payload_access: fact.payload_access(),
            projected_as_of: fact.status().projected_as_of().0,
            content: None,
            category: None,
            trust_score: None,
            retrieval_count: None,
            helpful_count: None,
        },
    }
}

fn graph_target_node(
    target: &ProjectMemoryGraphTargetV1,
    hydrated_facts: &BTreeMap<FactId, &ProjectMemoryFactProjectionV1>,
    entity_names: &BTreeMap<String, &str>,
) -> Result<(String, MemoryGraphNodeV1), String> {
    match target {
        ProjectMemoryGraphTargetV1::Fact(fact) => {
            let fact_id = fact.fact_id();
            let hydrated = hydrated_facts.get(fact_id).ok_or_else(|| {
                format!(
                    "verified memory graph relation references unhydrated canonical fact {}",
                    fact_id.as_str()
                )
            })?;
            Ok((fact_node_id(fact_id), fact_node(hydrated)))
        }
        ProjectMemoryGraphTargetV1::Entity(entity) => {
            let entity_id = entity.entity();
            let id = format!("entity:{entity_id}");
            let label = entity_names.get(entity_id).copied().unwrap_or(entity_id);
            Ok((
                id.clone(),
                MemoryGraphNodeV1::Entity {
                    id,
                    label: label.to_owned(),
                    entity_id: entity_id.to_owned(),
                },
            ))
        }
        ProjectMemoryGraphTargetV1::Assertion {
            fact_id,
            assertion_id,
            ..
        } => {
            if !hydrated_facts.contains_key(fact_id) {
                return Err(format!(
                    "verified memory graph assertion references unhydrated canonical fact {}",
                    fact_id.as_str()
                ));
            }
            let id = format!("assertion:{}", assertion_id.as_str());
            Ok((
                id.clone(),
                MemoryGraphNodeV1::Assertion {
                    id,
                    label: assertion_id.as_str().to_owned(),
                    assertion_id: assertion_id.clone(),
                    fact_id: fact_id.clone(),
                },
            ))
        }
        ProjectMemoryGraphTargetV1::RetrievalAnchor { anchor_id, .. } => {
            let id = format!("anchor:{}", anchor_id.as_str());
            Ok((
                id.clone(),
                MemoryGraphNodeV1::RetrievalAnchor {
                    id,
                    label: anchor_id.as_str().to_owned(),
                    anchor_id: anchor_id.clone(),
                },
            ))
        }
    }
}

fn render_verified_graph(
    page: &ProjectMemoryGraphPageV1,
    entity_names: &BTreeMap<String, &str>,
) -> Result<(Vec<MemoryGraphNodeV1>, Vec<MemoryGraphEdgeV1>), String> {
    let mut hydrated_facts = BTreeMap::new();
    for fact in page.facts() {
        if hydrated_facts
            .insert(fact.fact_id().clone(), fact)
            .is_some()
        {
            return Err(format!(
                "verified memory graph returned duplicate canonical fact {}",
                fact.fact_id().as_str()
            ));
        }
    }
    let mut nodes: BTreeMap<String, MemoryGraphNodeV1> = hydrated_facts
        .values()
        .map(|fact| (fact_node_id(fact.fact_id()), fact_node(fact)))
        .collect();
    let mut edges = Vec::with_capacity(page.relations().len());

    for relation in page.relations() {
        let (source, source_node) =
            graph_target_node(relation.source(), &hydrated_facts, entity_names)?;
        let (target, target_node) =
            graph_target_node(relation.target(), &hydrated_facts, entity_names)?;
        nodes.entry(source.clone()).or_insert(source_node);
        nodes.entry(target.clone()).or_insert(target_node);
        edges.push(MemoryGraphEdgeV1 {
            source,
            target,
            kind: relation.kind(),
        });
    }

    Ok((nodes.into_values().collect(), edges))
}

fn relation_page_is_proven_complete(relation_count: usize, relation_limit: usize) -> bool {
    relation_count < relation_limit
}

/// Reads and renders the verified project-memory topology.
///
/// The caller must derive `read_control` from the admitted request's live
/// cancellation signal. Supplying a fresh or snapshotted token here would
/// sever disconnect cancellation from the Grafeo read.
pub async fn graph_payload(
    state: &DashboardState,
    query: &str,
    limit: i64,
    request_control: &DashboardHttpRequestControlV1,
    read_control: &FactReadControl,
) -> Result<MemoryGraphPayloadV1, DashboardGraphReadError> {
    if read_control.interrupted() {
        return Err(request_terminal_error(
            request_control,
            "verified memory graph request lifecycle ended",
        )
        .unwrap_or_else(|| {
            DashboardGraphReadError::Cancelled(
                "verified memory graph read was cancelled".to_owned(),
            )
        }));
    }
    let limit = usize::try_from(limit.max(1))
        .map_err(|error| DashboardGraphReadError::Internal(error.to_string()))?;
    let relation_limit = limit.min(MAX_PROJECT_MEMORY_GRAPH_RELATIONS);
    let overview = dashboard_overview(state, 100, limit.min(1000), read_control)
        .await
        .map_err(|error| {
            request_terminal_error(request_control, error.clone())
                .unwrap_or(DashboardGraphReadError::Unavailable(error))
        })?;
    if read_control.interrupted() {
        return Err(request_terminal_error(
            request_control,
            "verified memory graph request lifecycle ended",
        )
        .unwrap_or_else(|| {
            DashboardGraphReadError::Cancelled(
                "verified memory graph read was cancelled".to_owned(),
            )
        }));
    }

    let mut matching_fact_ids = overview
        .facts
        .iter()
        .filter(|summary| fact_matches_query(&summary.fact, query))
        .map(|summary| summary.fact.fact_id().clone());
    let graph_roots: Vec<FactId> = matching_fact_ids.by_ref().take(limit).collect();
    let roots_limited = matching_fact_ids.next().is_some();
    let examined_fact_count = overview.facts.len();
    let unavailable_fact_candidates = overview
        .facts
        .iter()
        .filter(|summary| matches!(&summary.fact, ProjectMemoryFactProjectionV1::Unavailable(_)))
        .count();
    let fact_universe_complete =
        u64::try_from(examined_fact_count).is_ok_and(|examined| examined == overview.fact_count);
    let application = memory_application_for_db(state.memory_owner.clone(), &state.mem_db)
        .map_err(|error| DashboardGraphReadError::Internal(error.to_string()))?;

    if graph_roots.is_empty() {
        let mut omission_reasons = Vec::new();
        if !fact_universe_complete {
            omission_reasons.push("fact_universe_bounded");
        }
        if unavailable_fact_candidates != 0 {
            omission_reasons.push("unavailable_fact_roots");
        }
        let coverage = if omission_reasons.is_empty() {
            DashboardCoverageV1::complete(0, "memory_graph_roots")
        } else {
            let mut coverage = DashboardCoverageV1::unknown();
            coverage.omission_reasons = omission_reasons.into_iter().map(str::to_owned).collect();
            coverage
        };
        return Ok(MemoryGraphPayloadV1 {
            nodes: Vec::new(),
            edges: Vec::new(),
            coverage,
            fact_universe_count: overview.fact_count,
            fact_candidates_examined: examined_fact_count,
            unavailable_fact_candidates,
            root_count: 0,
            relation_limit,
            relation_count: 0,
        });
    }

    let page = application
        .project_memory_graph(
            ProjectMemoryGraphQueryV1::new(
                state.memory_owner.clone(),
                graph_roots.clone(),
                relation_limit,
            )
            .map_err(|error| DashboardGraphReadError::Internal(error.to_string()))?,
            read_control,
        )
        .await
        .map_err(|error| {
            graph_authority_error(
                error,
                request_control
                    .deadline()
                    .is_elapsed_at(crate::application::context::application_observed_at()),
                request_control.cancellation().is_cancelled(),
            )
        })?;

    let entity_names: BTreeMap<String, &str> = overview
        .entities
        .iter()
        .map(|entity: &ProjectMemoryDashboardEntityV1| {
            (entity.target.entity().to_owned(), entity.name.as_str())
        })
        .collect();
    let (nodes, edges) =
        render_verified_graph(&page, &entity_names).map_err(DashboardGraphReadError::Internal)?;

    let mut omission_reasons = BTreeSet::new();
    if !fact_universe_complete {
        omission_reasons.insert("fact_universe_bounded");
    }
    if roots_limited {
        omission_reasons.insert("root_limit_reached");
    }
    if unavailable_fact_candidates != 0 {
        omission_reasons.insert("unavailable_fact_roots");
    }
    if !relation_page_is_proven_complete(page.relations().len(), relation_limit) {
        omission_reasons.insert("relation_limit_reached");
    }
    let coverage = if omission_reasons.is_empty() {
        DashboardCoverageV1::complete(graph_roots.len() as u64, "memory_graph_roots")
    } else {
        let mut coverage = DashboardCoverageV1::unknown();
        coverage.omission_reasons = omission_reasons.into_iter().map(str::to_owned).collect();
        coverage
    };

    Ok(MemoryGraphPayloadV1 {
        nodes,
        edges,
        coverage,
        fact_universe_count: overview.fact_count,
        fact_candidates_examined: examined_fact_count,
        unavailable_fact_candidates,
        root_count: graph_roots.len(),
        relation_limit,
        relation_count: page.relations().len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verified_relation_kind_has_a_distinct_canonical_wire_name() {
        let names = [
            ProjectMemoryGraphRelationKindV1::Supports,
            ProjectMemoryGraphRelationKindV1::Contradicts,
            ProjectMemoryGraphRelationKindV1::Supersedes,
            ProjectMemoryGraphRelationKindV1::DerivedFrom,
            ProjectMemoryGraphRelationKindV1::Mentions,
            ProjectMemoryGraphRelationKindV1::ActiveAssertion,
            ProjectMemoryGraphRelationKindV1::EvidenceAnchor,
        ]
        .map(|kind| {
            serde_json::to_value(kind)
                .expect("serialize relation kind")
                .as_str()
                .expect("relation kind serializes as a string")
                .to_owned()
        });

        assert_eq!(
            names,
            [
                "supports".to_owned(),
                "contradicts".to_owned(),
                "supersedes".to_owned(),
                "derived_from".to_owned(),
                "mentions".to_owned(),
                "active_assertion".to_owned(),
                "evidence_anchor".to_owned(),
            ]
        );
        assert_eq!(names.into_iter().collect::<BTreeSet<_>>().len(), 7);
    }

    #[test]
    fn graph_wire_serializes_canonical_relation_and_payload_access_states() {
        let fact_id =
            FactId::new("fact.v1.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .expect("canonical fact id");
        let node = MemoryGraphNodeV1::Fact {
            id: fact_node_id(&fact_id),
            label: fact_id.as_str().to_owned(),
            fact_id,
            payload_access: PayloadAccessState::Redacted,
            projected_as_of: 42,
            content: None,
            category: None,
            trust_score: None,
            retrieval_count: None,
            helpful_count: None,
        };
        let edge = MemoryGraphEdgeV1 {
            source: "fact:source".to_owned(),
            target: "fact:target".to_owned(),
            kind: ProjectMemoryGraphRelationKindV1::DerivedFrom,
        };

        let node = serde_json::to_value(node).expect("serialize graph node");
        let edge = serde_json::to_value(edge).expect("serialize graph edge");
        assert_eq!(node["kind"], "fact");
        assert_eq!(node["payload_access"], "redacted");
        assert_eq!(edge["kind"], "derived_from");
    }

    #[test]
    fn typed_graph_deadline_and_request_terminal_state_have_precedence() {
        let error = graph_authority_error(
            MemoryApplicationError::Store(FactStoreError::GraphDeadlineExceeded),
            true,
            true,
        );

        assert!(matches!(error, DashboardGraphReadError::TimedOut(_)));

        let error = graph_authority_error(
            MemoryApplicationError::Store(FactStoreError::GraphConflict),
            true,
            true,
        );
        assert!(matches!(error, DashboardGraphReadError::TimedOut(_)));

        let error = graph_authority_error(
            MemoryApplicationError::Store(FactStoreError::GraphUnavailable),
            false,
            true,
        );
        assert!(matches!(error, DashboardGraphReadError::Cancelled(_)));

        let error = graph_authority_error(
            MemoryApplicationError::Store(FactStoreError::GraphResetRequired {
                owner: tracedecay_domain::FactOwnerV1::Profile,
                reason: "verified snapshot is incompatible".to_owned(),
            }),
            true,
            true,
        );
        let DashboardGraphReadError::ResetRequired(message) = error else {
            panic!("typed graph reset must not be collapsed into request timeout or cancellation");
        };
        assert!(message.contains("verified snapshot is incompatible"));
    }

    #[test]
    fn exact_relation_limit_cannot_prove_complete_graph_coverage() {
        assert!(relation_page_is_proven_complete(9, 10));
        assert!(!relation_page_is_proven_complete(10, 10));
    }
}
