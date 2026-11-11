//! Exact-endpoint call paths and source attested by their graph generation.

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_code_index_runtime::code_index_scheduler::sealed_source_bytes;
use tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1;
use tracedecay_contracts::retrieval::{
    ContextCallPathEdgeV1, ContextCallPathLimitV1, ContextCallPathNodeV1, ContextCallPathOutcomeV1,
    ContextCallPathRequestV1, ContextCallPathResultV1, ContextCodeBlockV1,
    ContextPathSourceOmissionV1, ContextPathSourceV1, PrimitiveUnavailableEvidenceV1,
    PrimitiveUnavailableStatusV1, ServedCodeGraphGenerationV1,
};
use tracedecay_contracts::{
    CancellationSignal, Deadline, RequestAdmission, RequestContext, now_micros,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{
    EdgeAuthorityV1, MAX_CHUNK_TEXT_BYTES, RelationEdgeKindV1, SanitizedCodeFileV1,
    SnapshotFileDispositionV1, SymbolOccurrenceId, path_matches_scope,
};
use tracedecay_graph_query::{CodeGraphReadError, GraphQueryError, VerifiedGraphQuery};
use tracedecay_session_memory::context::{RequestInterruption, run_deadline_signal_interruptible};

use crate::McpToolContext;
use crate::analysis::line_number_at;
use crate::handlers::bounded_search::SetFlagOnDrop;
use crate::handlers::dependency_hints::unavailable_evidence;

use super::primitive_surface::symbol_location;
use super::search_freshness::graph_read_freshness;
use super::{
    GRAPH_RELATION_READ_LIMIT, graph_occurrence_id, graph_symbol_corrupt, graph_symbol_end_line,
    required_graph_file_path, required_graph_metadata,
};

pub(super) struct ContextCallPathRequest {
    from: SymbolOccurrenceId,
    to: SymbolOccurrenceId,
    max_depth: u32,
}

pub(super) fn validate_call_path(
    request: &ContextCallPathRequestV1,
) -> Result<ContextCallPathRequest> {
    let max_depth = request.max_depth.unwrap_or(8);
    if !(1..=32).contains(&max_depth) {
        return Err(TraceDecayError::Config {
            message: "invalid parameter: call_path.max_depth must be between 1 and 32".to_owned(),
        });
    }
    Ok(ContextCallPathRequest {
        from: graph_occurrence_id(&request.from_node_id)?,
        to: graph_occurrence_id(&request.to_node_id)?,
        max_depth,
    })
}

pub(super) struct ContextCallPathProjection {
    pub result: ContextCallPathResultV1,
    pub code: Vec<ContextCodeBlockV1>,
}

impl ContextCallPathProjection {
    pub fn nodes(&self) -> &[ContextCallPathNodeV1] {
        match &self.result.outcome {
            ContextCallPathOutcomeV1::Found { nodes, .. } => nodes,
            _ => &[],
        }
    }
}

fn evidence(
    reason: &str,
    retryable: bool,
    detail: impl Into<String>,
) -> PrimitiveUnavailableEvidenceV1 {
    PrimitiveUnavailableEvidenceV1 {
        status: PrimitiveUnavailableStatusV1::Unavailable,
        reason_code: reason.to_owned(),
        retryable,
        detail: detail.into(),
    }
}

/// Source is requested only when `code_block_limit` carries its admitted cap.
#[tracing::instrument(name = "mcp.graph.context.call_path", level = "trace", skip_all)]
pub(super) async fn context_call_path(
    ctx: &McpToolContext<'_>,
    graph: Option<&VerifiedGraphQuery>,
    graph_unavailable: Option<&PrimitiveUnavailableEvidenceV1>,
    request: ContextCallPathRequest,
    scope_prefix: Option<&str>,
    freshness_payload: Option<&CodeIndexFreshnessPayloadV1>,
    code_block_limit: Option<usize>,
) -> ContextCallPathProjection {
    let mut result = ContextCallPathResultV1 {
        from_node_id: request.from.as_str().to_owned(),
        to_node_id: request.to.as_str().to_owned(),
        max_depth: request.max_depth,
        code_generation: None,
        freshness: None,
        outcome: ContextCallPathOutcomeV1::Unavailable {
            evidence: graph_unavailable.cloned().unwrap_or_else(|| {
                evidence(
                    "context-call-path-graph-unavailable",
                    true,
                    "the requested call path has no admitted graph authority",
                )
            }),
        },
    };
    let Some(graph) = graph else {
        return ContextCallPathProjection {
            result,
            code: Vec::new(),
        };
    };
    if let Err(error) = ctx.verify_graph_scope(graph) {
        result.outcome = ContextCallPathOutcomeV1::Unavailable {
            evidence: unavailable_evidence(&error),
        };
        return ContextCallPathProjection {
            result,
            code: Vec::new(),
        };
    }
    result.code_generation = Some(graph.generation().as_str().to_owned());
    result.freshness = Some(graph_read_freshness(
        &ServedCodeGraphGenerationV1 {
            generation: graph.generation().as_str().to_owned(),
            freshness: graph.freshness(),
            worktree: None,
        },
        freshness_payload,
    ));
    let (outcome, symbols) = match path_from_graph(graph, &request, scope_prefix) {
        Ok(path) => path,
        Err(error) => {
            result.outcome = ContextCallPathOutcomeV1::Unavailable {
                evidence: unavailable_evidence(&error),
            };
            return ContextCallPathProjection {
                result,
                code: Vec::new(),
            };
        }
    };
    result.outcome = outcome;
    let code = if let Some(max_code_blocks) = code_block_limit
        && let ContextCallPathOutcomeV1::Found { nodes, .. } = &mut result.outcome
    {
        match path_source(graph, symbols, max_code_blocks).await {
            Ok((sources, code)) => {
                for (node, source) in nodes.iter_mut().zip(sources) {
                    node.source = source;
                }
                code
            }
            Err(error) => {
                let evidence = unavailable_evidence(&error);
                for node in nodes {
                    node.source = ContextPathSourceV1::Unavailable {
                        evidence: evidence.clone(),
                    };
                }
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    ContextCallPathProjection { result, code }
}

fn path_symbol(
    graph: &VerifiedGraphQuery,
    occurrence: &SymbolOccurrenceId,
    scope_prefix: Option<&str>,
) -> Result<CodeGraphSymbolSummaryV1> {
    let symbol = graph.symbol_summary(occurrence)?.ok_or_else(|| {
        TraceDecayError::project_route(
            "context-call-path-node-unavailable",
            false,
            format!(
                "call-path node '{}' is absent from the admitted graph generation",
                occurrence.as_str()
            ),
        )
    })?;
    let file = required_graph_file_path(&symbol)?;
    if !path_matches_scope(file, scope_prefix) {
        return Err(TraceDecayError::project_route(
            "context-call-path-out-of-scope",
            false,
            "the connecting call path contains a symbol outside the requested path scope",
        ));
    }
    Ok(symbol)
}

fn path_from_graph(
    graph: &VerifiedGraphQuery,
    request: &ContextCallPathRequest,
    scope_prefix: Option<&str>,
) -> Result<(ContextCallPathOutcomeV1, Vec<CodeGraphSymbolSummaryV1>)> {
    // The reader's zero-edge path does not itself establish that either
    // endpoint exists. Missing or out-of-scope IDs must remain unavailable.
    let from = path_symbol(graph, &request.from, scope_prefix)?;
    let to = path_symbol(graph, &request.to, scope_prefix)?;
    let searched = match graph.shortest_path(
        &request.from,
        &request.to,
        &[RelationEdgeKindV1::Calls],
        request.max_depth,
        GRAPH_RELATION_READ_LIMIT,
    ) {
        Ok(searched) => searched,
        Err(GraphQueryError::Read(CodeGraphReadError::BudgetExhausted { .. })) => {
            return Ok((
                ContextCallPathOutcomeV1::Truncated {
                    limit: ContextCallPathLimitV1::Relations,
                },
                Vec::new(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let Some(edges) = searched.path else {
        return Ok((
            if searched.complete {
                ContextCallPathOutcomeV1::NoPath
            } else {
                ContextCallPathOutcomeV1::Truncated {
                    limit: ContextCallPathLimitV1::Depth,
                }
            },
            Vec::new(),
        ));
    };
    let mut symbols = vec![from];
    for edge in &edges {
        let symbol = if edge.to_occurrence == request.to {
            to.clone()
        } else {
            path_symbol(graph, &edge.to_occurrence, scope_prefix)?
        };
        symbols.push(symbol);
    }
    let nodes = symbols
        .iter()
        .map(|symbol| {
            Ok(ContextCallPathNodeV1 {
                symbol: symbol_location(symbol)?,
                source: ContextPathSourceV1::NotRequested,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let weakest_authority = edges
        .iter()
        .map(|edge| edge.authority)
        .reduce(EdgeAuthorityV1::weakest);
    let edges = edges
        .into_iter()
        .map(|edge| ContextCallPathEdgeV1 {
            from_node_id: edge.from_occurrence.as_str().to_owned(),
            to_node_id: edge.to_occurrence.as_str().to_owned(),
            authority: edge.authority,
            evidence_span: edge.evidence_span,
        })
        .collect();
    Ok((
        ContextCallPathOutcomeV1::Found {
            nodes,
            edges,
            weakest_authority,
        },
        symbols,
    ))
}

struct PathSourceControl {
    cancelled: Arc<AtomicBool>,
    cancellation: CancellationSignal,
    request: RequestContext,
}

impl PathSourceControl {
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) || self.cancellation.is_cancelled() {
            return Err(source_interruption_error(RequestInterruption::Cancelled));
        }
        match self.request.admission_at(now_micros()) {
            RequestAdmission::Admitted => Ok(()),
            RequestAdmission::Cancelled => {
                Err(source_interruption_error(RequestInterruption::Cancelled))
            }
            RequestAdmission::TimedOut => Err(source_interruption_error(
                RequestInterruption::DeadlineExceeded,
            )),
            RequestAdmission::GrantExpired => Err(TraceDecayError::project_route(
                "context-call-path-source-denied",
                false,
                "call-path source read outlived its admitted capability grant",
            )),
        }
    }
}

fn source_interruption_error(interruption: RequestInterruption) -> TraceDecayError {
    match interruption {
        RequestInterruption::Cancelled => TraceDecayError::project_route(
            "context-call-path-source-cancelled",
            false,
            "call-path source read was cancelled",
        ),
        RequestInterruption::DeadlineExceeded => TraceDecayError::project_route(
            "context-call-path-source-timed-out",
            true,
            "call-path source read exceeded its deadline",
        ),
    }
}

/// A blocked filesystem read cannot keep the request alive. The same guard
/// also tells its worker to stop when the awaiting request is dropped.
async fn await_path_source<T>(
    control: Arc<PathSourceControl>,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    let _cancel_on_drop = SetFlagOnDrop::new(Arc::clone(&control.cancelled));
    control.check()?;
    let deadline = Deadline {
        expires_at: control
            .request
            .deadline()
            .expires_at
            .min(control.request.grant().expires_at),
    };
    let settled = run_deadline_signal_interruptible(&deadline, &control.cancellation, work).await;
    // A source worker may become ready in the same poll as cancellation or
    // expiry. Recheck admission before any bytes can leave this boundary.
    control.check()?;
    settled.map_err(source_interruption_error)?
}

async fn path_source(
    graph: &VerifiedGraphQuery,
    symbols: Vec<CodeGraphSymbolSummaryV1>,
    max_code_blocks: usize,
) -> Result<(Vec<ContextPathSourceV1>, Vec<ContextCodeBlockV1>)> {
    let project_root = graph.project_root()?.to_path_buf();
    let mut files = BTreeMap::new();
    for symbol in &symbols {
        let path = required_graph_file_path(symbol)?;
        if !files.contains_key(path) {
            files.insert(path.to_owned(), graph.file_by_logical_path(path)?);
        }
    }
    let control = Arc::new(PathSourceControl {
        cancelled: Arc::new(AtomicBool::new(false)),
        cancellation: graph.cancellation_signal().clone(),
        request: graph.request_context().clone(),
    });
    let worker_control = Arc::clone(&control);
    await_path_source(control, async move {
        tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
            read_path_source(
                &project_root,
                &symbols,
                &files,
                max_code_blocks,
                &worker_control,
            )
        })
        .await
        .map_err(|error| {
            TraceDecayError::project_route(
                "context-call-path-source-worker-unavailable",
                true,
                format!("call-path source worker failed: {error}"),
            )
        })?
    })
    .await
}

fn read_path_source(
    project_root: &Path,
    symbols: &[CodeGraphSymbolSummaryV1],
    files: &BTreeMap<String, Option<SanitizedCodeFileV1>>,
    max_code_blocks: usize,
    control: &PathSourceControl,
) -> Result<(Vec<ContextPathSourceV1>, Vec<ContextCodeBlockV1>)> {
    let mut sources = Vec::with_capacity(symbols.len());
    let mut blocks = Vec::new();
    let mut source_by_file =
        BTreeMap::<String, std::result::Result<String, PrimitiveUnavailableEvidenceV1>>::new();
    for symbol in symbols {
        control.check()?;
        let path = required_graph_file_path(symbol)?;
        let span = symbol
            .binding
            .as_ref()
            .and_then(|binding| binding.source_span);
        if span.is_some_and(|span| span.len() > MAX_CHUNK_TEXT_BYTES as u64) {
            sources.push(ContextPathSourceV1::Omitted {
                reason: ContextPathSourceOmissionV1::Oversized,
            });
            continue;
        }
        if blocks.len() == max_code_blocks {
            sources.push(ContextPathSourceV1::Omitted {
                reason: ContextPathSourceOmissionV1::CodeBlockLimit,
            });
            continue;
        }
        let source = source_by_file.entry(path.to_owned()).or_insert_with(|| {
            sealed_path_source(project_root, path, files.get(path).and_then(Option::as_ref))
                .map_err(|error| unavailable_evidence(&error))
        });
        let block = match source {
            Ok(source) => path_code_block(symbol, files.get(path).and_then(Option::as_ref), source)
                .map_err(|error| unavailable_evidence(&error)),
            Err(error) => Err(error.clone()),
        };
        match block {
            Ok(block) => {
                blocks.push(block);
                sources.push(ContextPathSourceV1::Included);
            }
            Err(evidence) => sources.push(ContextPathSourceV1::Unavailable { evidence }),
        }
    }
    control.check()?;
    Ok((sources, blocks))
}

fn sealed_path_source(
    project_root: &Path,
    path: &str,
    file: Option<&SanitizedCodeFileV1>,
) -> Result<String> {
    let file = file
        .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
        .ok_or_else(|| {
            graph_symbol_corrupt(format!(
                "call-path file '{path}' has no present indexed file record"
            ))
        })?;
    let language = file.language.as_ref().ok_or_else(|| {
        graph_symbol_corrupt(format!("call-path file '{path}' has no indexed language"))
    })?;
    let source = sealed_source_bytes(project_root, path, language, &file.content_digest)
        .map_err(|error| {
            TraceDecayError::project_route(
                "context-call-path-source-unavailable",
                true,
                format!("cannot read the indexed bytes of '{path}': {error}"),
            )
        })?
        .ok_or_else(|| {
            TraceDecayError::project_route(
                "context-call-path-source-stale",
                true,
                format!("'{path}' no longer holds the sanitized bytes the graph generation sealed"),
            )
        })?;
    String::from_utf8(source)
        .map_err(|_| graph_symbol_corrupt(format!("the indexed source of '{path}' is not UTF-8")))
}

fn path_code_block(
    symbol: &CodeGraphSymbolSummaryV1,
    file: Option<&SanitizedCodeFileV1>,
    source: &str,
) -> Result<ContextCodeBlockV1> {
    let invalid = || {
        graph_symbol_corrupt(format!(
            "call-path symbol '{}' has no whole declaration span in its indexed file",
            symbol.occurrence.as_str(),
        ))
    };
    let binding = symbol.binding.as_ref().ok_or_else(invalid)?;
    let file = file
        .filter(|file| file.file_occurrence_id == binding.file)
        .ok_or_else(invalid)?;
    let span = binding.source_span.ok_or_else(invalid)?;
    let start = usize::try_from(span.start_byte).map_err(|_| invalid())?;
    let end = usize::try_from(span.end_byte).map_err(|_| invalid())?;
    let window = source
        .get(start..end)
        .filter(|code| !code.trim().is_empty())
        .ok_or_else(invalid)?;
    // Published spans may absorb neighboring whitespace. Trim it without
    // changing the generation-attested bytes inside the declaration.
    let start = start + window.len() - window.trim_start().len();
    let code = window.trim();
    let metadata = required_graph_metadata(symbol)?;
    let start_line = line_number_at(source, start);
    let end_line = line_number_at(source, start + code.len() - 1);
    if start_line > metadata.start_line.saturating_add(1)
        || end_line < graph_symbol_end_line(metadata)?.saturating_add(1)
    {
        return Err(invalid());
    }
    Ok(ContextCodeBlockV1 {
        node_id: symbol.occurrence.as_str().to_owned(),
        file: file.logical_path.clone(),
        start_line,
        end_line,
        code: code.to_owned(),
    })
}

/// A source cut may reduce code, never the evidence joining the endpoints.
pub(super) fn mark_path_source_budget_omissions(
    path: &mut ContextCallPathResultV1,
    code: &[ContextCodeBlockV1],
) {
    let ContextCallPathOutcomeV1::Found { nodes, .. } = &mut path.outcome else {
        return;
    };
    let kept = code
        .iter()
        .map(|block| block.node_id.as_str())
        .collect::<HashSet<_>>();
    for node in nodes {
        if node.source == ContextPathSourceV1::Included
            && !kept.contains(node.symbol.node_id.as_str())
        {
            node.source = ContextPathSourceV1::Omitted {
                reason: ContextPathSourceOmissionV1::TokenBudget,
            };
        }
    }
}
