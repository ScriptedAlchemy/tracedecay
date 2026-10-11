use std::future::Future;

use serde_json::Value;

use crate::tools::render::Md;
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_contracts::retrieval::{PrimitiveUnavailableEvidenceV1, SearchExternalImportHintV1};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_query::code_search::CodeIndexSearchDisplayV1;

use super::verified::required_graph_file_path;
use crate::McpToolContext;
use crate::handlers::dependency_hints;

#[tracing::instrument(name = "mcp.graph.search_race", level = "trace", skip_all)]
pub(super) async fn race_primary_search_with_graph<S, G>(
    search: S,
    graph: G,
    require_graph_for_empty_result: bool,
    sparse_result_limit: Option<usize>,
    require_graph_for_complete_result: bool,
) -> (
    tracedecay_query::code_search::CodeIndexSearchOutcomeV1,
    Result<VerifiedGraphQuery>,
)
where
    S: Future<Output = tracedecay_query::code_search::CodeIndexSearchOutcomeV1>,
    G: Future<Output = Result<VerifiedGraphQuery>>,
{
    tokio::pin!(graph);
    tokio::pin!(search);
    tokio::select! {
        biased;
        graph = &mut graph => (search.await, graph),
        outcome = &mut search => {
            let wait_for_graph = matches!(
                &outcome,
                tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(complete)
                    if require_graph_for_complete_result
                        || sparse_result_limit.is_some_and(|limit| {
                            dependency_hints::should_check_external_import_hint(
                                complete.ordered_candidates.len(),
                                limit,
                            )
                        })
                        || (require_graph_for_empty_result
                            && complete.ordered_candidates.is_empty())
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
    unavailable: Option<PrimitiveUnavailableEvidenceV1>,
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
            .map(|error| dependency_hints::unavailable_evidence(error));
        Self { graph, unavailable }
    }

    /// The graph symbol a search display names. A graph read failure is
    /// recorded as the response's unavailable graph evidence.
    pub(super) fn node_id_for(&mut self, display: &CodeIndexSearchDisplayV1) -> Option<String> {
        let Ok(graph) = self.graph else {
            return None;
        };
        match graph_symbol_for_display(graph, display) {
            Ok(symbol) => symbol.map(|symbol| symbol.occurrence.as_str().to_owned()),
            Err(error) => {
                self.unavailable = Some(dependency_hints::unavailable_evidence(&error));
                None
            }
        }
    }

    pub(super) fn unavailable(&self) -> Option<&PrimitiveUnavailableEvidenceV1> {
        self.unavailable.as_ref()
    }

    #[tracing::instrument(name = "mcp.graph.import_hint", level = "trace", skip_all)]
    pub(super) fn external_import_hint(
        &self,
        ctx: &McpToolContext<'_>,
        query: &str,
        limit: usize,
        scope_prefix: Option<&str>,
    ) -> Option<SearchExternalImportHintV1> {
        match self.graph {
            Ok(graph) => {
                match dependency_hints::external_import_hint(ctx, graph, query, limit, scope_prefix)
                {
                    Ok(hint) => hint.map(SearchExternalImportHintV1::Candidates),
                    Err(error) => Some(SearchExternalImportHintV1::Unavailable(
                        dependency_hints::unavailable_evidence(&error),
                    )),
                }
            }
            Err(error) => Some(SearchExternalImportHintV1::Unavailable(
                dependency_hints::unavailable_evidence(error),
            )),
        }
    }
}

/// The graph symbol a search display names: its exact occurrence, declared
/// in the display's file. A file hit names none.
pub(super) fn graph_symbol_for_display(
    graph: &VerifiedGraphQuery,
    display: &CodeIndexSearchDisplayV1,
) -> Result<Option<CodeGraphSymbolSummaryV1>> {
    let Some(occurrence) = display.occurrence.as_ref() else {
        return Ok(None);
    };
    let Some(symbol) = graph.symbol_summary(occurrence)? else {
        return Ok(None);
    };
    Ok((required_graph_file_path(&symbol)? == display.path).then_some(symbol))
}
