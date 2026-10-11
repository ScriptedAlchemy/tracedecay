//! `tracedecay_rename_preview`: what a rename of one symbol would touch.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    RenamePreviewNodeV1, RenamePreviewPrimitiveOutcomeV1, RenamePreviewPrimitiveRequestV1,
    RenamePreviewPrimitiveResultV1, RenamePreviewReferenceV1, RenamePreviewTextOnlyMatchV1,
};
use tracedecay_contracts::{CancellationSignal, Deadline};
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::McpToolContext;
use crate::handlers::bounded_search::CancelSearchOnDrop;
use crate::handlers::support::{decode_primitive_request, unique_file_paths};
use crate::server::{DispatchSettlement, dispatch_cancelled_error};
use crate::tools::dispatch_ceiling::{tool_dispatch_budget, tool_dispatch_deadline_error};

use tracedecay_runtime_core::sync::{DecodedSourceFile, SourceTextEncoding};

use super::{
    graph_occurrence_id, graph_symbol_corrupt, graph_tool_completion, node_not_found_result,
    required_graph_file_path, required_graph_metadata, single_graph_adjacency_batch, user_line,
};

/// Snippet and text-only text for one file.
///
/// Line offsets always use the canonical decode. Displayed snippets follow the
/// historical `read_to_string` body: a UTF-8 BOM stays on the first line, and
/// UTF-16 has no raw UTF-8 body so those snippets stay absent.
enum SnippetBody {
    Canonical,
    LeadingBom(String),
    Absent,
}

struct PreviewSource {
    text: String,
    line_starts: Vec<usize>,
    snippets: SnippetBody,
}

struct PreviewWalkControl {
    cancelled: Arc<AtomicBool>,
    cancellation: Option<CancellationSignal>,
    deadline: Option<Deadline>,
    budget: Duration,
}

impl PreviewWalkControl {
    fn check(&self) -> Result<()> {
        if self.deadline.as_ref().is_some_and(|deadline| {
            tracedecay_daemon_protocol::deadline_remaining(deadline).is_none()
        }) {
            return Err(tool_dispatch_deadline_error(
                "tracedecay_rename_preview",
                self.budget,
            ));
        }
        if self.cancelled.load(Ordering::Acquire)
            || self
                .cancellation
                .as_ref()
                .is_some_and(CancellationSignal::is_cancelled)
        {
            return Err(dispatch_cancelled_error(
                "tracedecay_rename_preview",
                DispatchSettlement::Settling,
                false,
            ));
        }
        Ok(())
    }
}

impl PreviewSource {
    /// Builds the newline index once from the canonical decode.
    fn from_decoded(decoded: DecodedSourceFile) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            decoded
                .text
                .match_indices('\n')
                .map(|(offset, _)| offset + 1),
        );
        let snippets = match decoded.encoding {
            SourceTextEncoding::Utf8 { leading_bom: false } => SnippetBody::Canonical,
            SourceTextEncoding::Utf8 { leading_bom: true } => {
                let mut raw = String::with_capacity(decoded.text.len() + '\u{feff}'.len_utf8());
                raw.push('\u{feff}');
                raw.push_str(&decoded.text);
                SnippetBody::LeadingBom(raw)
            }
            SourceTextEncoding::Utf16Le | SourceTextEncoding::Utf16Be => SnippetBody::Absent,
        };
        Self {
            text: decoded.text,
            line_starts,
            snippets,
        }
    }

    #[cfg(test)]
    fn from_canonical_utf8(text: String) -> Self {
        Self::from_decoded(DecodedSourceFile {
            text,
            encoding: SourceTextEncoding::Utf8 { leading_bom: false },
        })
    }

    fn snippet_lines(&self) -> Option<Vec<&str>> {
        let body = match &self.snippets {
            SnippetBody::Canonical => self.text.as_str(),
            SnippetBody::LeadingBom(raw) => raw.as_str(),
            SnippetBody::Absent => return None,
        };
        Some(body.lines().collect())
    }

    #[cfg(test)]
    fn line(&self, index: usize) -> Option<&str> {
        let start = *self.line_starts.get(index)?;
        if start == self.text.len() {
            return None;
        }
        let end = self
            .line_starts
            .get(index + 1)
            .map_or(self.text.len(), |next| next - 1);
        let line = &self.text[start..end];
        Some(if self.line_starts.get(index + 1).is_some() {
            line.strip_suffix('\r').unwrap_or(line)
        } else {
            line
        })
    }

    fn line_for_byte_offset(&self, byte_offset: u64) -> Result<u32> {
        let offset = usize::try_from(byte_offset).map_err(|_| {
            graph_symbol_corrupt("graph evidence byte offset exceeds this platform".to_owned())
        })?;
        if offset > self.text.len() {
            return Err(graph_symbol_corrupt(format!(
                "graph evidence byte offset {offset} exceeds source length {}",
                self.text.len()
            )));
        }
        let line = self.line_starts.partition_point(|start| *start <= offset) - 1;
        u32::try_from(line)
            .map_err(|_| graph_symbol_corrupt("graph evidence line exceeds u32".to_owned()))
    }
}

/// Trims and length-caps a source line for use as a preview snippet.
fn snippet_text(line: &str) -> String {
    tracedecay_runtime_core::text::utf8_prefix_at_or_before(line.trim(), 160).to_string()
}

/// Picks a current-text snippet near `approx_line` (0-based; edge line bases are
/// approximate, so neighbors are tried) that actually contains `name`, falling
/// back to the line itself. `None` when no line is available.
fn reference_line_snippet(lines: &[&str], approx_line: Option<u32>, name: &str) -> Option<String> {
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
    ordinal: usize,
    from_node_id: String,
    from_name: String,
    from_kind: String,
    edge_kind: String,
    file: String,
    evidence_start_byte: u64,
}

struct PreviewFileWalk {
    project_root: PathBuf,
    declaration_line: u32,
    result: RenamePreviewPrimitiveResultV1,
    reference_inputs: Vec<RenameReferenceSiteInput>,
    graph_counts: HashMap<String, usize>,
    touched_files: Vec<String>,
    control: PreviewWalkControl,
}

impl PreviewFileWalk {
    fn run(mut self) -> Result<RenamePreviewPrimitiveResultV1> {
        let mut reference_inputs = self.reference_inputs;
        reference_inputs.sort_unstable_by(|left, right| left.file.cmp(&right.file));
        let mut reference_inputs = reference_inputs.into_iter().peekable();
        let mut files = self.touched_files.clone();
        files.sort_unstable();
        let mut decl_snippet = None;
        let mut references = Vec::new();
        let mut text_only_matches = HashMap::new();
        // Only the current file's decoded text and line index are resident.
        // Ordinals retain the graph's reference order across the grouped walk.
        for file in files {
            self.control.check()?;
            let has_references = reference_inputs
                .peek()
                .is_some_and(|input| input.file == file);
            let decoded = match tracedecay_runtime_core::sync::read_decoded_source_file(
                &self.project_root.join(&file),
            ) {
                Ok(decoded) => decoded,
                Err(error) if has_references => return Err(error.into()),
                // Declaration snippets are optional; reference evidence is not.
                Err(_) => continue,
            };
            // The in-flight read can finish after cancellation. Stop before
            // the line index or the next file.
            self.control.check()?;
            // One decoded buffer and its newline index, dropped before the
            // next file so the preview does not retain every touched source.
            let source = PreviewSource::from_decoded(decoded);
            let lines = source.snippet_lines();
            if file == self.result.node.file {
                decl_snippet = lines.as_ref().and_then(|lines| {
                    lines
                        .get(self.declaration_line as usize)
                        .copied()
                        .map(snippet_text)
                });
            }
            while let Some(input) = reference_inputs.next_if(|input| input.file == file) {
                let line = source.line_for_byte_offset(input.evidence_start_byte)?;
                let snippet = lines.as_deref().and_then(|lines| {
                    reference_line_snippet(lines, Some(line), &self.result.symbol)
                });
                references.push((
                    input.ordinal,
                    RenamePreviewReferenceV1 {
                        from_node_id: input.from_node_id,
                        from_name: input.from_name,
                        from_kind: input.from_kind,
                        edge_kind: input.edge_kind,
                        file: input.file,
                        line: user_line(line),
                        snippet,
                    },
                ));
            }
            let total = lines.as_ref().map_or(0, |lines| {
                lines
                    .iter()
                    .map(|line| count_identifier_occurrences(line, &self.result.symbol))
                    .sum::<usize>()
            });
            let graph = self.graph_counts.get(&file).copied().unwrap_or(0);
            let text_only = total.saturating_sub(graph);
            if text_only > 0 {
                text_only_matches.insert(
                    file.clone(),
                    RenamePreviewTextOnlyMatchV1 {
                        file,
                        text_only_count: text_only,
                        note: "text-only matches, review manually".to_owned(),
                    },
                );
            }
        }
        references.sort_unstable_by_key(|(ordinal, _)| *ordinal);
        let references: Vec<_> = references
            .into_iter()
            .map(|(_, reference)| reference)
            .collect();
        let text_only_matches = self
            .touched_files
            .into_iter()
            .filter_map(|file| text_only_matches.remove(&file))
            .collect();
        self.control.check()?;
        self.result.node.snippet = decl_snippet;
        self.result.reference_count = references.len();
        self.result.references = references;
        self.result.text_only_matches = text_only_matches;
        Ok(self.result)
    }
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
    let (declaration, declaration_line, symbol_name, reference_inputs) = {
        let _span = tracing::trace_span!("mcp.graph.rename_preview.graph").entered();
        {
            let Some(node) = graph.symbol_summary(&occurrence)? else {
                return Ok(graph_tool_completion(
                    GraphToolResultV1::RenamePreview(RenamePreviewPrimitiveOutcomeV1::NotFound(
                        node_not_found_result(graph, &request.node_id, &occurrence)?,
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
            for (ordinal, edge) in incoming.into_iter().enumerate() {
                let source_node = edge.neighbor;
                let source_metadata = required_graph_metadata(&source_node)?;
                let source_file = required_graph_file_path(&source_node)?;
                touched.push(source_file.to_owned());
                *graph_counts.entry(source_file.to_owned()).or_default() += 1;
                reference_inputs.push(RenameReferenceSiteInput {
                    ordinal,
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

    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel_on_drop = CancelSearchOnDrop::new(Arc::clone(&cancelled));
    let budget = tool_dispatch_budget("tracedecay_rename_preview", ctx.deadline())
        .ok_or_else(|| tool_dispatch_deadline_error("tracedecay_rename_preview", Duration::ZERO))?;
    let control = PreviewWalkControl {
        cancelled,
        cancellation: ctx.cancellation().cloned(),
        deadline: ctx.deadline().cloned(),
        budget,
    };
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
        reference_count: 0,
        references: Vec::new(),
        text_only_matches: Vec::new(),
        freshness: None,
    };
    let walk = PreviewFileWalk {
        project_root: ctx.project_root().to_path_buf(),
        declaration_line,
        result,
        reference_inputs,
        graph_counts,
        touched_files: touched_files.clone(),
        control,
    };
    let walk_span = tracing::trace_span!("mcp.graph.rename_preview.walk");
    let result = tokio::task::spawn_blocking(move || walk_span.in_scope(|| walk.run()))
        .await
        .map_err(|join_error| TraceDecayError::Config {
            message: format!("rename preview file scan task failed: {join_error}"),
        })??;
    drop(cancel_on_drop);
    Ok(graph_tool_completion(
        GraphToolResultV1::RenamePreview(RenamePreviewPrimitiveOutcomeV1::Preview(result)),
        touched_files,
    ))
}

#[cfg(test)]
mod tests {
    use super::PreviewSource;
    use tracedecay_runtime_core::sync::{DecodedSourceFile, SourceTextEncoding};

    #[test]
    fn source_lines_preserve_byte_boundaries_and_refuse_stale_offsets() {
        for text in [
            "",
            "\n",
            "é😀\r\nlast\r\n",
            "first\n\nlast",
            "\r",
            "first\r\nlast\r",
        ] {
            let source = PreviewSource::from_canonical_utf8(text.to_owned());
            let lines = text.lines().collect::<Vec<_>>();
            for (index, expected) in lines.iter().enumerate() {
                assert_eq!(source.line(index), Some(*expected));
            }
            assert_eq!(source.line(lines.len()), None);
            for offset in 0..=text.len() {
                let expected = text.as_bytes()[..offset]
                    .split(|byte| *byte == b'\n')
                    .count()
                    .saturating_sub(1);
                assert_eq!(
                    source.line_for_byte_offset(offset as u64).unwrap(),
                    expected as u32
                );
            }
            for offset in [text.len() as u64 + 1, u64::MAX] {
                assert!(source.line_for_byte_offset(offset).is_err());
            }
        }
    }

    #[test]
    fn snippets_keep_a_utf8_bom_and_omit_utf16_bodies() {
        let bom = PreviewSource::from_decoded(DecodedSourceFile {
            text: "pub fn reserve_stock() {}\nnext\n".to_owned(),
            encoding: SourceTextEncoding::Utf8 { leading_bom: true },
        });
        let lines = bom.snippet_lines().expect("utf-8 bom still has raw lines");
        assert_eq!(lines[0], "\u{feff}pub fn reserve_stock() {}");
        assert_eq!(lines[1], "next");
        assert_eq!(bom.line_for_byte_offset(0).unwrap(), 0);

        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let utf16 = PreviewSource::from_decoded(DecodedSourceFile {
                text: "pub fn reserve_stock() {}\n".to_owned(),
                encoding,
            });
            assert!(utf16.snippet_lines().is_none());
            assert_eq!(utf16.line_for_byte_offset(0).unwrap(), 0);
        }
    }
}
