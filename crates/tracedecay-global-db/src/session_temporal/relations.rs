//! Typed session topology stored in the daemon-owned native Grafeo database.
//!
//! Graph entities contain only durable identities. Summary text, message
//! content, payload references, redaction state, and evidence bodies remain in
//! their owning session store and are hydrated there after graph traversal.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::{
    AgentInstanceId, CopyProofV1, MessageOccurrenceIdV1, ProjectId, RetrievalAnchorId, SessionId,
    TemporalValidityV1, ThreadId, UserProfileId, UtcMicros,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDb, GraphDbError, GraphEntity, GraphEntityId, GraphLabel,
    GraphNamespace, GraphProjectionId, GraphProjectionTelemetryRequest, GraphProperty,
    GraphPropertyName, GraphRelation, GraphRelationId, GraphRelationKind, GraphWatermark,
    NeverCancelled, ProjectionReplacement, SourceGeneration,
};

const SUMMARY_SOURCE_KIND: &str = "session-summary-source";
const SUMMARY_ANCHOR_SOURCE_KIND: &str = "session-summary-anchor-source";
const SUMMARY_SUCCESSOR_KIND: &str = "session-summary-successor";
const SUMMARY_PREDECESSOR_KIND: &str = "session-summary-predecessor";
const LOGICAL_COPY_KIND: &str = "session-logical-copy";
const THREAD_PARENT_KIND: &str = "session-thread-parent";
const THREAD_CHILD_OF_KIND: &str = "session-thread-child-of";
const AGENT_PARENT_KIND: &str = "session-agent-parent";
const AGENT_CHILD_OF_KIND: &str = "session-agent-child-of";
const SESSION_PARENT_KIND: &str = "session-parent";
const WORKFLOW_AGENT_KIND: &str = "session-workflow-agent";
const ORDINAL_PROPERTY: &str = "ordinal";
const COPY_PROOF_PROPERTY: &str = "copy-proof";
const KNOWLEDGE_AT_PROPERTY: &str = "knowledge-at";
const VALID_TIME_PROPERTY: &str = "valid-time";
const ENTITY_PREFIX: &str = "session-relation";
const SUMMARY_KIND: &str = "summary";
const OCCURRENCE_KIND: &str = "occurrence";
const THREAD_KIND: &str = "thread";
const AGENT_KIND: &str = "agent";
const SESSION_KIND: &str = "session";
const WORKFLOW_AGENT_ENTITY_KIND: &str = "workflow-agent";

mod projection_read;
mod read;
#[cfg(test)]
pub(crate) mod test_support;
mod validation;
pub use read::SummaryRelationRead;
#[cfg(test)]
pub(crate) use test_support::memory_relation_store;
pub(crate) use validation::validate_projection;

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
pub struct WorkflowAgentMembership {
    pub run_id: String,
    pub agent_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContextRelations {
    pub parent_session_id: Option<SessionId>,
    pub workflow_agents: Vec<WorkflowAgentMembership>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SessionRelationScope {
    ProjectSessions { project_id: ProjectId },
    ProfileSessions { profile_id: UserProfileId },
}

impl SessionRelationScope {
    #[must_use]
    pub fn project_sessions(project_id: ProjectId) -> Self {
        Self::ProjectSessions { project_id }
    }

    #[must_use]
    pub fn profile_sessions(profile_id: UserProfileId) -> Self {
        Self::ProfileSessions { profile_id }
    }

    pub fn identity(&self) -> &str {
        match self {
            Self::ProjectSessions { project_id } => project_id.as_str(),
            Self::ProfileSessions { profile_id } => profile_id.as_str(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionRelationProjection {
    pub scope: SessionRelationScope,
    pub session_id: SessionId,
    pub generation: u64,
    pub summaries: Vec<SummaryRelationNode>,
    pub logical_copies: Vec<LogicalCopyRelation>,
    pub thread_hierarchy: Vec<ThreadHierarchyRelation>,
    pub agent_hierarchy: Vec<AgentHierarchyRelation>,
    pub parent_session_id: Option<SessionId>,
    pub workflow_agents: Vec<WorkflowAgentMembership>,
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

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SessionRelationError {
    #[error("session relation projection is invalid")]
    Invalid,
    #[error("session relation graph contains a cycle")]
    Cycle,
    #[error("session relation graph was not found")]
    NotFound,
    #[error("session relation traversal exhausted its budget")]
    BudgetExhausted,
    #[error("session relation request was cancelled")]
    Cancelled,
    #[error("session relation request deadline exceeded")]
    DeadlineExceeded,
    #[error("session relation graph is unavailable")]
    Unavailable,
    #[error("session relation generation conflicts with an existing publication")]
    Conflict,
    #[error("session relation graph requires reset")]
    ResetRequired,
    #[error("session relation graph durability is uncertain")]
    DurabilityUncertain,
    #[error("session relation graph is corrupt")]
    Corrupt,
    #[error("session relation store failed: {0}")]
    Storage(String),
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

    pub fn replace(
        &self,
        relation_projection: &SessionRelationProjection,
    ) -> Result<GraphWatermark, SessionRelationError> {
        self.replace_with_cancellation(relation_projection, Arc::new(NeverCancelled))
    }

    pub fn replace_with_cancellation(
        &self,
        relation_projection: &SessionRelationProjection,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphWatermark, SessionRelationError> {
        validate_projection(relation_projection)?;
        let namespace = namespace(&relation_projection.scope)?;
        let projection = projection(
            &relation_projection.session_id,
            relation_projection.generation,
        )?;
        let watermark = projection_watermark(relation_projection)?;
        if let Some(existing) = self
            .database
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: namespace.clone(),
                projection: projection.clone(),
                cancellation: Arc::clone(&cancellation),
            })
            .map_err(map_graph_error)?
        {
            return if existing.watermark == watermark {
                Ok(watermark)
            } else {
                Err(SessionRelationError::Conflict)
            };
        }
        let (entities, relations) = build_graph(relation_projection)?;
        self.database
            .replace_projection_unverified(ProjectionReplacement {
                namespace,
                projection,
                source_generation: SourceGeneration::new(format!(
                    "session-relations:{}:{}",
                    relation_projection.session_id.as_str(),
                    relation_projection.generation
                ))
                .map_err(map_graph_error)?,
                next_watermark: watermark.clone(),
                entities,
                relations,
                cancellation,
            })
            .map_err(map_graph_error)?;
        Ok(watermark)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn summary_sources(
        &self,
        scope: &SessionRelationScope,
        session_id: &SessionId,
        generation: u64,
        root_summary_id: &str,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<SummarySourceVisit>, SessionRelationError> {
        if max_relations == 0 {
            return Err(SessionRelationError::BudgetExhausted);
        }
        let namespace = namespace(scope)?;
        let projection = projection(session_id, generation)?;
        let snapshot = self.database.snapshot().map_err(map_graph_error)?;
        if snapshot
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: namespace.clone(),
                projection,
                cancellation: Arc::clone(&cancellation),
            })
            .map_err(map_graph_error)?
            .is_none()
        {
            return Err(SessionRelationError::NotFound);
        }
        let root = summary_entity_id(session_id, generation, root_summary_id)?;
        let kinds = BTreeSet::from([
            GraphRelationKind::new(SUMMARY_SOURCE_KIND).map_err(map_graph_error)?,
            GraphRelationKind::new(SUMMARY_ANCHOR_SOURCE_KIND).map_err(map_graph_error)?,
        ]);
        let ordinal_property = GraphPropertyName::new(ORDINAL_PROPERTY).map_err(map_graph_error)?;
        let mut pending = VecDeque::from([(root, 1_usize)]);
        let mut visits = Vec::new();
        while let Some((parent, depth)) = pending.pop_front() {
            let remaining = max_relations
                .checked_sub(visits.len())
                .ok_or(SessionRelationError::BudgetExhausted)?;
            if remaining == 0 {
                return Err(SessionRelationError::BudgetExhausted);
            }
            // Ask for one more than the remaining budget. Grafeo then returns a
            // typed budget error instead of silently truncating topology.
            let query_budget = remaining
                .checked_add(1)
                .ok_or(SessionRelationError::BudgetExhausted)?;
            let mut batches = self
                .database
                .outgoing_relations(
                    &namespace,
                    std::slice::from_ref(&parent),
                    &kinds,
                    query_budget,
                    Arc::clone(&cancellation),
                )
                .map_err(map_graph_error)?;
            let mut outgoing = batches.pop().ok_or(SessionRelationError::Corrupt)?;
            outgoing.sort_by(|left, right| {
                relation_ordinal(left, &ordinal_property)
                    .cmp(&relation_ordinal(right, &ordinal_property))
                    .then_with(|| left.identity.cmp(&right.identity))
            });
            let parent_summary_id =
                parse_entity_id(parent.as_str(), session_id, generation, SUMMARY_KIND)?.to_owned();
            for relation in outgoing {
                if visits.len() == max_relations {
                    return Err(SessionRelationError::BudgetExhausted);
                }
                let ordinal = relation_ordinal(&relation, &ordinal_property)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(SessionRelationError::Corrupt)?;
                if relation.kind.as_str() == SUMMARY_SOURCE_KIND {
                    let summary_id = parse_entity_id(
                        relation.to.as_str(),
                        session_id,
                        generation,
                        SUMMARY_KIND,
                    )?
                    .to_owned();
                    visits.push(SummarySourceVisit {
                        parent_summary_id: parent_summary_id.clone(),
                        source: SummarySourceVisitKind::Summary {
                            summary_id: summary_id.clone(),
                        },
                        ordinal,
                        depth,
                    });
                    pending.push_back((
                        relation.to,
                        depth
                            .checked_add(1)
                            .ok_or(SessionRelationError::BudgetExhausted)?,
                    ));
                } else if relation.kind.as_str() == SUMMARY_ANCHOR_SOURCE_KIND {
                    let anchor_id = RetrievalAnchorId::new(parse_entity_id(
                        relation.to.as_str(),
                        session_id,
                        generation,
                        "anchor",
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?;
                    visits.push(SummarySourceVisit {
                        parent_summary_id: parent_summary_id.clone(),
                        source: SummarySourceVisitKind::Anchor { anchor_id },
                        ordinal,
                        depth,
                    });
                } else {
                    return Err(SessionRelationError::Corrupt);
                }
            }
        }
        Ok(visits)
    }

    pub fn has_summary_successor(
        &self,
        scope: &SessionRelationScope,
        session_id: &SessionId,
        generation: u64,
        summary_id: &str,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<bool, SessionRelationError> {
        let namespace = namespace(scope)?;
        let projection = projection(session_id, generation)?;
        let snapshot = self.database.snapshot().map_err(map_graph_error)?;
        if snapshot
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: namespace.clone(),
                projection,
                cancellation: Arc::clone(&cancellation),
            })
            .map_err(map_graph_error)?
            .is_none()
        {
            return Err(SessionRelationError::NotFound);
        }
        let starts = [summary_entity_id(session_id, generation, summary_id)?];
        let kinds = BTreeSet::from([
            GraphRelationKind::new(SUMMARY_SUCCESSOR_KIND).map_err(map_graph_error)?
        ]);
        self.database
            .outgoing_relations(&namespace, &starts, &kinds, 1, cancellation)
            .map(|mut batches| batches.pop().is_some_and(|relations| !relations.is_empty()))
            .map_err(map_graph_error)
    }
}

pub(crate) fn projection_watermark(
    projection: &SessionRelationProjection,
) -> Result<GraphWatermark, SessionRelationError> {
    let mut canonical = projection.clone();
    canonical
        .summaries
        .sort_by(|left, right| left.summary_id.cmp(&right.summary_id));
    canonical.logical_copies.sort_by(|left, right| {
        left.occurrence_id.cmp(&right.occurrence_id).then_with(|| {
            left.copied_from_occurrence_id
                .cmp(&right.copied_from_occurrence_id)
        })
    });
    canonical.thread_hierarchy.sort_by(|left, right| {
        left.parent_thread_id
            .cmp(&right.parent_thread_id)
            .then_with(|| left.child_thread_id.cmp(&right.child_thread_id))
    });
    canonical.agent_hierarchy.sort_by(|left, right| {
        left.parent_agent_id
            .cmp(&right.parent_agent_id)
            .then_with(|| left.child_agent_id.cmp(&right.child_agent_id))
    });
    canonical.workflow_agents.sort_by(|left, right| {
        left.run_id
            .cmp(&right.run_id)
            .then_with(|| left.agent_label.cmp(&right.agent_label))
    });
    let encoded = serde_json::to_vec(&canonical).map_err(|_| SessionRelationError::Invalid)?;
    GraphWatermark::new(format!(
        "session-relations:{}",
        hex::encode(Sha256::digest(encoded))
    ))
    .map_err(map_graph_error)
}

fn build_graph(
    projection: &SessionRelationProjection,
) -> Result<(Vec<GraphEntity>, Vec<GraphRelation>), SessionRelationError> {
    let mut entities = BTreeMap::new();
    let mut relations = Vec::new();
    for summary in &projection.summaries {
        let from = summary_entity_id(
            &projection.session_id,
            projection.generation,
            &summary.summary_id,
        )?;
        insert_entity(&mut entities, from.clone(), "session-summary")?;
        for (ordinal, source) in summary.sources.iter().enumerate() {
            let ordinal = u32::try_from(ordinal).map_err(|_| SessionRelationError::Invalid)?;
            let (target, target_label, relation_kind) = match source {
                SummarySourceRef::Summary { summary_id } => (
                    summary_entity_id(&projection.session_id, projection.generation, summary_id)?,
                    "session-summary",
                    SUMMARY_SOURCE_KIND,
                ),
                SummarySourceRef::Anchor { anchor_id } => (
                    entity_id(
                        &projection.session_id,
                        projection.generation,
                        "anchor",
                        anchor_id.as_str(),
                    )?,
                    "retrieval-anchor-reference",
                    SUMMARY_ANCHOR_SOURCE_KIND,
                ),
            };
            insert_entity(&mut entities, target.clone(), target_label)?;
            relations.push(ordered_relation(
                &projection.session_id,
                projection.generation,
                &summary.summary_id,
                ordinal,
                from.clone(),
                target,
                relation_kind,
            )?);
        }
        if let Some(predecessor) = &summary.predecessor_summary_id {
            let predecessor_id =
                summary_entity_id(&projection.session_id, projection.generation, predecessor)?;
            insert_entity(
                &mut entities,
                predecessor_id.clone(),
                "session-summary-reference",
            )?;
            relations.push(ordered_relation(
                &projection.session_id,
                projection.generation,
                &format!("{predecessor}:{}", summary.summary_id),
                0,
                predecessor_id.clone(),
                from.clone(),
                SUMMARY_SUCCESSOR_KIND,
            )?);
            relations.push(ordered_relation(
                &projection.session_id,
                projection.generation,
                &format!("{}:{predecessor}", summary.summary_id),
                0,
                from,
                predecessor_id,
                SUMMARY_PREDECESSOR_KIND,
            )?);
        }
    }
    for copy in &projection.logical_copies {
        let from = entity_id(
            &projection.session_id,
            projection.generation,
            OCCURRENCE_KIND,
            copy.occurrence_id.as_str(),
        )?;
        let to = entity_id(
            &projection.session_id,
            projection.generation,
            "occurrence",
            copy.copied_from_occurrence_id.as_str(),
        )?;
        insert_entity(&mut entities, from.clone(), "session-occurrence-reference")?;
        insert_entity(&mut entities, to.clone(), "session-occurrence-reference")?;
        relations.push(property_relation(
            &projection.session_id,
            projection.generation,
            &format!(
                "{}:{}",
                copy.occurrence_id.as_str(),
                copy.copied_from_occurrence_id.as_str()
            ),
            from.clone(),
            to.clone(),
            LOGICAL_COPY_KIND,
            BTreeMap::from([
                (
                    GraphPropertyName::new(COPY_PROOF_PROPERTY).map_err(map_graph_error)?,
                    GraphProperty::String(
                        serde_json::to_string(&copy.proof)
                            .map_err(|_| SessionRelationError::Invalid)?,
                    ),
                ),
                (
                    GraphPropertyName::new(KNOWLEDGE_AT_PROPERTY).map_err(map_graph_error)?,
                    GraphProperty::I64(copy.knowledge_at.0),
                ),
                (
                    GraphPropertyName::new(VALID_TIME_PROPERTY).map_err(map_graph_error)?,
                    GraphProperty::String(
                        serde_json::to_string(&copy.valid_time)
                            .map_err(|_| SessionRelationError::Invalid)?,
                    ),
                ),
            ]),
        )?);
    }
    for edge in &projection.thread_hierarchy {
        let from = entity_id(
            &projection.session_id,
            projection.generation,
            THREAD_KIND,
            edge.parent_thread_id.as_str(),
        )?;
        let to = entity_id(
            &projection.session_id,
            projection.generation,
            "thread",
            edge.child_thread_id.as_str(),
        )?;
        insert_entity(&mut entities, from.clone(), "session-thread-reference")?;
        insert_entity(&mut entities, to.clone(), "session-thread-reference")?;
        relations.push(ordered_relation(
            &projection.session_id,
            projection.generation,
            &format!(
                "{}:{}",
                edge.parent_thread_id.as_str(),
                edge.child_thread_id.as_str()
            ),
            edge.ordinal,
            from.clone(),
            to.clone(),
            THREAD_PARENT_KIND,
        )?);
        relations.push(ordered_relation(
            &projection.session_id,
            projection.generation,
            &format!(
                "{}:{}",
                edge.child_thread_id.as_str(),
                edge.parent_thread_id.as_str()
            ),
            edge.ordinal,
            to,
            from,
            THREAD_CHILD_OF_KIND,
        )?);
    }
    for edge in &projection.agent_hierarchy {
        let from = entity_id(
            &projection.session_id,
            projection.generation,
            AGENT_KIND,
            edge.parent_agent_id.as_str(),
        )?;
        let to = entity_id(
            &projection.session_id,
            projection.generation,
            "agent",
            edge.child_agent_id.as_str(),
        )?;
        insert_entity(&mut entities, from.clone(), "session-agent-reference")?;
        insert_entity(&mut entities, to.clone(), "session-agent-reference")?;
        relations.push(ordered_relation(
            &projection.session_id,
            projection.generation,
            &format!(
                "{}:{}",
                edge.parent_agent_id.as_str(),
                edge.child_agent_id.as_str()
            ),
            edge.ordinal,
            from.clone(),
            to.clone(),
            AGENT_PARENT_KIND,
        )?);
        relations.push(ordered_relation(
            &projection.session_id,
            projection.generation,
            &format!(
                "{}:{}",
                edge.child_agent_id.as_str(),
                edge.parent_agent_id.as_str()
            ),
            edge.ordinal,
            to,
            from,
            AGENT_CHILD_OF_KIND,
        )?);
    }
    let session = session_entity_id(
        &projection.session_id,
        projection.generation,
        &projection.session_id,
    )?;
    if let Some(parent_session_id) = &projection.parent_session_id {
        let parent = session_entity_id(
            &projection.session_id,
            projection.generation,
            parent_session_id,
        )?;
        insert_entity(&mut entities, session.clone(), "session-reference")?;
        insert_entity(&mut entities, parent.clone(), "session-reference")?;
        relations.push(ordered_relation(
            &projection.session_id,
            projection.generation,
            parent_session_id.as_str(),
            0,
            session.clone(),
            parent,
            SESSION_PARENT_KIND,
        )?);
    }
    for (ordinal, membership) in projection.workflow_agents.iter().enumerate() {
        let ordinal = u32::try_from(ordinal).map_err(|_| SessionRelationError::Invalid)?;
        let workflow = workflow_agent_entity_id(
            &projection.session_id,
            projection.generation,
            &membership.run_id,
            &membership.agent_label,
        )?;
        insert_entity(&mut entities, session.clone(), "session-reference")?;
        insert_entity(&mut entities, workflow.clone(), "workflow-agent-reference")?;
        relations.push(ordered_relation(
            &projection.session_id,
            projection.generation,
            &format!("{}:{}", membership.run_id, membership.agent_label),
            ordinal,
            session.clone(),
            workflow,
            WORKFLOW_AGENT_KIND,
        )?);
    }
    Ok((entities.into_values().collect(), relations))
}

fn insert_entity(
    entities: &mut BTreeMap<GraphEntityId, GraphEntity>,
    identity: GraphEntityId,
    label: &str,
) -> Result<(), SessionRelationError> {
    let label = GraphLabel::new(label).map_err(map_graph_error)?;
    if let Some(entity) = entities.get_mut(&identity) {
        entity.labels.insert(label);
    } else {
        entities.insert(
            identity.clone(),
            GraphEntity::new(identity, BTreeSet::from([label]), BTreeMap::new())
                .map_err(map_graph_error)?,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ordered_relation(
    session_id: &SessionId,
    generation: u64,
    owner: &str,
    ordinal: u32,
    from: GraphEntityId,
    to: GraphEntityId,
    kind: &str,
) -> Result<GraphRelation, SessionRelationError> {
    GraphRelation::new(
        GraphRelationId::new(format!(
            "{ENTITY_PREFIX}:{}:{generation}:relation:{kind}:{owner}:{ordinal:010}",
            session_id.as_str()
        ))
        .map_err(map_graph_error)?,
        from,
        to,
        GraphRelationKind::new(kind).map_err(map_graph_error)?,
        BTreeMap::from([(
            GraphPropertyName::new(ORDINAL_PROPERTY).map_err(map_graph_error)?,
            GraphProperty::I64(i64::from(ordinal)),
        )]),
    )
    .map_err(map_graph_error)
}

fn property_relation(
    session_id: &SessionId,
    generation: u64,
    owner: &str,
    from: GraphEntityId,
    to: GraphEntityId,
    kind: &str,
    properties: BTreeMap<GraphPropertyName, GraphProperty>,
) -> Result<GraphRelation, SessionRelationError> {
    GraphRelation::new(
        GraphRelationId::new(format!(
            "{ENTITY_PREFIX}:{}:{generation}:relation:{kind}:{owner}",
            session_id.as_str()
        ))
        .map_err(map_graph_error)?,
        from,
        to,
        GraphRelationKind::new(kind).map_err(map_graph_error)?,
        properties,
    )
    .map_err(map_graph_error)
}

fn namespace(scope: &SessionRelationScope) -> Result<GraphNamespace, SessionRelationError> {
    let prefix = match scope {
        SessionRelationScope::ProjectSessions { .. } => "project_sessions",
        SessionRelationScope::ProfileSessions { .. } => "profile_sessions",
    };
    GraphNamespace::new(format!("{prefix}:{}", scope.identity())).map_err(map_graph_error)
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

fn session_entity_id(
    projection_session_id: &SessionId,
    generation: u64,
    session_id: &SessionId,
) -> Result<GraphEntityId, SessionRelationError> {
    entity_id(
        projection_session_id,
        generation,
        SESSION_KIND,
        session_id.as_str(),
    )
}

fn workflow_agent_entity_id(
    session_id: &SessionId,
    generation: u64,
    run_id: &str,
    agent_label: &str,
) -> Result<GraphEntityId, SessionRelationError> {
    if run_id.trim().is_empty() || agent_label.trim().is_empty() {
        return Err(SessionRelationError::Invalid);
    }
    entity_id(
        session_id,
        generation,
        WORKFLOW_AGENT_ENTITY_KIND,
        &serde_json::to_string(&(run_id, agent_label))
            .map_err(|_| SessionRelationError::Invalid)?,
    )
}

fn entity_id(
    session_id: &SessionId,
    generation: u64,
    kind: &str,
    domain_id: &str,
) -> Result<GraphEntityId, SessionRelationError> {
    GraphEntityId::new(format!(
        "{ENTITY_PREFIX}:{}:{generation}:{kind}:{domain_id}",
        session_id.as_str()
    ))
    .map_err(map_graph_error)
}

fn parse_entity_id<'a>(
    value: &'a str,
    session_id: &SessionId,
    generation: u64,
    kind: &str,
) -> Result<&'a str, SessionRelationError> {
    value
        .strip_prefix(&format!(
            "{ENTITY_PREFIX}:{}:{generation}:{kind}:",
            session_id.as_str()
        ))
        .filter(|value| !value.is_empty())
        .ok_or(SessionRelationError::Corrupt)
}

fn relation_ordinal(relation: &GraphRelation, ordinal_property: &GraphPropertyName) -> Option<i64> {
    match relation.properties.get(ordinal_property) {
        Some(GraphProperty::I64(value)) => Some(*value),
        _ => None,
    }
}

fn map_graph_error(error: GraphDbError) -> SessionRelationError {
    match error {
        GraphDbError::Cancelled => SessionRelationError::Cancelled,
        GraphDbError::DeadlineExceeded => SessionRelationError::DeadlineExceeded,
        GraphDbError::BudgetExhausted => SessionRelationError::BudgetExhausted,
        GraphDbError::Conflict => SessionRelationError::Conflict,
        GraphDbError::InvalidRequest { .. } => SessionRelationError::Invalid,
        GraphDbError::ResetRequired { .. } => SessionRelationError::ResetRequired,
        GraphDbError::DurabilityUncertain { .. } => SessionRelationError::DurabilityUncertain,
        GraphDbError::Corrupt { .. } => SessionRelationError::Corrupt,
        GraphDbError::Unavailable { .. } | GraphDbError::Closed => {
            SessionRelationError::Unavailable
        }
    }
}

#[cfg(test)]
mod error_mapping_tests {
    use super::*;

    #[test]
    fn reset_and_uncertain_durability_remain_distinct() {
        assert_eq!(
            map_graph_error(GraphDbError::ResetRequired {
                message: "reset".to_owned(),
            }),
            SessionRelationError::ResetRequired
        );
        assert_eq!(
            map_graph_error(GraphDbError::DurabilityUncertain {
                message: "uncertain".to_owned(),
            }),
            SessionRelationError::DurabilityUncertain
        );
    }
}
