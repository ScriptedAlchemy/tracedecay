//! `tracedecay_search`: exact and lexical code search over the served code
//! generation.
//!
//! The graph-tool owner decodes the typed request and computes the typed
//! result; every surface renders that result through [`render_search`].

use std::future::Future;
use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::ApplicationProblemDetailV1;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    PrimitiveLaneCompleteV1, PrimitiveRecallV1, PrimitiveSearchFreshnessV1,
    PrimitiveUnavailableStatusV1, SearchCompleteV1, SearchCoverageV1, SearchLaneStateV1,
    SearchLaneStatusV1, SearchResultDisplayV1, SearchResultRowV1, SearchResultV1,
    SearchSurfaceRequestV1, SearchUnavailableV1,
};
use tracedecay_domain::CursorBindingV1;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_query::code_search::{CodeIndexLaneStatusV1, CodeIndexSearchCoverageV1};

use crate::handlers::dependency_hints;
use crate::handlers::support::{
    decode_primitive_request, decode_retrieval_cursor, encode_retrieval_cursor,
    rendered_tool_result, unique_file_paths,
};
use crate::tools::render::{self, Md};
use crate::{McpToolContext, ToolResult};

use super::search_evidence::{
    SearchGraphEvidence, bind_verified_graph_to_search, race_primary_search_with_graph,
};
use super::search_freshness::{
    ServedGenerationV1, freshness_lines, lanes_under_scheduler_freshness, search_freshness,
    worktree_freshness_from_payload,
};
use super::verified::CODE_SYMBOL_EVIDENCE_PREFIX;
use super::{graph_occurrence_id, graph_tool_completion};
use super::{lexical_routing, search_evidence};

/// The request parameters a search continuation is minted for: every field
/// that shapes the ranked result set, and the page size.
fn search_cursor_binding(
    request: &SearchSurfaceRequestV1,
    scope_prefix: Option<&str>,
) -> Result<CursorBindingV1> {
    CursorBindingV1::builder("search")
        .parameter("query", &request.query)
        .parameter("limit", &request.limit)
        .parameter("lexical_anchors", &request.lexical_anchors)
        .parameter("prefer_symbol", &request.prefer_symbol)
        .parameter("lexical_aliases", &request.lexical_aliases)
        .parameter("lexical_phrases", &request.lexical_phrases)
        .parameter("lexical_proximities", &request.lexical_proximities)
        .parameter("lexical_field_filters", &request.lexical_field_filters)
        .parameter(
            "lazy_index_ignored_dependencies",
            &request.lazy_index_ignored_dependencies,
        )
        .parameter("scope", &scope_prefix)
        .build()
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to bind search cursor: {error}"),
        })
}

pub(super) async fn execute_code_index_search(
    executor: Option<&tracedecay_query::code_search::CodeIndexSearchExecutor>,
    request: tracedecay_query::code_search::CodeIndexSearchRequestV1,
) -> tracedecay_query::code_search::CodeIndexSearchOutcomeV1 {
    match executor {
        Some(executor) => executor(request).await,
        None => tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(
            tracedecay_query::code_search::CodeIndexSearchUnavailableV1 {
                code_generation: None,
                reason:
                    tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
                coverage: tracedecay_query::code_search::CodeIndexSearchCoverageV1::unavailable(
                    "code_index_unavailable",
                ),
            },
        ),
    }
}

const IGNORED_DEPENDENCY_GENERATION_ADVANCED: &str =
    "application.symbol-graph.ignored-dependency-generation-advanced";

fn preserve_complete_search_after_lazy_admission(result: Result<()>) -> Result<()> {
    match result {
        Err(error)
            if error
                .project_route_context()
                .is_some_and(|(reason_code, _, _)| {
                    reason_code == IGNORED_DEPENDENCY_GENERATION_ADVANCED
                }) =>
        {
            Ok(())
        }
        result => result,
    }
}

/// The per-lane recall marker every search response carries, so "no
/// matches" is told apart from "the matching lane was not running".
fn search_coverage(coverage: &CodeIndexSearchCoverageV1) -> SearchCoverageV1 {
    fn lane(status: &CodeIndexLaneStatusV1) -> SearchLaneStatusV1 {
        match status {
            CodeIndexLaneStatusV1::Complete => {
                SearchLaneStatusV1::Complete(PrimitiveLaneCompleteV1::Complete)
            }
            CodeIndexLaneStatusV1::Stale { generation } => {
                SearchLaneStatusV1::State(SearchLaneStateV1::Stale {
                    generation: generation.clone(),
                })
            }
            CodeIndexLaneStatusV1::Partial { generation, reason } => {
                SearchLaneStatusV1::State(SearchLaneStateV1::Partial {
                    generation: generation.clone(),
                    reason: reason.map(str::to_owned),
                })
            }
            CodeIndexLaneStatusV1::Unavailable { reason } => {
                SearchLaneStatusV1::State(SearchLaneStateV1::Unavailable {
                    reason: (*reason).to_owned(),
                })
            }
        }
    }
    SearchCoverageV1 {
        exact: lane(&coverage.exact),
        lexical: lane(&coverage.lexical),
        graph: lane(&coverage.graph),
        recall: if coverage.is_degraded() {
            PrimitiveRecallV1::Partial
        } else {
            PrimitiveRecallV1::Full
        },
    }
}

/// Computes one `tracedecay_search` page on the graph-tool owner's side.
#[hotpath::measure(label = "mcp.graph.search.total")]
pub async fn compute_search<F>(
    ctx: &McpToolContext<'_>,
    graph: F,
    args: Value,
    scope_prefix: Option<&str>,
    ignored_dependency_admission: Option<
        &dyn tracedecay_application::code_index::CodeIndexIgnoredDependencyAdmissionPortV1,
    >,
) -> Result<GraphToolCompletionV1>
where
    F: Future<Output = Result<tracedecay_graph_query::VerifiedGraphQuery>>,
{
    let request: SearchSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_search")?;
    let search_executor = ctx.code_index_search_executor();
    let search_authority = ctx.code_index_search_authority();
    let deadline = ctx.deadline().cloned();
    let cancellation = ctx.cancellation().cloned();
    let lexical_routing = lexical_routing::routing_from_request(&request)?;
    let lazy_indexing_requested = request.lazy_index_ignored_dependencies.unwrap_or(false);
    let cursor_binding = search_cursor_binding(&request, scope_prefix)?;
    let cursor = decode_retrieval_cursor(&cursor_binding, request.cursor.as_deref())?;
    let include_graph_node_ids = render::wants_json(&args);
    let limit = request.limit.map_or(10, |v| v.min(500) as usize);
    let query = request.query.as_str();
    // A scope prefix cannot be applied as a post-filter here the way the
    // sibling handlers do it: the retrieval pipeline returns anchor-keyed
    // candidates that carry no file path. Refusing to search at all would make
    // the tool return nothing for the whole session (any serve launched from a
    // subdirectory sets a scope), so run the search and report below that the
    // scope was not honored rather than silently implying it was.
    let search_request = tracedecay_query::code_search::CodeIndexSearchRequestV1 {
        project_root: ctx.project_root().to_path_buf(),
        query: query.to_owned(),
        source_revision: None,
        source_tree: None,
        source_reference: None,
        limit,
        cursor,
        lexical_routing,
        authority: search_authority.cloned(),
        deadline: deadline.clone(),
        cancellation: cancellation.clone(),
    };
    let search = execute_code_index_search(search_executor, search_request.clone());
    let (mut outcome, graph) = race_primary_search_with_graph(
        search,
        graph,
        lazy_indexing_requested,
        Some(limit),
        scope_prefix.is_some(),
    )
    .await;
    let refresh_after_generation_mismatch = matches!(
        (&outcome, &graph),
        (
            tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(complete),
            Ok(graph),
        ) if graph.generation().as_str() != complete.code_generation
            && (scope_prefix.is_some()
                || dependency_hints::should_check_external_import_hint(
                    complete.ordered_candidates.len(),
                    limit,
                ))
    );
    if refresh_after_generation_mismatch {
        let refreshed = execute_code_index_search(search_executor, search_request).await;
        if matches!(
            refreshed,
            tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(_)
        ) {
            outcome = refreshed;
        }
    }
    // Read after the search settles: the verdict must describe the scheduler
    // state at serve time, not a snapshot taken before the lanes ran.
    let freshness_payload = ctx.freshness().await;
    let worktree_freshness = worktree_freshness_from_payload(freshness_payload.as_ref());
    match outcome {
        tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(complete) => {
            let graph = if lazy_indexing_requested && complete.ordered_candidates.is_empty() {
                // Explicit ignored-dependency admission is generation-checked
                // by the canonical admission port against the graph's own
                // active generation. It must therefore inspect the verified
                // graph before binding optional enrichment to the text-search
                // generation: text can truthfully serve one generation while
                // graph activation has already advanced to its successor.
                let graph = graph?;
                preserve_complete_search_after_lazy_admission(
                    hotpath::future!(
                        dependency_hints::admit_verified_ignored_dependency(
                            ctx,
                            ignored_dependency_admission,
                            &graph,
                            query,
                            scope_prefix
                        ),
                        label = "mcp.graph.search.admit"
                    )
                    .await,
                )?;
                bind_verified_graph_to_search(Ok(graph), &complete.code_generation)
            } else {
                bind_verified_graph_to_search(graph, &complete.code_generation)
            };
            let mut results = Vec::with_capacity(complete.ordered_candidates.len());
            let mut graph_evidence = SearchGraphEvidence::new(graph.as_ref());
            // The generation-bound display metadata names each result's
            // declaring file; that set is the raw-read counterfactual the
            // savings accounting charges this response against.
            let touched_files = unique_file_paths(
                complete
                    .ordered_candidates
                    .iter()
                    .filter_map(|ranked| {
                        complete.display_by_anchor.get(&ranked.candidate.anchor_id)
                    })
                    .map(|display| display.path.as_str()),
            );
            hotpath::measure_block!("mcp.graph.search.graph", {
                for ranked in &complete.ordered_candidates {
                    let anchor = ranked.candidate.anchor_id.as_str();
                    let mut node_id = if anchor.starts_with(CODE_SYMBOL_EVIDENCE_PREFIX) {
                        Some(graph_occurrence_id(anchor)?.as_str().to_owned())
                    } else {
                        None
                    };
                    let display = complete.display_by_anchor.get(&ranked.candidate.anchor_id);
                    let display_unavailable = match display {
                        Some(_) => None,
                        None => complete
                            .display_unavailable_by_anchor
                            .get(&ranked.candidate.anchor_id)
                            .copied(),
                    };
                    if include_graph_node_ids
                        && node_id.is_none()
                        && let Some(display) = display
                    {
                        node_id = graph_evidence.node_id_for(display);
                    }
                    results.push(SearchResultRowV1 {
                        candidate: ranked.candidate.clone(),
                        final_ordinal: ranked.final_ordinal,
                        node_id,
                        display: display.map(|display| SearchResultDisplayV1 {
                            name: display.name.clone(),
                            qualified_name: display.qualified_name.clone(),
                            kind: display.kind.clone(),
                            path: display.path.clone(),
                        }),
                        display_unavailable,
                        lexical_routes: None,
                    });
                }
            });
            let result_count = results.len();
            let (lexical_routes, lexical_anchors) =
                lexical_routing::route_evidence(&mut results, &complete.lexical_routes);
            let external_import_hint = if scope_prefix.is_some()
                || dependency_hints::should_check_external_import_hint(result_count, limit)
            {
                graph_evidence.external_import_hint(ctx, query, limit, scope_prefix)
            } else {
                None
            };
            let coverage = lanes_under_scheduler_freshness(
                complete.coverage.clone(),
                &complete.code_generation,
                &worktree_freshness,
            );
            let result = SearchCompleteV1 {
                freshness: search_freshness(
                    ServedGenerationV1::Served(&complete.code_generation),
                    &coverage,
                    &worktree_freshness,
                ),
                query_fallback_digest: complete.query_fallback.digest.as_str().to_owned(),
                next_cursor: encode_retrieval_cursor(
                    &cursor_binding,
                    complete.next_cursor.as_ref(),
                )?,
                coverage: search_coverage(&coverage),
                code_generation: complete.code_generation,
                results,
                lexical_routes,
                lexical_anchors,
                scope_prefix: scope_prefix.map(str::to_owned),
                scope_prefix_applied: scope_prefix.map(|_| false),
                verified_graph_evidence: graph_evidence.unavailable().cloned(),
                external_import_hint,
            };
            Ok(graph_tool_completion(
                GraphToolResultV1::Search(Box::new(SearchResultV1::Complete(result))),
                touched_files,
            ))
        }
        tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(unavailable) => {
            let reason = unavailable.reason.as_str();
            let freshness = search_freshness(
                ServedGenerationV1::Unavailable { reason },
                &unavailable.coverage,
                &worktree_freshness,
            );
            let detail = freshness
                .indexing
                .as_ref()
                .and_then(|indexing| indexing.parked.as_ref())
                .map(|parked| ApplicationProblemDetailV1::Parked {
                    cause: parked.reason.clone(),
                    remedy: parked.remediation.clone(),
                    retries_on_wake: parked.retries_on_wake,
                });
            let result = SearchUnavailableV1 {
                freshness,
                results: Vec::new(),
                code_generation: unavailable.code_generation,
                query_fallback_digest: None,
                status: PrimitiveUnavailableStatusV1::Unavailable,
                reason: reason.to_owned(),
                coverage: search_coverage(&unavailable.coverage),
                verified_graph_evidence: SearchGraphEvidence::new(graph.as_ref())
                    .unavailable()
                    .cloned(),
                detail,
            };
            Ok(graph_tool_completion(
                GraphToolResultV1::Search(Box::new(SearchResultV1::Unavailable(result))),
                Vec::new(),
            ))
        }
    }
}

/// Renders a search result as its tool result: the `freshness:` verdict line
/// ahead of the markdown body, or the typed JSON. An unavailable search names
/// its failure, parked indexing by its cause and remedy.
pub(crate) fn render_search(
    response_handle_root: Option<&Path>,
    args: &Value,
    result: &SearchResultV1,
) -> Result<ToolResult> {
    let value = serde_json::to_value(result)?;
    let freshness: &PrimitiveSearchFreshnessV1 = match result {
        SearchResultV1::Complete(complete) => &complete.freshness,
        SearchResultV1::Unavailable(unavailable) => &unavailable.freshness,
    };
    let rendered = rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
        format!("{}{}", freshness_lines(freshness), render_search_md(&value))
    });
    Ok(match result {
        SearchResultV1::Complete(_) => rendered,
        SearchResultV1::Unavailable(unavailable) => {
            rendered.with_failure_message(match &unavailable.detail {
                Some(detail) => detail.message(),
                None => format!("code-index search unavailable: {}", unavailable.reason),
            })
        }
    })
}

/// Warns, in the human-facing body, that a result list is short because a lane
/// was missing. A degraded page is otherwise indistinguishable from a thorough
/// one, which is exactly how a partial answer gets trusted as a complete one.
pub(super) fn append_coverage_md(md: &mut Md, value: &Value) {
    let Some(coverage) = value.get("coverage") else {
        return;
    };
    if coverage.get("recall").and_then(Value::as_str) != Some("partial") {
        return;
    }
    let mut notes = Vec::new();
    for lane in ["exact", "lexical", "graph"] {
        let status = coverage.get(lane);
        match status
            .and_then(|status| status.get("status"))
            .and_then(Value::as_str)
        {
            Some("stale") => {
                let generation = status
                    .and_then(|status| status.get("generation"))
                    .and_then(Value::as_str)
                    .unwrap_or("previous");
                notes.push(format!("{lane}: stale (generation `{generation}`)"));
            }
            Some("unavailable") => {
                let reason = status
                    .and_then(|status| status.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("unavailable");
                notes.push(format!("{lane}: unavailable ({reason})"));
            }
            _ => {}
        }
    }
    if notes.is_empty() {
        return;
    }
    md.blank()
        .heading(3, "Coverage")
        .line("Partial recall. Some retrieval lanes did not answer:");
    for note in notes {
        md.bullet(&note);
    }
}

/// The typed reason a result row carries no display, as a bullet suffix.
fn display_unavailable_suffix(row: &Value) -> String {
    row.get("display_unavailable")
        .and_then(Value::as_str)
        .map(|reason| format!(" · display unavailable: {reason}"))
        .unwrap_or_default()
}

fn render_search_md(value: &Value) -> String {
    let items = if value.is_array() {
        value.as_array()
    } else {
        value.get("results").and_then(Value::as_array)
    };
    let mut md = Md::new();
    md.heading(2, "Search Results");
    match items {
        Some(items) if !items.is_empty() => {
            for it in items {
                if let Some(candidate) = it.get("candidate") {
                    let anchor = render::field_str(candidate, "anchor_id");
                    let exact_class = render::field_str(candidate, "exact_class");
                    let utility = candidate
                        .get("utility_micros")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    let ordinal = it
                        .get("final_ordinal")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    let via = lexical_routing::result_route_suffix(it);
                    if let Some(display) = it.get("display") {
                        let name = render::field_str(display, "name");
                        let kind = render::field_str(display, "kind");
                        md.bullet(&format!(
                            "**{name}** ({kind}, {exact_class}), rank {} · utility {utility}{via}",
                            ordinal.saturating_add(1)
                        ));
                        md.line(&format!("  anchor_id: `{anchor}`"));
                    } else {
                        let omitted = display_unavailable_suffix(it);
                        md.bullet(&format!(
                            "**{anchor}** ({exact_class}), rank {} · utility {utility}{via}{omitted}",
                            ordinal.saturating_add(1)
                        ));
                    }
                    if let Some(node_id) = it.get("node_id").and_then(Value::as_str) {
                        md.line(&format!(
                            "  Read source: `tracedecay_source_body` with `node_id: {node_id}`"
                        ));
                    }
                    continue;
                }
                let name = render::field_str(it, "name");
                let kind = render::field_str(it, "kind");
                let file = render::field_str(it, "file");
                let line = render::field_i64(it, "line");
                let id = render::field_str(it, "id");
                let score = it.get("score").and_then(Value::as_f64).unwrap_or(0.0);
                md.bullet(&format!(
                    "**{name}** ({kind}), {file}:{line} · score {score:.1}"
                ));
                let sig = render::field_str(it, "signature");
                if sig.is_empty() {
                    md.line(&format!("  `{id}`"));
                } else {
                    md.line(&format!("  `{id}` · `{sig}`"));
                }
            }
        }
        _ => {
            md.empty_note("No matching symbols.");
        }
    }
    if let Some(reason) = value.get("reason").and_then(Value::as_str) {
        md.blank()
            .heading(3, "Availability")
            .line(&format!("Search unavailable: {reason}."));
    }
    lexical_routing::append_routes_md(&mut md, value);
    append_coverage_md(&mut md, value);
    if let Some(msg) = value
        .get("index_coverage_hint")
        .and_then(|h| h.get("message"))
        .and_then(Value::as_str)
    {
        md.blank().heading(3, "Index Coverage Hint").line(msg);
    }
    dependency_hints::append_external_import_hint_md(&mut md, value);
    search_evidence::append_verified_graph_evidence_md(&mut md, value);
    md.render()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_query::retrieval::lexical::LexicalRoutingV1;

    use super::*;

    #[test]
    fn complete_search_preserves_generation_advance_but_not_stale_admission() {
        let advanced = TraceDecayError::project_route(
            IGNORED_DEPENDENCY_GENERATION_ADVANCED,
            true,
            "new generation published",
        );
        assert!(matches!(
            preserve_complete_search_after_lazy_admission(Err(advanced)),
            Ok(())
        ));

        let stale = TraceDecayError::project_route(
            "application.symbol-graph.ignored-dependency-generation-stale",
            true,
            "source generation is stale",
        );
        let error = preserve_complete_search_after_lazy_admission(Err(stale))
            .expect_err("stale admission must remain a typed retrieval failure");
        assert!(matches!(
            error.project_route_context(),
            Some((
                "application.symbol-graph.ignored-dependency-generation-stale",
                true,
                _
            ))
        ));
    }

    #[test]
    fn schema_anchor_bound_matches_the_retrieval_kernel_bound() {
        assert_eq!(
            tracedecay_contracts::retrieval::SEARCH_MAX_LEXICAL_ANCHORS,
            tracedecay_query::retrieval::lexical::MAX_LEXICAL_ANCHORS_V1
        );
        assert_eq!(
            tracedecay_contracts::retrieval::SEARCH_MAX_LEXICAL_ANCHOR_BYTES,
            tracedecay_query::retrieval::lexical::MAX_LEXICAL_ANCHOR_BYTES_V1
        );
    }

    /// A warm response must render exactly as it did before coverage existed:
    /// every lane complete, no coverage section, no added lines.
    #[test]
    fn warm_coverage_leaves_the_rendered_body_unchanged() {
        let coverage = serde_json::to_value(search_coverage(
            &tracedecay_query::code_search::CodeIndexSearchCoverageV1::warm(),
        ))
        .expect("coverage JSON");
        assert_eq!(coverage["recall"], json!("full"));
        assert_eq!(coverage["exact"], json!("complete"));

        let without = json!({
            "results": [{
                "candidate": {
                    "anchor_id": "code-symbol:symbol.v1",
                    "exact_class": "exact_message",
                    "utility_micros": 4_000_000
                },
                "final_ordinal": 0,
            }],
            "code_generation": "generation.warm",
        });
        let mut with = without.clone();
        with["coverage"] = coverage;

        assert_eq!(
            render_search_md(&with),
            render_search_md(&without),
            "warm coverage must be additive metadata, never rendered output"
        );
    }

    #[test]
    fn search_renders_symbol_id_for_source_body_without_graph_enrichment() {
        let node_id =
            "symbol.v1.sha256:4ddd636456fccc2962006c7803bd94b2d7d732c6830993a429535e0b0ff0b688";
        let anchor = format!("{CODE_SYMBOL_EVIDENCE_PREFIX}{node_id}");
        let symbol = graph_occurrence_id(&anchor).expect("search symbol anchor");
        for display in [
            Value::Null,
            json!({"name": "load_current_mutations", "kind": "function"}),
        ] {
            let mut result = json!({
                "candidate": {"anchor_id": anchor, "exact_class": "approximate"},
                "node_id": symbol,
            });
            if !display.is_null() {
                result["display"] = display;
            }
            let rendered = render_search_md(&json!({"results": [result]}));
            assert!(rendered.contains(&format!(
                "Read source: `tracedecay_source_body` with `node_id: {node_id}`"
            )));
            assert!(!rendered.contains("node_id: code-symbol:"));
        }
        let chunk = render_search_md(&json!({"results": [{
            "candidate": {"anchor_id": "code-chunk:chunk.fixture"}
        }]}));
        assert!(!chunk.contains("tracedecay_source_body"));
    }

    #[test]
    fn search_renders_the_typed_reason_a_row_has_no_display() {
        let rendered = render_search_md(&json!({"results": [
            {
                "candidate": {"anchor_id": "code-symbol:a", "exact_class": "approximate", "utility_micros": 7},
                "final_ordinal": 0,
                "display_unavailable": "stale",
            },
            {
                "candidate": {"anchor_id": "code-symbol:b", "exact_class": "approximate", "utility_micros": 5},
                "final_ordinal": 1,
                "display": {"name": "clamp", "kind": "function"},
            },
        ]}));
        let bullets = rendered
            .lines()
            .filter(|line| line.starts_with("- **"))
            .collect::<Vec<_>>();
        assert_eq!(
            bullets,
            vec![
                "- **code-symbol:a** (approximate), rank 1 · utility 7 · display unavailable: stale",
                "- **clamp** (function, approximate), rank 2 · utility 5",
            ],
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn search_reads_freshness_after_the_search_future_resolves() {
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let search_order = std::sync::Arc::clone(&order);
        let freshness_order = std::sync::Arc::clone(&order);
        let executor: tracedecay_query::code_search::CodeIndexSearchExecutor = std::sync::Arc::new(
            move |_| {
                let search_order = std::sync::Arc::clone(&search_order);
                Box::pin(async move {
                    search_order.lock().expect("order").push("search_start");
                    tokio::task::yield_now().await;
                    search_order.lock().expect("order").push("search_done");
                    tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(
                        tracedecay_query::code_search::CodeIndexSearchUnavailableV1 {
                            code_generation: None,
                            reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            coverage: tracedecay_query::code_search::CodeIndexSearchCoverageV1::unavailable(
                                "authority_unavailable",
                            ),
                        },
                    )
                })
            },
        );
        let freshness: tracedecay_contracts::code_index_freshness::CodeIndexFreshnessReader =
            std::sync::Arc::new(move |_| {
                let freshness_order = std::sync::Arc::clone(&freshness_order);
                Box::pin(async move {
                    freshness_order.lock().expect("order").push("freshness");
                    Ok(None)
                })
            });
        let temp = tempfile::tempdir().expect("temp root");
        let admitted = crate::tool_context::tests::scope("freshness-order");
        let project = crate::tool_context::tests::project_bundle(temp.path(), &admitted, None);
        let authority = tracedecay_query::code_search::CodeIndexSearchAuthorityV1 {
            principal: tracedecay_domain::PrincipalId::new("principal.freshness-order")
                .expect("principal"),
            authorization_revision: tracedecay_domain::AuthorizationRevision::new(
                "revision.freshness-order",
            )
            .expect("revision"),
        };
        let code_index =
            crate::AdmittedCodeIndex::new(&authority, Some(&executor), None, None, None)
                .expect("search executor admits");
        let ctx = crate::McpToolContext::bind(crate::McpToolBinding {
            project: &project,
            request: crate::McpRequestAuthoritiesV1 {
                code_index: Some(code_index),
                freshness: Some(&freshness),
                ..crate::McpRequestAuthoritiesV1::default()
            },
        })
        .expect("admitted search binding");

        compute_search(
            &ctx,
            async {
                Err(TraceDecayError::project_route(
                    "verified-code-graph-read-unavailable",
                    true,
                    "ordering test does not admit a graph",
                ))
            },
            json!({"query": "fixture"}),
            None,
            None,
        )
        .await
        .expect("search answers without a graph");

        assert_eq!(
            *order.lock().expect("order"),
            ["search_start", "search_done", "freshness"],
            "freshness must be read after the search future resolves"
        );
    }

    #[tokio::test]
    async fn installed_search_executor_owns_dispatch() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&calls);
        let executor: tracedecay_query::code_search::CodeIndexSearchExecutor = std::sync::Arc::new(
            move |request| {
                observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                assert_eq!(request.query, "fixture");
                Box::pin(async {
                    tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(
                        tracedecay_query::code_search::CodeIndexSearchUnavailableV1 {
                            code_generation: Some("generation.fixture".to_owned()),
                            reason: tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            coverage: tracedecay_query::code_search::CodeIndexSearchCoverageV1::unavailable(
                                "authority_unavailable",
                            ),
                        },
                    )
                })
            },
        );
        let outcome = execute_code_index_search(
            Some(&executor),
            tracedecay_query::code_search::CodeIndexSearchRequestV1 {
                project_root: std::path::PathBuf::from("/fixture"),
                query: "fixture".to_owned(),
                source_revision: None,
                source_tree: None,
                source_reference: None,
                limit: 10,
                cursor: None,
                lexical_routing: LexicalRoutingV1::default(),
                authority: None,
                deadline: None,
                cancellation: None,
            },
        )
        .await;
        assert!(matches!(
            outcome,
            tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(
                tracedecay_query::code_search::CodeIndexSearchUnavailableV1 {
                    reason:
                        tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    ..
                }
            )
        ));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn missing_search_executor_is_typed_capability_unavailable() {
        let outcome = execute_code_index_search(
            None,
            tracedecay_query::code_search::CodeIndexSearchRequestV1 {
                project_root: std::path::PathBuf::from("/fixture"),
                query: "fixture".to_owned(),
                source_revision: None,
                source_tree: None,
                source_reference: None,
                limit: 10,
                cursor: None,
                lexical_routing: LexicalRoutingV1::default(),
                authority: None,
                deadline: None,
                cancellation: None,
            },
        )
        .await;
        assert!(matches!(
            outcome,
            tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(
                tracedecay_query::code_search::CodeIndexSearchUnavailableV1 {
                    reason:
                        tracedecay_query::code_search::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
                    ..
                }
            )
        ));
    }
}
