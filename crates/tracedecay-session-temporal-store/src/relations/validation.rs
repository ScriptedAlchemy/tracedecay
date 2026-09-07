use std::collections::{BTreeMap, BTreeSet};

use super::{
    SessionRelationError, SessionRelationProjection, SummaryRelationNode, SummarySourceRef,
};

pub(crate) fn validate_projection(
    projection: &SessionRelationProjection,
) -> Result<(), SessionRelationError> {
    if projection.generation == 0 {
        return Err(SessionRelationError::Invalid);
    }
    let summaries = projection
        .summaries
        .iter()
        .map(|summary| (summary.summary_id.as_str(), summary))
        .collect::<BTreeMap<_, _>>();
    if summaries.len() != projection.summaries.len()
        || summaries
            .keys()
            .any(|summary_id| summary_id.trim().is_empty())
    {
        return Err(SessionRelationError::Invalid);
    }
    let mut complete = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    for summary_id in summaries.keys() {
        visit_summary(summary_id, &summaries, &mut visiting, &mut complete)?;
    }
    if projection.summaries.iter().any(|summary| {
        summary
            .predecessor_summary_id
            .as_deref()
            .is_some_and(|predecessor| !summaries.contains_key(predecessor))
    }) {
        return Err(SessionRelationError::Invalid);
    }
    ensure_acyclic(projection.summaries.iter().filter_map(|summary| {
        summary
            .predecessor_summary_id
            .as_ref()
            .map(|predecessor| (predecessor.as_str(), summary.summary_id.as_str()))
    }))?;
    ensure_acyclic(projection.logical_copies.iter().map(|copy| {
        (
            copy.occurrence_id.as_str(),
            copy.copied_from_occurrence_id.as_str(),
        )
    }))?;
    if projection
        .logical_copies
        .iter()
        .any(|copy| copy.proof.source_occurrence_id() != &copy.copied_from_occurrence_id)
    {
        return Err(SessionRelationError::Invalid);
    }
    ensure_acyclic(projection.thread_hierarchy.iter().map(|edge| {
        (
            edge.parent_thread_id.as_str(),
            edge.child_thread_id.as_str(),
        )
    }))?;
    ensure_acyclic(
        projection
            .agent_hierarchy
            .iter()
            .map(|edge| (edge.parent_agent_id.as_str(), edge.child_agent_id.as_str())),
    )?;
    if projection
        .parent_session_id
        .as_ref()
        .is_some_and(|parent| parent == &projection.session_id)
    {
        return Err(SessionRelationError::Cycle);
    }
    let workflow_agents = projection
        .workflow_agents
        .iter()
        .map(|membership| (membership.run_id.as_str(), membership.agent_label.as_str()))
        .collect::<BTreeSet<_>>();
    if workflow_agents.len() != projection.workflow_agents.len()
        || workflow_agents
            .iter()
            .any(|(run_id, agent_label)| run_id.trim().is_empty() || agent_label.trim().is_empty())
    {
        return Err(SessionRelationError::Invalid);
    }
    Ok(())
}

fn ensure_acyclic<'a>(
    edges: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), SessionRelationError> {
    let mut adjacency = BTreeMap::<&str, Vec<&str>>::new();
    let mut identities = BTreeSet::new();
    for (from, to) in edges {
        if from.is_empty() || to.is_empty() || !identities.insert((from, to)) {
            return Err(SessionRelationError::Invalid);
        }
        adjacency.entry(from).or_default().push(to);
    }
    fn visit<'a>(
        node: &'a str,
        adjacency: &BTreeMap<&'a str, Vec<&'a str>>,
        visiting: &mut BTreeSet<&'a str>,
        complete: &mut BTreeSet<&'a str>,
    ) -> bool {
        if complete.contains(node) {
            return false;
        }
        if !visiting.insert(node) {
            return true;
        }
        if adjacency.get(node).is_some_and(|children| {
            children
                .iter()
                .any(|child| visit(child, adjacency, visiting, complete))
        }) {
            return true;
        }
        visiting.remove(node);
        complete.insert(node);
        false
    }
    let mut visiting = BTreeSet::new();
    let mut complete = BTreeSet::new();
    if adjacency
        .keys()
        .any(|node| visit(node, &adjacency, &mut visiting, &mut complete))
    {
        return Err(SessionRelationError::Cycle);
    }
    Ok(())
}

fn visit_summary<'a>(
    summary_id: &'a str,
    summaries: &BTreeMap<&'a str, &'a SummaryRelationNode>,
    visiting: &mut BTreeSet<&'a str>,
    complete: &mut BTreeSet<&'a str>,
) -> Result<(), SessionRelationError> {
    if complete.contains(summary_id) {
        return Ok(());
    }
    if !visiting.insert(summary_id) {
        return Err(SessionRelationError::Cycle);
    }
    let summary = summaries
        .get(summary_id)
        .ok_or(SessionRelationError::Invalid)?;
    for source in &summary.sources {
        if let SummarySourceRef::Summary { summary_id } = source {
            if !summaries.contains_key(summary_id.as_str()) {
                return Err(SessionRelationError::Invalid);
            }
            visit_summary(summary_id, summaries, visiting, complete)?;
        }
    }
    visiting.remove(summary_id);
    complete.insert(summary_id);
    Ok(())
}
