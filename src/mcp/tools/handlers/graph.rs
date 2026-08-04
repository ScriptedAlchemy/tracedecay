//! Graph traversal tool handlers: `search`, `context`, `callers`, `callees`,
//! `impact`, `node`, `similar`, `rename_preview`, `callers_for`, `by_qualified_name`,
//! `signature`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::context::{
    CONTEXT_CODE_HEADING, CONTEXT_ENTRY_POINTS_HEADING, CONTEXT_EXTENSION_POINTS_HEADING,
    CONTEXT_INDEX_COVERAGE_HINT_HEADING, CONTEXT_MEMORY_FEEDBACK_HINT,
    CONTEXT_MEMORY_MATCHES_HEADING, CONTEXT_RELATED_SYMBOLS_HEADING, CONTEXT_SEEN_NODE_IDS_LABEL,
    CONTEXT_TEST_COVERAGE_HEADING, format_context_as_markdown,
};
use crate::errors::{Result, TraceDecayError};
use crate::memory::types::{FactSearchResult, SearchFactsRequest};
use crate::path_tree::format_compact_path_list;
use crate::text::utf8_prefix_at_or_before;
use crate::tracedecay::TraceDecay;
use crate::types::{BuildContextOptions, EdgeKind, Node, NodeKind, TaskContext, Visibility};

const CONTEXT_MEMORY_MATCH_LIMIT: usize = 3;
const CONTEXT_MEMORY_MATCH_LIMIT_MAX: usize = 10;
const CONTEXT_LANE_TRUNCATED_NOTE: &str =
    "\n... lane truncated; retrieve the full response handle for omitted details.\n";

use super::super::ToolResult;
use super::super::render::{self, Md};
use super::dependency_hints;
use super::support::{
    self, CONTEXT_MEMORY_ANALYTICS_KEY, effective_path, filter_by_scope, require_node_id,
    require_object_args, string_array_values, take_internal_context_memory_analytics,
    text_tool_result, unique_file_paths,
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

fn retrieval_cursor(args: &Value) -> Result<Option<tracedecay_domain::RetrievalCursor>> {
    let Some(encoded) = args.get("cursor").and_then(Value::as_str) else {
        return Ok(None);
    };
    if encoded.len() > 4_096 {
        return Err(TraceDecayError::Config {
            message: "cursor exceeds its bounded authenticated envelope".to_owned(),
        });
    }
    let cursor: tracedecay_domain::RetrievalCursor = serde_json::from_str(encoded)?;
    cursor.validate().map_err(|_| TraceDecayError::Config {
        message: "cursor is not a valid authenticated retrieval continuation".to_owned(),
    })?;
    Ok(Some(cursor))
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

/// Resolves the nodes a symbol-addressing tool was pointed at: `node_id` (with
/// the `id` alias) when present, otherwise `qualified_name`. An id that no
/// longer resolves yields no nodes rather than an error, so the caller renders
/// an empty match list; only an argument-less call is rejected.
async fn nodes_addressed_by_args(cg: &TraceDecay, args: &Value) -> Result<Vec<Node>> {
    let node_id = args
        .get("node_id")
        .or_else(|| args.get("id"))
        .and_then(|v| v.as_str());
    if let Some(node_id) = node_id {
        return Ok(cg.get_node(node_id).await?.into_iter().collect());
    }

    let Some(qualified_name) = args.get("qualified_name").and_then(|v| v.as_str()) else {
        return Err(TraceDecayError::Config {
            message: "missing required parameter: qualified_name or node_id".to_string(),
        });
    };
    cg.get_nodes_by_qualified_name(qualified_name).await
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

async fn unique_graph_node_id_for_search_display(
    cg: &TraceDecay,
    display: &crate::mcp::server::CodeIndexSearchDisplayV1,
) -> Option<String> {
    let nodes = cg
        .get_nodes_by_qualified_name(&display.qualified_name)
        .await
        .ok()?;
    let mut matches = nodes
        .into_iter()
        .filter(|node| node.kind.as_str() == display.kind.as_str());
    let node = matches.next()?;
    matches.next().is_none().then_some(node.id)
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
pub(super) async fn handle_search(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    search_executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    search_authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    let query =
        args.get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: query".to_string(),
            })?;

    let semantic_mode = semantic_search_mode(&args)?;
    let cursor = retrieval_cursor(&args)?;
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
    let outcome = execute_code_index_search(
        search_executor,
        crate::mcp::server::CodeIndexSearchRequestV1 {
            project_root: cg.project_root().to_path_buf(),
            query: query.to_owned(),
            limit,
            cursor,
            mode: semantic_mode,
            authority: search_authority.cloned(),
            deadline: deadline.clone(),
            cancellation: cancellation.clone(),
        },
    )
    .await;
    match outcome {
        crate::mcp::server::CodeIndexSearchOutcomeV1::Complete(complete) => {
            let mut results = Vec::with_capacity(complete.ordered_candidates.len());
            for ranked in &complete.ordered_candidates {
                let mut result = json!(ranked);
                if let Some(display) = complete.display_by_anchor.get(&ranked.candidate.anchor_id) {
                    result["display"] = json!({
                        "name": display.name,
                        "qualified_name": display.qualified_name,
                        "kind": display.kind,
                    });
                    if include_graph_node_ids
                        && let Some(node_id) =
                            unique_graph_node_id_for_search_display(cg, display).await
                    {
                        result["node_id"] = json!(node_id);
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
            if dependency_hints::should_check_ignored_dependency_hint(result_count, limit)
                && let Some(hint) = dependency_hints::ignored_dependency_hint(
                    cg,
                    query,
                    limit,
                    scope_prefix,
                    deadline.as_ref(),
                    cancellation.as_ref(),
                )
                .await?
            {
                output["ignored_dependency_hint"] = hint;
            }
            let output = output;
            Ok(rendered_tool_result(cg, &args, &output, Vec::new(), || {
                render_search_md(&output)
            }))
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
    dependency_hints::append_ignored_dependency_hint_md(&mut md, value);
    md.render()
}

/// Handles `tracedecay_context` tool calls.
pub(super) async fn handle_context(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let task =
        args.get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: task".to_string(),
            })?;

    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("explore");
    let options = build_context_options(&args, scope_prefix);

    let context = cg.build_context(task, &options).await?;
    let memory_options = context_memory_options(&args);
    let (memory_matches, memory_matches_error) = if memory_options.include_memory {
        match context_memory_matches(cg, task, &memory_options).await {
            Ok(matches) => (matches, None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        }
    } else {
        (Vec::new(), None)
    };
    let touched_files = unique_file_paths(
        context
            .subgraph
            .nodes
            .iter()
            .map(|n| n.file_path.as_str())
            .chain(
                context
                    .related_files
                    .iter()
                    .map(std::string::String::as_str),
            ),
    );
    let mut output = format_context_as_markdown(&context);
    insert_context_memory_section(
        &mut output,
        &memory_matches,
        memory_matches_error.as_deref(),
    );
    if let Some(hint) = cg.index_coverage_hint(context.subgraph.nodes.len()) {
        let _ = writeln!(
            output,
            "\n{}\n{}\nSkipped trees seen: {}\nTo opt in, run: `{}`\n",
            CONTEXT_INDEX_COVERAGE_HINT_HEADING,
            hint.message,
            hint.skipped_dirs.join(", "),
            hint.suggested_command,
        );
    }

    // Plan mode: append extension points, test coverage, and dependency info
    if mode == "plan" {
        append_plan_context(cg, &context, &mut output).await?;
    }

    if !context.seen_node_ids.is_empty() {
        let _ = write!(
            output,
            "\n{} {}\n",
            CONTEXT_SEEN_NODE_IDS_LABEL,
            serde_json::to_string(&context.seen_node_ids).unwrap_or_default()
        );
    }

    let mut value = serde_json::to_value(&context).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "memory_matches".to_string(),
            serde_json::to_value(&memory_matches).unwrap_or_else(|_| json!([])),
        );
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
        if let Some(err) = memory_matches_error {
            object.insert("memory_matches_error".to_string(), json!(err));
        }
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

fn context_markdown_lane_preview(markdown: &str) -> String {
    let mut preview = String::with_capacity(markdown.len().min(24_000));
    let mut lane = String::new();
    let mut lane_key = String::new();
    let mut in_fence = false;

    for line in markdown.split_inclusive('\n') {
        if !in_fence && let Some(key) = context_lane_key(line) {
            push_context_lane_preview(&mut preview, &lane_key, &lane);
            lane.clear();
            lane_key = key.to_string();
        }
        lane.push_str(line);
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
    }
    push_context_lane_preview(&mut preview, &lane_key, &lane);
    preview
}

fn context_lane_key(line: &str) -> Option<&str> {
    if line.starts_with("### ") || line.starts_with(CONTEXT_SEEN_NODE_IDS_LABEL) {
        Some(line.trim_end())
    } else {
        None
    }
}

fn push_context_lane_preview(preview: &mut String, lane_key: &str, lane: &str) {
    if lane.is_empty() {
        return;
    }
    let budget = context_lane_budget(lane_key);
    if lane.len() <= budget {
        preview.push_str(lane);
        return;
    }
    let prefix = utf8_prefix_at_or_before(lane, budget);
    preview.push_str(prefix);
    if render::has_open_markdown_fence(prefix) {
        preview.push_str("\n```\n");
    }
    preview.push_str(CONTEXT_LANE_TRUNCATED_NOTE);
}

fn context_lane_budget(lane_key: &str) -> usize {
    if lane_key.starts_with(CONTEXT_SEEN_NODE_IDS_LABEL) {
        usize::MAX
    } else if lane_key.starts_with(CONTEXT_CODE_HEADING) {
        8_500
    } else if lane_key.starts_with(CONTEXT_RELATED_SYMBOLS_HEADING) {
        3_500
    } else if lane_key.starts_with(CONTEXT_ENTRY_POINTS_HEADING)
        || lane_key.starts_with(CONTEXT_TEST_COVERAGE_HEADING)
    {
        3_000
    } else if lane_key.starts_with(CONTEXT_MEMORY_MATCHES_HEADING) {
        2_500
    } else if lane_key.starts_with(CONTEXT_EXTENSION_POINTS_HEADING) {
        1_500
    } else if lane_key.starts_with(CONTEXT_INDEX_COVERAGE_HINT_HEADING) {
        1_000
    } else {
        2_000
    }
}

fn insert_context_memory_section(
    output: &mut String,
    memory_matches: &[FactSearchResult],
    memory_matches_error: Option<&str>,
) {
    let Some(section) = context_memory_section(memory_matches, memory_matches_error) else {
        return;
    };
    if let Some(idx) = output.find(&format!("\n{CONTEXT_ENTRY_POINTS_HEADING}")) {
        output.insert_str(idx, &section);
    } else {
        output.push_str(&section);
    }
}

fn context_memory_section(
    memory_matches: &[FactSearchResult],
    memory_matches_error: Option<&str>,
) -> Option<String> {
    let mut section = String::new();
    if !memory_matches.is_empty() {
        section.push('\n');
        section.push_str(CONTEXT_MEMORY_MATCHES_HEADING);
        section.push('\n');
        for hit in memory_matches {
            let fact = &hit.fact;
            let _ = writeln!(
                section,
                "- fact_id={} category={} trust={:.2} score={:.3}: {}",
                fact.fact_id,
                fact.category,
                fact.trust_score,
                hit.score,
                compact_memory_content(&fact.content)
            );
        }
        section.push('\n');
        section.push_str(CONTEXT_MEMORY_FEEDBACK_HINT);
        section.push('\n');
        return Some(section);
    }
    if let Some(err) = memory_matches_error {
        let _ = writeln!(
            section,
            "\n{CONTEXT_MEMORY_MATCHES_HEADING}\nUnavailable: {err}"
        );
        return Some(section);
    }
    None
}

fn compact_memory_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct ContextMemoryOptions {
    include_memory: bool,
    limit: usize,
    min_trust: f64,
}

fn context_memory_options(args: &Value) -> ContextMemoryOptions {
    let include_memory = args
        .get("include_memory")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let limit = args
        .get("memory_limit")
        .and_then(Value::as_u64)
        .map_or(CONTEXT_MEMORY_MATCH_LIMIT, |value| value as usize)
        .clamp(1, CONTEXT_MEMORY_MATCH_LIMIT_MAX);
    let min_trust = args
        .get("memory_min_trust")
        .and_then(Value::as_f64)
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    ContextMemoryOptions {
        include_memory,
        limit,
        min_trust,
    }
}

fn context_memory_analytics_value(
    options: &ContextMemoryOptions,
    memory_matches: &[FactSearchResult],
    memory_matches_error: Option<&str>,
) -> Value {
    let fact_ids: Vec<Value> = memory_matches
        .iter()
        .map(|hit| Value::from(hit.fact.fact_id))
        .collect();
    json!({
        "include_memory": options.include_memory,
        "limit": options.limit,
        "min_trust": options.min_trust,
        "match_count": fact_ids.len(),
        "fact_ids": fact_ids,
        "error": memory_matches_error,
    })
}

async fn context_memory_matches(
    cg: &TraceDecay,
    task: &str,
    options: &ContextMemoryOptions,
) -> Result<Vec<FactSearchResult>> {
    cg.search_facts_untracked(SearchFactsRequest {
        query: task.to_string(),
        category: None,
        limit: Some(options.limit),
        min_trust: Some(options.min_trust),
        include_why: false,
    })
    .await
}

fn build_context_options(args: &Value, scope_prefix: Option<&str>) -> BuildContextOptions {
    let max_nodes = args
        .get("max_nodes")
        .and_then(serde_json::Value::as_u64)
        .map_or(20, |v| v.min(100) as usize);

    let max_per_file = args
        .get("max_per_file")
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as usize)
        .or(Some((max_nodes / 3).max(3)));

    BuildContextOptions {
        max_nodes,
        max_code_blocks: args
            .get("max_code_blocks")
            .and_then(serde_json::Value::as_u64)
            .map_or(5, |v| v.min(20) as usize),
        include_code: args
            .get("include_code")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        extra_keywords: string_array_values(args, "keywords"),
        exclude_node_ids: string_array_values(args, "exclude_node_ids")
            .into_iter()
            .collect(),
        merge_adjacent: args
            .get("merge_adjacent")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        max_per_file,
        path_prefix: effective_path(args, scope_prefix).map(String::from),
        ..Default::default()
    }
}

async fn append_plan_context(
    cg: &TraceDecay,
    context: &TaskContext,
    output: &mut String,
) -> Result<()> {
    output.push_str("\n### Extension Points\n");
    let mut found_extension = false;
    for node in &context.subgraph.nodes {
        if matches!(node.kind, NodeKind::Trait | NodeKind::Interface)
            && node.visibility == Visibility::Pub
        {
            let implementors = cg.get_callers(&node.id, 1).await?;
            let impl_count = implementors
                .iter()
                .filter(|(_, e)| matches!(e.kind, EdgeKind::Implements))
                .count();
            let _ = writeln!(
                output,
                "- **{}** ({}) - {}:{} ({} implementors)",
                node.name,
                node.kind.as_str(),
                node.file_path,
                user_line(node.start_line),
                impl_count,
            );
            found_extension = true;
        }
    }
    if !found_extension {
        output.push_str("_No public traits/interfaces found in context._\n");
    }

    append_plan_test_coverage(cg, context, output).await
}

async fn append_plan_test_coverage(
    cg: &TraceDecay,
    context: &TaskContext,
    output: &mut String,
) -> Result<()> {
    let file_paths: Vec<String> = context
        .subgraph
        .nodes
        .iter()
        .map(|n| n.file_path.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if file_paths.is_empty() {
        return Ok(());
    }

    output.push_str("\n### Test Coverage\n");
    let mut test_files: HashSet<String> = HashSet::new();
    for file in &file_paths {
        let nodes = cg.get_nodes_by_file(file).await?;
        for node in &nodes {
            let callers = cg.get_callers(&node.id, 2).await?;
            let caller_ids: Vec<String> = callers.iter().map(|(n, _)| n.id.clone()).collect();
            let test_annotated = cg.get_test_annotated_node_ids(&caller_ids).await?;
            for (caller, _) in &callers {
                if crate::tracedecay::is_test_file(&caller.file_path)
                    || test_annotated.contains(&caller.id)
                {
                    test_files.insert(caller.file_path.clone());
                }
            }
        }
    }
    if test_files.is_empty() {
        output.push_str("_No test files found covering these modules._\n");
    } else {
        let mut sorted: Vec<_> = test_files.into_iter().collect();
        sorted.sort();
        output.push_str(&compact_path_list_markdown(&sorted));
        output.push('\n');
    }

    Ok(())
}

fn compact_path_list_markdown(paths: &[String]) -> String {
    format_compact_path_list(paths.iter().map(String::as_str), "- ", "")
}

/// Handles `tracedecay_callers` tool calls.
pub(super) async fn handle_callers(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(3, |v| v.min(10) as usize);

    let results = cg.get_callers(node_id, max_depth).await?;

    let touched_files = unique_file_paths(results.iter().map(|(n, _)| n.file_path.as_str()));

    let items: Vec<Value> = results
        .iter()
        .map(|(node, edge)| {
            json!({
                "node_id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "line": user_line(node.start_line),
                "edge_kind": edge.kind.as_str(),
            })
        })
        .collect();

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
pub(super) async fn handle_callees(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(3, |v| v.min(10) as usize);

    let resolve_dispatch = args
        .get("resolve_dispatch")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let results = cg.get_callees(node_id, max_depth).await?;
    let mut seen: HashSet<String> = results.iter().map(|(n, _)| n.id.clone()).collect();

    let mut items: Vec<Value> = results
        .iter()
        .map(|(node, edge)| {
            json!({
                "node_id": node.id,
                "name": node.name,
                "kind": node.kind.as_str(),
                "file": node.file_path,
                "line": user_line(node.start_line),
                "edge_kind": edge.kind.as_str(),
                "dispatch_via_trait": false,
            })
        })
        .collect();

    if resolve_dispatch {
        for (callee, _) in &results {
            let impls = cg.get_trait_dispatch_targets(callee).await?;
            for impl_method in impls {
                if !seen.insert(impl_method.id.clone()) {
                    continue;
                }
                items.push(json!({
                    "node_id": impl_method.id,
                    "name": impl_method.name,
                    "kind": impl_method.kind.as_str(),
                    "file": impl_method.file_path,
                    "line": user_line(impl_method.start_line),
                    "edge_kind": "calls",
                    "dispatch_via_trait": true,
                    "dispatch_from": callee.id.clone(),
                }));
            }
        }
    }

    let touched_files = unique_file_paths(
        items
            .iter()
            .filter_map(|v| v.get("file").and_then(Value::as_str)),
    );

    let value = json!(items);
    Ok(generic_tool_result(cg, &args, &value, touched_files))
}

/// Handles `tracedecay_find_exact_symbol` tool calls. Bare-name lookup against
/// `idx_nodes_name` — no BM25 scoring, no fuzzy match, no qualified-name
/// suffix walk. Returns every node whose `name` column equals the query
/// exactly. Useful when you already know the symbol and want the apples-to-
/// apples cost of an index hit instead of `tracedecay_search`'s ranked query.
pub(super) async fn handle_find_exact_symbol(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
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

    let mut nodes = cg.get_nodes_by_name(name).await?;
    nodes = filter_by_scope(nodes, scope_prefix, |n| &n.file_path);
    let mut lazy_indexed_files = Vec::new();
    if nodes.is_empty() && dependency_hints::lazy_indexing_requested(&args) {
        lazy_indexed_files = dependency_hints::lazy_index_ignored_dependency_candidates(
            cg,
            name,
            limit,
            scope_prefix,
            deadline,
            cancellation,
        )
        .await?;
        if !lazy_indexed_files.is_empty() {
            nodes = filter_by_scope(cg.get_nodes_by_name(name).await?, scope_prefix, |n| {
                &n.file_path
            });
        }
    }
    if nodes.len() > limit {
        nodes.truncate(limit);
    }

    let touched_files = unique_file_paths(
        nodes
            .iter()
            .map(|n| n.file_path.as_str())
            .chain(lazy_indexed_files.iter().map(String::as_str)),
    );

    let items: Vec<Value> = nodes
        .iter()
        .map(|n| {
            json!({
                "id": n.id,
                "name": n.name,
                "qualified_name": n.qualified_name,
                "kind": n.kind.as_str(),
                "file": n.file_path,
                "line": user_line(n.start_line),
                "signature": n.signature,
            })
        })
        .collect();

    let body = json!({
        "name": name,
        "count": items.len(),
        "matches": items,
        "lazy_indexed_ignored_dependency_files": lazy_indexed_files,
    });
    Ok(generic_tool_result(cg, &args, &body, touched_files))
}

/// Handles `tracedecay_impact` tool calls.
pub(super) async fn handle_impact(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(3, |v| v.min(10) as usize);

    let subgraph = cg.get_impact_radius(node_id, max_depth).await?;

    let touched_files = unique_file_paths(subgraph.nodes.iter().map(|n| n.file_path.as_str()));

    let nodes: Vec<Value> = subgraph
        .nodes
        .iter()
        .map(|n| {
            json!({
                "id": n.id,
                "name": n.name,
                "kind": n.kind.as_str(),
                "file": n.file_path,
                "line": user_line(n.start_line),
            })
        })
        .collect();

    let output = json!({
        "node_count": subgraph.nodes.len(),
        "edge_count": subgraph.edges.len(),
        "nodes": nodes,
    });

    Ok(generic_tool_result(cg, &args, &output, touched_files))
}

/// Handles `tracedecay_node` tool calls.
pub(super) async fn handle_node(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;

    let node = cg.get_node(node_id).await?;

    match node {
        Some(n) => {
            let touched_files = vec![n.file_path.clone()];
            let file_size_bytes = cg.get_file_size_bytes(&n.file_path).await;
            // For type-kind nodes, also surface the `#[derive(...)]` macros
            // attached. Costs one extra edge query per node lookup; skipped
            // for non-type kinds where derives never apply.
            let derives: Vec<Value> = if matches!(
                n.kind,
                NodeKind::Struct
                    | NodeKind::Enum
                    | NodeKind::Union
                    | NodeKind::CaseClass
                    | NodeKind::DataClass
                    | NodeKind::Record
                    | NodeKind::PascalRecord
            ) {
                cg.get_derives_for_node(&n.id)
                    .await?
                    .into_iter()
                    .map(|name| {
                        let look = crate::derive_table::enrich(&name);
                        json!({
                            "derive": look.derive_name,
                            "trait": look.known.as_ref().map(|k| k.trait_path),
                            "methods": look.known.as_ref().map(|k| k.methods.to_vec()),
                            "well_known": look.known.is_some(),
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let output = json!({
                "id": n.id,
                "name": n.name,
                "kind": n.kind.as_str(),
                "qualified_name": n.qualified_name,
                "file": n.file_path,
                "start_line": user_line(n.start_line),
                "end_line": user_line(n.end_line),
                "signature": n.signature,
                "docstring": n.docstring,
                "visibility": n.visibility.as_str(),
                "is_async": n.is_async,
                "branches": n.branches,
                "loops": n.loops,
                "returns": n.returns,
                "max_nesting": n.max_nesting,
                "unsafe_blocks": n.unsafe_blocks,
                "unchecked_calls": n.unchecked_calls,
                "assertions": n.assertions,
                "cyclomatic_complexity": n.branches + 1,
                "cost_to_expand": cost_to_expand(&n, file_size_bytes),
                "derives": derives,
            });
            Ok(generic_tool_result(cg, &args, &output, touched_files))
        }
        None => Ok(text_tool_result(
            &format!("Node not found: {node_id}"),
            vec![],
        )),
    }
}

/// Handles `tracedecay_similar` tool calls.
pub(super) async fn handle_similar(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    require_object_args(&args, "tracedecay_similar")?;
    let symbol =
        args.get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: symbol".to_string(),
            })?;

    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(10, |v| v.min(100) as usize);

    // Use FTS search first
    let mut results = cg.search(symbol, limit).await?;

    // If FTS didn't return enough, supplement with substring matching
    if results.len() < limit {
        let all_nodes = cg.get_all_nodes().await?;
        let lower_symbol = symbol.to_ascii_lowercase();
        let existing_ids: HashSet<String> = results.iter().map(|r| r.node.id.clone()).collect();

        let mut substring_matches: Vec<crate::types::SearchResult> = all_nodes
            .into_iter()
            .filter(|n| {
                !existing_ids.contains(&n.id)
                    && (n.name.to_ascii_lowercase().contains(&lower_symbol)
                        || n.qualified_name
                            .to_ascii_lowercase()
                            .contains(&lower_symbol))
            })
            .map(|n| crate::types::SearchResult {
                node: n,
                score: 0.5,
            })
            .collect();

        substring_matches.truncate(limit.saturating_sub(results.len()));
        results.extend(substring_matches);
    }

    let touched_files = unique_file_paths(results.iter().map(|r| r.node.file_path.as_str()));

    let items: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "id": r.node.id,
                "name": r.node.name,
                "kind": r.node.kind.as_str(),
                "file": r.node.file_path,
                "line": user_line(r.node.start_line),
                "signature": r.node.signature,
                "score": r.score,
            })
        })
        .collect();

    let value = json!(items);
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
    utf8_prefix_at_or_before(line.trim(), 160).to_string()
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
pub(super) async fn handle_rename_preview(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;
    let new_name = args.get("new_name").and_then(Value::as_str);

    let Some(node) = cg.get_node(node_id).await? else {
        return Ok(text_tool_result(
            &format!("Node not found: {node_id}"),
            vec![],
        ));
    };
    let symbol_name = node.name.clone();

    let mut lines_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    // Graph occurrences per file (declaration + reference sites) — subtracted
    // from the literal textual count to isolate the text-only matches.
    let mut graph_counts: HashMap<String, usize> = HashMap::new();
    let mut touched: Vec<String> = vec![node.file_path.clone()];

    *graph_counts.entry(node.file_path.clone()).or_default() += 1;
    let decl_snippet = cached_file_lines(cg, &mut lines_cache, &node.file_path).and_then(|lines| {
        lines
            .get(node.start_line as usize)
            .map(|line| snippet_text(line))
    });
    let declaration = json!({
        "id": node.id,
        "name": node.name,
        "kind": node.kind.as_str(),
        "file": node.file_path,
        "line": user_line(node.start_line),
        "snippet": decl_snippet,
    });

    // Reference sites: incoming edges are the callers/users that name this
    // symbol. NOTE: call-edge coverage improves as the resolver improves; the
    // text-only counts below catch what the graph currently misses.
    let incoming = cg.get_incoming_edges(node_id).await?;
    let mut references: Vec<Value> = Vec::new();
    for edge in &incoming {
        if let Some(source_node) = cg.get_node(&edge.source).await? {
            touched.push(source_node.file_path.clone());
            *graph_counts
                .entry(source_node.file_path.clone())
                .or_default() += 1;
            let snippet = cached_file_lines(cg, &mut lines_cache, &source_node.file_path)
                .and_then(|lines| reference_line_snippet(lines, edge.line, &symbol_name));
            references.push(json!({
                "from_node_id": source_node.id,
                "from_name": source_node.name,
                "from_kind": source_node.kind.as_str(),
                "edge_kind": edge.kind.as_str(),
                "file": source_node.file_path,
                "line": edge.line,
                "snippet": snippet,
            }));
        }
    }

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    // Text-only matches per touched file: literal identifier occurrences of the
    // name minus the graph occurrences already accounted for. These are the
    // comments/strings/dynamic-dispatch/unresolved sites a graph-only rename
    // would miss — the scan is bounded to files that already appear in the
    // preview, so occurrences in wholly unrelated files are not counted.
    let mut text_only_matches: Vec<Value> = Vec::new();
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
            text_only_matches.push(json!({
                "file": file,
                "text_only_count": text_only,
                "note": "text-only matches — review manually",
            }));
        }
    }

    let output = json!({
        "read_only": true,
        "note": "Preview only — no files are edited. 'references' are graph reference \
                 sites (the declaration is reported separately in 'node'); \
                 'text_only_matches' are literal name occurrences NOT backed by a graph \
                 edge (comments, strings, dynamic dispatch, unresolved refs) and must be \
                 reviewed by hand. Graph call-edge coverage improves as the resolver does.",
        "symbol": symbol_name,
        "new_name": new_name,
        "node": declaration,
        "reference_count": references.len(),
        "references": references,
        "text_only_matches": text_only_matches,
    });

    Ok(generic_tool_result(cg, &args, &output, touched_files))
}

/// Handles `tracedecay_callers_for` tool calls — bulk caller lookup over many IDs.
pub(super) async fn handle_callers_for(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
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
    let kinds: Vec<EdgeKind> = if kind_arg.is_empty() {
        Vec::new()
    } else {
        match EdgeKind::from_str(kind_arg) {
            Some(k) => vec![k],
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

    let edges = cg.get_incoming_edges_bulk(&node_ids, &kinds).await?;

    // Group source IDs by target. Cap each list at max_per_item.
    let mut by_target: HashMap<String, Vec<String>> = HashMap::new();
    let mut truncated = false;
    for edge in edges {
        let entry = by_target.entry(edge.target).or_default();
        if entry.len() < max_per_item {
            entry.push(edge.source);
        } else {
            truncated = true;
        }
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
pub(super) async fn handle_by_qualified_name(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let qname = args
        .get("qualified_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: qualified_name".to_string(),
        })?;

    let nodes = cg.get_nodes_by_qualified_name(qname).await?;
    let touched_files = unique_file_paths(nodes.iter().map(|n| n.file_path.as_str()));

    let items: Vec<Value> = nodes
        .iter()
        .map(|n| {
            json!({
                "node_id": n.id,
                "name": n.name,
                "qualified_name": n.qualified_name,
                "kind": n.kind.as_str(),
                "file": n.file_path,
                "start_line": user_line(n.start_line),
                "attrs_start_line": user_line(n.attrs_start_line),
                "end_line": user_line(n.end_line),
            })
        })
        .collect();

    let value = json!(items);
    Ok(generic_tool_result(cg, &args, &value, touched_files))
}

/// Handles `tracedecay_signature` — signature-only lookup (no body) by
/// qualified name or node ID. Returns the public-API surface of a symbol so
/// callers can avoid reading the source file just to inspect the signature.
pub(super) async fn handle_signature(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let nodes = nodes_addressed_by_args(cg, &args).await?;
    let touched_files = unique_file_paths(nodes.iter().map(|n| n.file_path.as_str()));

    let mut items: Vec<Value> = Vec::with_capacity(nodes.len());
    for n in &nodes {
        let file_size_bytes = cg.get_file_size_bytes(&n.file_path).await;
        items.push(json!({
            "node_id": n.id,
            "name": n.name,
            "qualified_name": n.qualified_name,
            "kind": n.kind.as_str(),
            "visibility": n.visibility.as_str(),
            "is_async": n.is_async,
            "signature": n.signature,
            "docstring": n.docstring,
            "file": n.file_path,
            "start_line": user_line(n.start_line),
            "attrs_start_line": user_line(n.attrs_start_line),
            "end_line": user_line(n.end_line),
            "cost_to_expand": cost_to_expand(n, file_size_bytes),
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
pub(super) async fn handle_impls(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let trait_filter = args.get("trait").and_then(|v| v.as_str());
    let type_filter = args.get("type").and_then(|v| v.as_str());
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map_or(100, |v| v.min(1000) as usize);

    let mut results = cg.get_impls(trait_filter, type_filter).await?;
    let truncated = results.len() > limit;
    results.truncate(limit);

    let touched_files = unique_file_paths(
        results
            .iter()
            .map(|(impl_node, _)| impl_node.file_path.as_str()),
    );

    let items: Vec<Value> = results
        .iter()
        .map(|(impl_node, trait_node)| {
            json!({
                "impl_id": impl_node.id,
                "type": impl_node.name,
                "qualified_name": impl_node.qualified_name,
                "trait": trait_node.as_ref().map(|t| t.name.clone()),
                "trait_qualified_name": trait_node.as_ref().map(|t| t.qualified_name.clone()),
                "trait_id": trait_node.as_ref().map(|t| t.id.clone()),
                "file": impl_node.file_path,
                "start_line": user_line(impl_node.start_line),
                "end_line": user_line(impl_node.end_line),
                "signature": impl_node.signature,
            })
        })
        .collect();

    let output = json!({
        "count": items.len(),
        "truncated": truncated,
        "impls": items,
    });
    Ok(generic_tool_result(cg, &args, &output, touched_files))
}

/// Handles `tracedecay_derives` — lists `#[derive(...)]` macros on a type
/// and the trait + method names each one synthesizes (per the static
/// `derive_table`). Accepts either `node_id` or `qualified_name`.
pub(super) async fn handle_derives(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let nodes = nodes_addressed_by_args(cg, &args).await?;
    let touched_files = unique_file_paths(nodes.iter().map(|n| n.file_path.as_str()));

    let mut items: Vec<Value> = Vec::with_capacity(nodes.len());
    for n in &nodes {
        let derive_names = cg.get_derives_for_node(&n.id).await?;
        let derives: Vec<Value> = derive_names
            .iter()
            .map(|name| {
                let look = crate::derive_table::enrich(name);
                json!({
                    "derive": look.derive_name,
                    "trait": look.known.as_ref().map(|k| k.trait_path),
                    "methods": look.known.as_ref().map(|k| k.methods.to_vec()),
                    "source": look.known.as_ref().map(|k| k.source),
                    "well_known": look.known.is_some(),
                })
            })
            .collect();
        items.push(json!({
            "node_id": n.id,
            "name": n.name,
            "kind": n.kind.as_str(),
            "qualified_name": n.qualified_name,
            "file": n.file_path,
            "start_line": user_line(n.start_line),
            "derives": derives,
        }));
    }

    let value = json!(items);
    Ok(generic_tool_result(cg, &args, &value, touched_files))
}

/// Approximate token cost of expanding a node's body and its full file.
///
/// `body` uses ~20 tokens/line (≈80 chars/line at 4 chars/token), tuned for
/// Rust source — denser languages like Haskell or Python will be over-estimated
/// by ~2-3x and ultra-terse declarations (one-line `use`, single-line `pub fn`)
/// resolve to the single-line floor of 20 tokens. Good enough to decide whether
/// to set `include_code=true`; not a reliable absolute count.
/// `full_file` uses `size_bytes / 4` from the indexed `files.size`.
fn cost_to_expand(node: &Node, file_size_bytes: u64) -> Value {
    let line_count = node
        .end_line
        .saturating_sub(node.start_line)
        .saturating_add(1);
    let body_tokens = u64::from(line_count) * 20;
    let full_file_tokens = file_size_bytes / 4;
    json!({
        "body": body_tokens,
        "full_file": full_file_tokens,
    })
}

/// Handles `tracedecay_implementations` — trait / method implementor lookup.
pub(super) async fn handle_implementations(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let trait_name = args.get("trait").and_then(|v| v.as_str());
    let method_name = args.get("method").and_then(|v| v.as_str());

    if trait_name.is_none() && method_name.is_none() {
        return Err(TraceDecayError::Config {
            message: "tracedecay_implementations requires either 'trait' or 'method'".to_string(),
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
        let candidates = cg
            .db()
            .search_nodes_by_exact_name(&[name.to_string()], 50)
            .await?;
        let trait_nodes: Vec<&crate::types::Node> = candidates
            .iter()
            .filter(|n| {
                matches!(
                    n.kind,
                    NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType
                )
            })
            .collect();
        if trait_nodes.is_empty() {
            return Ok(text_tool_result(
                &format!("No trait or interface named '{name}' found."),
                vec![],
            ));
        }

        for trait_node in trait_nodes {
            let implementors = cg
                .db()
                .get_incoming_edges(&trait_node.id, &[EdgeKind::Implements])
                .await?;
            for edge in implementors {
                let Some(impl_node) = cg.db().get_node_by_id(&edge.source).await? else {
                    continue;
                };
                if scope_prefix.is_some_and(|p| !impl_node.file_path.starts_with(p)) {
                    continue;
                }
                let methods = collect_method_bodies(cg, &impl_node, &project_root).await?;
                if !touched.contains(&impl_node.file_path) {
                    touched.push(impl_node.file_path.clone());
                }
                entries.push(json!({
                    "type": impl_node.name,
                    "qualified_name": impl_node.qualified_name,
                    "kind": impl_node.kind.as_str(),
                    "file": impl_node.file_path,
                    "line": user_line(impl_node.start_line),
                    "trait": trait_node.qualified_name,
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
        let nodes = cg
            .db()
            .search_nodes_by_exact_name(&[name.to_string()], limit * 4)
            .await?;
        let method_nodes: Vec<&crate::types::Node> = nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
            .filter(|n| scope_prefix.is_none_or(|p| n.file_path.starts_with(p)))
            .take(limit)
            .collect();
        if method_nodes.is_empty() {
            return Ok(text_tool_result(
                &format!("No function or method named '{name}' found."),
                vec![],
            ));
        }
        for n in method_nodes {
            let abs_path = project_root.join(&n.file_path);
            let body = match crate::sync::read_source_file(&abs_path) {
                Ok(source) => super::info::extract_lines(&source, n.start_line, n.end_line),
                Err(_) => String::from("<file unreadable>"),
            };
            if !touched.contains(&n.file_path) {
                touched.push(n.file_path.clone());
            }
            entries.push(json!({
                "name": n.name,
                "qualified_name": n.qualified_name,
                "kind": n.kind.as_str(),
                "file": n.file_path,
                "line": user_line(n.start_line),
                "end_line": user_line(n.end_line),
                "signature": n.signature,
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

async fn collect_method_bodies(
    cg: &TraceDecay,
    impl_node: &crate::types::Node,
    project_root: &std::path::Path,
) -> Result<Vec<Value>> {
    let children = cg.db().get_children_of(&impl_node.id).await?;
    let mut out: Vec<Value> = Vec::new();
    for child in children {
        if !matches!(child.kind, NodeKind::Method | NodeKind::Function) {
            continue;
        }
        let abs_path = project_root.join(&child.file_path);
        let body = match crate::sync::read_source_file(&abs_path) {
            Ok(source) => super::info::extract_lines(&source, child.start_line, child.end_line),
            Err(_) => String::from("<file unreadable>"),
        };
        out.push(json!({
            "name": child.name,
            "kind": child.kind.as_str(),
            "line": user_line(child.start_line),
            "signature": child.signature,
            "body": body,
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::types::{FactRecord, FactSearchResult, MemoryCategory};

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
        let hit = FactSearchResult {
            fact: FactRecord {
                fact_id: 7,
                content: content.clone(),
                category: MemoryCategory::Project,
                trust_score: 0.9,
                source: Some("test".to_string()),
                entities: vec![],
                tags: vec![],
                metadata: serde_json::Value::Null,
                created_at: 0,
                updated_at: 0,
                access_count: 0,
                last_retrieved_at: None,
                retrieval_count: 0,
                helpful_count: 0,
                unhelpful_count: 0,
                last_feedback_at: None,
                last_recalled_at: None,
            },
            score: 0.5,
            fts_score: 0.25,
            jaccard_score: 0.25,
            holographic_score: 0.0,
            trust_score: 0.9,
            why: None,
        };

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
        let hit = FactSearchResult {
            fact: FactRecord {
                fact_id: 7,
                content: "first line\n# heading\n- item".to_string(),
                category: MemoryCategory::Project,
                trust_score: 0.9,
                source: Some("test".to_string()),
                entities: vec![],
                tags: vec![],
                metadata: serde_json::Value::Null,
                created_at: 0,
                updated_at: 0,
                access_count: 0,
                last_retrieved_at: None,
                retrieval_count: 0,
                helpful_count: 0,
                unhelpful_count: 0,
                last_feedback_at: None,
                last_recalled_at: None,
            },
            score: 0.5,
            fts_score: 0.25,
            jaccard_score: 0.25,
            holographic_score: 0.0,
            trust_score: 0.9,
            why: None,
        };

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
