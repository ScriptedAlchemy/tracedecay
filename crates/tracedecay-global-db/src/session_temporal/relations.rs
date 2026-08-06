//! Durable session relation DAGs backed by the daemon-owned graph database.
//!
//! Summary text, raw messages, payload references, redaction state, evidence
//! spans, and refresh/retention receipts remain in SQLite. This module stores
//! only typed identities, topology, and ordering properties.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    AgentInstanceId, CopyProofV1, LogicalCopyRecordV1, MessageOccurrenceIdV1, ProjectId,
    RetrievalAnchorId, SessionId, TemporalValidityV1, ThreadId, UtcMicros,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphEntityId, GraphFormatVersion, GraphLabel, GraphNamespace, GraphProjectionId,
    GraphProjectionReadRequest, GraphProjectionTelemetryRequest, GraphProperty, GraphPropertyName,
    GraphRelation, GraphRelationId, GraphRelationKind, GraphWatermark, NeverCancelled,
    ProjectionReplacement, SourceGeneration,
};

mod validation;
pub use validation::validate_projection;

const GRAPH_FORMAT_VERSION: u32 = 2;
const PAGE_SIZE: usize = 1_000;
const ORDINAL_PROPERTY: &str = "ordinal";
const COPY_PROOF_PROPERTY: &str = "copy_proof_json";
const KNOWLEDGE_AT_PROPERTY: &str = "knowledge_at";
const VALID_TIME_PROPERTY: &str = "valid_time_json";
const ENTITY_SCOPE_PREFIX: &str = "session-relations:";
const SUMMARY_KIND: &str = "summary";
const ANCHOR_KIND: &str = "anchor";
const OCCURRENCE_KIND: &str = "occurrence";
const THREAD_KIND: &str = "thread";
const AGENT_KIND: &str = "agent";
const SESSION_LABEL: &str = "session";
const SUMMARY_SOURCE_KIND: &str = "session-summary-source";
const SUMMARY_ANCHOR_SOURCE_KIND: &str = "session-summary-anchor-source";
const SUMMARY_SUCCESSOR_KIND: &str = "session-summary-successor";
const LOGICAL_COPY_KIND: &str = "session-logical-copy";
const THREAD_PARENT_KIND: &str = "session-thread-parent";
const AGENT_PARENT_KIND: &str = "session-agent-parent";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SummarySourceRef {
    Anchor { anchor_id: RetrievalAnchorId },
    Summary { summary_id: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SummaryRelationNode {
    pub summary_id: String,
    pub sources: Vec<SummarySourceRef>,
    pub predecessor_summary_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LogicalCopyRelation {
    pub occurrence_id: MessageOccurrenceIdV1,
    pub copied_from_occurrence_id: MessageOccurrenceIdV1,
    pub proof: CopyProofV1,
    pub knowledge_at: UtcMicros,
    pub valid_time: TemporalValidityV1,
}

impl From<&LogicalCopyRecordV1> for LogicalCopyRelation {
    fn from(copy: &LogicalCopyRecordV1) -> Self {
        Self {
            occurrence_id: copy.occurrence_id.clone(),
            copied_from_occurrence_id: copy.copied_from_occurrence_id.clone(),
            proof: copy.proof.clone(),
            knowledge_at: copy.knowledge_at,
            valid_time: copy.valid_time.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ThreadHierarchyRelation {
    pub parent_thread_id: ThreadId,
    pub child_thread_id: ThreadId,
    pub ordinal: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentHierarchyRelation {
    pub parent_agent_id: AgentInstanceId,
    pub child_agent_id: AgentInstanceId,
    pub ordinal: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionRelationProjection {
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub generation: u64,
    pub summaries: Vec<SummaryRelationNode>,
    pub logical_copies: Vec<LogicalCopyRelation>,
    pub thread_hierarchy: Vec<ThreadHierarchyRelation>,
    pub agent_hierarchy: Vec<AgentHierarchyRelation>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SummarySourceVisitKind {
    Anchor { anchor_id: RetrievalAnchorId },
    Summary { summary_id: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SummarySourceVisit {
    pub parent_summary_id: String,
    pub source: SummarySourceVisitKind,
    pub ordinal: u32,
    pub depth: usize,
}

impl SummarySourceVisit {
    #[must_use]
    pub fn summary(parent: &str, summary: &str, ordinal: u32, depth: usize) -> Self {
        Self {
            parent_summary_id: parent.to_owned(),
            source: SummarySourceVisitKind::Summary {
                summary_id: summary.to_owned(),
            },
            ordinal,
            depth,
        }
    }

    #[must_use]
    pub fn anchor(parent: &str, anchor_id: RetrievalAnchorId, ordinal: u32, depth: usize) -> Self {
        Self {
            parent_summary_id: parent.to_owned(),
            source: SummarySourceVisitKind::Anchor { anchor_id },
            ordinal,
            depth,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SessionRelationError {
    #[error("Session relation projection is invalid")]
    Invalid,
    #[error("Session relation graph contains a cycle")]
    Cycle,
    #[error("Session relation graph was not found")]
    NotFound,
    #[error("Session relation generation publication is pending")]
    Pending,
    #[error("Session relation traversal exhausted its budget")]
    BudgetExhausted,
    #[error("Session relation request was cancelled")]
    Cancelled,
    #[error("Session relation graph is unavailable")]
    Unavailable,
    #[error("Session relation generation conflicts with an existing publication")]
    Conflict,
    #[error("Session relation graph is corrupt")]
    Corrupt,
}

#[derive(Clone)]
pub struct SessionRelationGraphStore {
    database: Arc<GraphDb>,
}

impl std::fmt::Debug for SessionRelationGraphStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionRelationGraphStore")
            .finish_non_exhaustive()
    }
}

impl SessionRelationGraphStore {
    #[must_use]
    pub const fn new(database: Arc<GraphDb>) -> Self {
        Self { database }
    }

    pub fn memory() -> Result<Self, SessionRelationError> {
        GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::new(GRAPH_FORMAT_VERSION)
                .map_err(map_graph_error)?,
            durability: GraphDurability::Memory,
            cancellation: cancellation(),
        })
        .map(Arc::new)
        .map(Self::new)
        .map_err(map_graph_error)
    }

    pub fn replace(
        &self,
        relation_projection: &SessionRelationProjection,
    ) -> Result<GraphWatermark, SessionRelationError> {
        validate_projection(relation_projection)?;
        let namespace = namespace(&relation_projection.project_id)?;
        let projection = projection(
            &relation_projection.session_id,
            relation_projection.generation,
        )?;
        let (entities, relations) = build_graph(relation_projection)?;
        let source_generation = SourceGeneration::new(format!(
            "session-relations:{}:{}",
            relation_projection.session_id.as_str(),
            relation_projection.generation
        ))
        .map_err(map_graph_error)?;
        let projection_digest = serde_json::to_vec(relation_projection)
            .map(|encoded| hex::encode(Sha256::digest(encoded)))
            .map_err(|_| SessionRelationError::Invalid)?;
        let watermark = GraphWatermark::new(format!("session-relations:{projection_digest}"))
            .map_err(map_graph_error)?;
        if let Some(existing) = self
            .database
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: namespace.clone(),
                projection: projection.clone(),
                cancellation: cancellation(),
            })
            .map_err(map_graph_error)?
        {
            return if existing.watermark == watermark {
                Ok(watermark)
            } else {
                Err(SessionRelationError::Conflict)
            };
        }
        self.database
            .replace_projection(ProjectionReplacement {
                namespace,
                projection,
                source_generation,
                next_watermark: watermark.clone(),
                entities,
                relations,
                cancellation: cancellation(),
            })
            .map_err(map_graph_error)?;
        Ok(watermark)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn summary_sources(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        generation: u64,
        root_summary_id: &str,
        max_relations: usize,
    ) -> Result<Vec<SummarySourceVisit>, SessionRelationError> {
        if max_relations == 0 {
            return Err(SessionRelationError::BudgetExhausted);
        }
        let graph = self.projection_graph(project_id, session_id, generation)?;
        let root = summary_entity_id(session_id, generation, root_summary_id)?;
        if !graph.entities.iter().any(|entity| entity.identity == root) {
            return Err(SessionRelationError::NotFound);
        }
        let summary_source_kind =
            GraphRelationKind::new(SUMMARY_SOURCE_KIND).map_err(map_graph_error)?;
        let anchor_source_kind =
            GraphRelationKind::new(SUMMARY_ANCHOR_SOURCE_KIND).map_err(map_graph_error)?;
        let ordinal_property = GraphPropertyName::new(ORDINAL_PROPERTY).map_err(map_graph_error)?;
        let mut outgoing = BTreeMap::<GraphEntityId, Vec<&GraphRelation>>::new();
        for relation in &graph.relations {
            if relation.kind == summary_source_kind || relation.kind == anchor_source_kind {
                outgoing
                    .entry(relation.from.clone())
                    .or_default()
                    .push(relation);
            }
        }
        for edges in outgoing.values_mut() {
            edges.sort_by(|left, right| {
                relation_ordinal(left, &ordinal_property)
                    .cmp(&relation_ordinal(right, &ordinal_property))
                    .then_with(|| left.identity.cmp(&right.identity))
            });
        }
        let mut pending = VecDeque::from([(root, 1_usize)]);
        let mut result = Vec::new();
        while let Some((parent, depth)) = pending.pop_front() {
            let parent_summary_id =
                parse_entity_id(parent.as_str(), session_id, generation, SUMMARY_KIND)?;
            for relation in outgoing.get(&parent).into_iter().flatten() {
                if result.len() == max_relations {
                    return Err(SessionRelationError::BudgetExhausted);
                }
                let ordinal = relation_ordinal(relation, &ordinal_property)
                    .ok_or(SessionRelationError::Corrupt)?;
                let ordinal = u32::try_from(ordinal).map_err(|_| SessionRelationError::Corrupt)?;
                if relation.kind == summary_source_kind {
                    let summary_id = parse_entity_id(
                        relation.to.as_str(),
                        session_id,
                        generation,
                        SUMMARY_KIND,
                    )?;
                    result.push(SummarySourceVisit::summary(
                        parent_summary_id,
                        summary_id,
                        ordinal,
                        depth,
                    ));
                    pending.push_back((relation.to.clone(), depth.saturating_add(1)));
                } else {
                    let anchor =
                        parse_entity_id(relation.to.as_str(), session_id, generation, ANCHOR_KIND)?;
                    let anchor_id = RetrievalAnchorId::new(anchor)
                        .map_err(|_| SessionRelationError::Corrupt)?;
                    result.push(SummarySourceVisit::anchor(
                        parent_summary_id,
                        anchor_id,
                        ordinal,
                        depth,
                    ));
                }
            }
        }
        Ok(result)
    }

    pub fn load_projection(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        generation: u64,
    ) -> Result<SessionRelationProjection, SessionRelationError> {
        let graph = self.projection_graph(project_id, session_id, generation)?;
        decode_projection(project_id, session_id, generation, graph)
    }

    fn projection_graph(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        generation: u64,
    ) -> Result<ProjectionGraph, SessionRelationError> {
        let namespace = namespace(project_id)?;
        let projection = projection(session_id, generation)?;
        let snapshot = self.database.snapshot().map_err(map_graph_error)?;
        if snapshot
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: namespace.clone(),
                projection: projection.clone(),
                cancellation: cancellation(),
            })
            .map_err(map_graph_error)?
            .is_none()
        {
            return Err(SessionRelationError::Pending);
        }
        read_projection(&snapshot, &namespace, &projection)
    }
}

struct ProjectionGraph {
    entities: Vec<GraphEntity>,
    relations: Vec<GraphRelation>,
}

fn decode_projection(
    project_id: &ProjectId,
    session_id: &SessionId,
    generation: u64,
    graph: ProjectionGraph,
) -> Result<SessionRelationProjection, SessionRelationError> {
    let summary_label = GraphLabel::new("session-summary").map_err(map_graph_error)?;
    let mut summaries = BTreeMap::<String, SummaryRelationNode>::new();
    for entity in &graph.entities {
        if entity.labels.contains(&summary_label) {
            let summary_id = parse_entity_id(
                entity.identity.as_str(),
                session_id,
                generation,
                SUMMARY_KIND,
            )?;
            summaries.insert(
                summary_id.to_owned(),
                SummaryRelationNode {
                    summary_id: summary_id.to_owned(),
                    sources: Vec::new(),
                    predecessor_summary_id: None,
                },
            );
        }
    }
    let summary_source_kind =
        GraphRelationKind::new(SUMMARY_SOURCE_KIND).map_err(map_graph_error)?;
    let anchor_source_kind =
        GraphRelationKind::new(SUMMARY_ANCHOR_SOURCE_KIND).map_err(map_graph_error)?;
    let successor_kind = GraphRelationKind::new(SUMMARY_SUCCESSOR_KIND).map_err(map_graph_error)?;
    let logical_copy_kind = GraphRelationKind::new(LOGICAL_COPY_KIND).map_err(map_graph_error)?;
    let thread_parent_kind = GraphRelationKind::new(THREAD_PARENT_KIND).map_err(map_graph_error)?;
    let agent_parent_kind = GraphRelationKind::new(AGENT_PARENT_KIND).map_err(map_graph_error)?;
    let ordinal_property = GraphPropertyName::new(ORDINAL_PROPERTY).map_err(map_graph_error)?;
    let proof_property = GraphPropertyName::new(COPY_PROOF_PROPERTY).map_err(map_graph_error)?;
    let knowledge_property =
        GraphPropertyName::new(KNOWLEDGE_AT_PROPERTY).map_err(map_graph_error)?;
    let valid_time_property =
        GraphPropertyName::new(VALID_TIME_PROPERTY).map_err(map_graph_error)?;
    let mut ordered_sources = BTreeMap::<String, Vec<(u32, SummarySourceRef)>>::new();
    let mut logical_copies = Vec::new();
    let mut thread_hierarchy = Vec::new();
    let mut agent_hierarchy = Vec::new();
    for relation in &graph.relations {
        if relation.kind == summary_source_kind || relation.kind == anchor_source_kind {
            let summary_id =
                parse_entity_id(relation.from.as_str(), session_id, generation, SUMMARY_KIND)?;
            let ordinal = relation_ordinal(relation, &ordinal_property)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or(SessionRelationError::Corrupt)?;
            let source = if relation.kind == summary_source_kind {
                SummarySourceRef::Summary {
                    summary_id: parse_entity_id(
                        relation.to.as_str(),
                        session_id,
                        generation,
                        SUMMARY_KIND,
                    )?
                    .to_owned(),
                }
            } else {
                SummarySourceRef::Anchor {
                    anchor_id: RetrievalAnchorId::new(parse_entity_id(
                        relation.to.as_str(),
                        session_id,
                        generation,
                        ANCHOR_KIND,
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                }
            };
            ordered_sources
                .entry(summary_id.to_owned())
                .or_default()
                .push((ordinal, source));
        } else if relation.kind == successor_kind {
            let predecessor =
                parse_entity_id(relation.from.as_str(), session_id, generation, SUMMARY_KIND)?;
            let successor =
                parse_entity_id(relation.to.as_str(), session_id, generation, SUMMARY_KIND)?;
            let node = summaries
                .get_mut(successor)
                .ok_or(SessionRelationError::Corrupt)?;
            if node
                .predecessor_summary_id
                .replace(predecessor.to_owned())
                .is_some()
            {
                return Err(SessionRelationError::Corrupt);
            }
        } else if relation.kind == logical_copy_kind {
            let occurrence_id = MessageOccurrenceIdV1::new(parse_entity_id(
                relation.from.as_str(),
                session_id,
                generation,
                OCCURRENCE_KIND,
            )?)
            .map_err(|_| SessionRelationError::Corrupt)?;
            let copied_from_occurrence_id = MessageOccurrenceIdV1::new(parse_entity_id(
                relation.to.as_str(),
                session_id,
                generation,
                OCCURRENCE_KIND,
            )?)
            .map_err(|_| SessionRelationError::Corrupt)?;
            let proof = match relation.properties.get(&proof_property) {
                Some(GraphProperty::String(value)) => {
                    serde_json::from_str(value).map_err(|_| SessionRelationError::Corrupt)?
                }
                _ => return Err(SessionRelationError::Corrupt),
            };
            let knowledge_at = match relation.properties.get(&knowledge_property) {
                Some(GraphProperty::I64(value)) => UtcMicros(*value),
                _ => return Err(SessionRelationError::Corrupt),
            };
            let valid_time = match relation.properties.get(&valid_time_property) {
                Some(GraphProperty::String(value)) => {
                    serde_json::from_str(value).map_err(|_| SessionRelationError::Corrupt)?
                }
                _ => return Err(SessionRelationError::Corrupt),
            };
            logical_copies.push(LogicalCopyRelation {
                occurrence_id,
                copied_from_occurrence_id,
                proof,
                knowledge_at,
                valid_time,
            });
        } else if relation.kind == thread_parent_kind {
            thread_hierarchy.push(ThreadHierarchyRelation {
                parent_thread_id: ThreadId::new(parse_entity_id(
                    relation.from.as_str(),
                    session_id,
                    generation,
                    THREAD_KIND,
                )?)
                .map_err(|_| SessionRelationError::Corrupt)?,
                child_thread_id: ThreadId::new(parse_entity_id(
                    relation.to.as_str(),
                    session_id,
                    generation,
                    THREAD_KIND,
                )?)
                .map_err(|_| SessionRelationError::Corrupt)?,
                ordinal: relation_ordinal(relation, &ordinal_property)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(SessionRelationError::Corrupt)?,
            });
        } else if relation.kind == agent_parent_kind {
            agent_hierarchy.push(AgentHierarchyRelation {
                parent_agent_id: AgentInstanceId::new(parse_entity_id(
                    relation.from.as_str(),
                    session_id,
                    generation,
                    AGENT_KIND,
                )?)
                .map_err(|_| SessionRelationError::Corrupt)?,
                child_agent_id: AgentInstanceId::new(parse_entity_id(
                    relation.to.as_str(),
                    session_id,
                    generation,
                    AGENT_KIND,
                )?)
                .map_err(|_| SessionRelationError::Corrupt)?,
                ordinal: relation_ordinal(relation, &ordinal_property)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(SessionRelationError::Corrupt)?,
            });
        }
    }
    for (summary_id, mut sources) in ordered_sources {
        sources.sort_by_key(|(ordinal, _)| *ordinal);
        let node = summaries
            .get_mut(&summary_id)
            .ok_or(SessionRelationError::Corrupt)?;
        node.sources = sources.into_iter().map(|(_, source)| source).collect();
    }
    Ok(SessionRelationProjection {
        project_id: project_id.clone(),
        session_id: session_id.clone(),
        generation,
        summaries: summaries.into_values().collect(),
        logical_copies,
        thread_hierarchy,
        agent_hierarchy,
    })
}

fn read_projection(
    snapshot: &tracedecay_graph_db::GraphSnapshot,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> Result<ProjectionGraph, SessionRelationError> {
    let mut entities = Vec::new();
    let mut relations = Vec::new();
    let mut after_entity = None;
    let mut after_relation = None;
    let mut read_entities = true;
    let mut read_relations = true;
    while read_entities || read_relations {
        let page = snapshot
            .read_projection(GraphProjectionReadRequest {
                namespace: namespace.clone(),
                projection: projection.clone(),
                after_entity: after_entity.clone(),
                after_relation: after_relation.clone(),
                max_entities: if read_entities { PAGE_SIZE } else { 0 },
                max_relations: if read_relations { PAGE_SIZE } else { 0 },
                cancellation: cancellation(),
            })
            .map_err(map_graph_error)?;
        entities.extend(page.entities);
        relations.extend(page.relations);
        if read_entities {
            after_entity = page.next_entity;
            read_entities = after_entity.is_some();
        }
        if read_relations {
            after_relation = page.next_relation;
            read_relations = after_relation.is_some();
        }
    }
    Ok(ProjectionGraph {
        entities,
        relations,
    })
}

fn build_graph(
    projection: &SessionRelationProjection,
) -> Result<(Vec<GraphEntity>, Vec<GraphRelation>), SessionRelationError> {
    let mut entities = BTreeMap::<GraphEntityId, GraphEntity>::new();
    let mut relations = Vec::new();
    let session_id = &projection.session_id;
    let generation = projection.generation;
    insert_entity(&mut entities, session_entity_id(session_id)?, SESSION_LABEL)?;
    for summary in &projection.summaries {
        insert_entity(
            &mut entities,
            summary_entity_id(session_id, generation, &summary.summary_id)?,
            "session-summary",
        )?;
        for (ordinal, source) in summary.sources.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).map_err(|_| SessionRelationError::Invalid)?;
            let (target, kind) = match source {
                SummarySourceRef::Anchor { anchor_id } => (
                    anchor_entity_id(session_id, generation, anchor_id)?,
                    GraphRelationKind::new(SUMMARY_ANCHOR_SOURCE_KIND).map_err(map_graph_error)?,
                ),
                SummarySourceRef::Summary { summary_id } => (
                    summary_entity_id(session_id, generation, summary_id)?,
                    GraphRelationKind::new(SUMMARY_SOURCE_KIND).map_err(map_graph_error)?,
                ),
            };
            insert_entity(
                &mut entities,
                target.clone(),
                if matches!(source, SummarySourceRef::Anchor { .. }) {
                    "retrieval-anchor-reference"
                } else {
                    "session-summary"
                },
            )?;
            relations.push(relation(
                relation_id(
                    session_id,
                    generation,
                    &format!("summary-source:{}:{ordinal}", summary.summary_id),
                ),
                summary_entity_id(session_id, generation, &summary.summary_id)?,
                target,
                kind,
                ordinal,
            )?);
        }
        if let Some(predecessor) = &summary.predecessor_summary_id {
            let predecessor_id = summary_entity_id(session_id, generation, predecessor)?;
            insert_entity(
                &mut entities,
                predecessor_id.clone(),
                "session-summary-reference",
            )?;
            relations.push(relation(
                relation_id(
                    session_id,
                    generation,
                    &format!("summary-successor:{}:{}", predecessor, summary.summary_id),
                ),
                predecessor_id,
                summary_entity_id(session_id, generation, &summary.summary_id)?,
                GraphRelationKind::new(SUMMARY_SUCCESSOR_KIND).map_err(map_graph_error)?,
                0,
            )?);
        }
    }
    for copy in &projection.logical_copies {
        let from = occurrence_entity_id(session_id, generation, &copy.occurrence_id)?;
        let to = occurrence_entity_id(session_id, generation, &copy.copied_from_occurrence_id)?;
        insert_entity(&mut entities, from.clone(), "session-occurrence-reference")?;
        insert_entity(&mut entities, to.clone(), "session-occurrence-reference")?;
        let proof =
            serde_json::to_string(&copy.proof).map_err(|_| SessionRelationError::Invalid)?;
        let valid_time =
            serde_json::to_string(&copy.valid_time).map_err(|_| SessionRelationError::Invalid)?;
        relations.push(relation_with_properties(
            relation_id(
                session_id,
                generation,
                &format!(
                    "logical-copy:{}:{}",
                    copy.occurrence_id.as_str(),
                    copy.copied_from_occurrence_id.as_str()
                ),
            ),
            from,
            to,
            GraphRelationKind::new(LOGICAL_COPY_KIND).map_err(map_graph_error)?,
            BTreeMap::from([
                (
                    GraphPropertyName::new(COPY_PROOF_PROPERTY).map_err(map_graph_error)?,
                    GraphProperty::String(proof),
                ),
                (
                    GraphPropertyName::new(KNOWLEDGE_AT_PROPERTY).map_err(map_graph_error)?,
                    GraphProperty::I64(copy.knowledge_at.0),
                ),
                (
                    GraphPropertyName::new(VALID_TIME_PROPERTY).map_err(map_graph_error)?,
                    GraphProperty::String(valid_time),
                ),
            ]),
        )?);
    }
    for edge in &projection.thread_hierarchy {
        let from = thread_entity_id(session_id, generation, &edge.parent_thread_id)?;
        let to = thread_entity_id(session_id, generation, &edge.child_thread_id)?;
        insert_entity(&mut entities, from.clone(), "session-thread-reference")?;
        insert_entity(&mut entities, to.clone(), "session-thread-reference")?;
        relations.push(relation(
            relation_id(
                session_id,
                generation,
                &format!(
                    "thread-parent:{}:{}",
                    edge.parent_thread_id.as_str(),
                    edge.child_thread_id.as_str()
                ),
            ),
            from,
            to,
            GraphRelationKind::new(THREAD_PARENT_KIND).map_err(map_graph_error)?,
            edge.ordinal,
        )?);
    }
    for edge in &projection.agent_hierarchy {
        let from = agent_entity_id(session_id, generation, &edge.parent_agent_id)?;
        let to = agent_entity_id(session_id, generation, &edge.child_agent_id)?;
        insert_entity(&mut entities, from.clone(), "session-agent-reference")?;
        insert_entity(&mut entities, to.clone(), "session-agent-reference")?;
        relations.push(relation(
            relation_id(
                session_id,
                generation,
                &format!(
                    "agent-parent:{}:{}",
                    edge.parent_agent_id.as_str(),
                    edge.child_agent_id.as_str()
                ),
            ),
            from,
            to,
            GraphRelationKind::new(AGENT_PARENT_KIND).map_err(map_graph_error)?,
            edge.ordinal,
        )?);
    }
    Ok((entities.into_values().collect(), relations))
}

/// Returns the shared cross-domain graph identity for one canonical session.
pub fn session_entity_id(session_id: &SessionId) -> Result<GraphEntityId, SessionRelationError> {
    GraphEntityId::new(format!("session:{}", session_id.as_str())).map_err(map_graph_error)
}

fn insert_entity(
    entities: &mut BTreeMap<GraphEntityId, GraphEntity>,
    identity: GraphEntityId,
    label: &str,
) -> Result<(), SessionRelationError> {
    let label = GraphLabel::new(label).map_err(map_graph_error)?;
    if let Some(entity) = entities.get_mut(&identity) {
        entity.labels.insert(label);
        return Ok(());
    }
    let entity = GraphEntity::new(identity.clone(), BTreeSet::from([label]), BTreeMap::new())
        .map_err(map_graph_error)?;
    entities.insert(identity, entity);
    Ok(())
}

fn relation(
    identity: String,
    from: GraphEntityId,
    to: GraphEntityId,
    kind: GraphRelationKind,
    ordinal: u32,
) -> Result<GraphRelation, SessionRelationError> {
    relation_with_properties(
        identity,
        from,
        to,
        kind,
        BTreeMap::from([(
            GraphPropertyName::new(ORDINAL_PROPERTY).map_err(map_graph_error)?,
            GraphProperty::I64(i64::from(ordinal)),
        )]),
    )
}

fn relation_with_properties(
    identity: String,
    from: GraphEntityId,
    to: GraphEntityId,
    kind: GraphRelationKind,
    properties: BTreeMap<GraphPropertyName, GraphProperty>,
) -> Result<GraphRelation, SessionRelationError> {
    GraphRelation::new(
        GraphRelationId::new(identity).map_err(map_graph_error)?,
        from,
        to,
        kind,
        properties,
    )
    .map_err(map_graph_error)
}

fn namespace(project_id: &ProjectId) -> Result<GraphNamespace, SessionRelationError> {
    GraphNamespace::new(format!("project:{}", project_id.as_str())).map_err(map_graph_error)
}

fn projection(
    session_id: &SessionId,
    generation: u64,
) -> Result<GraphProjectionId, SessionRelationError> {
    if generation == 0 {
        return Err(SessionRelationError::Invalid);
    }
    GraphProjectionId::new(format!(
        "session-relations:{}:{generation}",
        session_id.as_str()
    ))
    .map_err(map_graph_error)
}

fn summary_entity_id(
    session_id: &SessionId,
    generation: u64,
    summary_id: &str,
) -> Result<GraphEntityId, SessionRelationError> {
    if summary_id.trim().is_empty() {
        return Err(SessionRelationError::Invalid);
    }
    entity_id(session_id, generation, SUMMARY_KIND, summary_id)
}

fn anchor_entity_id(
    session_id: &SessionId,
    generation: u64,
    anchor_id: &RetrievalAnchorId,
) -> Result<GraphEntityId, SessionRelationError> {
    entity_id(session_id, generation, ANCHOR_KIND, anchor_id.as_str())
}

fn occurrence_entity_id(
    session_id: &SessionId,
    generation: u64,
    occurrence_id: &MessageOccurrenceIdV1,
) -> Result<GraphEntityId, SessionRelationError> {
    entity_id(
        session_id,
        generation,
        OCCURRENCE_KIND,
        occurrence_id.as_str(),
    )
}

fn thread_entity_id(
    session_id: &SessionId,
    generation: u64,
    thread_id: &ThreadId,
) -> Result<GraphEntityId, SessionRelationError> {
    entity_id(session_id, generation, THREAD_KIND, thread_id.as_str())
}

fn agent_entity_id(
    session_id: &SessionId,
    generation: u64,
    agent_id: &AgentInstanceId,
) -> Result<GraphEntityId, SessionRelationError> {
    entity_id(session_id, generation, AGENT_KIND, agent_id.as_str())
}

fn entity_id(
    session_id: &SessionId,
    generation: u64,
    kind: &str,
    domain_id: &str,
) -> Result<GraphEntityId, SessionRelationError> {
    GraphEntityId::new(format!(
        "{ENTITY_SCOPE_PREFIX}{}:{generation}:{kind}:{domain_id}",
        session_id.as_str()
    ))
    .map_err(map_graph_error)
}

fn relation_id(session_id: &SessionId, generation: u64, domain_id: &str) -> String {
    format!(
        "{ENTITY_SCOPE_PREFIX}{}:{generation}:relation:{domain_id}",
        session_id.as_str()
    )
}

fn parse_entity_id<'a>(
    value: &'a str,
    session_id: &SessionId,
    generation: u64,
    kind: &str,
) -> Result<&'a str, SessionRelationError> {
    let prefix = format!(
        "{ENTITY_SCOPE_PREFIX}{}:{generation}:{kind}:",
        session_id.as_str()
    );
    value
        .strip_prefix(&prefix)
        .filter(|value| !value.is_empty())
        .ok_or(SessionRelationError::Corrupt)
}

fn relation_ordinal(relation: &GraphRelation, property: &GraphPropertyName) -> Option<i64> {
    match relation.properties.get(property)? {
        GraphProperty::I64(value) => Some(*value),
        _ => None,
    }
}

fn cancellation() -> Arc<dyn GraphCancellation> {
    Arc::new(NeverCancelled)
}

fn map_graph_error(error: GraphDbError) -> SessionRelationError {
    match error {
        GraphDbError::Cancelled => SessionRelationError::Cancelled,
        GraphDbError::BudgetExhausted => SessionRelationError::BudgetExhausted,
        GraphDbError::Corrupt { .. }
        | GraphDbError::ResetRequired { .. }
        | GraphDbError::DurabilityUncertain { .. } => SessionRelationError::Corrupt,
        GraphDbError::Conflict => SessionRelationError::Conflict,
        GraphDbError::InvalidRequest { .. } => SessionRelationError::Invalid,
        GraphDbError::Unavailable { .. } | GraphDbError::Closed => {
            SessionRelationError::Unavailable
        }
    }
}
