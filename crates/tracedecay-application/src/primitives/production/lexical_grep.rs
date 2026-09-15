//! Lexical grep and redundancy authorities over the project code graph.

use std::sync::Arc;

use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_code_index::grep_search::{
    GrepSearchQuery, search_tree_with_cancel as lexical_search_tree_with_cancel,
};
use tracedecay_contracts::retrieval::grep_analysis::{
    GrepAnalysisProblemV1, GrepHitV1, GrepRequestV1, GrepResultV1, LexicalGrepAuthorityV1,
    PrimitiveFutureV1, PrimitiveOutcomeV1, PrimitivePageV1, PrimitivePortContextV1,
    RedundancyAuthorityV1, RedundancyRequestV1, RedundancyResultV1,
};
use tracedecay_graph_query::{
    CodeGraphProjectionReadPort, SourceReadContext, request_graph_cancellation,
};

use super::super::support::{BoundedSourceSearch, run_bounded_source_search};
use super::{coverage, logical_file_symbols, open_code_graph, symbol_at_line};

pub struct TraceDecayLexicalGrepAuthorityV1 {
    source_runtime: Arc<SourceReadContext>,
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
}

impl TraceDecayLexicalGrepAuthorityV1 {
    pub fn new(
        source_runtime: Arc<SourceReadContext>,
        code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    ) -> Self {
        Self {
            source_runtime,
            code_graph,
        }
    }
}

impl LexicalGrepAuthorityV1 for TraceDecayLexicalGrepAuthorityV1 {
    fn grep<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a GrepRequestV1,
    ) -> PrimitiveFutureV1<'a, GrepResultV1> {
        Box::pin(hotpath::future!(
            async move {
                if request.window.cursor.is_some() {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        "compatibility cursor unsupported".to_owned(),
                    ));
                }
                let project_root = self.source_runtime.project_root().to_path_buf();
                let query = GrepSearchQuery {
                    pattern: request.pattern.clone(),
                    fixed_strings: request.fixed_strings,
                    case_sensitive: request.case_sensitive,
                    path_glob: request.path_glob.clone(),
                    context_lines: request.context_lines as usize,
                    max_results: request.window.limit as usize,
                };
                let scan = match run_bounded_source_search(
                    context.request.deadline(),
                    context.request.cancellation(),
                    move |cancelled| {
                        lexical_search_tree_with_cancel(&project_root, &query, || {
                            cancelled.load(std::sync::atomic::Ordering::Acquire)
                        })
                    },
                )
                .await
                {
                    BoundedSourceSearch::Completed(Ok(scan)) if !scan.cancelled => scan,
                    BoundedSourceSearch::Completed(Ok(_)) | BoundedSourceSearch::Cancelled => {
                        return PrimitiveOutcomeV1::Cancelled;
                    }
                    BoundedSourceSearch::TimedOut => return PrimitiveOutcomeV1::TimedOut,
                    BoundedSourceSearch::Completed(Err(error)) => {
                        return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                            error.to_string(),
                        ));
                    }
                    BoundedSourceSearch::WorkerFailed => {
                        return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                            "lexical grep worker failed".to_owned(),
                        ));
                    }
                };
                let files_scanned = scan.files_scanned;
                let truncated = scan.truncated;
                let graph_cancellation = request_graph_cancellation(context.request);
                let reader = match open_code_graph(
                    self.code_graph.as_ref(),
                    context.request,
                    context.observed_at,
                    Arc::clone(&graph_cancellation),
                )
                .await
                {
                    Ok(reader) => reader,
                    Err(_) => {
                        return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                            "lexical grep symbol projection unavailable".to_owned(),
                        ));
                    }
                };
                let mut matches = Vec::with_capacity(scan.hits.len());
                // Hits cluster within files, so the per-file symbol list is read
                // from the graph once and reused for every hit in that file.
                let mut symbols_by_file: std::collections::HashMap<
                    String,
                    Vec<CodeGraphSymbolSummaryV1>,
                > = std::collections::HashMap::new();
                // A graph read that fails cannot distinguish "this line is in no
                // symbol" from "the enclosing symbol could not be read", so the
                // page reports itself incomplete rather than attributing the hit
                // to nothing.
                let mut unread_enclosing_symbols = false;
                for hit in scan.hits {
                    if context.request.cancellation().is_cancelled() {
                        return PrimitiveOutcomeV1::Cancelled;
                    }
                    if !symbols_by_file.contains_key(&hit.file)
                        && let Ok(symbols) = logical_file_symbols(
                            &reader,
                            Arc::clone(&graph_cancellation),
                            &hit.file,
                        )
                    {
                        symbols_by_file.insert(hit.file.clone(), symbols);
                    }
                    let enclosing = match symbols_by_file
                        .get(&hit.file)
                        .ok_or(())
                        .and_then(|symbols| symbol_at_line(symbols, hit.line))
                    {
                        Ok(enclosing) => enclosing,
                        Err(()) => {
                            unread_enclosing_symbols = true;
                            None
                        }
                    };
                    matches.push(GrepHitV1 {
                        file: hit.file,
                        line: hit.line,
                        text: hit.text,
                        before: hit.before,
                        after: hit.after,
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
                let returned = matches.len() as u64;
                let incomplete = truncated || unread_enclosing_symbols;
                let page = PrimitivePageV1 {
                    payload: GrepResultV1 {
                        matches,
                        truncated,
                        files_scanned: files_scanned as u64,
                    },
                    coverage: coverage(files_scanned as u64, returned, incomplete),
                    continuation: None,
                    finished_at: context.observed_at,
                };
                if incomplete {
                    PrimitiveOutcomeV1::Partial(page)
                } else {
                    PrimitiveOutcomeV1::Completed(page)
                }
            },
            label = "usecases.primitives.grep"
        ))
    }
}

pub struct TraceDecayRedundancyAuthorityV1;

impl RedundancyAuthorityV1 for TraceDecayRedundancyAuthorityV1 {
    fn redundancy<'a>(
        &'a self,
        _context: &'a PrimitivePortContextV1<'a>,
        request: &'a RedundancyRequestV1,
    ) -> PrimitiveFutureV1<'a, RedundancyResultV1> {
        Box::pin(hotpath::future!(
            async move {
                if request.cursor.is_some() {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        "compatibility cursor unsupported".to_owned(),
                    ));
                }
                PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                    "the verified graph generation does not publish redundancy fingerprints"
                        .to_owned(),
                ))
            },
            label = "usecases.primitives.redundancy"
        ))
    }
}
