use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_domain::{AgentInstanceId, MessageOccurrenceIdV1, RetrievalAnchorId, ThreadId};
use tracedecay_graph_db::{
    GraphCancellation, GraphEntityId, GraphProjectionTelemetryRequest, GraphProperty,
    GraphPropertyName, GraphRelation, GraphRelationKind,
};

use super::{
    AGENT_CHILD_OF_KIND, AGENT_KIND, AGENT_PARENT_KIND, COPY_PROOF_PROPERTY, KNOWLEDGE_AT_PROPERTY,
    LOGICAL_COPY_KIND, OCCURRENCE_KIND, ORDINAL_PROPERTY, SESSION_KIND, SESSION_PARENT_KIND,
    SUMMARY_ANCHOR_SOURCE_KIND, SUMMARY_KIND, SUMMARY_PREDECESSOR_KIND, SUMMARY_SOURCE_KIND,
    SUMMARY_SUCCESSOR_KIND, SessionRelationError, SessionRelationGraphStore, SummarySourceRef,
    THREAD_CHILD_OF_KIND, THREAD_KIND, THREAD_PARENT_KIND, VALID_TIME_PROPERTY,
    WORKFLOW_AGENT_ENTITY_KIND, WORKFLOW_AGENT_KIND, agent_entity_id, map_graph_error, namespace,
    occurrence_entity_id, parse_entity_id, projection, relation_ordinal, session_entity_id,
    summary_entity_id, thread_entity_id,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryRelationRead {
    pub summary_id: String,
    pub sources: Vec<SummarySourceRef>,
    pub predecessor_summary_id: Option<String>,
    pub successor_summary_ids: Vec<String>,
}

impl SessionRelationGraphStore {
    pub fn session_context(
        &self,
        scope: &super::SessionRelationScope,
        session_id: &tracedecay_domain::SessionId,
        generation: u64,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<super::SessionContextRelations, SessionRelationError> {
        require_budget(max_relations)?;
        self.require_projection(scope, session_id, generation, Arc::clone(&cancellation))?;
        let starts = [session_entity_id(session_id, generation, session_id)?];
        let mut batches = self
            .database
            .outgoing_relations(
                &namespace(scope)?,
                &starts,
                &relation_kinds(&[SESSION_PARENT_KIND, WORKFLOW_AGENT_KIND])?,
                max_relations,
                cancellation,
            )
            .map_err(map_graph_error)?;
        let relations = batches.pop().ok_or(SessionRelationError::Corrupt)?;
        let mut parent_session_id = None;
        let mut workflow_agents = Vec::new();
        for relation in relations {
            match relation.kind.as_str() {
                SESSION_PARENT_KIND => {
                    let parent = tracedecay_domain::SessionId::new(parse_entity_id(
                        relation.to.as_str(),
                        session_id,
                        generation,
                        SESSION_KIND,
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?;
                    if parent_session_id.replace(parent).is_some() {
                        return Err(SessionRelationError::Corrupt);
                    }
                }
                WORKFLOW_AGENT_KIND => {
                    let encoded = parse_entity_id(
                        relation.to.as_str(),
                        session_id,
                        generation,
                        WORKFLOW_AGENT_ENTITY_KIND,
                    )?;
                    let (run_id, agent_label): (String, String) =
                        serde_json::from_str(encoded).map_err(|_| SessionRelationError::Corrupt)?;
                    workflow_agents.push(super::WorkflowAgentMembership {
                        run_id,
                        agent_label,
                    });
                }
                _ => return Err(SessionRelationError::Corrupt),
            }
        }
        workflow_agents.sort_by(|left, right| {
            left.run_id
                .cmp(&right.run_id)
                .then_with(|| left.agent_label.cmp(&right.agent_label))
        });
        Ok(super::SessionContextRelations {
            parent_session_id,
            workflow_agents,
        })
    }

    pub fn summary_relations(
        &self,
        scope: &super::SessionRelationScope,
        session_id: &tracedecay_domain::SessionId,
        generation: u64,
        summary_ids: &[String],
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<SummaryRelationRead>, SessionRelationError> {
        require_budget(max_relations)?;
        self.require_projection(scope, session_id, generation, Arc::clone(&cancellation))?;
        let namespace = namespace(scope)?;
        let starts = summary_ids
            .iter()
            .map(|summary_id| summary_entity_id(session_id, generation, summary_id))
            .collect::<Result<Vec<_>, _>>()?;
        let kinds = relation_kinds(&[
            SUMMARY_SOURCE_KIND,
            SUMMARY_ANCHOR_SOURCE_KIND,
            SUMMARY_SUCCESSOR_KIND,
            SUMMARY_PREDECESSOR_KIND,
        ])?;
        let batches = self
            .database
            .outgoing_relations(&namespace, &starts, &kinds, max_relations, cancellation)
            .map_err(map_graph_error)?;
        let ordinal_property = GraphPropertyName::new(ORDINAL_PROPERTY).map_err(map_graph_error)?;
        summary_ids
            .iter()
            .zip(batches)
            .map(|(summary_id, mut relations)| {
                relations.sort_by(|left, right| {
                    relation_ordinal(left, &ordinal_property)
                        .cmp(&relation_ordinal(right, &ordinal_property))
                        .then_with(|| left.identity.cmp(&right.identity))
                });
                let mut sources = Vec::new();
                let mut predecessor_summary_id = None;
                let mut successor_summary_ids = Vec::new();
                for relation in relations {
                    match relation.kind.as_str() {
                        SUMMARY_SOURCE_KIND => sources.push(SummarySourceRef::Summary {
                            summary_id: parse_entity_id(
                                relation.to.as_str(),
                                session_id,
                                generation,
                                SUMMARY_KIND,
                            )?
                            .to_owned(),
                        }),
                        SUMMARY_ANCHOR_SOURCE_KIND => sources.push(SummarySourceRef::Anchor {
                            anchor_id: RetrievalAnchorId::new(parse_entity_id(
                                relation.to.as_str(),
                                session_id,
                                generation,
                                "anchor",
                            )?)
                            .map_err(|_| SessionRelationError::Corrupt)?,
                        }),
                        SUMMARY_PREDECESSOR_KIND => {
                            if predecessor_summary_id
                                .replace(
                                    parse_entity_id(
                                        relation.to.as_str(),
                                        session_id,
                                        generation,
                                        SUMMARY_KIND,
                                    )?
                                    .to_owned(),
                                )
                                .is_some()
                            {
                                return Err(SessionRelationError::Corrupt);
                            }
                        }
                        SUMMARY_SUCCESSOR_KIND => successor_summary_ids.push(
                            parse_entity_id(
                                relation.to.as_str(),
                                session_id,
                                generation,
                                SUMMARY_KIND,
                            )?
                            .to_owned(),
                        ),
                        _ => return Err(SessionRelationError::Corrupt),
                    }
                }
                Ok(SummaryRelationRead {
                    summary_id: summary_id.clone(),
                    sources,
                    predecessor_summary_id,
                    successor_summary_ids,
                })
            })
            .collect()
    }

    pub fn logical_copies(
        &self,
        scope: &super::SessionRelationScope,
        session_id: &tracedecay_domain::SessionId,
        generation: u64,
        occurrence_ids: &[MessageOccurrenceIdV1],
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<super::LogicalCopyRelation>>, SessionRelationError> {
        require_budget(max_relations)?;
        self.require_projection(scope, session_id, generation, Arc::clone(&cancellation))?;
        let starts = occurrence_ids
            .iter()
            .map(|occurrence_id| occurrence_entity_id(session_id, generation, occurrence_id))
            .collect::<Result<Vec<_>, _>>()?;
        let batches = self
            .database
            .outgoing_relations(
                &namespace(scope)?,
                &starts,
                &relation_kinds(&[LOGICAL_COPY_KIND])?,
                max_relations,
                cancellation,
            )
            .map_err(map_graph_error)?;
        let proof_property =
            GraphPropertyName::new(COPY_PROOF_PROPERTY).map_err(map_graph_error)?;
        let knowledge_property =
            GraphPropertyName::new(KNOWLEDGE_AT_PROPERTY).map_err(map_graph_error)?;
        let valid_time_property =
            GraphPropertyName::new(VALID_TIME_PROPERTY).map_err(map_graph_error)?;
        occurrence_ids
            .iter()
            .zip(batches)
            .map(|(occurrence_id, relations)| {
                relations
                    .into_iter()
                    .map(|relation| {
                        let copied_from_occurrence_id =
                            MessageOccurrenceIdV1::new(parse_entity_id(
                                relation.to.as_str(),
                                session_id,
                                generation,
                                OCCURRENCE_KIND,
                            )?)
                            .map_err(|_| SessionRelationError::Corrupt)?;
                        let proof = string_property(&relation, &proof_property)
                            .and_then(|value| serde_json::from_str(value).ok())
                            .ok_or(SessionRelationError::Corrupt)?;
                        let knowledge_at = match relation.properties.get(&knowledge_property) {
                            Some(GraphProperty::I64(value)) => tracedecay_domain::UtcMicros(*value),
                            _ => return Err(SessionRelationError::Corrupt),
                        };
                        let valid_time = string_property(&relation, &valid_time_property)
                            .and_then(|value| serde_json::from_str(value).ok())
                            .ok_or(SessionRelationError::Corrupt)?;
                        Ok(super::LogicalCopyRelation {
                            occurrence_id: occurrence_id.clone(),
                            copied_from_occurrence_id,
                            proof,
                            knowledge_at,
                            valid_time,
                        })
                    })
                    .collect()
            })
            .collect()
    }

    pub fn thread_relations(
        &self,
        scope: &super::SessionRelationScope,
        session_id: &tracedecay_domain::SessionId,
        generation: u64,
        thread_ids: &[ThreadId],
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<super::ThreadHierarchyRelation>>, SessionRelationError> {
        let starts = thread_ids
            .iter()
            .map(|thread_id| thread_entity_id(session_id, generation, thread_id))
            .collect::<Result<Vec<_>, _>>()?;
        self.hierarchy_relations(
            scope,
            session_id,
            generation,
            &starts,
            &[THREAD_PARENT_KIND, THREAD_CHILD_OF_KIND],
            max_relations,
            cancellation,
            |relation, ordinal| {
                let (parent, child) = if relation.kind.as_str() == THREAD_PARENT_KIND {
                    (&relation.from, &relation.to)
                } else {
                    (&relation.to, &relation.from)
                };
                Ok(super::ThreadHierarchyRelation {
                    parent_thread_id: ThreadId::new(parse_entity_id(
                        parent.as_str(),
                        session_id,
                        generation,
                        THREAD_KIND,
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                    child_thread_id: ThreadId::new(parse_entity_id(
                        child.as_str(),
                        session_id,
                        generation,
                        THREAD_KIND,
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                    ordinal,
                })
            },
        )
    }

    pub fn agent_relations(
        &self,
        scope: &super::SessionRelationScope,
        session_id: &tracedecay_domain::SessionId,
        generation: u64,
        agent_ids: &[AgentInstanceId],
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<super::AgentHierarchyRelation>>, SessionRelationError> {
        let starts = agent_ids
            .iter()
            .map(|agent_id| agent_entity_id(session_id, generation, agent_id))
            .collect::<Result<Vec<_>, _>>()?;
        self.hierarchy_relations(
            scope,
            session_id,
            generation,
            &starts,
            &[AGENT_PARENT_KIND, AGENT_CHILD_OF_KIND],
            max_relations,
            cancellation,
            |relation, ordinal| {
                let (parent, child) = if relation.kind.as_str() == AGENT_PARENT_KIND {
                    (&relation.from, &relation.to)
                } else {
                    (&relation.to, &relation.from)
                };
                Ok(super::AgentHierarchyRelation {
                    parent_agent_id: AgentInstanceId::new(parse_entity_id(
                        parent.as_str(),
                        session_id,
                        generation,
                        AGENT_KIND,
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                    child_agent_id: AgentInstanceId::new(parse_entity_id(
                        child.as_str(),
                        session_id,
                        generation,
                        AGENT_KIND,
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                    ordinal,
                })
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn hierarchy_relations<T>(
        &self,
        scope: &super::SessionRelationScope,
        session_id: &tracedecay_domain::SessionId,
        generation: u64,
        starts: &[GraphEntityId],
        kind_names: &[&str],
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
        decode: impl Fn(&GraphRelation, u32) -> Result<T, SessionRelationError>,
    ) -> Result<Vec<Vec<T>>, SessionRelationError> {
        require_budget(max_relations)?;
        self.require_projection(scope, session_id, generation, Arc::clone(&cancellation))?;
        let batches = self
            .database
            .outgoing_relations(
                &namespace(scope)?,
                starts,
                &relation_kinds(kind_names)?,
                max_relations,
                cancellation,
            )
            .map_err(map_graph_error)?;
        let ordinal_property = GraphPropertyName::new(ORDINAL_PROPERTY).map_err(map_graph_error)?;
        batches
            .into_iter()
            .map(|relations| {
                relations
                    .iter()
                    .map(|relation| {
                        let ordinal = relation_ordinal(relation, &ordinal_property)
                            .and_then(|value| u32::try_from(value).ok())
                            .ok_or(SessionRelationError::Corrupt)?;
                        decode(relation, ordinal)
                    })
                    .collect()
            })
            .collect()
    }

    fn require_projection(
        &self,
        scope: &super::SessionRelationScope,
        session_id: &tracedecay_domain::SessionId,
        generation: u64,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<(), SessionRelationError> {
        let exists = self
            .database
            .projection_telemetry(GraphProjectionTelemetryRequest {
                namespace: namespace(scope)?,
                projection: projection(session_id, generation)?,
                cancellation,
            })
            .map_err(map_graph_error)?
            .is_some();
        if exists {
            Ok(())
        } else {
            Err(SessionRelationError::NotFound)
        }
    }
}

fn relation_kinds(names: &[&str]) -> Result<BTreeSet<GraphRelationKind>, SessionRelationError> {
    names
        .iter()
        .map(|name| GraphRelationKind::new(*name).map_err(map_graph_error))
        .collect()
}

fn require_budget(max_relations: usize) -> Result<(), SessionRelationError> {
    if max_relations == 0 {
        Err(SessionRelationError::BudgetExhausted)
    } else {
        Ok(())
    }
}

fn string_property<'a>(
    relation: &'a GraphRelation,
    property: &GraphPropertyName,
) -> Option<&'a str> {
    match relation.properties.get(property) {
        Some(GraphProperty::String(value)) => Some(value),
        _ => None,
    }
}
