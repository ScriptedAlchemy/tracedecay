//! `tracedecay_rename_preview`: what a rename of one symbol would touch.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    RenamePreviewNodeV1, RenamePreviewPrimitiveOutcomeV1, RenamePreviewPrimitiveRequestV1,
    RenamePreviewPrimitiveResultV1, RenamePreviewReferenceV1, RenamePreviewTextOnlyMatchV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::McpToolContext;
use crate::handlers::support::{decode_primitive_request, unique_file_paths};

use super::{
    graph_occurrence_id, graph_tool_completion, line_for_byte_offset, node_not_found_result,
    required_graph_file_path, required_graph_metadata, single_graph_adjacency_batch, user_line,
};

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

/// READ-ONLY: reports what a rename of the given symbol WOULD touch, the
/// declaration site and every graph reference site (incoming edges; outgoing
/// edges reference other symbols and so are excluded), each with a
/// current-text snippet, plus a per-file count of literal name occurrences
/// that are NOT backed by a graph edge ("text-only matches, review
/// manually"). Nothing is rewritten.
#[tracing::instrument(name = "mcp.graph.rename_preview.total", level = "trace", skip_all)]
pub async fn compute_rename_preview(
    ctx: &McpToolContext<'_>,
    graph: &tracedecay_graph_query::VerifiedGraphQuery,
    args: Value,
) -> Result<GraphToolCompletionV1> {
    let request: RenamePreviewPrimitiveRequestV1 =
        decode_primitive_request(&args, "tracedecay_rename_preview")?;

    let occurrence = graph_occurrence_id(&request.node_id)?;
    // Graph occurrences per file (declaration + reference sites), subtracted
    // from the literal textual count to isolate the text-only matches.
    let mut graph_counts: HashMap<String, usize> = HashMap::new();
    let mut touched: Vec<String> = Vec::new();
    // Graph phase: extract owned declaration fields and per-reference-site
    // inputs so the blocking file walk below needs no graph value at all.
    let (mut declaration, declaration_line, symbol_name, reference_inputs) = {
        let _span = tracing::trace_span!("mcp.graph.rename_preview.graph").entered();
        {
            let Some(node) = graph.symbol_summary(&occurrence)? else {
                return Ok(graph_tool_completion(
                    GraphToolResultV1::RenamePreview(RenamePreviewPrimitiveOutcomeV1::NotFound(
                        node_not_found_result(&request.node_id),
                    )),
                    Vec::new(),
                ));
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
                    edge_kind: edge.edge.kind.as_str().to_owned(),
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
        }
    };

    let touched_files = unique_file_paths(touched.iter().map(std::string::String::as_str));

    // File-walk phase: every referenced source file is read from disk, so it
    // runs on a blocking worker like the sibling analysis scans instead of
    // holding the async dispatch thread through the reads.
    let project_root = ctx.project_root().to_path_buf();
    let declaration_file = declaration.file.clone();
    let walk_symbol_name = symbol_name.clone();
    let walk_graph_counts = graph_counts;
    let walk_touched_files = touched_files.clone();
    let (decl_snippet, references, text_only_matches) = tracing::Instrument::instrument(tokio::task::spawn_blocking(
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
            // graph-only rename would miss, the scan is bounded to files that
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
                        note: "text-only matches, review manually".to_owned(),
                    });
                }
            }
            Ok((decl_snippet, references, text_only_matches))
        }
        ), tracing::trace_span!("mcp.graph.rename_preview.walk"))
    .await
    .map_err(|join_error| TraceDecayError::Config {
        message: format!("rename preview file scan task failed: {join_error}"),
    })??;
    declaration.snippet = decl_snippet;

    let result = RenamePreviewPrimitiveResultV1 {
        read_only: true,
        note: "Preview only. Nothing is edited. 'references' are graph reference sites \
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
    };
    Ok(graph_tool_completion(
        GraphToolResultV1::RenamePreview(RenamePreviewPrimitiveOutcomeV1::Preview(result)),
        touched_files,
    ))
}
