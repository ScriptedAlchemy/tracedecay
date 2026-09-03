//! Root-owned graph handlers that still require search, memory, or extra
//! admission ports: `search`, `context`, `similar`, `find_exact_symbol`,
//! `rename_preview`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay_application::retrieval::{
    ContextCodeBlockV1, ContextModeV1, ContextResultV1, ContextSearchMatchV1,
    ContextSurfaceRequestV1, RenamePreviewNodeV1, RenamePreviewPrimitiveRequestV1,
    RenamePreviewPrimitiveResultV1, RenamePreviewReferenceV1, RenamePreviewTextOnlyMatchV1,
    SimilarSurfaceRequestV1, SimilarSymbolV1,
};
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_domain::ExactClass;

use crate::tracedecay::TraceDecay;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_mcp::context_headings::CONTEXT_SEEN_NODE_IDS_LABEL;

use super::dependency_hints;
use super::support::{
    self, CONTEXT_MEMORY_ANALYTICS_KEY, decode_primitive_request,
    take_internal_context_memory_analytics, text_tool_result, unique_file_paths,
};
use tracedecay_mcp::ToolResult;
use tracedecay_mcp::tools::render::{self, Md};

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
    search_coverage as primitive_search_coverage,
    semantic_search_mode as primitive_semantic_search_mode,
    symbol_location as primitive_symbol_location,
};
use search_evidence::{
    SearchGraphEvidence, bind_verified_graph_to_search, race_primary_search_with_graph,
};

use tracedecay_mcp::handlers::graph::{
    canonical_relation_kind_name, graph_occurrence_id, graph_symbol_end_line, graph_symbol_paths,
    graph_symbols_in_scope, line_for_byte_offset, node_not_found as node_not_found_result,
    required_graph_file_path, required_graph_metadata, single_graph_adjacency_batch,
};
use verified::{append_verified_plan_context, verified_context_markdown};

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
            crate::mcp::server::CodeIndexLaneStatusV1::Partial { generation } => json!({
                "status": "partial",
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

#[hotpath::measure(label = "mcp.graph.search.total")]
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
    F: Future<Output = Result<tracedecay_graph_query::VerifiedGraphQuery>>,
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
    let search_request = crate::mcp::server::CodeIndexSearchRequestV1 {
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
            crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete),
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
            crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(_)
        ) {
            outcome = refreshed;
        }
    }
    match outcome {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete) => {
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
                            ignored_dependency_admission,
                            &graph,
                            query,
                            scope_prefix,
                            deadline.as_ref(),
                            cancellation.as_ref()
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
                    let mut result = json!(ranked);
                    if let Some(display) =
                        complete.display_by_anchor.get(&ranked.candidate.anchor_id)
                    {
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
            });
            let result_count = results.len();
            let mut output = hotpath::measure_block!(
                "mcp.graph.search.serialize",
                json!({
                "results": results,
                "code_generation": complete.code_generation,
                "query_fallback_digest": &complete.query_fallback.digest,
                "semantic": semantic_status_value(semantic_mode, &complete.semantic),
                "next_cursor": complete.next_cursor
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                "coverage": coverage_value(&complete.coverage),
                })
            );
            if let Some(scope) = scope_prefix {
                output["scope_prefix"] = json!(scope);
                output["scope_prefix_applied"] = json!(false);
            }
            if let Some(unavailable) = graph_evidence.unavailable() {
                output["verified_graph_evidence"] = unavailable.clone();
            }
            if (scope_prefix.is_some()
                || dependency_hints::should_check_external_import_hint(result_count, limit))
                && let Some(hint) = graph_evidence
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
            let graph_evidence = SearchGraphEvidence::new(graph.as_ref());
            let mut output = hotpath::measure_block!(
                "mcp.graph.search.serialize",
                json!({
                    "results": [],
                    "code_generation": unavailable.code_generation,
                    "query_fallback_digest": Value::Null,
                    "semantic": semantic_status_value(semantic_mode, &unavailable.semantic),
                    "status": "unavailable",
                    "reason": reason,
                    "coverage": coverage_value(&unavailable.coverage),
                })
            );
            if let Some(unavailable_graph) = graph_evidence.unavailable() {
                output["verified_graph_evidence"] = unavailable_graph.clone();
            }
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

/// Related-symbol assembly is a page, not a complete fan-out. The callers
/// tool uses a 50k refuse budget; context used to keep that budget, hydrate
/// every edge, then discard all but `max_nodes`. That walk is CPU-bound and
/// shows no warm benefit. Cap examination at a small multiple of the kept
/// page. Semantic kind lives on the edge entity (not the physical
/// SOURCE/TARGET relation type), so the page is all-kinds — the same
/// neighborhood the previous complete walk returned, just a prefix.
fn context_related_relation_budget(max_nodes: usize) -> usize {
    max_nodes.saturating_mul(4).clamp(16, 64)
}

#[derive(Default)]
struct ContextGraphProjection {
    selected: Vec<CodeGraphSymbolSummaryV1>,
    related: Vec<CodeGraphSymbolSummaryV1>,
    code_blocks: Vec<ContextCodeBlockV1>,
    touched_files: Vec<String>,
}

fn context_search_matches(
    complete: &crate::mcp::server::CodeIndexSearchCompletedV1,
    scope_prefix: Option<&str>,
) -> Vec<ContextSearchMatchV1> {
    complete
        .ordered_candidates
        .iter()
        .filter_map(|ranked| {
            let display = complete
                .display_by_anchor
                .get(&ranked.candidate.anchor_id)?;
            if scope_prefix.is_some_and(|prefix| !display.path.starts_with(prefix)) {
                return None;
            }
            let exact_class = match ranked.candidate.exact_class {
                ExactClass::ExactMessage => "exact_message",
                ExactClass::ExactLiteralPhrase => "exact_literal_phrase",
                ExactClass::Approximate => "approximate",
            };
            Some(ContextSearchMatchV1 {
                anchor_id: ranked.candidate.anchor_id.as_str().to_owned(),
                name: display.name.clone(),
                qualified_name: display.qualified_name.clone(),
                kind: display.kind.clone(),
                file: display.path.clone(),
                exact_class: exact_class.to_owned(),
                rank: ranked.final_ordinal.saturating_add(1),
                utility_micros: ranked.candidate.utility_micros,
            })
        })
        .collect()
}

fn context_graph_projection(
    cg: &TraceDecay,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    complete: &crate::mcp::server::CodeIndexSearchCompletedV1,
    scope_prefix: Option<&str>,
    max_nodes: usize,
    include_code: bool,
    max_code_blocks: usize,
) -> Result<ContextGraphProjection> {
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
        let related_budget = context_related_relation_budget(max_nodes);
        for batches in [
            graph.callers_truncated(&seeds, &[], related_budget)?,
            graph.callees_truncated(&seeds, &[], related_budget)?,
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

    let mut all_symbols = selected.clone();
    all_symbols.extend(related.iter().cloned());
    let touched_files = graph_symbol_paths(&all_symbols)?;
    let mut code_blocks = Vec::new();
    if include_code {
        // Context snippets are filesystem windows, not the mmap'd sealed
        // lexical artifact (that path serves n-gram search). Cache by path
        // so five symbols in one file do not re-read the whole source.
        let mut source_by_path = HashMap::<String, String>::new();
        for symbol in selected.iter().take(max_code_blocks) {
            let metadata = required_graph_metadata(symbol)?;
            let file_path = required_graph_file_path(symbol)?;
            if !source_by_path.contains_key(file_path) {
                source_by_path.insert(
                    file_path.to_owned(),
                    tracedecay_runtime_core::sync::read_source_file(
                        &cg.project_root().join(file_path),
                    )?,
                );
            }
            let Some(source) = source_by_path.get(file_path) else {
                return Err(TraceDecayError::Config {
                    message: format!("context source window missing for '{file_path}'"),
                });
            };
            code_blocks.push(ContextCodeBlockV1 {
                node_id: symbol.occurrence.as_str().to_owned(),
                file: file_path.to_owned(),
                start_line: user_line(metadata.start_line),
                end_line: user_line(graph_symbol_end_line(metadata)?),
                code: tracedecay_mcp::handlers::info::extract_lines(
                    source,
                    metadata.start_line,
                    graph_symbol_end_line(metadata)?,
                ),
            });
        }
    }
    Ok(ContextGraphProjection {
        selected,
        related,
        code_blocks,
        touched_files,
    })
}

fn append_context_search_matches(output: &mut String, matches: &[ContextSearchMatchV1]) {
    if matches.is_empty() {
        return;
    }
    output.push_str("\n### Available Code Search Matches\n");
    for search_match in matches {
        let _ = writeln!(
            output,
            "- **{}** ({}) — `{}` · rank {} · utility {}",
            search_match.name,
            search_match.kind,
            search_match.file,
            search_match.rank,
            search_match.utility_micros,
        );
    }
}

fn append_context_semantic_pending(output: &mut String, value: &Value) {
    let semantic = &value["coverage"]["semantic"];
    let reason = semantic.get("reason").and_then(Value::as_str);
    if semantic.get("status").and_then(Value::as_str) == Some("unavailable")
        && matches!(
            reason,
            Some("semantic_generation_warming" | "generation_rebuilding")
        )
    {
        output.push_str("\n### Semantic\nSemantic results pending while the generation warms; available fallback and memory results are shown above.\n");
    }
}

#[hotpath::measure(label = "mcp.graph.context.total")]
pub(super) async fn handle_context<F>(
    cg: &TraceDecay,
    graph: F,
    args: Value,
    scope_prefix: Option<&str>,
    search_executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    search_authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult>
where
    F: Future<Output = Result<tracedecay_graph_query::VerifiedGraphQuery>>,
{
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
    // Search, graph enrichment, and memory are independent. Search is the
    // primary code lane: once it answers, a still-pending graph must not hold
    // lexical/exact results or memory hostage.
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
    let search_and_graph = race_primary_search_with_graph(search, graph, false, None, false);
    let ((outcome, graph), memory_outcome) = tokio::join!(search_and_graph, memory);
    let strict_semantic_unavailable = semantic_mode
        == crate::mcp::server::CodeIndexSearchModeV1::StrictSemantic
        && matches!(
            &outcome,
            crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(_)
        );
    let (complete, code_generation, coverage, search_matches) = match outcome {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete) => {
            let search_matches = context_search_matches(&complete, scope_prefix);
            let code_generation = Some(complete.code_generation.clone());
            let coverage = primitive_search_coverage(&complete.coverage);
            (Some(complete), code_generation, coverage, search_matches)
        }
        crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(unavailable) => (
            None,
            unavailable.code_generation,
            primitive_search_coverage(&unavailable.coverage),
            Vec::new(),
        ),
    };
    let graph = match complete.as_ref() {
        Some(complete) => bind_verified_graph_to_search(graph, &complete.code_generation),
        None => graph,
    };
    let (graph, projection, verified_graph_evidence) = match (graph, complete.as_ref()) {
        (Ok(graph), Some(complete)) => match hotpath::measure_block!(
            "mcp.graph.context.graph",
            context_graph_projection(
                cg,
                &graph,
                complete,
                scope_prefix,
                max_nodes,
                include_code,
                max_code_blocks,
            )
        ) {
            Ok(projection) => (Some(graph), projection, None),
            Err(error) => (
                None,
                ContextGraphProjection::default(),
                Some(dependency_hints::unavailable_evidence(&error)),
            ),
        },
        (Ok(graph), None) => (Some(graph), ContextGraphProjection::default(), None),
        (Err(error), _) => (
            None,
            ContextGraphProjection::default(),
            Some(dependency_hints::unavailable_evidence(&error)),
        ),
    };
    let ContextMemoryOutcome {
        hits: memory_matches,
        graph_coverage: memory_graph_coverage,
        error: memory_matches_error,
    } = memory_outcome;
    let seeds = projection
        .selected
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    let symbol_values = projection
        .selected
        .iter()
        .map(primitive_symbol_location)
        .collect::<Result<Vec<_>>>()?;
    let related_values = projection
        .related
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
    let code_render_values = projection
        .code_blocks
        .iter()
        .map(serde_json::to_value)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut output = verified_context_markdown(
        task,
        &symbol_render_values,
        &related_render_values,
        &code_render_values,
    )?;
    if symbol_values.is_empty() {
        append_context_search_matches(&mut output, &search_matches);
    }
    insert_context_memory_section(
        &mut output,
        &memory_matches,
        memory_matches_error.as_deref(),
    );
    if mode == ContextModeV1::Plan
        && let Some(graph) = graph.as_ref()
    {
        append_verified_plan_context(graph, &projection.selected, &mut output)?;
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
        code_generation,
        search_matches: search_matches.clone(),
        symbols: symbol_values,
        related_symbols: related_values,
        code: projection.code_blocks,
        coverage,
        memory_matches: memory_matches.clone(),
        memory_graph_coverage,
        memory_matches_error: memory_matches_error.clone(),
        verified_graph_evidence,
    };
    let mut value =
        hotpath::measure_block!("mcp.graph.context.serialize", serde_json::to_value(result)?);
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
    append_context_semantic_pending(&mut output, &value);
    let mut degradation = Md::new();
    append_coverage_md(&mut degradation, &value);
    search_evidence::append_verified_graph_evidence_md(&mut degradation, &value);
    let degradation = degradation.render();
    if !degradation.is_empty() {
        output.push('\n');
        output.push_str(&degradation);
    }
    let touched_files = unique_file_paths(
        projection.touched_files.iter().map(String::as_str).chain(
            search_matches
                .iter()
                .map(|search_match| search_match.file.as_str()),
        ),
    );
    let preview = (!render::wants_json(&args)).then(|| context_markdown_lane_preview(&output));
    let result =
        rendered_context_tool_result(cg, &args, value, touched_files, output, preview.as_deref());
    if strict_semantic_unavailable {
        Ok(result.with_semantic_error(true))
    } else {
        Ok(result)
    }
}

/// Bare-name lookup against `idx_nodes_name` — no BM25 scoring, no fuzzy
/// match, no qualified-name suffix walk. Returns every node whose `name`
/// column equals the query exactly. Useful when you already know the symbol
/// and want the apples-to-apples cost of an index hit instead of
/// `tracedecay_search`'s ranked query.
#[hotpath::measure(label = "mcp.graph.find_exact_symbol.total")]
pub(super) async fn handle_find_exact_symbol(
    cg: &TraceDecay,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
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

    let mut nodes = hotpath::measure_block!("mcp.graph.find_exact_symbol.graph", {
        let nodes = graph.resolve_simple_name(name, None, limit.saturating_mul(4))?;
        graph_symbols_in_scope(nodes, scope_prefix)?
    });
    if nodes.is_empty() && dependency_hints::lazy_indexing_requested(&args) {
        hotpath::future!(
            dependency_hints::admit_verified_ignored_dependency(
                ignored_dependency_admission,
                graph,
                name,
                scope_prefix,
                deadline,
                cancellation,
            ),
            label = "mcp.graph.find_exact_symbol.admit"
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

    let body = hotpath::measure_block!(
        "mcp.graph.find_exact_symbol.serialize",
        json!({
            "name": name,
            "count": items.len(),
            "matches": items,
        })
    );
    Ok(generic_tool_result(cg, &args, &body, touched_files))
}

#[hotpath::measure(label = "mcp.graph.similar.total")]
pub(super) async fn handle_similar(
    cg: &TraceDecay,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
    search_executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    search_authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let request: SimilarSurfaceRequestV1 = decode_primitive_request(&args, "tracedecay_similar")?;
    let limit = request.limit.map_or(10, |value| value.min(100) as usize);
    let semantic_mode = primitive_semantic_search_mode(request.semantic_mode);

    let outcome = hotpath::future!(
        execute_code_index_search(
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
            }
        ),
        label = "mcp.graph.similar.query"
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
    hotpath::measure_block!("mcp.graph.similar.graph", {
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
    });
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

    let value =
        hotpath::measure_block!("mcp.graph.similar.serialize", serde_json::to_value(items)?);
    Ok(generic_tool_result(cg, &args, &value, touched_files))
}

/// Reads a file's lines (0-based) for snippet extraction, memoizing by path so
/// a file with many references is read once. `None` when the file cannot be
/// read (e.g. deleted since indexing).
fn cached_file_lines<'a>(
    project_root: &Path,
    cache: &'a mut HashMap<String, Option<Vec<String>>>,
    file_path: &str,
) -> Option<&'a [String]> {
    if !cache.contains_key(file_path) {
        let abs = project_root.join(file_path);
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
    tracedecay_runtime_core::text::utf8_prefix_at_or_before(line.trim(), 160).to_string()
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

/// Graph-derived inputs for one rename-preview reference site, extracted
/// before the blocking file walk so the worker needs no graph access.
struct RenameReferenceSiteInput {
    from_node_id: String,
    from_name: String,
    from_kind: String,
    edge_kind: String,
    file: String,
    evidence_start_byte: u64,
}

/// READ-ONLY: reports what a rename of the given symbol WOULD touch — the
/// declaration site and every graph reference site (incoming edges; outgoing
/// edges reference other symbols and so are excluded), each with a
/// current-text snippet, plus a per-file count of literal name occurrences
/// that are NOT backed by a graph edge ("text-only matches — review
/// manually"). Nothing is rewritten.
#[hotpath::measure(label = "mcp.graph.rename_preview.total")]
pub(super) async fn handle_rename_preview(
    cg: &TraceDecay,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let request: RenamePreviewPrimitiveRequestV1 =
        decode_primitive_request(&args, "tracedecay_rename_preview")?;

    let occurrence = graph_occurrence_id(&request.node_id)?;
    // Graph occurrences per file (declaration + reference sites) — subtracted
    // from the literal textual count to isolate the text-only matches.
    let mut graph_counts: HashMap<String, usize> = HashMap::new();
    let mut touched: Vec<String> = Vec::new();
    // Graph phase: extract owned declaration fields and per-reference-site
    // inputs so the blocking file walk below needs no graph value at all.
    let (mut declaration, declaration_line, symbol_name, reference_inputs) =
        hotpath::measure_block!("mcp.graph.rename_preview.graph", {
            let Some(node) = graph.symbol_summary(&occurrence)? else {
                return node_not_found_result(&request.node_id);
            };
            let node_metadata = required_graph_metadata(&node)?;
            let node_file = required_graph_file_path(&node)?;
            let symbol_name = node_metadata.simple_name.clone();

            touched.push(node_file.to_owned());
            *graph_counts.entry(node_file.to_owned()).or_default() += 1;
            let declaration = RenamePreviewNodeV1 {
                id: node.occurrence.as_str().to_owned(),
                name: node_metadata.simple_name.clone(),
                qualified_name: node_metadata.qualified_name.clone(),
                kind: node_metadata.kind.clone(),
                file: node_file.to_owned(),
                line: user_line(node_metadata.start_line),
                snippet: None,
            };

            // Reference sites: incoming edges are the callers/users that name this
            // symbol. NOTE: call-edge coverage improves as the resolver improves;
            // the text-only counts below catch what the graph currently misses.
            let incoming = single_graph_adjacency_batch(graph.callers(
                std::slice::from_ref(&node.occurrence),
                &[],
                2_000_000,
            )?)?;
            let mut reference_inputs =
                Vec::<RenameReferenceSiteInput>::with_capacity(incoming.len());
            for edge in incoming {
                let source_node = edge.neighbor;
                let source_metadata = required_graph_metadata(&source_node)?;
                let source_file = required_graph_file_path(&source_node)?;
                touched.push(source_file.to_owned());
                *graph_counts.entry(source_file.to_owned()).or_default() += 1;
                reference_inputs.push(RenameReferenceSiteInput {
                    from_node_id: source_node.occurrence.as_str().to_owned(),
                    from_name: source_metadata.simple_name.clone(),
                    from_kind: source_metadata.kind.clone(),
                    edge_kind: canonical_relation_kind_name(edge.edge.kind).to_owned(),
                    file: source_file.to_owned(),
                    evidence_start_byte: edge.edge.evidence_span.start_byte,
                });
            }
            (
                declaration,
                node_metadata.start_line,
                symbol_name,
                reference_inputs,
            )
        });

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    // File-walk phase: every referenced source file is read from disk, so it
    // runs on a blocking worker like the sibling analysis scans instead of
    // holding the async dispatch thread through the reads.
    let project_root = cg.project_root().to_path_buf();
    let declaration_file = declaration.file.clone();
    let walk_symbol_name = symbol_name.clone();
    let walk_graph_counts = graph_counts;
    let walk_touched_files = touched_files.clone();
    let (decl_snippet, references, text_only_matches) = hotpath::future!(
        tokio::task::spawn_blocking(
        move || -> Result<(
            Option<String>,
            Vec<RenamePreviewReferenceV1>,
            Vec<RenamePreviewTextOnlyMatchV1>,
        )> {
            let mut lines_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
            let decl_snippet =
                cached_file_lines(&project_root, &mut lines_cache, &declaration_file).and_then(
                    |lines| {
                        lines
                            .get(declaration_line as usize)
                            .map(|line| snippet_text(line))
                    },
                );

            let mut references =
                Vec::<RenamePreviewReferenceV1>::with_capacity(reference_inputs.len());
            for input in reference_inputs {
                let source = tracedecay_runtime_core::sync::read_source_file(&project_root.join(&input.file))?;
                let line = line_for_byte_offset(&source, input.evidence_start_byte)?;
                let snippet = cached_file_lines(&project_root, &mut lines_cache, &input.file)
                    .and_then(|lines| reference_line_snippet(lines, Some(line), &walk_symbol_name));
                references.push(RenamePreviewReferenceV1 {
                    from_node_id: input.from_node_id,
                    from_name: input.from_name,
                    from_kind: input.from_kind,
                    edge_kind: input.edge_kind,
                    file: input.file,
                    line: user_line(line),
                    snippet,
                });
            }

            // Text-only matches per touched file: literal identifier occurrences
            // of the name minus the graph occurrences already accounted for.
            // These are the comments/strings/dynamic-dispatch/unresolved sites a
            // graph-only rename would miss — the scan is bounded to files that
            // already appear in the preview, so occurrences in wholly unrelated
            // files are not counted.
            let mut text_only_matches = Vec::<RenamePreviewTextOnlyMatchV1>::new();
            for file in &walk_touched_files {
                let total =
                    cached_file_lines(&project_root, &mut lines_cache, file).map_or(0, |lines| {
                        lines
                            .iter()
                            .map(|line| count_identifier_occurrences(line, &walk_symbol_name))
                            .sum::<usize>()
                    });
                let graph = walk_graph_counts.get(file).copied().unwrap_or(0);
                let text_only = total.saturating_sub(graph);
                if text_only > 0 {
                    text_only_matches.push(RenamePreviewTextOnlyMatchV1 {
                        file: file.clone(),
                        text_only_count: text_only,
                        note: "text-only matches — review manually".to_owned(),
                    });
                }
            }
            Ok((decl_snippet, references, text_only_matches))
        }
        ),
        label = "mcp.graph.rename_preview.walk"
    )
    .await
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("rename preview file scan task failed: {join_error}"),
    })??;
    declaration.snippet = decl_snippet;

    let output = hotpath::measure_block!(
        "mcp.graph.rename_preview.serialize",
        serde_json::to_value(RenamePreviewPrimitiveResultV1 {
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
        })?
    );

    Ok(generic_tool_result(cg, &args, &output, touched_files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_application::memory::FactSearchHitV1;

    #[test]
    fn complete_search_preserves_generation_advance_but_not_stale_admission() {
        let advanced = TraceDecayError::project_route(
            IGNORED_DEPENDENCY_GENERATION_ADVANCED,
            true,
            "new generation published",
        );
        assert!(preserve_complete_search_after_lazy_admission(Err(advanced)).is_ok());

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

    fn completed_sparse_search() -> crate::mcp::server::CodeIndexSearchOutcomeV1 {
        completed_sparse_search_for_generation("generation.mcp-verified-graph-fixture.1")
    }

    fn completed_sparse_search_for_generation(
        generation: &str,
    ) -> crate::mcp::server::CodeIndexSearchOutcomeV1 {
        let candidate = tracedecay_domain::RankedCandidate {
            candidate: tracedecay_domain::FusedCandidate {
                anchor_id: tracedecay_domain::RetrievalAnchorId::new(
                    "code-symbol:sparse-lexical-widget",
                )
                .expect("sparse lexical candidate anchor"),
                logical_evidence_id: tracedecay_domain::LogicalEvidenceId::new(
                    "logical.sparse-lexical-widget",
                )
                .expect("sparse lexical candidate logical evidence"),
                occurrences: Vec::new(),
                exact_class: ExactClass::Approximate,
                utility_micros: 1,
                contributions: Vec::new(),
                freshness: Vec::new(),
                decisions: Vec::new(),
            },
            final_ordinal: 0,
        };
        let fallback_coverage = tracedecay_domain::RetrieverKind::QUERY_FALLBACK_LANES
            .into_iter()
            .map(|lane| (lane, tracedecay_domain::PublicRetrieverStatus::Complete))
            .collect();
        let query_fallback = tracedecay_domain::QueryFallbackSubpayload::new(
            tracedecay_domain::FusionProfileId::new("profile.sparse-search")
                .expect("sparse search profile"),
            vec![candidate.clone()],
            fallback_coverage,
            Vec::new(),
            None,
        )
        .expect("canonical sparse lexical fallback payload");
        let anchor = candidate.candidate.anchor_id.clone();
        let semantic = crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
            reason: "semantic_generation_warming",
        };
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(
            crate::mcp::server::CodeIndexSearchCompletedV1 {
                code_generation: generation.to_owned(),
                ordered_candidates: vec![candidate],
                query_fallback: std::sync::Arc::new(query_fallback),
                display_by_anchor: HashMap::from([(
                    anchor,
                    crate::mcp::server::CodeIndexSearchDisplayV1 {
                        name: "SparseLexicalWidget".to_owned(),
                        qualified_name: "crate::SparseLexicalWidget".to_owned(),
                        kind: "function".to_owned(),
                        path: "src/lib.rs".to_owned(),
                    },
                )]),
                coverage: crate::mcp::server::CodeIndexSearchCoverageV1::fused(&semantic),
                semantic,
                next_cursor: None,
            },
        )
    }

    fn unavailable_search() -> crate::mcp::server::CodeIndexSearchOutcomeV1 {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Unavailable(
            crate::mcp::server::CodeIndexSearchUnavailableV1 {
                code_generation: Some("generation.mcp-verified-graph-fixture.1".to_owned()),
                reason:
                    crate::mcp::server::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                semantic: crate::mcp::server::CodeIndexSemanticStatusV1::Unavailable {
                    reason: "search_attempt_repeated",
                },
                coverage: crate::mcp::server::CodeIndexSearchCoverageV1::unavailable(
                    "search_attempt_repeated",
                ),
            },
        )
    }

    fn search_test_options<'a>(
        cg: &TraceDecay,
        executor: crate::mcp::server::CodeIndexSearchExecutor,
    ) -> crate::mcp::tools::handlers::ToolCallRegistryOptions<'a> {
        crate::mcp::tools::handlers::dispatch_test_support::verified_graph_options(
            cg,
            crate::mcp::tools::handlers::ToolCallRegistryOptions {
                code_index_search_executor: Some(executor),
                code_index_search_authority: Some(crate::mcp::server::CodeIndexSearchAuthorityV1 {
                    principal: tracedecay_domain::PrincipalId::new("principal.search-attempt-test")
                        .expect("search attempt principal"),
                    authorization_revision: tracedecay_domain::AuthorizationRevision::new(
                        "authorization.search-attempt-test",
                    )
                    .expect("search attempt authorization revision"),
                }),
                ..crate::mcp::tools::handlers::ToolCallRegistryOptions::default()
            },
        )
    }

    #[tokio::test]
    async fn completed_primary_search_is_not_retried_after_graph_admission() {
        let _env_lock = crate::config::lock_user_data_dir_test_env();
        let dir = tempfile::TempDir::new().expect("single search attempt isolation");
        let _env = crate::mcp::tools::handlers::dispatch_test_support::SelectorEnv::new(dir.path());
        let project = dir.path().join("single-search-attempt");
        std::fs::create_dir_all(project.join("src")).expect("create search attempt sources");
        std::fs::write(
            project.join("src/lib.rs"),
            "pub fn SparseLexicalWidget() {}\n",
        )
        .expect("write search attempt fixture");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &project,
            "project.single-search-attempt",
        )
        .await
        .expect("registered search attempt fixture");

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&calls);
        let executor: crate::mcp::server::CodeIndexSearchExecutor =
            std::sync::Arc::new(move |_| {
                let attempt = observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Box::pin(async move {
                    if attempt == 0 {
                        completed_sparse_search()
                    } else {
                        unavailable_search()
                    }
                })
            });
        let options = search_test_options(&cg, executor);

        let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
            &cg,
            "tracedecay_search",
            json!({
                "query": "SparseLexicalWidget",
                "limit": 5,
                "format": "json",
            }),
            None,
            None,
            options,
        )
        .await
        .expect("first complete search outcome must remain authoritative");
        let payload: Value = serde_json::from_str(
            result.value["content"][0]["text"]
                .as_str()
                .expect("single-attempt search JSON text"),
        )
        .expect("single-attempt search JSON payload");

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            payload["results"][0]["display"]["name"],
            "SparseLexicalWidget"
        );
        assert!(payload["status"].is_null());
        cg.close();
    }

    #[tokio::test]
    async fn generation_mismatch_retry_cannot_erase_a_complete_sparse_search() {
        let _env_lock = crate::config::lock_user_data_dir_test_env();
        let dir = tempfile::TempDir::new().expect("generation mismatch isolation");
        let _env = crate::mcp::tools::handlers::dispatch_test_support::SelectorEnv::new(dir.path());
        let project = dir.path().join("generation-mismatch-search");
        std::fs::create_dir_all(project.join("src")).expect("create mismatch search sources");
        std::fs::write(
            project.join("src/lib.rs"),
            "pub fn SparseLexicalWidget() {}\n",
        )
        .expect("write mismatch search fixture");
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &project,
            "project.generation-mismatch-search",
        )
        .await
        .expect("registered mismatch search fixture");

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = std::sync::Arc::clone(&calls);
        let executor: crate::mcp::server::CodeIndexSearchExecutor =
            std::sync::Arc::new(move |_| {
                let attempt = observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Box::pin(async move {
                    if attempt == 0 {
                        completed_sparse_search_for_generation("generation.search-before-graph")
                    } else {
                        unavailable_search()
                    }
                })
            });
        let options = search_test_options(&cg, executor);

        let result = crate::mcp::tools::handlers::handle_tool_call_with_registry_options(
            &cg,
            "tracedecay_search",
            json!({
                "query": "SparseLexicalWidget",
                "limit": 5,
                "format": "json",
            }),
            None,
            None,
            options,
        )
        .await
        .expect("failed refresh must preserve the first complete search");
        let payload: Value = serde_json::from_str(
            result.value["content"][0]["text"]
                .as_str()
                .expect("generation mismatch JSON text"),
        )
        .expect("generation mismatch JSON payload");

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(payload["results"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            payload["results"][0]["display"]["name"],
            "SparseLexicalWidget"
        );
        assert_eq!(
            payload["verified_graph_evidence"]["reason_code"],
            "verified-code-graph-generation-mismatch"
        );
        cg.close();
    }

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

    #[test]
    fn context_related_relation_budget_is_a_page_not_a_complete_walk() {
        assert_eq!(context_related_relation_budget(1), 16);
        assert_eq!(context_related_relation_budget(20), 64);
        assert_eq!(context_related_relation_budget(200), 64);
        assert!(
            context_related_relation_budget(20) < 50_000,
            "context must not reuse the callers complete-walk budget"
        );
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
