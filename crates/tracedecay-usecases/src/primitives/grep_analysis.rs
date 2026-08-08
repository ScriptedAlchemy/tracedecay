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
    ComplexityItemV1, ComplexityRequestV1, ComplexityResultV1, DependencyDepthAuthorityV1,
    DependencyDepthChainV1, DependencyDepthRequestV1, DependencyDepthResultV1,
    GrepAnalysisProblemV1, PrimitiveCoverageV1, PrimitiveFutureV1, PrimitiveOutcomeV1,
    PrimitivePageV1, PrimitivePortContextV1,
};

use crate::graph::health::{dependency_depth, depth_score};
use crate::graph::queries::GraphQueryManager;
use crate::tracedecay::TraceDecay;
use tracedecay_code_index::ast_grep_search::search_tree_scoped_with_cancel;
use tracedecay_runtime_core::types::NodeKind;

macro_rules! graph_authority {
    ($name:ident) => {
        pub struct $name {
            graph: Arc<TraceDecay>,
        }

        impl $name {
            pub fn new(graph: Arc<TraceDecay>) -> Self {
                Self { graph }
            }
        }
    };
}

graph_authority!(TraceDecayAstGrepAuthorityV1);

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
            let project_root = self.graph.project_root().to_path_buf();
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

            let files_scanned = count(search.files_scanned);
            let truncated = search.truncated;
            let mut matches = Vec::with_capacity(search.matches.len());
            for item in search.matches {
                if context.request.cancellation().is_cancelled() {
                    return PrimitiveOutcomeV1::Cancelled;
                }
                let enclosing = self
                    .graph
                    .node_at_location(&item.file, item.line)
                    .await
                    .ok()
                    .flatten();
                matches.push(AstGrepHitV1 {
                    file: item.file,
                    line: item.line,
                    column: item.column,
                    lang: item.lang,
                    matched_text: item.matched_text,
                    line_text: item.line_text,
                    symbol: enclosing.as_ref().map(|node| node.name.clone()),
                    node_id: enclosing.as_ref().map(|node| node.id.clone()),
                    kind: enclosing.as_ref().map(|node| node.kind.as_str().to_owned()),
                });
            }

            let returned = count(matches.len());
            let page = PrimitivePageV1 {
                payload: AstGrepResultV1 {
                    matches,
                    truncated,
                    files_scanned,
                },
                coverage: coverage(files_scanned, returned, truncated),
                continuation: None,
                finished_at: context.observed_at,
            };
            if truncated {
                PrimitiveOutcomeV1::Partial(page)
            } else {
                PrimitiveOutcomeV1::Completed(page)
            }
        })
    }
}

graph_authority!(TraceDecayComplexityAuthorityV1);

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
            let node_kind = request.node_kind.as_deref().and_then(NodeKind::from_str);
            let rows = match self
                .graph
                .get_complexity_ranked(
                    node_kind.as_ref(),
                    path.as_deref(),
                    request.window.limit as usize,
                )
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        error.to_string(),
                    ));
                }
            };
            if context.request.cancellation().is_cancelled() {
                return PrimitiveOutcomeV1::Cancelled;
            }

            let ranking = rows
                .into_iter()
                .map(|(node, lines, fan_out, fan_in, score)| ComplexityItemV1 {
                    id: node.id,
                    name: node.name,
                    kind: node.kind.as_str().to_owned(),
                    file: node.file_path,
                    line: node.start_line,
                    lines,
                    cyclomatic_complexity: node.branches.saturating_add(1),
                    branches: node.branches,
                    loops: node.loops,
                    returns: node.returns,
                    max_nesting: node.max_nesting,
                    unsafe_blocks: node.unsafe_blocks,
                    unchecked_calls: node.unchecked_calls,
                    assertions: node.assertions,
                    fan_out,
                    fan_in,
                    score,
                })
                .collect::<Vec<_>>();
            let returned = count(ranking.len());
            PrimitiveOutcomeV1::Completed(PrimitivePageV1 {
                payload: ComplexityResultV1 {
                    formula: "lines + (fan_out × 3) + fan_in".to_owned(),
                    note:
                        "cyclomatic_complexity = branches + 1 (computed from AST during extraction)"
                            .to_owned(),
                    result_count: returned,
                    ranking,
                },
                coverage: coverage(returned, returned, false),
                continuation: None,
                finished_at: context.observed_at,
            })
        })
    }
}

graph_authority!(TraceDecayDependencyDepthAuthorityV1);

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
            let adjacency = match GraphQueryManager::new(self.graph.db())
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

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}
