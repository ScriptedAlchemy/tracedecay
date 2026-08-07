use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_domain::{
    AgentInstanceId, MessageOccurrenceIdV1, RetrievalAnchorId, SessionId, ThreadId, UtcMicros,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphLabel, GraphProjectionReadRequest, GraphProperty, GraphPropertyName,
};

use super::{
    AGENT_PARENT_KIND, COPY_PROOF_PROPERTY, KNOWLEDGE_AT_PROPERTY, LOGICAL_COPY_KIND,
    OCCURRENCE_KIND, ORDINAL_PROPERTY, SESSION_KIND, SESSION_PARENT_KIND,
    SUMMARY_ANCHOR_SOURCE_KIND, SUMMARY_KIND, SUMMARY_SOURCE_KIND, SUMMARY_SUCCESSOR_KIND,
    SessionRelationError, SessionRelationGraphStore, SessionRelationProjection,
    SummaryRelationNode, SummarySourceRef, THREAD_KIND, THREAD_PARENT_KIND, VALID_TIME_PROPERTY,
    WORKFLOW_AGENT_ENTITY_KIND, WORKFLOW_AGENT_KIND, map_graph_error, namespace, parse_entity_id,
    projection, relation_ordinal,
};

impl SessionRelationGraphStore {
    /// Loads one immutable projection for direct publication of its successor.
    /// The caller owns both bounds; no partial projection is returned.
    #[allow(clippy::too_many_arguments)]
    pub fn load_projection(
        &self,
        scope: &super::SessionRelationScope,
        session_id: &SessionId,
        generation: u64,
        max_entities: usize,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<SessionRelationProjection, SessionRelationError> {
        if max_entities == 0 || max_relations == 0 {
            return Err(SessionRelationError::BudgetExhausted);
        }
        let namespace = namespace(scope)?;
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
        decode_projection(scope, session_id, generation, page.entities, page.relations)
    }
}

fn decode_projection(
    scope: &super::SessionRelationScope,
    session_id: &SessionId,
    generation: u64,
    entities: Vec<tracedecay_graph_db::GraphEntity>,
    relations: Vec<tracedecay_graph_db::GraphRelation>,
) -> Result<SessionRelationProjection, SessionRelationError> {
    let summary_label = GraphLabel::new("session-summary").map_err(map_graph_error)?;
    let mut summaries = BTreeMap::<String, SummaryRelationNode>::new();
    for entity in entities {
        if entity.labels.contains(&summary_label) {
            let summary_id = parse_entity_id(
                entity.identity.as_str(),
                session_id,
                generation,
                SUMMARY_KIND,
            )?
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
    let mut summary_successors = BTreeSet::new();
    let mut summary_predecessors = BTreeSet::new();
    let mut thread_parents = BTreeSet::new();
    let mut thread_children = BTreeSet::new();
    let mut agent_parents = BTreeSet::new();
    let mut agent_children = BTreeSet::new();
    let mut parent_session_id = None;
    let mut workflow_agents = Vec::new();
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
                summary_successors.insert((
                    predecessor.to_owned(),
                    successor.to_owned(),
                    ordinal(&relation, &ordinal_property)?,
                ));
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
                let parent_thread_id = ThreadId::new(parse_entity_id(
                    relation.from.as_str(),
                    session_id,
                    generation,
                    THREAD_KIND,
                )?)
                .map_err(|_| SessionRelationError::Corrupt)?;
                let child_thread_id = ThreadId::new(parse_entity_id(
                    relation.to.as_str(),
                    session_id,
                    generation,
                    THREAD_KIND,
                )?)
                .map_err(|_| SessionRelationError::Corrupt)?;
                thread_parents.insert((
                    parent_thread_id.as_str().to_owned(),
                    child_thread_id.as_str().to_owned(),
                    ordinal(&relation, &ordinal_property)?,
                ));
                thread_hierarchy.push(super::ThreadHierarchyRelation {
                    parent_thread_id,
                    child_thread_id,
                    ordinal: ordinal(&relation, &ordinal_property)?,
                });
            }
            AGENT_PARENT_KIND => {
                let parent_agent_id = AgentInstanceId::new(parse_entity_id(
                    relation.from.as_str(),
                    session_id,
                    generation,
                    "agent",
                )?)
                .map_err(|_| SessionRelationError::Corrupt)?;
                let child_agent_id = AgentInstanceId::new(parse_entity_id(
                    relation.to.as_str(),
                    session_id,
                    generation,
                    "agent",
                )?)
                .map_err(|_| SessionRelationError::Corrupt)?;
                agent_parents.insert((
                    parent_agent_id.as_str().to_owned(),
                    child_agent_id.as_str().to_owned(),
                    ordinal(&relation, &ordinal_property)?,
                ));
                agent_hierarchy.push(super::AgentHierarchyRelation {
                    parent_agent_id,
                    child_agent_id,
                    ordinal: ordinal(&relation, &ordinal_property)?,
                });
            }
            SESSION_PARENT_KIND => {
                let parent = SessionId::new(parse_entity_id(
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
                workflow_agents.push((
                    ordinal(&relation, &ordinal_property)?,
                    super::WorkflowAgentMembership {
                        run_id,
                        agent_label,
                    },
                ));
            }
            super::SUMMARY_PREDECESSOR_KIND => {
                let successor =
                    parse_entity_id(relation.from.as_str(), session_id, generation, SUMMARY_KIND)?;
                let predecessor =
                    parse_entity_id(relation.to.as_str(), session_id, generation, SUMMARY_KIND)?;
                if !summary_predecessors.insert((
                    predecessor.to_owned(),
                    successor.to_owned(),
                    ordinal(&relation, &ordinal_property)?,
                )) {
                    return Err(SessionRelationError::Corrupt);
                }
            }
            super::THREAD_CHILD_OF_KIND => {
                let child =
                    parse_entity_id(relation.from.as_str(), session_id, generation, THREAD_KIND)?;
                let parent =
                    parse_entity_id(relation.to.as_str(), session_id, generation, THREAD_KIND)?;
                if !thread_children.insert((
                    parent.to_owned(),
                    child.to_owned(),
                    ordinal(&relation, &ordinal_property)?,
                )) {
                    return Err(SessionRelationError::Corrupt);
                }
            }
            super::AGENT_CHILD_OF_KIND => {
                let child =
                    parse_entity_id(relation.from.as_str(), session_id, generation, "agent")?;
                let parent =
                    parse_entity_id(relation.to.as_str(), session_id, generation, "agent")?;
                if !agent_children.insert((
                    parent.to_owned(),
                    child.to_owned(),
                    ordinal(&relation, &ordinal_property)?,
                )) {
                    return Err(SessionRelationError::Corrupt);
                }
            }
            _ => return Err(SessionRelationError::Corrupt),
        }
    }
    if summary_successors != summary_predecessors
        || thread_parents != thread_children
        || agent_parents != agent_children
    {
        return Err(SessionRelationError::Corrupt);
    }
    for (summary_id, mut sources) in ordered_sources {
        sources.sort_by_key(|(ordinal, _)| *ordinal);
        if sources
            .iter()
            .enumerate()
            .any(|(expected, (actual, _))| u32::try_from(expected).ok() != Some(*actual))
        {
            return Err(SessionRelationError::Corrupt);
        }
        summaries
            .get_mut(&summary_id)
            .ok_or(SessionRelationError::Corrupt)?
            .sources = sources.into_iter().map(|(_, source)| source).collect();
    }
    logical_copies.sort_by(|left, right| {
        left.occurrence_id.cmp(&right.occurrence_id).then_with(|| {
            left.copied_from_occurrence_id
                .cmp(&right.copied_from_occurrence_id)
        })
    });
    thread_hierarchy.sort_by(|left, right| {
        left.parent_thread_id
            .cmp(&right.parent_thread_id)
            .then_with(|| left.child_thread_id.cmp(&right.child_thread_id))
    });
    agent_hierarchy.sort_by(|left, right| {
        left.parent_agent_id
            .cmp(&right.parent_agent_id)
            .then_with(|| left.child_agent_id.cmp(&right.child_agent_id))
    });
    workflow_agents.sort_by_key(|(ordinal, _)| *ordinal);
    if workflow_agents
        .iter()
        .enumerate()
        .any(|(expected, (actual, _))| u32::try_from(expected).ok() != Some(*actual))
    {
        return Err(SessionRelationError::Corrupt);
    }
    let workflow_agents = workflow_agents
        .into_iter()
        .map(|(_, membership)| membership)
        .collect();
    let projection = SessionRelationProjection {
        scope: scope.clone(),
        session_id: session_id.clone(),
        generation,
        summaries: summaries.into_values().collect(),
        logical_copies,
        thread_hierarchy,
        agent_hierarchy,
        parent_session_id,
        workflow_agents,
    };
    super::validate_projection(&projection).map_err(|error| match error {
        SessionRelationError::Cycle => SessionRelationError::Cycle,
        _ => SessionRelationError::Corrupt,
    })?;
    Ok(projection)
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

#[cfg(test)]
mod tests {
    use super::super::{
        SUMMARY_PREDECESSOR_KIND, SUMMARY_SOURCE_KIND, SessionRelationProjection,
        SessionRelationScope, SummaryRelationNode, SummarySourceRef, build_graph,
        memory_relation_store, namespace, ordered_relation, projection, summary_entity_id,
    };
    use super::*;
    use tracedecay_domain::{ProjectId, RetrievalAnchorId};
    use tracedecay_graph_db::{
        GraphWatermark, NeverCancelled, ProjectionReplacement, SourceGeneration,
    };

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("valid test identity")
    }

    fn relation_projection() -> SessionRelationProjection {
        SessionRelationProjection {
            scope: SessionRelationScope::project_sessions(id::<ProjectId>(
                "project.projection-read",
            )),
            session_id: id::<SessionId>("session.projection-read"),
            generation: 1,
            summaries: vec![
                SummaryRelationNode {
                    summary_id: "summary.root".to_owned(),
                    sources: vec![SummarySourceRef::Summary {
                        summary_id: "summary.child".to_owned(),
                    }],
                    predecessor_summary_id: None,
                },
                SummaryRelationNode {
                    summary_id: "summary.child".to_owned(),
                    sources: vec![SummarySourceRef::Anchor {
                        anchor_id: id::<RetrievalAnchorId>("anchor.child"),
                    }],
                    predecessor_summary_id: None,
                },
            ],
            logical_copies: Vec::new(),
            thread_hierarchy: Vec::new(),
            agent_hierarchy: Vec::new(),
            parent_session_id: None,
            workflow_agents: Vec::new(),
        }
    }

    fn publish_raw(
        store: &SessionRelationGraphStore,
        relation_projection: &SessionRelationProjection,
        relations: Vec<tracedecay_graph_db::GraphRelation>,
        entities: Vec<tracedecay_graph_db::GraphEntity>,
    ) {
        store
            .database
            .replace_projection_unverified(ProjectionReplacement {
                namespace: namespace(&relation_projection.scope).expect("namespace"),
                projection: projection(
                    &relation_projection.session_id,
                    relation_projection.generation,
                )
                .expect("projection"),
                source_generation: SourceGeneration::new("projection-read-corruption")
                    .expect("generation"),
                next_watermark: GraphWatermark::new("projection-read-corruption")
                    .expect("watermark"),
                entities,
                relations,
                cancellation: Arc::new(NeverCancelled),
            })
            .expect("raw graph projection");
    }

    #[test]
    fn projection_read_reports_a_summary_cycle_distinctly() {
        let mut relation_projection = relation_projection();
        relation_projection.summaries[1].sources.clear();
        let store = memory_relation_store();
        let (entities, mut relations) = build_graph(&relation_projection).expect("valid graph");
        relations.push(
            ordered_relation(
                &relation_projection.session_id,
                relation_projection.generation,
                "cycle",
                0,
                summary_entity_id(
                    &relation_projection.session_id,
                    relation_projection.generation,
                    "summary.child",
                )
                .expect("child"),
                summary_entity_id(
                    &relation_projection.session_id,
                    relation_projection.generation,
                    "summary.root",
                )
                .expect("root"),
                SUMMARY_SOURCE_KIND,
            )
            .expect("cycle relation"),
        );
        publish_raw(&store, &relation_projection, relations, entities);

        assert_eq!(
            store.load_projection(
                &relation_projection.scope,
                &relation_projection.session_id,
                relation_projection.generation,
                100,
                100,
                Arc::new(NeverCancelled),
            ),
            Err(SessionRelationError::Cycle)
        );
    }

    #[test]
    fn projection_read_rejects_a_missing_reverse_relation() {
        let mut relation_projection = relation_projection();
        relation_projection.summaries[1].predecessor_summary_id = Some("summary.root".to_owned());
        let store = memory_relation_store();
        let (entities, mut relations) = build_graph(&relation_projection).expect("valid graph");
        relations.retain(|relation| relation.kind.as_str() != SUMMARY_PREDECESSOR_KIND);
        publish_raw(&store, &relation_projection, relations, entities);

        assert_eq!(
            store.load_projection(
                &relation_projection.scope,
                &relation_projection.session_id,
                relation_projection.generation,
                100,
                100,
                Arc::new(NeverCancelled),
            ),
            Err(SessionRelationError::Corrupt)
        );
    }
}
