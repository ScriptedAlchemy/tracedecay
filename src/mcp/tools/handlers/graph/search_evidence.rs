use std::future::Future;

use serde_json::Value;

use crate::errors::{Result, TraceDecayError};
use crate::mcp::server::CodeIndexSearchDisplayV1;
use crate::mcp::tools::render::Md;
use crate::tracedecay::queries::graph::VerifiedGraphQuery;

use super::super::dependency_hints;

pub(super) async fn race_primary_search_with_graph<S, G>(
    search: S,
    graph: G,
    require_graph_for_empty_result: bool,
) -> (
    crate::mcp::server::CodeIndexSearchOutcomeV1,
    Result<VerifiedGraphQuery>,
)
where
    S: Future<Output = crate::mcp::server::CodeIndexSearchOutcomeV1>,
    G: Future<Output = Result<VerifiedGraphQuery>>,
{
    tokio::pin!(graph);
    tokio::pin!(search);
    tokio::select! {
        biased;
        graph = &mut graph => (search.await, graph),
        outcome = &mut search => {
            let wait_for_graph = require_graph_for_empty_result
                && matches!(
                    &outcome,
                    crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete)
                        if complete.ordered_candidates.is_empty()
                );
            let graph = if wait_for_graph {
                graph.await
            } else {
                Err(TraceDecayError::project_route(
                    "verified-code-graph-read-unavailable",
                    true,
                    "verified graph evidence did not become available before primary search completed",
                ))
            };
            (outcome, graph)
        },
    }
}

pub(super) fn bind_verified_graph_to_search(
    graph: Result<VerifiedGraphQuery>,
    search_generation: &str,
) -> Result<VerifiedGraphQuery> {
    match graph {
        Ok(graph) if graph.generation().as_str() == search_generation => Ok(graph),
        Ok(graph) => Err(TraceDecayError::project_route(
            "verified-code-graph-generation-mismatch",
            true,
            format!(
                "primary search generation {search_generation} does not match verified graph generation {}",
                graph.generation().as_str()
            ),
        )),
        Err(error) => Err(error),
    }
}

pub(super) struct SearchGraphEvidence<'a> {
    graph: std::result::Result<&'a VerifiedGraphQuery, &'a TraceDecayError>,
    unavailable: Option<Value>,
}

pub(super) fn append_verified_graph_evidence_md(md: &mut Md, value: &Value) {
    let Some(evidence) = value.get("verified_graph_evidence") else {
        return;
    };
    if evidence.get("status").and_then(Value::as_str) != Some("unavailable") {
        return;
    }
    let detail = evidence
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("verified graph evidence is unavailable");
    md.blank()
        .heading(3, "Verified Graph Evidence")
        .line(&format!("Graph enrichment unavailable: {detail}"));
}

impl<'a> SearchGraphEvidence<'a> {
    pub(super) fn new(
        graph: std::result::Result<&'a VerifiedGraphQuery, &'a TraceDecayError>,
    ) -> Self {
        let unavailable = graph
            .as_ref()
            .err()
            .map(|error| dependency_hints::unavailable_hint(error));
        Self { graph, unavailable }
    }

    pub(super) fn enrich_node_id(
        &mut self,
        result: &mut Value,
        display: &CodeIndexSearchDisplayV1,
    ) {
        let Ok(graph) = self.graph else {
            return;
        };
        match unique_graph_node_id_for_search_display(graph, display) {
            Ok(Some(node_id)) => result["node_id"] = Value::String(node_id),
            Ok(None) => {}
            Err(error) => {
                self.unavailable = Some(dependency_hints::unavailable_hint(&error));
            }
        }
    }

    pub(super) fn unavailable(&self) -> Option<&Value> {
        self.unavailable.as_ref()
    }

    pub(super) async fn external_import_hint(
        &self,
        query: &str,
        limit: usize,
        scope_prefix: Option<&str>,
        deadline: Option<&tracedecay_application::Deadline>,
        cancellation: Option<&tracedecay_application::CancellationSignal>,
    ) -> Option<Value> {
        match self.graph {
            Ok(graph) => match dependency_hints::external_import_hint(
                graph,
                query,
                limit,
                scope_prefix,
                deadline,
                cancellation,
            )
            .await
            {
                Ok(hint) => hint,
                Err(error) => Some(dependency_hints::unavailable_hint(&error)),
            },
            Err(error) => Some(dependency_hints::unavailable_hint(error)),
        }
    }
}

fn unique_graph_node_id_for_search_display(
    graph: &VerifiedGraphQuery,
    display: &CodeIndexSearchDisplayV1,
) -> Result<Option<String>> {
    let nodes = graph.resolve_qualified_name(&display.qualified_name, Some(&display.kind), 16)?;
    let mut matches = nodes.into_iter().filter(|node| {
        node.binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_deref())
            == Some(display.path.as_str())
    });
    let Some(node) = matches.next() else {
        return Ok(None);
    };
    Ok(matches
        .next()
        .is_none()
        .then(|| node.occurrence.as_str().to_owned()))
}
