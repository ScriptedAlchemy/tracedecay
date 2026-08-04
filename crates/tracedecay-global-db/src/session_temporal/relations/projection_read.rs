use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_domain::{
    AgentInstanceId, MessageOccurrenceIdV1, ProjectId, RetrievalAnchorId, SessionId, ThreadId,
    UtcMicros,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphLabel, GraphProjectionReadRequest, GraphProperty, GraphPropertyName,
};

use super::{
    AGENT_PARENT_KIND, COPY_PROOF_PROPERTY, KNOWLEDGE_AT_PROPERTY, LOGICAL_COPY_KIND,
    OCCURRENCE_KIND, ORDINAL_PROPERTY, SUMMARY_ANCHOR_SOURCE_KIND, SUMMARY_KIND,
    SUMMARY_SOURCE_KIND, SUMMARY_SUCCESSOR_KIND, SessionRelationError, SessionRelationGraphStore,
    SessionRelationProjection, SummaryRelationNode, SummarySourceRef, THREAD_KIND,
    THREAD_PARENT_KIND, VALID_TIME_PROPERTY, map_graph_error, namespace, parse_entity_id,
    projection, relation_ordinal,
};

impl SessionRelationGraphStore {
    /// Loads one immutable projection for direct publication of its successor.
    /// The caller owns both bounds; no partial projection is returned.
    #[allow(clippy::too_many_arguments)]
    pub fn load_projection(
        &self,
        project_id: &ProjectId,
        session_id: &SessionId,
        generation: u64,
        max_entities: usize,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<SessionRelationProjection, SessionRelationError> {
        if max_entities == 0 || max_relations == 0 {
            return Err(SessionRelationError::BudgetExhausted);
        }
        let namespace = namespace(project_id)?;
        let projection_id = projection(session_id, generation)?;
        let page = self
            .database
            .read_projection(GraphProjectionReadRequest {
                namespace,
                projection: projection_id,
                after_entity: None,
                after_relation: None,
                max_entities,
                max_relations,
                cancellation,
            })
            .map_err(map_graph_error)?;
        if page.next_entity.is_some() || page.next_relation.is_some() {
            return Err(SessionRelationError::BudgetExhausted);
        }
        decode_projection(project_id, session_id, generation, page.entities, page.relations)
    }
}

fn decode_projection(
    project_id: &ProjectId,
    session_id: &SessionId,
    generation: u64,
    entities: Vec<tracedecay_graph_db::GraphEntity>,
    relations: Vec<tracedecay_graph_db::GraphRelation>,
) -> Result<SessionRelationProjection, SessionRelationError> {
    let summary_label = GraphLabel::new("session-summary").map_err(map_graph_error)?;
    let mut summaries = BTreeMap::<String, SummaryRelationNode>::new();
    for entity in entities {
        if entity.labels.contains(&summary_label) {
            let summary_id =
                parse_entity_id(entity.identity.as_str(), session_id, generation, SUMMARY_KIND)?
                    .to_owned();
            summaries.insert(
                summary_id.clone(),
                SummaryRelationNode {
                    summary_id,
                    sources: Vec::new(),
                    predecessor_summary_id: None,
                },
            );
        }
    }
    let ordinal_property =
        GraphPropertyName::new(ORDINAL_PROPERTY).map_err(map_graph_error)?;
    let proof_property =
        GraphPropertyName::new(COPY_PROOF_PROPERTY).map_err(map_graph_error)?;
    let knowledge_property =
        GraphPropertyName::new(KNOWLEDGE_AT_PROPERTY).map_err(map_graph_error)?;
    let valid_time_property =
        GraphPropertyName::new(VALID_TIME_PROPERTY).map_err(map_graph_error)?;
    let mut ordered_sources = BTreeMap::<String, Vec<(u32, SummarySourceRef)>>::new();
    let mut logical_copies = Vec::new();
    let mut thread_hierarchy = Vec::new();
    let mut agent_hierarchy = Vec::new();
    for relation in relations {
        match relation.kind.as_str() {
            SUMMARY_SOURCE_KIND | SUMMARY_ANCHOR_SOURCE_KIND => {
                let summary_id =
                    parse_entity_id(relation.from.as_str(), session_id, generation, SUMMARY_KIND)?
                        .to_owned();
                let ordinal = ordinal(&relation, &ordinal_property)?;
                let source = if relation.kind.as_str() == SUMMARY_SOURCE_KIND {
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
                            "anchor",
                        )?)
                        .map_err(|_| SessionRelationError::Corrupt)?,
                    }
                };
                ordered_sources
                    .entry(summary_id)
                    .or_default()
                    .push((ordinal, source));
            }
            SUMMARY_SUCCESSOR_KIND => {
                let predecessor =
                    parse_entity_id(relation.from.as_str(), session_id, generation, SUMMARY_KIND)?;
                let successor =
                    parse_entity_id(relation.to.as_str(), session_id, generation, SUMMARY_KIND)?;
                if summaries
                    .get_mut(successor)
                    .ok_or(SessionRelationError::Corrupt)?
                    .predecessor_summary_id
                    .replace(predecessor.to_owned())
                    .is_some()
                {
                    return Err(SessionRelationError::Corrupt);
                }
            }
            LOGICAL_COPY_KIND => {
                logical_copies.push(super::LogicalCopyRelation {
                    occurrence_id: MessageOccurrenceIdV1::new(parse_entity_id(
                        relation.from.as_str(),
                        session_id,
                        generation,
                        OCCURRENCE_KIND,
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                    copied_from_occurrence_id: MessageOccurrenceIdV1::new(parse_entity_id(
                        relation.to.as_str(),
                        session_id,
                        generation,
                        OCCURRENCE_KIND,
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                    proof: serde_json::from_str(string_property(&relation, &proof_property)?)
                        .map_err(|_| SessionRelationError::Corrupt)?,
                    knowledge_at: match relation.properties.get(&knowledge_property) {
                        Some(GraphProperty::I64(value)) => UtcMicros(*value),
                        _ => return Err(SessionRelationError::Corrupt),
                    },
                    valid_time: serde_json::from_str(string_property(
                        &relation,
                        &valid_time_property,
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                });
            }
            THREAD_PARENT_KIND => {
                thread_hierarchy.push(super::ThreadHierarchyRelation {
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
                    ordinal: ordinal(&relation, &ordinal_property)?,
                });
            }
            AGENT_PARENT_KIND => {
                agent_hierarchy.push(super::AgentHierarchyRelation {
                    parent_agent_id: AgentInstanceId::new(parse_entity_id(
                        relation.from.as_str(),
                        session_id,
                        generation,
                        "agent",
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                    child_agent_id: AgentInstanceId::new(parse_entity_id(
                        relation.to.as_str(),
                        session_id,
                        generation,
                        "agent",
                    )?)
                    .map_err(|_| SessionRelationError::Corrupt)?,
                    ordinal: ordinal(&relation, &ordinal_property)?,
                });
            }
            // Reverse indexes are derived from the canonical relation above.
            super::SUMMARY_PREDECESSOR_KIND
            | super::THREAD_CHILD_OF_KIND
            | super::AGENT_CHILD_OF_KIND => {}
            _ => return Err(SessionRelationError::Corrupt),
        }
    }
    for (summary_id, mut sources) in ordered_sources {
        sources.sort_by_key(|(ordinal, _)| *ordinal);
        summaries
            .get_mut(&summary_id)
            .ok_or(SessionRelationError::Corrupt)?
            .sources = sources.into_iter().map(|(_, source)| source).collect();
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

fn ordinal(
    relation: &tracedecay_graph_db::GraphRelation,
    property: &GraphPropertyName,
) -> Result<u32, SessionRelationError> {
    relation_ordinal(relation, property)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(SessionRelationError::Corrupt)
}

fn string_property<'a>(
    relation: &'a tracedecay_graph_db::GraphRelation,
    property: &GraphPropertyName,
) -> Result<&'a str, SessionRelationError> {
    match relation.properties.get(property) {
        Some(GraphProperty::String(value)) => Ok(value),
        _ => Err(SessionRelationError::Corrupt),
    }
}
