//! Graph traversal tool handlers: `search`, `context`, `callers`, `callees`,
//! `impact`, `node`, `similar`, `rename_preview`, `callers_for`, `by_qualified_name`,
//! `signature`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::future::Future;

use serde_json::{Value, json};
use tracedecay_application::retrieval::{
    CalleeV1, CalleesSurfaceRequestV1, ContextCodeBlockV1, ContextModeV1, ContextResultV1,
    ContextSurfaceRequestV1, ImpactNodeV1, ImpactResultV1, ImpactSurfaceRequestV1, NodeDetailsV1,
    NodeExpansionCostV1, NodeSurfaceRequestV1, RenamePreviewNodeV1,
    RenamePreviewPrimitiveRequestV1, RenamePreviewPrimitiveResultV1, RenamePreviewReferenceV1,
    RenamePreviewTextOnlyMatchV1, SimilarSurfaceRequestV1, SimilarSymbolV1,
};
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_domain::RelationEdgeKindV1;

use crate::context::CONTEXT_SEEN_NODE_IDS_LABEL;
use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;
use crate::types::{EdgeKind, NodeKind};

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::dependency_hints;
use super::support::{
    self, CONTEXT_MEMORY_ANALYTICS_KEY, decode_primitive_request, require_node_id,
    take_internal_context_memory_analytics, text_tool_result, unique_file_paths,
};

mod context_support;
mod primitive_surface;
mod search_evidence;
mod verified;

#[cfg(test)]
use context_support::context_memory_section;
use context_support::{
    ContextMemoryOutcome, context_markdown_lane_preview, context_memory_analytics_value,
    context_memory_options, context_memory_outcome, context_memory_read_control,
    insert_context_memory_section,
};
use primitive_surface::{
    node_not_found as node_not_found_result, search_coverage as primitive_search_coverage,
    semantic_search_mode as primitive_semantic_search_mode,
    symbol_location as primitive_symbol_location,
};
use search_evidence::{
    SearchGraphEvidence, bind_verified_graph_to_search, race_primary_search_with_graph,
};

use verified::{
    GRAPH_RELATION_READ_LIMIT, append_verified_plan_context, canonical_relation_kind,
    canonical_relation_kind_name, cost_to_expand_verified, graph_name_matches, graph_occurrence_id,
    graph_symbol_corrupt, graph_symbol_end_line, graph_symbol_location_value, graph_symbol_paths,
    graph_symbols_in_scope, line_for_byte_offset, nodes_addressed_by_args,
    required_graph_file_path, required_graph_metadata, single_graph_adjacency_batch,
    traverse_verified_neighbors, verified_context_markdown, verified_neighbor_value,
    verified_trait_dispatch_targets,
};

fn semantic_search_mode(args: &Value) -> Result<crate::mcp::server::CodeIndexSearchModeV1> {
    match args.get("semantic_mode").and_then(Value::as_str) {
        None | Some("fallback_allowed") => {
            Ok(crate::mcp::server::CodeIndexSearchModeV1::FallbackAllowed)
        }
        Some("strict_semantic") => Ok(crate::mcp::server::CodeIndexSearchModeV1::StrictSemantic),
        Some(_) => Err(TraceDecayError::Config {
            message: "semantic_mode must be one of fallback_allowed, strict_semantic".to_owned(),
        }),
    }
}

async fn execute_code_index_search(
    executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    request: crate::mcp::server::CodeIndexSearchRequestV1,
) -> crate::mcp::server::CodeIndexSearchOutcomeV1 {
    match executor {
        Some(executor) => executor(request).await,
        None => crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
            crate::mcp::server::CodeIndexSearchUnavailableV1 {
                code_generation: None,
                reason:
                    crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
                semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                    reason: "code_index_unavailable",
                },
                coverage: crate::mcp::server::CodeIndexSearchCoverageV1::unavailable(
                    "code_index_unavailable",
                ),
            },
        ),
    }
}

fn semantic_status_value(
    mode: crate::mcp::server::CodeIndexSearchModeV1,
    status: &crate::mcp::server::CodeIndexSemanticStatusV1,
) -> Value {
    let mode = match mode {
        crate::mcp::server::CodeIndexSearchModeV1::FallbackAllowed => "fallback_allowed",
        crate::mcp::server::CodeIndexSearchModeV1::StrictSemantic => "strict_semantic",
    };
    match status {
        crate::mcp::server::CodeIndexSemanticStatusV1::Complete => json!({
            "status": "complete",
            "mode": mode,
        }),
        crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable { reason } => json!({
            "status": "unavailable",
            "mode": mode,
            "reason": reason,
        }),
    }
}

/// Renders the per-lane recall marker so a caller can tell a full-recall
/// answer from one produced while a lane was down. Emitted on every search
/// response, including the successful ones, because "no matches" and "the
/// matching lane was not running" are otherwise indistinguishable.
fn coverage_value(coverage: &crate::mcp::server::CodeIndexSearchCoverageV1) -> Value {
    fn lane(status: &crate::mcp::server::CodeIndexLaneStatusV1) -> Value {
        match status {
            crate::mcp::server::CodeIndexLaneStatusV1::Complete => json!("complete"),
            crate::mcp::server::CodeIndexLaneStatusV1::Stale { generation } => json!({
                "status": "stale",
                "generation": generation,
            }),
            crate::mcp::server::CodeIndexLaneStatusV1::Unavailable { reason } => json!({
                "status": "unavailable",
                "reason": reason,
            }),
        }
    }

    json!({
        "exact": lane(&coverage.exact),
        "lexical": lane(&coverage.lexical),
        "graph": lane(&coverage.graph),
        "semantic": lane(&coverage.semantic),
        "recall": if coverage.is_degraded() { "partial" } else { "full" },
    })
}

fn user_line(line: u32) -> u32 {
    line.saturating_add(1)
}

fn rendered_tool_result<F>(
    cg: &TraceDecay,
    args: &Value,
    value: &Value,
    touched_files: Vec<String>,
    md: F,
) -> ToolResult
where
    F: FnOnce() -> String,
{
    support::rendered_tool_result(Some(cg.project_root()), args, value, touched_files, md)
}

/// [`rendered_tool_result`] with the default [`render::generic_md`] body.
fn generic_tool_result(
    cg: &TraceDecay,
    args: &Value,
    value: &Value,
    touched_files: Vec<String>,
) -> ToolResult {
    support::generic_tool_result(Some(cg.project_root()), args, value, touched_files)
}

fn rendered_context_tool_result(
    cg: &TraceDecay,
    args: &Value,
    mut value: Value,
    touched_files: Vec<String>,
    full_markdown: String,
    preview_markdown: Option<&str>,
) -> ToolResult {
    let internal_analytics = take_internal_context_memory_analytics(&mut value);
    let text = if render::wants_json(args) {
        render::finalize(Some(cg.project_root()), args, &value, || full_markdown)
    } else {
        render::markdown_preview_with_handle(
            Some(cg.project_root()),
            &full_markdown,
            preview_markdown.unwrap_or(&full_markdown),
        )
    };
    let result = text_tool_result(&text, touched_files);
    if let Some(internal_analytics) = internal_analytics {
        result.with_internal_analytics(internal_analytics)
    } else {
        result
    }
}

/// Handles `tracedecay_search` tool calls.
pub(super) async fn handle_search<F>(
    cg: &TraceDecay,
    graph: F,
    args: Value,
    scope_prefix: Option<&str>,
    search_executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    search_authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    ignored_dependency_admission: Option<
        &dyn tracedecay_usecases::code_index::CodeIndexIgnoredDependencyAdmissionPortV1,
    >,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult>
where
    F: Future<Output = Result<crate::tracedecay::queries::graph::VerifiedGraphQuery>>,
{
    let query =
        args.get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: query".to_string(),
            })?;

    let semantic_mode = semantic_search_mode(&args)?;
    let lazy_indexing_requested = dependency_hints::lazy_indexing_requested(&args);
    let cursor = support::retrieval_cursor(&args)?;
    let include_graph_node_ids = render::wants_json(&args);
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(500) as usize);
    // A scope prefix cannot be applied as a post-filter here the way the
    // sibling handlers do it: the retrieval pipeline returns anchor-keyed
    // candidates that carry no file path. Refusing to search at all would make
    // the tool return nothing for the whole session (any serve launched from a
    // subdirectory sets a scope), so run the search and report below that the
    // scope was not honored rather than silently implying it was.
    let search = execute_code_index_search(
        search_executor,
        crate::mcp::server::CodeIndexSearchRequestV1 {
            project_root: cg.project_root().to_path_buf(),
            query: query.to_owned(),
            source_revision: None,
            source_tree: None,
            source_reference: None,
            limit,
            cursor,
            mode: semantic_mode,
            authority: search_authority.cloned(),
            deadline: deadline.clone(),
            cancellation: cancellation.clone(),
        },
    );
    let (outcome, graph) =
        race_primary_search_with_graph(search, graph, lazy_indexing_requested).await;
    match outcome {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete) => {
            let graph = bind_verified_graph_to_search(graph, &complete.code_generation);
            let graph = if lazy_indexing_requested && complete.ordered_candidates.is_empty() {
                let graph = graph?;
                dependency_hints::admit_verified_ignored_dependency(
                    ignored_dependency_admission,
                    &graph,
                    query,
                    scope_prefix,
                    deadline.as_ref(),
                    cancellation.as_ref(),
                )
                .await?;
                Ok(graph)
            } else {
                graph
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
            for ranked in &complete.ordered_candidates {
                let mut result = json!(ranked);
                if let Some(display) = complete.display_by_anchor.get(&ranked.candidate.anchor_id) {
                    result["display"] = json!({
                        "name": display.name,
                        "qualified_name": display.qualified_name,
                        "kind": display.kind,
                        "path": display.path,
                    });
                    if include_graph_node_ids {
                        graph_evidence.enrich_node_id(&mut result, display);
                    }
                }
                results.push(result);
            }
            let result_count = results.len();
            let mut output = json!({
                "results": results,
                "code_generation": complete.code_generation,
                "query_fallback_digest": &complete.query_fallback.digest,
                "semantic": semantic_status_value(semantic_mode, &complete.semantic),
                "next_cursor": complete.next_cursor
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                "coverage": coverage_value(&complete.coverage),
            });
            if let Some(scope) = scope_prefix {
                output["scope_prefix"] = json!(scope);
                output["scope_prefix_applied"] = json!(false);
            }
            if let Some(unavailable) = graph_evidence.unavailable() {
                output["verified_graph_evidence"] = unavailable.clone();
            }
            if dependency_hints::should_check_external_import_hint(result_count, limit) {
                if let Some(hint) = graph_evidence
                    .external_import_hint(
                        query,
                        limit,
                        scope_prefix,
                        deadline.as_ref(),
                        cancellation.as_ref(),
                    )
                    .await
                {
                    output["external_import_hint"] = hint;
                }
            }
            let output = output;
            Ok(rendered_tool_result(
                cg,
                &args,
                &output,
                touched_files,
                || render_search_md(&output),
            ))
        }
        crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(unavailable) => {
            let reason = unavailable.reason.as_str();
            let output = json!({
                "results": [],
                "code_generation": unavailable.code_generation,
                "query_fallback_digest": Value::Null,
                "semantic": semantic_status_value(semantic_mode, &unavailable.semantic),
                "status": "unavailable",
                "reason": reason,
                "coverage": coverage_value(&unavailable.coverage),
            });
            let failure = format!("code-index search unavailable: {reason}");
            let mut result =
                rendered_tool_result(cg, &args, &output, Vec::new(), || render_search_md(&output))
                    .with_failure_message(failure);
            if semantic_mode == crate::mcp::server::CodeIndexSearchModeV1::StrictSemantic {
                result = result.with_semantic_error(true);
            }
            Ok(result)
        }
    }
}

/// Warns, in the human-facing body, that a result list is short because a lane
/// was missing. A degraded page is otherwise indistinguishable from a thorough
/// one, which is exactly how a partial answer gets trusted as a complete one.
fn append_coverage_md(md: &mut Md, value: &Value) {
    let Some(coverage) = value.get("coverage") else {
        return;
    };
    if coverage.get("recall").and_then(Value::as_str) != Some("partial") {
        return;
    }
    let mut notes = Vec::new();
    for lane in ["exact", "lexical", "graph", "semantic"] {
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
        .line("Partial recall — some retrieval lanes did not answer:");
    for note in notes {
        md.bullet(&note);
    }
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
                    if let Some(display) = it.get("display") {
                        let name = render::field_str(display, "name");
                        let kind = render::field_str(display, "kind");
                        md.bullet(&format!(
                            "**{name}** ({kind}, {exact_class}) — rank {} · utility {utility}",
                            ordinal.saturating_add(1)
                        ));
                        md.line(&format!("  `{anchor}`"));
                    } else {
                        md.bullet(&format!(
                            "**{anchor}** ({exact_class}) — rank {} · utility {utility}",
                            ordinal.saturating_add(1)
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
                    "**{name}** ({kind}) — {file}:{line} · score {score:.1}"
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
    append_coverage_md(&mut md, value);
    if let Some(semantic) = value.get("semantic")
        && semantic.get("status").and_then(Value::as_str) == Some("unavailable")
        && let Some(reason) = semantic.get("reason").and_then(Value::as_str)
    {
        md.blank()
            .heading(3, "Semantic")
            .line(&format!("Semantic lane unavailable: {reason}."));
    }
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

/// Handles `tracedecay_context` tool calls.
pub(super) async fn handle_context(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
    search_executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    search_authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let request: ContextSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_context")?;
    let task = request.task.as_str();
    let mode = request.mode.unwrap_or(ContextModeV1::Explore);
    let max_nodes = request
        .max_nodes
        .map_or(20, |value| value.clamp(1, 200) as usize);
    let include_code = request.include_code.unwrap_or(false);
    let max_code_blocks = request
        .max_code_blocks
        .map_or(5, |value| value.clamp(1, 20) as usize);
    let semantic_mode = primitive_semantic_search_mode(request.semantic_mode);
    let memory_options = context_memory_options(&args);
    let memory_read_control =
        context_memory_read_control(&memory_options, deadline.as_ref(), cancellation.as_ref())?;
    // The two reads are independent: the code-index search depends on the task
    // and the search authority, the memory read on the task and the memory
    // options. Awaiting them in sequence would pay both latencies for a single
    // response, so they are driven together the way the sibling search handler
    // drives its two independent futures.
    let search = execute_code_index_search(
        search_executor,
        crate::mcp::server::CodeIndexSearchRequestV1 {
            project_root: cg.project_root().to_path_buf(),
            query: task.to_owned(),
            source_revision: None,
            source_tree: None,
            source_reference: None,
            limit: max_nodes,
            cursor: None,
            mode: semantic_mode,
            authority: search_authority.cloned(),
            deadline,
            cancellation,
        },
    );
    let memory = context_memory_outcome(cg, task, &memory_options, memory_read_control.as_ref());
    let (outcome, memory_outcome) = tokio::join!(search, memory);
    let complete = match outcome {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete) => complete,
        crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(unavailable) => {
            let detail = match &unavailable.semantic {
                crate::mcp::server::CodeIndexSemanticStatusV1::Complete => {
                    "the exact, lexical, or graph search lane is unavailable"
                }
                crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable { reason } => reason,
            };
            return Err(TraceDecayError::ProjectRoute {
                reason_code: "verified-code-context-search-unavailable".to_owned(),
                retryable: false,
                detail: detail.to_owned(),
            });
        }
    };
    let mut selected = Vec::new();
    for ranked in &complete.ordered_candidates {
        let Some(display) = complete.display_by_anchor.get(&ranked.candidate.anchor_id) else {
            continue;
        };
        if scope_prefix.is_some_and(|prefix| !display.path.starts_with(prefix)) {
            continue;
        }
        let candidates =
            graph.resolve_qualified_name(&display.qualified_name, Some(&display.kind), 16)?;
        for candidate in candidates {
            if required_graph_file_path(&candidate)? == display.path.as_str()
                && !selected.iter().any(|existing: &CodeGraphSymbolSummaryV1| {
                    existing.occurrence == candidate.occurrence
                })
            {
                selected.push(candidate);
                break;
            }
        }
    }
    let seeds = selected
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    let mut related = Vec::new();
    if !seeds.is_empty() {
        for batches in [
            graph.callers(&seeds, &[], GRAPH_RELATION_READ_LIMIT)?,
            graph.callees(&seeds, &[], GRAPH_RELATION_READ_LIMIT)?,
        ] {
            for edge in batches.into_iter().flatten() {
                if !seeds.contains(&edge.neighbor.occurrence)
                    && !related.iter().any(|existing: &CodeGraphSymbolSummaryV1| {
                        existing.occurrence == edge.neighbor.occurrence
                    })
                {
                    related.push(edge.neighbor);
                }
            }
        }
    }
    related.truncate(max_nodes);
    let ContextMemoryOutcome {
        hits: memory_matches,
        graph_coverage: memory_graph_coverage,
        error: memory_matches_error,
    } = memory_outcome;
    let mut all_symbols = selected.clone();
    all_symbols.extend(related.iter().cloned());
    let touched_files = graph_symbol_paths(&all_symbols)?;
    let mut code_blocks = Vec::<ContextCodeBlockV1>::new();
    if include_code {
        for symbol in selected.iter().take(max_code_blocks) {
            let metadata = required_graph_metadata(symbol)?;
            let file_path = required_graph_file_path(symbol)?;
            let source = crate::sync::read_source_file(&cg.project_root().join(file_path))?;
            code_blocks.push(ContextCodeBlockV1 {
                node_id: symbol.occurrence.as_str().to_owned(),
                file: file_path.to_owned(),
                start_line: user_line(metadata.start_line),
                end_line: user_line(graph_symbol_end_line(metadata)?),
                code: super::info::extract_lines(
                    &source,
                    metadata.start_line,
                    graph_symbol_end_line(metadata)?,
                ),
            });
        }
    }
    let symbol_values = selected
        .iter()
        .map(primitive_symbol_location)
        .collect::<Result<Vec<_>>>()?;
    let related_values = related
        .iter()
        .map(primitive_symbol_location)
        .collect::<Result<Vec<_>>>()?;
    let symbol_render_values = symbol_values
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let related_render_values = related_values
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let code_render_values = code_blocks
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut output = verified_context_markdown(
        task,
        &symbol_render_values,
        &related_render_values,
        &code_render_values,
    )?;
    insert_context_memory_section(
        &mut output,
        &memory_matches,
        memory_matches_error.as_deref(),
    );
    // Plan mode: append extension points, test coverage, and dependency info
    if mode == ContextModeV1::Plan {
        append_verified_plan_context(graph, &selected, &mut output)?;
    }

    if !seeds.is_empty() {
        let _ = write!(
            output,
            "\n{} {}\n",
            CONTEXT_SEEN_NODE_IDS_LABEL,
            serde_json::to_string(&seeds)?
        );
    }

    let result = ContextResultV1 {
        task: request.task,
        mode,
        code_generation: graph.generation().as_str().to_owned(),
        symbols: symbol_values,
        related_symbols: related_values,
        code: code_blocks,
        coverage: primitive_search_coverage(&complete.coverage),
        memory_matches: memory_matches.clone(),
        memory_graph_coverage,
        memory_matches_error: memory_matches_error.clone(),
    };
    let mut value = serde_json::to_value(result)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            CONTEXT_MEMORY_ANALYTICS_KEY.to_string(),
            json!({
                "context_memory": context_memory_analytics_value(
                    &memory_options,
                    &memory_matches,
                    memory_matches_error.as_deref()
                ),
            }),
        );
    }
    let preview = (!render::wants_json(&args)).then(|| context_markdown_lane_preview(&output));
    Ok(rendered_context_tool_result(
        cg,
        &args,
        value,
        touched_files,
        output,
        preview.as_deref(),
    ))
}

/// Handles `tracedecay_callers` tool calls.
pub(super) async fn handle_callers(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(3, |v| v.min(10) as usize);

    let occurrence = graph_occurrence_id(node_id)?;
    let results = traverse_verified_neighbors(
        graph,
        occurrence,
        &[RelationEdgeKindV1::Calls],
        true,
        max_depth,
    )?;
    let summaries = results
        .iter()
        .map(|result| result.symbol.clone())
        .collect::<Vec<_>>();
    let touched_files = graph_symbol_paths(&summaries)?;
    let items = results
        .iter()
        .map(verified_neighbor_value)
        .collect::<Result<Vec<_>>>()?;

    let value = json!(items);
    Ok(generic_tool_result(cg, &args, &value, touched_files))
}

/// Handles `tracedecay_callees` tool calls.
///
/// Beyond the direct `Calls` edges, this handler also surfaces *trait
/// dispatch targets*: when a callee is a method whose enclosing scope is a
/// trait, the concrete impl methods reachable through that trait are added
/// to the result list and tagged with `dispatch_via_trait: true`. The
/// original trait-method entry is preserved so callers can still see what
/// they statically called.
///
/// Dispatch resolution skipped when `resolve_dispatch=false` is passed.
pub(super) async fn handle_callees(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let request: CalleesSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_callees")?;
    let max_depth = request.max_depth.map_or(3, |value| value.min(10) as usize);
    let resolve_dispatch = request.resolve_dispatch.unwrap_or(true);

    let occurrence = graph_occurrence_id(&request.node_id)?;
    let results = traverse_verified_neighbors(
        graph,
        occurrence,
        &[RelationEdgeKindV1::Calls],
        false,
        max_depth,
    )?;
    let mut seen = results
        .iter()
        .map(|result| result.symbol.occurrence.clone())
        .collect::<HashSet<_>>();

    let mut items = results
        .iter()
        .map(|result| {
            let metadata = required_graph_metadata(&result.symbol)?;
            Ok(CalleeV1 {
                node_id: result.symbol.occurrence.as_str().to_owned(),
                name: metadata.simple_name.clone(),
                kind: metadata.kind.clone(),
                file: required_graph_file_path(&result.symbol)?.to_owned(),
                line: user_line(metadata.start_line),
                edge_kind: canonical_relation_kind_name(result.edge_kind).to_owned(),
                dispatch_via_trait: false,
                depth: Some(u32::try_from(result.depth).map_err(|_| {
                    graph_symbol_corrupt("callee traversal depth exceeds u32".to_owned())
                })?),
                dispatch_from: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    if resolve_dispatch {
        for callee in &results {
            for impl_method in verified_trait_dispatch_targets(graph, &callee.symbol)? {
                if !seen.insert(impl_method.occurrence.clone()) {
                    continue;
                }
                let metadata = required_graph_metadata(&impl_method)?;
                items.push(CalleeV1 {
                    node_id: impl_method.occurrence.as_str().to_owned(),
                    name: metadata.simple_name.clone(),
                    kind: metadata.kind.clone(),
                    file: required_graph_file_path(&impl_method)?.to_owned(),
                    line: user_line(metadata.start_line),
                    edge_kind: "calls".to_owned(),
                    dispatch_via_trait: true,
                    depth: None,
                    dispatch_from: Some(callee.symbol.occurrence.as_str().to_owned()),
                });
            }
        }
    }

    let touched_files = unique_file_paths(items.iter().map(|item| item.file.as_str()));

    let value = serde_json::to_value(items)?;
    Ok(generic_tool_result(cg, &args, &value, touched_files))
}

/// Handles `tracedecay_find_exact_symbol` tool calls. Bare-name lookup against
/// `idx_nodes_name` — no BM25 scoring, no fuzzy match, no qualified-name
/// suffix walk. Returns every node whose `name` column equals the query
/// exactly. Useful when you already know the symbol and want the apples-to-
/// apples cost of an index hit instead of `tracedecay_search`'s ranked query.
pub(super) async fn handle_find_exact_symbol(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
    ignored_dependency_admission: Option<
        &dyn tracedecay_usecases::code_index::CodeIndexIgnoredDependencyAdmissionPortV1,
    >,
    deadline: Option<&tracedecay_application::Deadline>,
    cancellation: Option<&tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let name =
        args.get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: name".to_string(),
            })?;
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(20, |v| v.min(200) as usize);

    let mut nodes = graph.resolve_simple_name(name, None, limit.saturating_mul(4))?;
    nodes = graph_symbols_in_scope(nodes, scope_prefix)?;
    if nodes.is_empty() && dependency_hints::lazy_indexing_requested(&args) {
        dependency_hints::admit_verified_ignored_dependency(
            ignored_dependency_admission,
            graph,
            name,
            scope_prefix,
            deadline,
            cancellation,
        )
        .await?;
    }
    if nodes.len() > limit {
        nodes.truncate(limit);
    }

    let touched_files = graph_symbol_paths(&nodes)?;
    let items = nodes
        .iter()
        .map(|node| {
            let metadata = required_graph_metadata(node)?;
            let file_path = required_graph_file_path(node)?;
            Ok(json!({
                "id": node.occurrence.as_str(),
                "name": metadata.simple_name,
                "qualified_name": metadata.qualified_name,
                "kind": metadata.kind,
                "file": file_path,
                "line": user_line(metadata.start_line),
                "signature": metadata.signature,
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    let body = json!({
        "name": name,
        "count": items.len(),
        "matches": items,
    });
    Ok(generic_tool_result(cg, &args, &body, touched_files))
}

/// Handles `tracedecay_impact` tool calls.
pub(super) async fn handle_impact(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let request: ImpactSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_impact")?;
    let max_depth = request.max_depth.map_or(3, |value| value.min(10));

    let occurrence = graph_occurrence_id(&request.node_id)?;
    let impact = graph.impact(
        std::slice::from_ref(&occurrence),
        &[],
        max_depth,
        50_000,
        GRAPH_RELATION_READ_LIMIT,
    )?;
    let summaries = impact
        .impacted
        .iter()
        .map(|item| item.summary.clone())
        .collect::<Vec<_>>();
    let touched_files = graph_symbol_paths(&summaries)?;
    let nodes = impact
        .impacted
        .iter()
        .map(|item| {
            let metadata = required_graph_metadata(&item.summary)?;
            Ok(ImpactNodeV1 {
                id: item.summary.occurrence.as_str().to_owned(),
                name: metadata.simple_name.clone(),
                kind: metadata.kind.clone(),
                file: required_graph_file_path(&item.summary)?.to_owned(),
                line: user_line(metadata.start_line),
                depth: item.depth,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let output = serde_json::to_value(ImpactResultV1 {
        node_count: nodes.len(),
        complete: impact.complete,
        unavailable_fields: vec!["edge_count".to_owned()],
        nodes,
    })?;

    Ok(generic_tool_result(cg, &args, &output, touched_files))
}

/// Handles `tracedecay_node` tool calls.
pub(super) async fn handle_node(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let request: NodeSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_node")?;
    let occurrence = graph_occurrence_id(&request.node_id)?;
    let node = graph.symbol_summary(&occurrence)?;

    match node {
        Some(n) => {
            let metadata = required_graph_metadata(&n)?;
            let file_path = required_graph_file_path(&n)?;
            let touched_files = vec![file_path.to_owned()];
            let file_size_bytes = std::fs::metadata(cg.project_root().join(file_path))?.len();
            let end_line = graph_symbol_end_line(metadata)?;
            let cyclomatic_complexity = metadata.branches.checked_add(1).ok_or_else(|| {
                graph_symbol_corrupt(format!(
                    "verified graph symbol '{}' branch count overflows complexity",
                    n.occurrence.as_str()
                ))
            })?;
            let line_count = end_line - metadata.start_line + 1;
            let output = serde_json::to_value(NodeDetailsV1 {
                id: n.occurrence.as_str().to_owned(),
                name: metadata.simple_name.clone(),
                kind: metadata.kind.clone(),
                qualified_name: metadata.qualified_name.clone(),
                file: file_path.to_owned(),
                start_line: user_line(metadata.start_line),
                end_line: user_line(end_line),
                signature: metadata.signature.clone(),
                visibility: metadata.visibility.clone(),
                branches: metadata.branches,
                loops: metadata.loops,
                max_nesting: metadata.max_nesting,
                cyclomatic_complexity,
                cost_to_expand: NodeExpansionCostV1 {
                    body: u64::from(line_count) * 20,
                    full_file: file_size_bytes / 4,
                },
                unavailable_fields: [
                    "assertions",
                    "attrs_start_line",
                    "derives",
                    "docstring",
                    "is_async",
                    "returns",
                    "unchecked_calls",
                    "unsafe_blocks",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            })?;
            Ok(generic_tool_result(cg, &args, &output, touched_files))
        }
        None => node_not_found_result(&request.node_id),
    }
}

/// Handles `tracedecay_similar` tool calls.
pub(super) async fn handle_similar(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    search_executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    search_authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let request: SimilarSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_similar")?;
    let limit = request.limit.map_or(10, |value| value.min(100) as usize);
    let semantic_mode = primitive_semantic_search_mode(request.semantic_mode);

    let outcome = execute_code_index_search(
        search_executor,
        crate::mcp::server::CodeIndexSearchRequestV1 {
            project_root: cg.project_root().to_path_buf(),
            query: request.symbol,
            source_revision: None,
            source_tree: None,
            source_reference: None,
            limit,
            cursor: None,
            mode: semantic_mode,
            authority: search_authority.cloned(),
            deadline,
            cancellation,
        },
    )
    .await;
    let complete = match outcome {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete) => complete,
        crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(_) => {
            return Err(TraceDecayError::ProjectRoute {
                reason_code: "verified-code-similarity-unavailable".to_owned(),
                retryable: false,
                detail: "the maintained code-index search lanes are unavailable".to_owned(),
            });
        }
    };
    let mut results = Vec::new();
    for ranked in &complete.ordered_candidates {
        let Some(display) = complete.display_by_anchor.get(&ranked.candidate.anchor_id) else {
            continue;
        };
        let candidates =
            graph.resolve_qualified_name(&display.qualified_name, Some(&display.kind), 16)?;
        let mut matched = None;
        for node in candidates {
            if required_graph_file_path(&node)? == display.path.as_str() {
                matched = Some(node);
                break;
            }
        }
        if let Some(node) = matched {
            results.push((node, ranked.candidate.utility_micros));
        }
    }
    let result_nodes = results
        .iter()
        .map(|(node, _)| node.clone())
        .collect::<Vec<_>>();
    let touched_files = graph_symbol_paths(&result_nodes)?;
    let items = results
        .iter()
        .map(|(node, utility_micros)| {
            let metadata = required_graph_metadata(node)?;
            Ok(SimilarSymbolV1 {
                id: node.occurrence.as_str().to_owned(),
                name: metadata.simple_name.clone(),
                kind: metadata.kind.clone(),
                file: required_graph_file_path(node)?.to_owned(),
                line: user_line(metadata.start_line),
                signature: metadata.signature.clone(),
                utility_micros: *utility_micros,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let value = serde_json::to_value(items)?;
    Ok(generic_tool_result(cg, &args, &value, touched_files))
}

/// Reads a file's lines (0-based) for snippet extraction, memoizing by path so
/// a file with many references is read once. `None` when the file cannot be
/// read (e.g. deleted since indexing).
fn cached_file_lines<'a>(
    cg: &TraceDecay,
    cache: &'a mut HashMap<String, Option<Vec<String>>>,
    file_path: &str,
) -> Option<&'a [String]> {
    if !cache.contains_key(file_path) {
        let abs = cg.project_root().join(file_path);
        let lines = std::fs::read_to_string(&abs)
            .ok()
            .map(|source| source.lines().map(str::to_string).collect::<Vec<_>>());
        cache.insert(file_path.to_string(), lines);
    }
    cache
        .get(file_path)
        .and_then(Option::as_ref)
        .map(Vec::as_slice)
}

/// Trims and length-caps a source line for use as a preview snippet.
fn snippet_text(line: &str) -> String {
    crate::text::utf8_prefix_at_or_before(line.trim(), 160).to_string()
}

/// Picks a current-text snippet near `approx_line` (0-based; edge line bases are
/// approximate, so neighbors are tried) that actually contains `name`, falling
/// back to the line itself. `None` when no line is available.
fn reference_line_snippet(
    lines: &[String],
    approx_line: Option<u32>,
    name: &str,
) -> Option<String> {
    let approx = approx_line? as usize;
    let candidates = [approx, approx.saturating_sub(1), approx + 1];
    let idx = candidates
        .into_iter()
        .find(|&i| lines.get(i).is_some_and(|line| line.contains(name)))
        .unwrap_or(approx);
    lines.get(idx).map(|line| snippet_text(line))
}

/// True for bytes that can appear inside an identifier. Non-ASCII bytes count so
/// multi-byte unicode identifiers are not falsely split at a boundary.
fn is_ident_byte(b: u8) -> bool {
    b == b'_' || b.is_ascii_alphanumeric() || b >= 0x80
}

/// Counts occurrences of `name` in `haystack` bounded as a whole identifier
/// (neither neighbouring byte is an identifier byte). Used to estimate the
/// literal textual matches a rename would touch, independent of the graph.
fn count_identifier_occurrences(haystack: &str, name: &str) -> usize {
    if name.is_empty() {
        return 0;
    }
    let bytes = haystack.as_bytes();
    let name_len = name.len();
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(name) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_ident_byte(bytes[abs - 1]);
        let after_idx = abs + name_len;
        let after_ok = after_idx >= bytes.len() || !is_ident_byte(bytes[after_idx]);
        if before_ok && after_ok {
            count += 1;
        }
        start = abs + name_len;
    }
    count
}

/// Handles `tracedecay_rename_preview` tool calls. READ-ONLY: reports what a
/// rename of the given symbol WOULD touch — the declaration site and every graph
/// reference site (incoming edges; outgoing edges reference other symbols and so
/// are excluded), each with a current-text snippet, plus a per-file count of
/// literal name occurrences that are NOT backed by a graph edge ("text-only
/// matches — review manually"). Nothing is rewritten.
pub(super) async fn handle_rename_preview(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let request: RenamePreviewPrimitiveRequestV1 =
        decode_primitive_request(&args, "tracedecay_rename_preview")?;

    let occurrence = graph_occurrence_id(&request.node_id)?;
    let Some(node) = graph.symbol_summary(&occurrence)? else {
        return node_not_found_result(&request.node_id);
    };
    let node_metadata = required_graph_metadata(&node)?;
    let node_file = required_graph_file_path(&node)?;
    let symbol_name = node_metadata.simple_name.clone();

    let mut lines_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    // Graph occurrences per file (declaration + reference sites) — subtracted
    // from the literal textual count to isolate the text-only matches.
    let mut graph_counts: HashMap<String, usize> = HashMap::new();
    let mut touched: Vec<String> = vec![node_file.to_owned()];

    *graph_counts.entry(node_file.to_owned()).or_default() += 1;
    let decl_snippet = cached_file_lines(cg, &mut lines_cache, node_file).and_then(|lines| {
        lines
            .get(node_metadata.start_line as usize)
            .map(|line| snippet_text(line))
    });
    let declaration = RenamePreviewNodeV1 {
        id: node.occurrence.as_str().to_owned(),
        name: node_metadata.simple_name.clone(),
        qualified_name: node_metadata.qualified_name.clone(),
        kind: node_metadata.kind.clone(),
        file: node_file.to_owned(),
        line: user_line(node_metadata.start_line),
        snippet: decl_snippet,
    };

    // Reference sites: incoming edges are the callers/users that name this
    // symbol. NOTE: call-edge coverage improves as the resolver improves; the
    // text-only counts below catch what the graph currently misses.
    let incoming = single_graph_adjacency_batch(graph.callers(
        std::slice::from_ref(&node.occurrence),
        &[],
        2_000_000,
    )?)?;
    let mut references = Vec::<RenamePreviewReferenceV1>::new();
    for edge in incoming {
        let source_node = edge.neighbor;
        let source_metadata = required_graph_metadata(&source_node)?;
        let source_file = required_graph_file_path(&source_node)?;
        touched.push(source_file.to_owned());
        *graph_counts.entry(source_file.to_owned()).or_default() += 1;
        let source = crate::sync::read_source_file(&cg.project_root().join(source_file))?;
        let line = line_for_byte_offset(&source, edge.edge.evidence_span.start_byte)?;
        let snippet = cached_file_lines(cg, &mut lines_cache, source_file)
            .and_then(|lines| reference_line_snippet(lines, Some(line), &symbol_name));
        references.push(RenamePreviewReferenceV1 {
            from_node_id: source_node.occurrence.as_str().to_owned(),
            from_name: source_metadata.simple_name.clone(),
            from_kind: source_metadata.kind.clone(),
            edge_kind: canonical_relation_kind_name(edge.edge.kind).to_owned(),
            file: source_file.to_owned(),
            line: user_line(line),
            snippet,
        });
    }

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    // Text-only matches per touched file: literal identifier occurrences of the
    // name minus the graph occurrences already accounted for. These are the
    // comments/strings/dynamic-dispatch/unresolved sites a graph-only rename
    // would miss — the scan is bounded to files that already appear in the
    // preview, so occurrences in wholly unrelated files are not counted.
    let mut text_only_matches = Vec::<RenamePreviewTextOnlyMatchV1>::new();
    for file in &touched_files {
        let total = cached_file_lines(cg, &mut lines_cache, file).map_or(0, |lines| {
            lines
                .iter()
                .map(|line| count_identifier_occurrences(line, &symbol_name))
                .sum::<usize>()
        });
        let graph = graph_counts.get(file).copied().unwrap_or(0);
        let text_only = total.saturating_sub(graph);
        if text_only > 0 {
            text_only_matches.push(RenamePreviewTextOnlyMatchV1 {
                file: file.clone(),
                text_only_count: text_only,
                note: "text-only matches — review manually".to_owned(),
            });
        }
    }

    let output = serde_json::to_value(RenamePreviewPrimitiveResultV1 {
        read_only: true,
        note: "Preview only — nothing is edited. 'references' are graph reference sites \
               (the declaration is reported separately in 'node'); 'text_only_matches' are \
               literal name occurrences NOT backed by a graph edge (comments, strings, \
               dynamic dispatch, unresolved refs) and must be reviewed by hand. Graph \
               call-edge coverage improves as the resolver does."
            .to_owned(),
        symbol: symbol_name,
        new_name: request.new_name,
        node: declaration,
        reference_count: references.len(),
        references,
        text_only_matches,
    })?;

    Ok(generic_tool_result(cg, &args, &output, touched_files))
}

/// Handles `tracedecay_callers_for` tool calls — bulk caller lookup over many IDs.
pub(super) async fn handle_callers_for(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let node_ids: Vec<String> = args
        .get("node_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    if node_ids.is_empty() {
        return Err(TraceDecayError::Config {
            message: "callers_for requires non-empty node_ids".to_string(),
        });
    }

    // Default to "calls" but allow any kind (or empty string for all kinds).
    let kind_arg = args.get("kind").and_then(|v| v.as_str()).unwrap_or("calls");
    let kinds: Vec<RelationEdgeKindV1> = if kind_arg.is_empty() {
        Vec::new()
    } else {
        match EdgeKind::from_str(kind_arg) {
            Some(k) => vec![canonical_relation_kind(k)?],
            None => {
                return Err(TraceDecayError::Config {
                    message: format!("unknown edge kind: {kind_arg}"),
                });
            }
        }
    };

    let max_per_item = args
        .get("max_per_item")
        .and_then(serde_json::Value::as_u64)
        .map_or(1000usize, |v| v.min(10_000) as usize);

    let occurrences = node_ids
        .iter()
        .map(|node_id| graph_occurrence_id(node_id))
        .collect::<Result<Vec<_>>>()?;
    let batches = graph.callers(&occurrences, &kinds, 2_000_000)?;
    if batches.len() != node_ids.len() {
        return Err(graph_symbol_corrupt(format!(
            "verified graph returned {} caller batches for {} symbols",
            batches.len(),
            node_ids.len()
        )));
    }

    let mut truncated = false;
    let mut by_target = HashMap::new();
    for (target, callers) in node_ids.iter().zip(batches) {
        if callers.len() > max_per_item {
            truncated = true;
        }
        by_target.insert(
            target,
            callers
                .into_iter()
                .take(max_per_item)
                .map(|edge| edge.neighbor.occurrence.as_str().to_owned())
                .collect::<Vec<_>>(),
        );
    }

    // Ensure every requested ID appears in the response, even if no callers.
    let result_map: HashMap<&String, Vec<String>> = node_ids
        .iter()
        .map(|id| (id, by_target.remove(id).unwrap_or_default()))
        .collect();

    let output = json!({
        "callers": result_map,
        "truncated": truncated,
        "max_per_item": max_per_item,
    });
    Ok(generic_tool_result(cg, &args, &output, vec![]))
}

/// Handles `tracedecay_by_qualified_name` — cross-run node lookup by name.
pub(super) async fn handle_by_qualified_name(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let qname = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: qualified_name".to_string(),
        })?;

    let nodes = graph.resolve_qualified_name(qname, None, 1_000)?;
    let touched_files = graph_symbol_paths(&nodes)?;
    let items = nodes
        .iter()
        .map(graph_symbol_location_value)
        .collect::<Result<Vec<_>>>()?;

    let value = json!(items);
    Ok(generic_tool_result(cg, &args, &value, touched_files))
}

/// Handles `tracedecay_signature` — signature-only lookup (no body) by
/// qualified name or node ID. Returns the public-API surface of a symbol so
/// callers can avoid reading the source file just to inspect the signature.
pub(super) async fn handle_signature(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let nodes = nodes_addressed_by_args(graph, &args)?;
    let touched_files = graph_symbol_paths(&nodes)?;

    let mut items: Vec<Value> = Vec::with_capacity(nodes.len());
    for n in &nodes {
        let metadata = required_graph_metadata(n)?;
        let file_path = required_graph_file_path(n)?;
        let file_size_bytes = std::fs::metadata(cg.project_root().join(file_path))?.len();
        let end_line = graph_symbol_end_line(metadata)?;
        items.push(json!({
            "node_id": n.occurrence.as_str(),
            "name": metadata.simple_name,
            "qualified_name": metadata.qualified_name,
            "kind": metadata.kind,
            "visibility": metadata.visibility,
            "signature": metadata.signature,
            "file": file_path,
            "start_line": user_line(metadata.start_line),
            "end_line": user_line(end_line),
            "cost_to_expand": cost_to_expand_verified(metadata, file_size_bytes)?,
            "unavailable_fields": ["attrs_start_line", "docstring", "is_async"],
        }));
    }

    let value = json!(items);
    Ok(generic_tool_result(cg, &args, &value, touched_files))
}

/// Handles `tracedecay_impls` — index of `impl Trait for Type` blocks.
///
/// Both `trait` and `type` arguments are optional. With neither, every impl
/// in the graph is returned (capped by `limit`). Surfaces trait-dispatch
/// information that is otherwise hidden behind raw `Implements` edges.
pub(super) async fn handle_impls(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let trait_filter = args.get("trait").and_then(|v| v.as_str());
    let type_filter = args.get("type").and_then(|v| v.as_str());
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(100, |v| v.min(1000) as usize);

    let mut after = None;
    let mut results = Vec::new();
    let mut examined = 0usize;
    let mut generation_complete = false;
    while results.len() <= limit {
        if examined >= 500_000 {
            return Err(TraceDecayError::ProjectRoute {
                reason_code: "verified-code-graph-budget-exhausted".to_owned(),
                retryable: false,
                detail: "impl census exceeded 500000 verified symbols".to_owned(),
            });
        }
        let page = graph.symbols_page(after.as_ref(), 1_024)?;
        examined = examined.saturating_add(page.symbols.len());
        after = page.symbols.last().map(|symbol| symbol.occurrence.clone());
        for impl_node in page.symbols {
            let metadata = required_graph_metadata(&impl_node)?;
            if metadata.kind != NodeKind::Impl.as_str()
                || type_filter.is_some_and(|query| !graph_name_matches(metadata, query))
            {
                continue;
            }
            let traits = single_graph_adjacency_batch(graph.callees(
                std::slice::from_ref(&impl_node.occurrence),
                &[RelationEdgeKindV1::Implements],
                GRAPH_RELATION_READ_LIMIT,
            )?)?;
            let trait_node = traits.into_iter().next().map(|edge| edge.neighbor);
            if trait_filter.is_some_and(|query| {
                trait_node
                    .as_ref()
                    .and_then(|node| node.metadata.as_ref())
                    .is_none_or(|metadata| !graph_name_matches(metadata, query))
            }) {
                continue;
            }
            results.push((impl_node, trait_node));
            if results.len() > limit {
                break;
            }
        }
        if !page.has_more {
            generation_complete = true;
            break;
        }
    }
    let truncated = !generation_complete || results.len() > limit;
    results.truncate(limit);

    let result_paths = results
        .iter()
        .map(|(impl_node, _)| required_graph_file_path(impl_node))
        .collect::<Result<Vec<_>>>()?;
    let touched_files = unique_file_paths(result_paths.into_iter());

    let items = results
        .iter()
        .map(|(impl_node, trait_node)| {
            let metadata = required_graph_metadata(impl_node)?;
            let file_path = required_graph_file_path(impl_node)?;
            let trait_metadata = trait_node
                .as_ref()
                .map(required_graph_metadata)
                .transpose()?;
            Ok(json!({
                "impl_id": impl_node.occurrence.as_str(),
                "type": metadata.simple_name,
                "qualified_name": metadata.qualified_name,
                "trait": trait_metadata.map(|value| value.simple_name.as_str()),
                "trait_qualified_name": trait_metadata.map(|value| value.qualified_name.as_str()),
                "trait_id": trait_node.as_ref().map(|value| value.occurrence.as_str()),
                "file": file_path,
                "start_line": user_line(metadata.start_line),
                "end_line": user_line(graph_symbol_end_line(metadata)?),
                "signature": metadata.signature,
            }))
        })
        .collect::<Result<Vec<_>>>()?;

    let output = json!({
        "count": items.len(),
        "truncated": truncated,
        "impls": items,
    });
    Ok(generic_tool_result(cg, &args, &output, touched_files))
}

/// Handles `tracedecay_derives`. Derive annotations are not published in the
/// verified code graph generation, so a matched symbol reports a typed
/// evidence-unavailable route error. Accepts `node_id` or `qualified_name`.
pub(super) async fn handle_derives(
    _cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let nodes = nodes_addressed_by_args(graph, &args)?;
    if nodes.is_empty() {
        return Ok(text_tool_result("No matching symbol found.", Vec::new()));
    }
    Err(TraceDecayError::ProjectRoute {
        reason_code: "verified-code-graph-evidence-unavailable".to_owned(),
        retryable: false,
        detail: "derive annotations are not published in the verified code graph generation"
            .to_owned(),
    })
}

/// Handles `tracedecay_implementations` — trait / method implementor lookup.
pub(super) async fn handle_implementations(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let trait_name = args.get("trait").and_then(|v| v.as_str());
    let method_name = args.get("method").and_then(|v| v.as_str());

    if trait_name.is_none() && method_name.is_none() {
        return Err(TraceDecayError::Config {
            message: "missing required parameter: 'trait' or 'method'".to_string(),
        });
    }
    if trait_name.is_some() && method_name.is_some() {
        return Err(TraceDecayError::Config {
            message: "tracedecay_implementations: 'trait' and 'method' are mutually exclusive"
                .to_string(),
        });
    }

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(20, |v| v.clamp(1, 200) as usize);

    let project_root = cg.project_root().to_path_buf();
    let mut entries: Vec<Value> = Vec::new();
    let mut touched: Vec<String> = Vec::new();

    if let Some(name) = trait_name {
        let candidates = graph.resolve_simple_name(name, None, 50)?;
        let trait_nodes: Vec<_> = candidates
            .into_iter()
            .filter(|node| {
                node.metadata.as_ref().is_some_and(|metadata| {
                    matches!(
                        NodeKind::from_str(&metadata.kind),
                        Some(NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType)
                    )
                })
            })
            .collect();
        if trait_nodes.is_empty() {
            return Ok(text_tool_result(
                &format!("No trait or interface named '{name}' found."),
                vec![],
            ));
        }

        for trait_node in trait_nodes {
            let trait_metadata = required_graph_metadata(&trait_node)?;
            let implementors = single_graph_adjacency_batch(graph.callers(
                std::slice::from_ref(&trait_node.occurrence),
                &[RelationEdgeKindV1::Implements],
                GRAPH_RELATION_READ_LIMIT,
            )?)?;
            for implementor in implementors {
                let impl_node = implementor.neighbor;
                let impl_metadata = required_graph_metadata(&impl_node)?;
                let impl_file = required_graph_file_path(&impl_node)?;
                if scope_prefix.is_some_and(|prefix| !impl_file.starts_with(prefix)) {
                    continue;
                }
                let methods = collect_method_bodies(graph, &impl_node, &project_root)?;
                if !touched.iter().any(|path| path == impl_file) {
                    touched.push(impl_file.to_owned());
                }
                entries.push(json!({
                    "type": impl_metadata.simple_name,
                    "qualified_name": impl_metadata.qualified_name,
                    "kind": impl_metadata.kind,
                    "file": impl_file,
                    "line": user_line(impl_metadata.start_line),
                    "trait": trait_metadata.qualified_name,
                    "methods": methods,
                }));
                if entries.len() >= limit {
                    break;
                }
            }
            if entries.len() >= limit {
                break;
            }
        }
    } else if let Some(name) = method_name {
        let nodes = graph.resolve_simple_name(name, None, limit.saturating_mul(4))?;
        let mut method_nodes = Vec::new();
        for node in nodes {
            let metadata = required_graph_metadata(&node)?;
            if !matches!(
                NodeKind::from_str(&metadata.kind),
                Some(NodeKind::Function | NodeKind::Method)
            ) {
                continue;
            }
            let file_path = required_graph_file_path(&node)?;
            if scope_prefix.is_none_or(|prefix| file_path.starts_with(prefix)) {
                method_nodes.push(node);
                if method_nodes.len() == limit {
                    break;
                }
            }
        }
        if method_nodes.is_empty() {
            return Ok(text_tool_result(
                &format!("No function or method named '{name}' found."),
                vec![],
            ));
        }
        for n in method_nodes {
            let metadata = required_graph_metadata(&n)?;
            let file_path = required_graph_file_path(&n)?;
            let abs_path = project_root.join(file_path);
            let source = crate::sync::read_source_file(&abs_path)?;
            let end_line = graph_symbol_end_line(metadata)?;
            let body = super::info::extract_lines(&source, metadata.start_line, end_line);
            if !touched.iter().any(|path| path == file_path) {
                touched.push(file_path.to_owned());
            }
            entries.push(json!({
                "name": metadata.simple_name,
                "qualified_name": metadata.qualified_name,
                "kind": metadata.kind,
                "file": file_path,
                "line": user_line(metadata.start_line),
                "end_line": user_line(end_line),
                "signature": metadata.signature,
                "body": body,
            }));
        }
    }

    let payload = json!({
        "match_count": entries.len(),
        "implementations": entries,
    });
    Ok(generic_tool_result(cg, &args, &payload, touched))
}

fn collect_method_bodies(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    impl_node: &CodeGraphSymbolSummaryV1,
    project_root: &std::path::Path,
) -> Result<Vec<Value>> {
    let children = single_graph_adjacency_batch(graph.callees(
        std::slice::from_ref(&impl_node.occurrence),
        &[RelationEdgeKindV1::Contains],
        GRAPH_RELATION_READ_LIMIT,
    )?)?;
    let mut methods = Vec::new();
    for child in children {
        let child = child.neighbor;
        let metadata = required_graph_metadata(&child)?;
        if !matches!(
            NodeKind::from_str(&metadata.kind),
            Some(NodeKind::Method | NodeKind::Function)
        ) {
            continue;
        }
        let file_path = required_graph_file_path(&child)?.to_owned();
        methods.push((file_path, metadata.start_line, child));
    }
    methods.sort_by(|left, right| {
        (&left.0, left.1, &left.2.occurrence).cmp(&(&right.0, right.1, &right.2.occurrence))
    });

    let mut out: Vec<Value> = Vec::new();
    for (file_path, _, child) in methods {
        let metadata = required_graph_metadata(&child)?;
        let abs_path = project_root.join(&file_path);
        let source = crate::sync::read_source_file(&abs_path)?;
        let end_line = graph_symbol_end_line(metadata)?;
        let body = super::info::extract_lines(&source, metadata.start_line, end_line);
        out.push(json!({
            "name": metadata.simple_name,
            "kind": metadata.kind,
            "line": user_line(metadata.start_line),
            "signature": metadata.signature,
            "body": body,
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_application::memory::FactSearchHitV1;

    fn context_memory_hit(content: String) -> FactSearchHitV1 {
        serde_json::from_value(json!({
            "fact": {
                "owner": {"kind": "profile"},
                "fact_id": "fact.0000000000000000000000000000000000000000000000000000000000000000.1111111111111111111111111111111111111111111111111111111111111111",
                "content": content,
                "category": "project",
                "tags": [],
                "entities": [],
                "trust_score_millionths": 900_000,
                "source": {"kind": "application", "operation_id": "operation.context-memory"},
                "source_label": "context-test",
                "active_assertion_id": "assertion.context-memory",
                "last_event_id": "event.context-memory",
                "projected_as_of": 1,
                "telemetry": {
                    "retrieval_count": 0,
                    "access_count": 0,
                    "helpful_count": 0,
                    "unhelpful_count": 0,
                    "created_at": 1,
                    "updated_at": 1,
                    "last_retrieved_at": null,
                    "last_recalled_at": null,
                    "last_feedback_at": null
                },
                "metadata": {}
            },
            "scores": {
                "score_millionths": 500_000,
                "fts_score_millionths": 250_000,
                "jaccard_score_millionths": 250_000,
                "holographic_score_millionths": 0,
                "trust_score_millionths": 900_000
            },
            "why": null
        }))
        .expect("canonical context memory hit")
    }

    /// A warm response must render exactly as it did before coverage existed:
    /// every lane complete, no coverage section, no added lines.
    #[test]
    fn warm_coverage_leaves_the_rendered_body_unchanged() {
        let coverage = coverage_value(&crate::mcp::server::CodeIndexSearchCoverageV1::warm());
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
    fn a_rebuilding_generation_remains_typed_unavailable() {
        let unavailable = crate::mcp::server::CodeIndexSearchUnavailableV1 {
            code_generation: None,
            reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable,
            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                reason: crate::mcp::server::lane_reason::GENERATION_REBUILDING,
            },
            coverage: crate::mcp::server::CodeIndexSearchCoverageV1::unavailable(
                crate::mcp::server::lane_reason::GENERATION_REBUILDING,
            ),
        };

        assert!(!unavailable.coverage.any_servable());
        assert_eq!(
            unavailable.coverage.exact,
            crate::mcp::server::CodeIndexLaneStatusV1::Unavailable {
                reason: crate::mcp::server::lane_reason::GENERATION_REBUILDING,
            }
        );
    }

    #[tokio::test]
    async fn installed_search_executor_owns_fallback_allowed_dispatch() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&calls);
        let executor: crate::mcp::server::CodeIndexSearchExecutor = std::sync::Arc::new(
            move |request| {
                observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                assert_eq!(
                    request.mode,
                    crate::mcp::server::CodeIndexSearchModeV1::FallbackAllowed
                );
                assert_eq!(request.query, "fixture");
                Box::pin(async {
                    crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                        crate::mcp::server::CodeIndexSearchUnavailableV1 {
                            code_generation: Some("generation.fixture".to_owned()),
                            reason: crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                            semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                                reason: "calibration_unavailable",
                            },
                            coverage: crate::mcp::server::CodeIndexSearchCoverageV1::unavailable(
                                "calibration_unavailable",
                            ),
                        },
                    )
                })
            },
        );
        let outcome = execute_code_index_search(
            Some(&executor),
            crate::mcp::server::CodeIndexSearchRequestV1 {
                project_root: std::path::PathBuf::from("/fixture"),
                query: "fixture".to_owned(),
                source_revision: None,
                source_tree: None,
                source_reference: None,
                limit: 10,
                cursor: None,
                mode: crate::mcp::server::CodeIndexSearchModeV1::FallbackAllowed,
                authority: None,
                deadline: None,
                cancellation: None,
            },
        )
        .await;
        assert!(matches!(
            outcome,
            crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                crate::mcp::server::CodeIndexSearchUnavailableV1 {
                    reason:
                        crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
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
            crate::mcp::server::CodeIndexSearchRequestV1 {
                project_root: std::path::PathBuf::from("/fixture"),
                query: "fixture".to_owned(),
                source_revision: None,
                source_tree: None,
                source_reference: None,
                limit: 10,
                cursor: None,
                mode: crate::mcp::server::CodeIndexSearchModeV1::StrictSemantic,
                authority: None,
                deadline: None,
                cancellation: None,
            },
        )
        .await;
        assert!(matches!(
            outcome,
            crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
                crate::mcp::server::CodeIndexSearchUnavailableV1 {
                    reason:
                        crate::mcp::server::CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable,
                    ..
                }
            )
        ));
    }

    #[test]
    fn search_markdown_prefers_hydrated_symbol_identity_over_opaque_anchor() {
        let rendered = render_search_md(&json!({
            "results": [{
                "candidate": {
                    "anchor_id": "code-symbol:symbol.v1.sha256:opaque",
                    "exact_class": "exact_message",
                    "utility_micros": 4_000_000
                },
                "final_ordinal": 0,
                "display": {
                    "name": "main",
                    "qualified_name": "main",
                    "kind": "function"
                }
            }]
        }));

        assert!(rendered.contains("**main** (function, exact_message)"));
        assert!(rendered.contains("`code-symbol:symbol.v1.sha256:opaque`"));
    }

    #[test]
    fn context_markdown_lane_preview_keeps_all_lanes_visible() {
        let full = format!(
            "## Code Context\n**Query:** q\n\n### Memory Matches\n{}\n### Entry Points\n{}\n### Related Symbols\n{}\n### Code\n{}\n### Index Coverage Hint\n{}\n### Extension Points\n{}\n### Test Coverage\n{}\nseen_node_ids: [{}]\n",
            "memory fact with unicode caf\u{e9}\n".repeat(300),
            "- **entry** src/lib.rs:1\n".repeat(300),
            "- related\n".repeat(500),
            "```rust\nfn demo() {}\n```\n".repeat(500),
            "hint\n".repeat(500),
            "- trait\n".repeat(400),
            "- tests/context_test.rs\n".repeat(400),
            "\"node-id\",".repeat(400)
        );

        let preview = context_markdown_lane_preview(&full);

        for heading in [
            "## Code Context",
            "### Memory Matches",
            "### Entry Points",
            "### Related Symbols",
            "### Code",
            "### Index Coverage Hint",
            "### Extension Points",
            "### Test Coverage",
            "seen_node_ids:",
        ] {
            assert!(preview.contains(heading), "missing {heading}: {preview}");
        }
        assert!(preview.len() < full.len());
        assert!(preview.contains("lane truncated"));
        assert!(preview.is_char_boundary(preview.len()));
    }

    #[test]
    fn context_lane_preview_keeps_seen_node_ids_parseable() {
        let ids: Vec<String> = (0..100).map(|i| format!("function:{i:032x}")).collect();
        let markdown = format!(
            "{} {}\n",
            CONTEXT_SEEN_NODE_IDS_LABEL,
            serde_json::to_string(&ids)
                .unwrap_or_else(|err| panic!("failed to serialize seen node ids: {err}"))
        );

        let preview = context_markdown_lane_preview(&markdown);
        let json = match preview.strip_prefix(CONTEXT_SEEN_NODE_IDS_LABEL) {
            Some(json) => json.trim(),
            None => panic!("preview should keep seen_node_ids label: {preview}"),
        };
        let parsed: Vec<String> = serde_json::from_str(json)
            .unwrap_or_else(|err| panic!("failed to parse seen node ids: {err}: {json}"));

        assert_eq!(parsed, ids);
        assert!(!preview.contains("lane truncated"));
    }

    #[test]
    fn context_memory_section_keeps_full_content_for_retrieval_handle() {
        let content = format!("{}tail-marker", "long memory body ".repeat(100));
        let hit = context_memory_hit(content.clone());

        let Some(section) = context_memory_section(&[hit], None) else {
            panic!("memory hit should render");
        };

        assert!(section.contains(&content));
        assert!(section.contains("tail-marker"));
        assert!(!section.contains("..."));
        assert!(section.contains("tracedecay_fact_feedback"));
    }

    #[test]
    fn context_memory_section_compacts_multiline_content() {
        let hit = context_memory_hit("first line\n# heading\n- item".to_owned());

        let Some(section) = context_memory_section(&[hit], None) else {
            panic!("memory hit should render");
        };

        assert!(section.contains("first line # heading - item"));
        assert!(!section.contains("\n# heading"));
        assert!(!section.contains("\n- item"));
    }

    #[test]
    fn context_lane_preview_closes_open_code_fence_before_truncation_note() {
        let markdown = format!("### Code\n```rust\n{}\n", "fn demo() {}\n".repeat(1_000));

        let preview = context_markdown_lane_preview(&markdown);

        assert!(preview.contains("```\n\n... lane truncated"));
    }

    #[test]
    fn context_lane_preview_ignores_heading_markers_inside_code_fences() {
        let markdown = format!(
            "### Code\n```markdown\n{}\n```\n### Test Coverage\n- real lane\n",
            "### not a lane\n".repeat(1_000)
        );

        let preview = context_markdown_lane_preview(&markdown);

        assert!(preview.contains("### Code"));
        assert!(preview.contains("### Test Coverage"));
        assert_eq!(preview.matches("lane truncated").count(), 1);
    }
}
