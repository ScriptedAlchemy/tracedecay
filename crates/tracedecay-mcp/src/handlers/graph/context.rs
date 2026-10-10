//! `tracedecay_context`: ranked search matches, their graph neighborhood,
//! code blocks, and scoped memory, assembled for one task.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_contracts::InvocationAnalyticsV1;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    ContextCodeBlockV1, ContextLexicalAnchorV1, ContextModeV1, ContextRelatedOmissionV1,
    ContextResultV1, ContextRetrievalPlanV1, ContextRetrievalRouteV1, ContextSearchMatchV1,
    ContextStageV1, ContextSurfaceRequestV1, LexicalAnchorDropReasonV1, PrimitiveSymbolLocationV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{ExactClass, RankedCandidate, RelationEdgeKindV1, RetrieverKind};
use tracedecay_query::retrieval::lexical::{preferred_symbol_tokens, task_is_name_shaped};

use crate::McpToolContext;
use crate::analysis::{is_ident_byte, line_number_at};
#[cfg(test)]
use crate::context_headings::CONTEXT_SEEN_NODE_IDS_LABEL;
use crate::handlers::dependency_hints;
use crate::handlers::support::{decode_primitive_request, unique_file_paths};

use super::context_markdown::verified_plan_context;
use super::context_support::{
    ContextMemoryOutcome, context_memory_analytics, context_memory_options, context_memory_outcome,
    context_memory_read_control, context_memory_stage,
};
use super::lexical_routing;
use super::primitive_surface::{
    search_coverage as primitive_search_coverage, symbol_location as primitive_symbol_location,
};
use super::search::execute_code_index_search;
use super::search_evidence::{bind_verified_graph_to_search, race_primary_search_with_graph};
use super::search_freshness::{
    ServedGenerationV1, lanes_under_scheduler_freshness, search_freshness,
    worktree_freshness_from_payload,
};
use super::{
    graph_symbol_end_line, graph_symbol_paths, required_graph_file_path, required_graph_metadata,
    user_line,
};

#[cfg(test)]
use super::context_support::{context_markdown_lane_preview, context_memory_section};

/// Edge rows each direction of the related-symbol walk reads across all
/// selected symbols. The walk takes each neighbor from its edge row and
/// ranks it from the catalog without reading a symbol entity, so it covers
/// a hub's neighborhood before the `max_nodes` cut; past this many rows the
/// omission reports its total as a lower bound.
const CONTEXT_RELATED_WALK_ROWS: usize = 4_096;

/// Related symbols rank by the best edge kind joining them to a selected
/// symbol, lower first: behavior (calls), then the type hierarchy
/// (implements, extends), then signatures (`type_of`, returns, receives),
/// then looser references (uses, annotates), then containment.
fn context_related_kind_rank(kind: RelationEdgeKindV1) -> u8 {
    match kind {
        RelationEdgeKindV1::Calls => 0,
        RelationEdgeKindV1::Implements => 1,
        RelationEdgeKindV1::Extends => 2,
        RelationEdgeKindV1::TypeOf => 3,
        RelationEdgeKindV1::Returns => 4,
        RelationEdgeKindV1::Receives => 5,
        RelationEdgeKindV1::Uses => 6,
        RelationEdgeKindV1::Annotates => 7,
        RelationEdgeKindV1::Contains => 8,
    }
}

#[derive(Default)]
struct ContextGraphProjection {
    selected: Vec<CodeGraphSymbolSummaryV1>,
    related: Vec<CodeGraphSymbolSummaryV1>,
    related_omission: Option<ContextRelatedOmissionV1>,
    code_blocks: Vec<ContextCodeBlockV1>,
    touched_files: Vec<String>,
}

/// Whether only the graph lane ranked this candidate. Such a candidate is a
/// neighbor of the task's matches, not a match; context ranks the neighbors
/// of its matches itself.
fn graph_lane_only(ranked: &RankedCandidate) -> bool {
    let contributions = &ranked.candidate.contributions;
    !contributions.is_empty()
        && contributions
            .iter()
            .all(|contribution| contribution.retriever == RetrieverKind::Graph)
}

fn context_search_matches(
    complete: &tracedecay_query::code_search::CodeIndexSearchCompletedV1,
    scope_prefix: Option<&str>,
) -> Vec<ContextSearchMatchV1> {
    complete
        .ordered_candidates
        .iter()
        .filter(|ranked| !graph_lane_only(ranked))
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

/// The kernel's per-anchor receipts in the context wire shape, counted
/// against the search matches this context carries: a served site outside
/// `scope_prefix` is dropped as out of scope.
fn context_lexical_anchors(
    complete: &tracedecay_query::code_search::CodeIndexSearchCompletedV1,
    scope_prefix: Option<&str>,
) -> Vec<ContextLexicalAnchorV1> {
    let mut receipt = complete.lexical_routes.clone();
    if let Some(prefix) = scope_prefix {
        receipt.reconcile_served(|site| {
            complete
                .display_by_anchor
                .get(site)
                .filter(|display| !display.path.starts_with(prefix))
                .map(|_| LexicalAnchorDropReasonV1::OutOfScope)
        });
    }
    receipt
        .anchors
        .iter()
        .map(lexical_routing::anchor_outcome)
        .collect()
}

fn context_graph_projection(
    ctx: &McpToolContext<'_>,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    complete: &tracedecay_query::code_search::CodeIndexSearchCompletedV1,
    scope_prefix: Option<&str>,
    max_nodes: usize,
    include_code: bool,
    max_code_blocks: usize,
) -> Result<ContextGraphProjection> {
    let mut selected = Vec::new();
    for ranked in complete
        .ordered_candidates
        .iter()
        .filter(|ranked| !graph_lane_only(ranked))
    {
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
    let (related, related_omission) = if seeds.is_empty() {
        (Vec::new(), None)
    } else {
        let ranked = graph.ranked_neighbors(
            &seeds,
            context_related_kind_rank,
            CONTEXT_RELATED_WALK_ROWS,
            max_nodes,
        )?;
        let omitted = ranked.total.saturating_sub(ranked.neighbors.len());
        let omission = (omitted > 0 || ranked.walk_truncated).then_some(ContextRelatedOmissionV1 {
            total: ranked.total,
            omitted,
            total_is_lower_bound: ranked.walk_truncated,
        });
        (ranked.neighbors, omission)
    };

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
                        &ctx.project_root().join(file_path),
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
                code: extract_lines(
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
        related_omission,
        code_blocks,
        touched_files,
    })
}

/// Extract the source spanning tree-sitter rows `start_line..=end_line`
/// (0-based, inclusive) from `source`. Node line fields are stored as the
/// raw tree-sitter row index, so the caller passes them through unchanged.
/// Returns the empty string if the range is out of bounds.
fn extract_lines(source: &str, start_line: u32, end_line: u32) -> String {
    let start = start_line as usize;
    let end_exclusive = (end_line as usize).saturating_add(1);
    if start >= end_exclusive {
        return String::new();
    }
    let mut selected = source.lines().skip(start).take(end_exclusive - start);
    let Some(first) = selected.next() else {
        return String::new();
    };
    let mut body = String::with_capacity(first.len());
    body.push_str(first);
    for line in selected {
        body.push('\n');
        body.push_str(line);
    }
    body
}

/// Bound for a filesystem window when the graph catalog cannot name a
/// symbol's line span. The window starts at the identifier and keeps
/// following blank or more-indented lines; it is not a language parse.
const SEARCH_MATCH_CODE_WINDOW_LINES: usize = 40;

struct SearchMatchHydration {
    symbols: Vec<PrimitiveSymbolLocationV1>,
    code_blocks: Vec<ContextCodeBlockV1>,
    touched_files: Vec<String>,
}

fn leading_ws_len(line: &str) -> usize {
    line.as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count()
}

/// First 1-based line whose text contains `name` as an identifier token.
fn first_identifier_line(source: &str, name: &str) -> Option<u32> {
    if name.is_empty() {
        return None;
    }
    let bytes = source.as_bytes();
    let needle = name.as_bytes();
    let mut offset = 0;
    while offset + needle.len() <= bytes.len() {
        if bytes[offset..].starts_with(needle) {
            let before = offset.checked_sub(1).and_then(|index| bytes.get(index));
            let after = bytes.get(offset + needle.len());
            if !before.copied().is_some_and(is_ident_byte)
                && !after.copied().is_some_and(is_ident_byte)
            {
                return Some(line_number_at(source, offset));
            }
            offset += needle.len();
        } else {
            offset += 1;
        }
    }
    None
}

fn identifier_window(source: &str, name: &str) -> Option<(u32, u32, String)> {
    let start_line = first_identifier_line(source, name)?;
    let start_idx = usize::try_from(start_line.saturating_sub(1)).ok()?;
    let lines: Vec<&str> = source.lines().collect();
    let start_text = *lines.get(start_idx)?;
    let indent = leading_ws_len(start_text);
    let mut end_idx = start_idx;
    for (idx, line) in lines
        .iter()
        .enumerate()
        .skip(start_idx.saturating_add(1))
        .take(SEARCH_MATCH_CODE_WINDOW_LINES.saturating_sub(1))
    {
        if line.trim().is_empty() || leading_ws_len(line) > indent {
            end_idx = idx;
            continue;
        }
        break;
    }
    if let Some(closer) = lines.get(end_idx.saturating_add(1)) {
        if matches!(closer.trim(), "}" | "};" | "}," | ")" | ");" | "]" | "],") {
            end_idx = end_idx.saturating_add(1);
        }
    }
    let end_line = u32::try_from(end_idx.saturating_add(1)).ok()?;
    Some((
        start_line,
        end_line,
        extract_lines(
            source,
            start_line.saturating_sub(1),
            end_line.saturating_sub(1),
        ),
    ))
}

/// Symbols and code from search matches when the graph catalog cannot
/// resolve them. Search already named the files; the filesystem supplies
/// the line window. A missing file or identifier is a typed unavailable
/// field, not an empty success.
fn hydrate_context_from_search_matches(
    ctx: &McpToolContext<'_>,
    search_matches: &[ContextSearchMatchV1],
    include_code: bool,
    max_code_blocks: usize,
) -> SearchMatchHydration {
    let mut symbols = Vec::with_capacity(search_matches.len());
    let mut code_blocks = Vec::new();
    let mut source_by_path = HashMap::<String, Option<String>>::new();
    let mut touched_files = Vec::new();
    for search_match in search_matches {
        if !touched_files.iter().any(|path| path == &search_match.file) {
            touched_files.push(search_match.file.clone());
        }
        let source = match source_by_path.entry(search_match.file.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut().as_deref(),
            std::collections::hash_map::Entry::Vacant(entry) => entry
                .insert(
                    tracedecay_runtime_core::sync::read_source_file(
                        &ctx.project_root().join(&search_match.file),
                    )
                    .ok(),
                )
                .as_deref(),
        };
        let (start_line, end_line, unavailable_fields, window) = match source {
            Some(source) if search_match.kind != "file" => {
                match identifier_window(source, &search_match.name) {
                    Some((start_line, end_line, code)) => (
                        start_line,
                        end_line,
                        vec!["attrs_start_line".to_owned()],
                        Some(code),
                    ),
                    None => (
                        0,
                        0,
                        vec![
                            "start_line".to_owned(),
                            "end_line".to_owned(),
                            "attrs_start_line".to_owned(),
                        ],
                        None,
                    ),
                }
            }
            Some(_) => (1, 1, vec!["attrs_start_line".to_owned()], None),
            None => (
                0,
                0,
                vec![
                    "start_line".to_owned(),
                    "end_line".to_owned(),
                    "attrs_start_line".to_owned(),
                ],
                None,
            ),
        };
        let node_id = search_match.anchor_id.clone();
        symbols.push(PrimitiveSymbolLocationV1 {
            node_id: node_id.clone(),
            name: search_match.name.clone(),
            qualified_name: search_match.qualified_name.clone(),
            kind: search_match.kind.clone(),
            file: search_match.file.clone(),
            start_line,
            end_line,
            unavailable_fields,
        });
        if include_code && code_blocks.len() < max_code_blocks {
            if let Some(code) = window {
                code_blocks.push(ContextCodeBlockV1 {
                    node_id,
                    file: search_match.file.clone(),
                    start_line,
                    end_line,
                    code,
                });
            }
        }
    }
    SearchMatchHydration {
        symbols,
        code_blocks,
        touched_files,
    }
}

#[tracing::instrument(name = "mcp.graph.context.total", level = "trace", skip_all)]
pub async fn compute_context<G, F>(
    ctx: &McpToolContext<'_>,
    open_graph: G,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1>
where
    G: Fn() -> F,
    F: Future<Output = Result<tracedecay_graph_query::VerifiedGraphQuery>>,
{
    let search_executor = ctx.code_index_search_executor();
    let search_authority = ctx.code_index_search_authority();
    let deadline = ctx.deadline().cloned();
    let cancellation = ctx.cancellation().cloned();
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
    let prefer_symbol = request
        .prefer_symbol
        .unwrap_or_else(|| task_is_name_shaped(task));
    let lexical_routing = lexical_routing::routing_from_parts(
        request.lexical_anchors.clone().unwrap_or_default(),
        Some(prefer_symbol),
    )?
    .with_task_identifiers(task);
    let requested_anchors: Vec<String> = lexical_routing
        .anchors
        .iter()
        .map(|anchor| anchor.as_str().to_owned())
        .collect();
    let memory_options = context_memory_options(&args);
    let memory_read_control =
        context_memory_read_control(&memory_options, deadline.as_ref(), cancellation.as_ref())?;
    // Graph enrichment is optional unless the caller asks for source bodies.
    // That request waits for graph admission under the same deadline and
    // cancellation as search; ordinary lexical/exact retrieval stays independent.
    let search = execute_code_index_search(
        search_executor,
        tracedecay_query::code_search::CodeIndexSearchRequestV1 {
            project_root: ctx.project_root().to_path_buf(),
            query: task.to_owned(),
            source_revision: None,
            source_tree: None,
            source_reference: None,
            limit: max_nodes,
            cursor: None,
            lexical_routing,
            authority: search_authority.cloned(),
            deadline,
            cancellation,
        },
    );
    let memory = context_memory_outcome(ctx, task, &memory_options, memory_read_control.as_ref());
    let graph_refused_early = AtomicBool::new(false);
    let graph = async {
        let graph = open_graph().await;
        graph_refused_early.store(graph.is_err(), Ordering::Relaxed);
        graph
    };
    let search_and_graph = race_primary_search_with_graph(search, graph, false, None, include_code);
    let ((outcome, graph), memory_outcome) = tokio::join!(search_and_graph, memory);
    // A graph open refused while search was still waiting for a restart's
    // retained generation to seat saw the unseated graph; a complete search
    // proves that seat, so the graph is read again rather than answered empty.
    let graph = match (&outcome, graph) {
        (tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(_), Err(_))
            if graph_refused_early.load(Ordering::Relaxed) =>
        {
            open_graph().await
        }
        (_, graph) => graph,
    };
    // Read after the search settles: the verdict must describe the scheduler
    // state at serve time, not a snapshot taken before the lanes ran.
    let freshness_payload = ctx.freshness().await;
    let worktree_freshness = worktree_freshness_from_payload(freshness_payload.as_ref());
    let (complete, code_generation, coverage, freshness, search_matches, lexical_anchors) =
        match outcome {
            tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Complete(complete) => {
                let search_matches = context_search_matches(&complete, scope_prefix);
                let lexical_anchors = context_lexical_anchors(&complete, scope_prefix);
                let code_generation = Some(complete.code_generation.clone());
                let lanes = lanes_under_scheduler_freshness(
                    complete.coverage.clone(),
                    &complete.code_generation,
                    &worktree_freshness,
                );
                let coverage = primitive_search_coverage(&lanes);
                let freshness = search_freshness(
                    ServedGenerationV1::Served(&complete.code_generation),
                    &lanes,
                    &worktree_freshness,
                );
                (
                    Some(complete),
                    code_generation,
                    coverage,
                    freshness,
                    search_matches,
                    lexical_anchors,
                )
            }
            tracedecay_query::code_search::CodeIndexSearchOutcomeV1::Unavailable(unavailable) => (
                None,
                unavailable.code_generation,
                primitive_search_coverage(&unavailable.coverage),
                search_freshness(
                    ServedGenerationV1::Unavailable {
                        reason: unavailable.reason.as_str(),
                    },
                    &unavailable.coverage,
                    &worktree_freshness,
                ),
                Vec::new(),
                requested_anchors
                    .iter()
                    .map(|anchor| ContextLexicalAnchorV1::NotServed {
                        anchor: anchor.clone(),
                    })
                    .collect(),
            ),
        };
    let graph = match complete.as_ref() {
        Some(complete) => bind_verified_graph_to_search(graph, &complete.code_generation),
        None => graph,
    };
    let (graph, mut projection, verified_graph_evidence, graph_stage) =
        match (graph, complete.as_ref()) {
            (Ok(graph), Some(complete)) => {
                let match_result = {
                    let _span = tracing::trace_span!("mcp.graph.context.graph").entered();
                    context_graph_projection(
                        ctx,
                        &graph,
                        complete,
                        scope_prefix,
                        max_nodes,
                        include_code,
                        max_code_blocks,
                    )
                };
                match match_result {
                    Ok(projection) => {
                        let stage =
                            ContextStageV1::ran(max_nodes, projection.selected.len(), false);
                        (Some(graph), projection, None, stage)
                    }
                    Err(error) => (
                        None,
                        ContextGraphProjection::default(),
                        Some(dependency_hints::unavailable_evidence(&error)),
                        ContextStageV1::Unavailable,
                    ),
                }
            }
            (Ok(graph), None) => (
                Some(graph),
                ContextGraphProjection::default(),
                None,
                ContextStageV1::Skipped,
            ),
            (Err(error), _) => (
                None,
                ContextGraphProjection::default(),
                Some(dependency_hints::unavailable_evidence(&error)),
                ContextStageV1::Unavailable,
            ),
        };
    // Search already ranked the sites. When the catalog is still warming,
    // name lookup admits nothing; hydrate symbols and code from those
    // matches instead of answering an empty success.
    let search_hydration =
        (projection.selected.is_empty() && !search_matches.is_empty()).then(|| {
            hydrate_context_from_search_matches(ctx, &search_matches, include_code, max_code_blocks)
        });
    if let Some(hydrated) = search_hydration.as_ref() {
        if include_code && projection.code_blocks.is_empty() {
            projection.code_blocks.clone_from(&hydrated.code_blocks);
        }
        projection.touched_files = unique_file_paths(
            projection
                .touched_files
                .iter()
                .map(String::as_str)
                .chain(hydrated.touched_files.iter().map(String::as_str)),
        );
    }
    let retrieval = ContextRetrievalPlanV1 {
        search: match complete.as_ref() {
            Some(complete) => ContextStageV1::ran(
                max_nodes,
                search_matches.len(),
                complete.next_cursor.is_some(),
            ),
            None => ContextStageV1::Unavailable,
        },
        graph: graph_stage,
        related: if projection.selected.is_empty() {
            ContextStageV1::Skipped
        } else {
            ContextStageV1::ran(
                max_nodes,
                projection.related.len(),
                projection.related_omission.is_some(),
            )
        },
        code: if !include_code {
            ContextStageV1::NotRequested
        } else if !projection.code_blocks.is_empty() {
            ContextStageV1::ran(
                max_code_blocks,
                projection.code_blocks.len(),
                search_matches.len() > max_code_blocks,
            )
        } else if projection.selected.is_empty() && search_matches.is_empty() {
            ContextStageV1::Skipped
        } else {
            ContextStageV1::ran(
                max_code_blocks,
                projection.code_blocks.len(),
                search_matches.len() > max_code_blocks,
            )
        },
        memory: ContextStageV1::NotRequested,
    };
    let ContextMemoryOutcome {
        hits: memory_matches,
        graph_coverage: memory_graph_coverage,
        error: memory_matches_error,
    } = memory_outcome;
    let symbols = if let Some(hydrated) = search_hydration {
        hydrated.symbols
    } else {
        projection
            .selected
            .iter()
            .map(primitive_symbol_location)
            .collect::<Result<Vec<_>>>()?
    };
    let related_symbols = projection
        .related
        .iter()
        .map(primitive_symbol_location)
        .collect::<Result<Vec<_>>>()?;
    let plan = match (mode, graph.as_ref()) {
        (ContextModeV1::Plan, Some(graph)) => {
            Some(verified_plan_context(graph, &projection.selected)?)
        }
        _ => None,
    };
    let touched_files = unique_file_paths(
        projection.touched_files.iter().map(String::as_str).chain(
            search_matches
                .iter()
                .map(|search_match| search_match.file.as_str()),
        ),
    );
    let analytics = InvocationAnalyticsV1 {
        context_memory: Some(context_memory_analytics(
            &memory_options,
            &memory_matches,
            memory_matches_error.as_deref(),
        )),
        pr_context: None,
    };
    let retrieval = ContextRetrievalPlanV1 {
        memory: context_memory_stage(
            &memory_options,
            &memory_matches,
            memory_matches_error.as_deref(),
        ),
        ..retrieval
    };
    let cost = graph
        .as_ref()
        .map(tracedecay_graph_query::VerifiedGraphQuery::read_cost);
    // Mirror the lexical planner's admission rule: a name preference only
    // becomes a served name route when at least one eligible symbol token
    // survives normalization and the stoplist.
    let route = if prefer_symbol && !preferred_symbol_tokens(task).is_empty() {
        ContextRetrievalRouteV1::Name
    } else {
        ContextRetrievalRouteV1::Prose
    };
    let budget_tokens = request.budget_tokens;
    let mut result = ContextResultV1 {
        task: request.task,
        route,
        mode,
        freshness,
        code_generation,
        search_matches,
        lexical_anchors,
        symbols,
        related_symbols,
        related_omission: projection.related_omission,
        code: projection.code_blocks,
        coverage,
        memory_matches,
        memory_graph_coverage,
        memory_matches_error,
        verified_graph_evidence,
        plan,
        retrieval,
        token_budget: None,
    };
    if let Some(budget) = budget_tokens {
        let shares = crate::handlers::token_budget::quotas(budget, &[40, 25, 25, 10]);
        let sections = vec![
            crate::handlers::token_budget::trim_section("symbols", &mut result.symbols, shares[0])?,
            crate::handlers::token_budget::trim_section(
                "related_symbols",
                &mut result.related_symbols,
                shares[1],
            )?,
            crate::handlers::token_budget::trim_section("code", &mut result.code, shares[2])?,
            crate::handlers::token_budget::trim_section(
                "memory_matches",
                &mut result.memory_matches,
                shares[3],
            )?,
        ];
        result.token_budget = Some(crate::handlers::token_budget::cut(budget, sections)?);
    }
    Ok(GraphToolCompletionV1 {
        result: GraphToolResultV1::Context(Box::new(result)),
        touched_files,
        code_graph: None,
        analytics: Some(analytics),
        cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tracedecay_contracts::memory::FactSearchHitV1;

    fn context_memory_hit(content: &str) -> FactSearchHitV1 {
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
        let hit = context_memory_hit(&content);

        let Some(section) = context_memory_section(&[hit], None) else {
            panic!("memory hit should render");
        };

        assert!(section.contains(&content));
        assert!(section.contains("tail-marker"));
        assert!(!section.contains("..."));
        assert!(section.contains("tracedecay_fact_feedback"));
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

    #[test]
    fn first_identifier_line_uses_word_boundaries() {
        let source = "spread\nfn read() {}\n";
        assert_eq!(first_identifier_line(source, "read"), Some(2));
        assert_eq!(first_identifier_line(source, "spread"), Some(1));
        assert_eq!(first_identifier_line(source, "missing"), None);
        assert_eq!(first_identifier_line(source, ""), None);
    }

    #[test]
    fn identifier_window_keeps_the_indented_body() {
        let source = "pub fn invoice_total(cents: u32) -> u32 {\n    cents\n}\n";
        let Some((start, end, code)) = identifier_window(source, "invoice_total") else {
            panic!("invoice_total should hydrate");
        };
        assert_eq!(start, 1);
        assert_eq!(end, 3);
        assert_eq!(code, source.trim_end());
    }
}
