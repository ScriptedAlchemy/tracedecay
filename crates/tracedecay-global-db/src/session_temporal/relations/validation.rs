use std::collections::{BTreeMap, BTreeSet};

use super::{SessionRelationError, SessionRelationProjection, SummarySourceRef};

pub fn validate_projection(
    projection: &SessionRelationProjection,
) -> Result<(), SessionRelationError> {
    if projection.generation == 0 {
        return Err(SessionRelationError::Invalid);
    }
    let mut summaries = BTreeSet::new();
    for summary in &projection.summaries {
        if summary.summary_id.trim().is_empty() || !summaries.insert(summary.summary_id.as_str()) {
            return Err(SessionRelationError::Invalid);
        }
    }
    let summary_edges = projection
        .summaries
        .iter()
        .map(|summary| {
            (
                summary.summary_id.as_str(),
                summary
                    .sources
                    .iter()
                    .filter_map(|source| match source {
                        SummarySourceRef::Summary { summary_id } => Some(summary_id.as_str()),
                        SummarySourceRef::Anchor { .. } => None,
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if summary_edges
        .values()
        .flatten()
        .any(|source| !summaries.contains(source))
        || directed_cycle(&summary_edges)
    {
        return Err(SessionRelationError::Cycle);
    }
    let successor_edges = projection
        .summaries
        .iter()
        .map(|summary| {
            (
                summary.summary_id.as_str(),
                summary
                    .predecessor_summary_id
                    .as_deref()
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if directed_cycle(&successor_edges) {
        return Err(SessionRelationError::Cycle);
    }
    let mut copy_identities = BTreeSet::new();
    let copy_edges = projection
        .logical_copies
        .iter()
        .map(|copy| {
            if copy.occurrence_id == copy.copied_from_occurrence_id
                || copy.proof.source_occurrence_id() != &copy.copied_from_occurrence_id
                || !copy_identities.insert((
                    copy.occurrence_id.as_str(),
                    copy.copied_from_occurrence_id.as_str(),
                ))
            {
                return Err(SessionRelationError::Invalid);
            }
            Ok((
                copy.occurrence_id.as_str(),
                copy.copied_from_occurrence_id.as_str(),
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let thread_edges = unique_edges(projection.thread_hierarchy.iter().map(|edge| {
        (
            edge.parent_thread_id.as_str(),
            edge.child_thread_id.as_str(),
        )
    }))?;
    let agent_edges = unique_edges(
        projection
            .agent_hierarchy
            .iter()
            .map(|edge| (edge.parent_agent_id.as_str(), edge.child_agent_id.as_str())),
    )?;
    if edge_list_cycle(&copy_edges)
        || edge_list_cycle(&thread_edges)
        || edge_list_cycle(&agent_edges)
    {
        return Err(SessionRelationError::Cycle);
    }
    Ok(())
}

fn unique_edges<'a>(
    edges: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<Vec<(&'a str, &'a str)>, SessionRelationError> {
    let mut identities = BTreeSet::new();
    edges
        .into_iter()
        .map(|edge| {
            if identities.insert(edge) {
                Ok(edge)
            } else {
                Err(SessionRelationError::Invalid)
            }
        })
        .collect()
}

fn directed_cycle(graph: &BTreeMap<&str, Vec<&str>>) -> bool {
    fn visit<'a>(
        node: &'a str,
        graph: &BTreeMap<&'a str, Vec<&'a str>>,
        visiting: &mut BTreeSet<&'a str>,
        visited: &mut BTreeSet<&'a str>,
    ) -> bool {
        if visited.contains(node) {
            return false;
        }
        if !visiting.insert(node) {
            return true;
        }
        if graph
            .get(node)
            .into_iter()
            .flatten()
            .any(|target| visit(target, graph, visiting, visited))
        {
            return true;
        }
        visiting.remove(node);
        visited.insert(node);
        false
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    graph
        .keys()
        .any(|node| visit(node, graph, &mut visiting, &mut visited))
}

fn edge_list_cycle(edges: &[(&str, &str)]) -> bool {
    let mut graph = BTreeMap::<&str, Vec<&str>>::new();
    for (from, to) in edges {
        graph.entry(from).or_default().push(to);
        graph.entry(to).or_default();
    }
    directed_cycle(&graph)
}
