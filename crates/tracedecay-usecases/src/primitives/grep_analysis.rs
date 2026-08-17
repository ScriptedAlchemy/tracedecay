//! Production adapters for the existing single-root grep and analysis
//! authorities.
//!
//! Lexical grep and redundancy remain injected because their callers own
//! request composition and result rendering. Filesystem search is delegated
//! to the canonical code-index implementation.

use std::sync::Arc;

use tracedecay_application::CoverageCompleteness;
use tracedecay_application::retrieval::grep_analysis::{
    AstGrepAuthorityV1, AstGrepHitV1, AstGrepRequestV1, AstGrepResultV1, ComplexityAuthorityV1,
    ComplexityRequestV1, ComplexityResultV1, DependencyDepthAuthorityV1, DependencyDepthChainV1,
    DependencyDepthRequestV1, DependencyDepthResultV1, GrepAnalysisProblemV1, PrimitiveCoverageV1,
    PrimitiveFutureV1, PrimitiveOutcomeV1, PrimitivePageV1, PrimitivePortContextV1,
};

use crate::graph::health::{dependency_depth, depth_score};
use crate::graph::queries::GraphQueryManager;
use crate::tracedecay::SourceReadRuntime;
use tracedecay_code_index::ast_grep_search::search_tree_scoped_with_cancel;
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1,
};
use tracedecay_graph_db::GraphCancellation;
use tracedecay_runtime_core::redundancy::round4;

pub struct TraceDecayAstGrepAuthorityV1 {
    source_runtime: Arc<SourceReadRuntime>,
    code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
}

impl TraceDecayAstGrepAuthorityV1 {
    pub fn new(
        source_runtime: Arc<SourceReadRuntime>,
        code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
    ) -> Self {
        Self {
            source_runtime,
            code_graph,
        }
    }
}

impl AstGrepAuthorityV1 for TraceDecayAstGrepAuthorityV1 {
    fn ast_grep<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a AstGrepRequestV1,
    ) -> PrimitiveFutureV1<'a, AstGrepResultV1> {
        Box::pin(async move {
            if request.window.cursor.is_some() {
                return unsupported_compatibility_cursor();
            }
            let project_root = self.source_runtime.project_root().to_path_buf();
            let pattern = request.pattern.clone();
            let lang = request.lang.clone();
            let path_glob = request.path_glob.clone();
            let max_results = request.window.limit as usize;
            let scope_prefix = context.scope_prefix.map(str::to_owned);
            let search = match super::support::run_bounded_source_search(
                context.request.deadline(),
                context.request.cancellation(),
                move |cancelled| {
                    search_tree_scoped_with_cancel(
                        &project_root,
                        &pattern,
                        lang.as_deref(),
                        path_glob.as_deref(),
                        max_results,
                        scope_prefix.as_deref(),
                        || cancelled.load(std::sync::atomic::Ordering::Acquire),
                    )
                },
            )
            .await
            {
                super::support::BoundedSourceSearch::Completed(Ok(search)) if !search.cancelled => {
                    search
                }
                super::support::BoundedSourceSearch::Completed(Ok(_))
                | super::support::BoundedSourceSearch::Cancelled => {
                    return PrimitiveOutcomeV1::Cancelled;
                }
                super::support::BoundedSourceSearch::Completed(Err(error)) => {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        error.to_string(),
                    ));
                }
                super::support::BoundedSourceSearch::TimedOut => {
                    return PrimitiveOutcomeV1::TimedOut;
                }
                super::support::BoundedSourceSearch::WorkerFailed => {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        "AST grep worker failed".to_owned(),
                    ));
                }
            };

            if context.request.cancellation().is_cancelled() {
                return PrimitiveOutcomeV1::Cancelled;
            }

            let graph_cancellation = crate::graph::request_graph_cancellation(context.request);
            let verified = match self
                .code_graph
                .open(crate::graph::CodeGraphReadRequest::new(
                    context.request,
                    context.observed_at,
                    Arc::clone(&graph_cancellation),
                ))
                .await
            {
                Ok(verified) => verified,
                Err(error) => {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        error.to_string(),
                    ));
                }
            };
            let reader = match verified.reader_with_cancellation(
                context.request,
                context.observed_at,
                Arc::clone(&graph_cancellation),
            ) {
                Ok(reader) => reader,
                Err(error) => {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        error.to_string(),
                    ));
                }
            };
            let files_scanned = count(search.files_scanned);
            let truncated = search.truncated;
            let mut incomplete_symbol_evidence = false;
            let mut matches = Vec::with_capacity(search.matches.len());
            for item in search.matches {
                if context.request.cancellation().is_cancelled() {
                    return PrimitiveOutcomeV1::Cancelled;
                }
                let enclosing = match symbol_at_location(
                    &reader,
                    Arc::clone(&graph_cancellation),
                    &item.file,
                    item.line,
                ) {
                    Ok(enclosing) => enclosing,
                    Err(()) => {
                        incomplete_symbol_evidence = true;
                        None
                    }
                };
                matches.push(AstGrepHitV1 {
                    file: item.file,
                    line: item.line,
                    column: item.column,
                    lang: item.lang,
                    matched_text: item.matched_text,
                    line_text: item.line_text,
                    symbol: enclosing
                        .as_ref()
                        .and_then(|node| node.metadata.as_ref())
                        .map(|metadata| metadata.simple_name.clone()),
                    node_id: enclosing
                        .as_ref()
                        .map(|node| node.occurrence.as_str().to_owned()),
                    kind: enclosing
                        .as_ref()
                        .and_then(|node| node.metadata.as_ref())
                        .map(|metadata| metadata.kind.clone()),
                });
            }

            let returned = count(matches.len());
            let partial = truncated || incomplete_symbol_evidence;
            let page = PrimitivePageV1 {
                payload: AstGrepResultV1 {
                    matches,
                    truncated,
                    files_scanned,
                },
                coverage: coverage(files_scanned, returned, partial),
                continuation: None,
                finished_at: context.observed_at,
            };
            if partial {
                PrimitiveOutcomeV1::Partial(page)
            } else {
                PrimitiveOutcomeV1::Completed(page)
            }
        })
    }
}

pub struct TraceDecayComplexityAuthorityV1 {
    code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
}

impl TraceDecayComplexityAuthorityV1 {
    pub fn new(code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>) -> Self {
        Self { code_graph }
    }
}

impl ComplexityAuthorityV1 for TraceDecayComplexityAuthorityV1 {
    fn complexity<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a ComplexityRequestV1,
    ) -> PrimitiveFutureV1<'a, ComplexityResultV1> {
        Box::pin(async move {
            if request.window.cursor.is_some() {
                return unsupported_compatibility_cursor();
            }
            let path = match effective_scoped_path(request.path.as_deref(), context.scope_prefix) {
                Ok(path) => path,
                Err(problem) => return PrimitiveOutcomeV1::Failed(problem),
            };
            let _ = (&self.code_graph, path);
            PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                "the verified graph generation does not publish the full complexity metric contract"
                    .to_owned(),
            ))
        })
    }
}

pub struct TraceDecayDependencyDepthAuthorityV1 {
    code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
}

impl TraceDecayDependencyDepthAuthorityV1 {
    pub fn new(code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>) -> Self {
        Self { code_graph }
    }
}

impl DependencyDepthAuthorityV1 for TraceDecayDependencyDepthAuthorityV1 {
    fn dependency_depth<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a DependencyDepthRequestV1,
    ) -> PrimitiveFutureV1<'a, DependencyDepthResultV1> {
        Box::pin(async move {
            if request.window.cursor.is_some() {
                return unsupported_compatibility_cursor();
            }
            let path = match effective_scoped_path(request.path.as_deref(), context.scope_prefix) {
                Ok(path) => path,
                Err(problem) => return PrimitiveOutcomeV1::Failed(problem),
            };
            let cancellation = crate::graph::request_graph_cancellation(context.request);
            let verified = match self
                .code_graph
                .open(crate::graph::CodeGraphReadRequest::new(
                    context.request,
                    context.observed_at,
                    Arc::clone(&cancellation),
                ))
                .await
            {
                Ok(read) => read,
                Err(error) => {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        error.to_string(),
                    ));
                }
            };
            let reader = match verified.reader_with_cancellation(
                context.request,
                context.observed_at,
                Arc::clone(&cancellation),
            ) {
                Ok(reader) => reader,
                Err(error) => {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        error.to_string(),
                    ));
                }
            };
            let adjacency = match GraphQueryManager::new(&reader, cancellation)
                .build_file_adjacency(path.as_deref())
                .await
            {
                Ok(adjacency) => adjacency,
                Err(error) => {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        error.to_string(),
                    ));
                }
            };
            if context.request.cancellation().is_cancelled() {
                return PrimitiveOutcomeV1::Cancelled;
            }

            let result = dependency_depth(&adjacency, request.window.limit as usize);
            let chains = result
                .chains
                .into_iter()
                .map(|chain| DependencyDepthChainV1 {
                    file: chain.file,
                    depth: count(chain.depth),
                    chain: chain.chain,
                })
                .collect::<Vec<_>>();
            let returned = count(chains.len());
            PrimitiveOutcomeV1::Completed(PrimitivePageV1 {
                payload: DependencyDepthResultV1 {
                    max_depth: count(result.max_depth),
                    ideal_depth: count(result.ideal_depth),
                    depth_score: round4(depth_score(result.max_depth, result.ideal_depth)),
                    chains,
                },
                coverage: coverage(count(adjacency.len()), returned, false),
                continuation: None,
                finished_at: context.observed_at,
            })
        })
    }
}

fn effective_scoped_path(
    requested: Option<&str>,
    authorized: Option<&str>,
) -> Result<Option<String>, GrepAnalysisProblemV1> {
    match (requested, authorized) {
        (None, None) => Ok(None),
        (Some(path), None) | (None, Some(path)) => Ok(Some(path.to_owned())),
        (Some(path), Some(scope)) if path == scope || path.starts_with(&format!("{scope}/")) => {
            Ok(Some(path.to_owned()))
        }
        (Some(_), Some(_)) => Err(GrepAnalysisProblemV1::Denied),
    }
}

fn symbol_at_location(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn GraphCancellation>,
    file: &str,
    line_1based: u32,
) -> Result<Option<CodeGraphSymbolSummaryV1>, ()> {
    let line = line_1based.checked_sub(1).ok_or(())?;
    let symbols = graph
        .symbols_in_logical_file(file, 100_000, cancellation)
        .map_err(|_| ())?;
    let mut enclosing = symbols
        .into_iter()
        .filter(|symbol| {
            symbol.metadata.as_ref().is_some_and(|metadata| {
                metadata.line_span > 0
                    && metadata.start_line <= line
                    && metadata
                        .start_line
                        .checked_add(metadata.line_span)
                        .is_some_and(|end_exclusive| line < end_exclusive)
            })
        })
        .collect::<Vec<_>>();
    enclosing.sort_by(|left, right| {
        let left = left.metadata.as_ref();
        let right = right.metadata.as_ref();
        left.map(|metadata| metadata.line_span)
            .cmp(&right.map(|metadata| metadata.line_span))
            .then(
                left.map(|metadata| metadata.start_line)
                    .cmp(&right.map(|metadata| metadata.start_line)),
            )
            .then(
                left.map(|metadata| &metadata.occurrence)
                    .cmp(&right.map(|metadata| &metadata.occurrence)),
            )
    });
    Ok(enclosing.into_iter().next())
}

fn coverage(visited: u64, returned: u64, partial: bool) -> PrimitiveCoverageV1 {
    PrimitiveCoverageV1 {
        completeness: if partial {
            CoverageCompleteness::Partial
        } else {
            CoverageCompleteness::Complete
        },
        visited: Some(visited),
        eligible: None,
        returned,
        unsupported_languages: Vec::new(),
    }
}

fn unsupported_compatibility_cursor<T>() -> PrimitiveOutcomeV1<T> {
    PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::InvalidRequest(
        "this compatibility primitive does not define a resumable cursor".to_owned(),
    ))
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

