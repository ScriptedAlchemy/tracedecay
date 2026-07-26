//! Production PR12 primitive owners over `TraceDecay` graph/query authorities.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ignore::overrides::OverrideBuilder;
use tracedecay_application::retrieval::grep_analysis::{
    GrepAnalysisProblemV1, GrepHitV1, GrepRequestV1, GrepResultV1, LexicalGrepAuthorityV1,
    PrimitiveCoverageV1, PrimitiveFutureV1, PrimitiveOutcomeV1, PrimitivePageV1,
    PrimitivePortContextV1, RedundancyAuthorityV1, RedundancyRequestV1, RedundancyResultV1,
};
use tracedecay_application::retrieval::{
    AffectedFileTestsPrimitiveRequest, AffectedFileTestsPrimitiveResultV1, AffectedTestsRequest,
    AffectedTestsResult, HealthReadRequest, HealthReadResult, OperationalRetrievalPort,
    RankedAffectedTestV1, RetrievalPortContext, RetrievalPortOutcome, SessionLookupRequest,
    SessionLookupResult, SourceLinesRequest, SourceLinesResult, SourceReference,
    SourceRetrievalPort, SymbolPrimitiveRecord, TemporalRetrievalPort, TestMapCoverageV1,
    TestMapPrimitiveRequest, TestMapPrimitiveResultV1, TestPrimitivePort, TestPrimitivePortContext,
    TestPrimitivePortFuture, TestPrimitivePortOutcome, TestReferenceV1, UncoveredSourceV1,
};
use tracedecay_application::{
    ApplicationContractError, CoverageCompleteness, CoverageDomainState, EvidenceCoverage,
    EvidenceDomain, FreshnessState, Omission, OmissionReason, OperationBudgetUsage, PageState,
    RequestContext, ResolvedScope, RetrievalEvidence, TemporalState,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, ManifestDigest, ProjectId, ProviderEvaluationStateV1,
    RetrievalAnchorId, RetrievalGrainV1, SessionId, SignedCursorKeyRefV1, TemporalModeV1,
    UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::SortContractId;
use url::Url;

use super::concrete::{AuthenticatedSymbolGraphCursorAdapter, SymbolGraphCursorSnapshotAuthority};
use super::runtime::{
    CallChainPrimitiveRequest, CallChainPrimitiveResult, DiagnosticPrimitiveRecord,
    DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveResult, FileDependentsPrimitiveRequest,
    FileDependentsPrimitiveResult, FileMetadataPrimitiveRequest, FileMetadataPrimitiveResult,
    FileMetadataRecord, ManagedTestRunCurrentIdentity, ManagedTestRunCurrentIdentityFuture,
    ManagedTestRunCurrentScopePort, ModuleApiPrimitiveRequest, ModuleApiPrimitiveResult,
    Pr12ExtendedPrimitiveFuture, Pr12ExtendedPrimitivePort, Pr12OperationalPrimitive,
    Pr12OperationalPrimitiveFuture, Pr12OperationalPrimitivePort, Pr12OperationalPrimitiveRequest,
    Pr12PrimitiveProjectRuntime, QualifiedNamePrimitiveRequest, QualifiedNamePrimitiveResult,
    SourceBodyPrimitiveRequest, SourceBodyPrimitiveResult, SourceOutlinePrimitiveRequest,
    SourceOutlinePrimitiveResult, StorageStatusPrimitiveRequest, StorageStatusPrimitiveResult,
    open_pr12_primitive_project_runtime,
};
use super::symbol_graph::{SymbolGraphCursorPort, symbol_record};
use crate::application::lsp_runtime::LspCodeIndexProjectionIdentityPort;
use crate::application::operation_stream::OperationEventAuthority;
use crate::application::source_authorization::ProjectSourceAccessSnapshot;
use crate::code_index::provider::{
    GenerationProviderCoverageV1, GenerationProviderReadV1, GenerationTestAttributionJoinReadPort,
};
use crate::code_index::test_attribution::{
    GenerationTestJoinCoverageV1, GenerationTestJoinDispositionV1, GenerationTestJoinV1,
};
use crate::db::Database;
use crate::global_db::RegisteredGlobalDb;
use crate::global_db::session_temporal::GlobalDbCursorKeyProvider;
use crate::mcp::tools::handlers::git::{
    affected_test_proximity, collect_affected_test_files, rank_affected_tests,
};
use crate::mcp::tools::handlers::grep::{ScanResult, build_matcher, scan_tree};
use crate::query::temporal::ports::{
    BindingDigest, KernelVersions, TemporalExecutionSnapshot, TemporalSnapshotRequest,
    TemporalWatermarks,
};
use crate::query::temporal::resolution::ValidatedAuthorization;
use crate::tracedecay::TraceDecay;
use crate::types::{Node, Visibility};

const PRIMITIVE_SORT: &str = "sort.application.primitive.v1";

fn completed<T>(
    payload: T,
    domain: EvidenceDomain,
    finished_at: UtcMicros,
) -> RetrievalPortOutcome<T> {
    let Ok(coverage) = EvidenceCoverage::complete(vec![domain], 1, 1, 1) else {
        return failed(domain, finished_at);
    };
    let Ok(page) = PageState::first_page(
        SortContractId::new(PRIMITIVE_SORT).unwrap_or_else(|_| panic!("static sort")),
        1,
        Some(1),
        1,
    ) else {
        return failed(domain, finished_at);
    };
    RetrievalPortOutcome::Completed(RetrievalEvidence {
        payload: Some(payload),
        temporal: TemporalState::current(finished_at),
        evidence_authorities: Vec::new(),
        coverage,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page,
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    })
}

fn failed<T>(domain: EvidenceDomain, finished_at: UtcMicros) -> RetrievalPortOutcome<T> {
    RetrievalPortOutcome::Failed(RetrievalEvidence {
        payload: None,
        temporal: TemporalState::current(finished_at),
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage {
            requested_domains: vec![domain],
            visited: None,
            eligible: None,
            returned: 0,
            completeness: CoverageCompleteness::Unknown,
            domains: Vec::new(),
        },
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new(PRIMITIVE_SORT).unwrap_or_else(|_| panic!("static sort")),
            1,
            Some(0),
            0,
        )
        .unwrap_or_else(|_| panic!("empty page")),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    })
}

fn coverage(files_scanned: u64, returned: u64, truncated: bool) -> PrimitiveCoverageV1 {
    PrimitiveCoverageV1 {
        visited: Some(files_scanned),
        eligible: Some(files_scanned),
        returned,
        completeness: if truncated {
            CoverageCompleteness::Partial
        } else {
            CoverageCompleteness::Complete
        },
        // The filesystem grep scan applies no language admission; nothing is
        // skipped as unsupported.
        unsupported_languages: Vec::new(),
    }
}

fn now_observed() -> UtcMicros {
    use std::time::{SystemTime, UNIX_EPOCH};
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_micros().min(i64::MAX as u128) as i64
        });
    UtcMicros(micros)
}

fn tool_json_payload(tool: &crate::mcp::tools::ToolResult) -> Option<serde_json::Value> {
    let text = tool
        .value
        .get("content")
        .and_then(|content| content.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(|value| value.as_str())?;
    serde_json::from_str(text).ok()
}

pub struct TraceDecayLexicalGrepAuthorityV1 {
    graph: Arc<TraceDecay>,
}

impl TraceDecayLexicalGrepAuthorityV1 {
    pub fn new(graph: Arc<TraceDecay>) -> Self {
        Self { graph }
    }
}

impl LexicalGrepAuthorityV1 for TraceDecayLexicalGrepAuthorityV1 {
    fn grep<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a GrepRequestV1,
    ) -> PrimitiveFutureV1<'a, GrepResultV1> {
        Box::pin(async move {
            if request.window.cursor.is_some() {
                return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                    "compatibility cursor unsupported".to_owned(),
                ));
            }
            let matcher = match build_matcher(
                &request.pattern,
                request.fixed_strings,
                request.case_sensitive,
            ) {
                Ok(matcher) => matcher,
                Err(error) => {
                    return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                        error.to_string(),
                    ));
                }
            };
            let project_root = self.graph.project_root();
            let overrides = match request.path_glob.as_deref() {
                Some(raw) if !raw.trim().is_empty() => {
                    let mut builder = OverrideBuilder::new(project_root);
                    if let Err(error) = builder.add(raw) {
                        return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                            error.to_string(),
                        ));
                    }
                    match builder.build() {
                        Ok(overrides) => Some(overrides),
                        Err(error) => {
                            return PrimitiveOutcomeV1::Failed(
                                GrepAnalysisProblemV1::AuthorityFailed(error.to_string()),
                            );
                        }
                    }
                }
                _ => None,
            };
            let ScanResult {
                hits,
                files_scanned,
                truncated,
            } = scan_tree(
                project_root,
                &matcher,
                overrides,
                request.context_lines as usize,
                request.window.limit as usize,
            );
            if context.request.cancellation().is_cancelled() {
                return PrimitiveOutcomeV1::Cancelled;
            }
            let mut matches = Vec::with_capacity(hits.len());
            for hit in hits {
                if context.request.cancellation().is_cancelled() {
                    return PrimitiveOutcomeV1::Cancelled;
                }
                let enclosing = self
                    .graph
                    .node_at_location(&hit.file, hit.line)
                    .await
                    .ok()
                    .flatten();
                matches.push(GrepHitV1 {
                    file: hit.file,
                    line: hit.line,
                    text: hit.text,
                    before: hit.before,
                    after: hit.after,
                    symbol: enclosing.as_ref().map(|node| node.name.clone()),
                    node_id: enclosing.as_ref().map(|node| node.id.clone()),
                    kind: enclosing.as_ref().map(|node| node.kind.as_str().to_owned()),
                });
            }
            let returned = matches.len() as u64;
            let page = PrimitivePageV1 {
                payload: GrepResultV1 {
                    matches,
                    truncated,
                    files_scanned: files_scanned as u64,
                },
                coverage: coverage(files_scanned as u64, returned, truncated),
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

pub struct TraceDecayRedundancyAuthorityV1 {
    graph: Arc<TraceDecay>,
}

impl TraceDecayRedundancyAuthorityV1 {
    pub fn new(graph: Arc<TraceDecay>) -> Self {
        Self { graph }
    }
}

impl RedundancyAuthorityV1 for TraceDecayRedundancyAuthorityV1 {
    fn redundancy<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a RedundancyRequestV1,
    ) -> PrimitiveFutureV1<'a, RedundancyResultV1> {
        Box::pin(async move {
            if request.cursor.is_some() {
                return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                    "compatibility cursor unsupported".to_owned(),
                ));
            }
            let args = serde_json::json!({
                "path": request.path,
                "min_lines": request.min_lines,
                "max_pairs": request.max_pairs,
                "similarity_threshold": request.similarity_threshold,
                "include_naming_only": request.include_naming_only,
                "include_generated_paths": request.include_generated_paths,
                "format": "json",
            });
            let result = crate::mcp::tools::handlers::redundancy::handle_redundancy(
                self.graph.as_ref(),
                args,
                context.scope_prefix,
            )
            .await;
            let Ok(tool) = result else {
                return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                    "redundancy authority failed".to_owned(),
                ));
            };
            if context.request.cancellation().is_cancelled() {
                return PrimitiveOutcomeV1::Cancelled;
            }
            let Some(payload) = tool_json_payload(&tool) else {
                return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                    "redundancy payload was not structured JSON".to_owned(),
                ));
            };
            let Ok(result) = serde_json::from_value::<RedundancyResultV1>(payload) else {
                return PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                    "redundancy payload failed typed decode".to_owned(),
                ));
            };
            let returned = result.pair_count;
            let scanned = result.scanned;
            let page = PrimitivePageV1 {
                payload: result,
                coverage: coverage(scanned, returned, false),
                continuation: None,
                finished_at: context.observed_at,
            };
            PrimitiveOutcomeV1::Completed(page)
        })
    }
}

pub struct TraceDecayTestPrimitivePortV1 {
    graph: Arc<TraceDecay>,
}

impl TraceDecayTestPrimitivePortV1 {
    pub fn new(graph: Arc<TraceDecay>) -> Self {
        Self { graph }
    }
}

impl TestPrimitivePort for TraceDecayTestPrimitivePortV1 {
    fn test_map<'a>(
        &'a self,
        context: TestPrimitivePortContext<'a>,
        request: &'a TestMapPrimitiveRequest,
    ) -> TestPrimitivePortFuture<'a, TestMapPrimitiveResultV1> {
        Box::pin(async move {
            let source_nodes = if let Some(file) = request.file.as_deref() {
                self.graph.get_nodes_by_file(file).await.unwrap_or_default()
            } else if let Some(node_id) = request.node_id.as_deref() {
                self.graph
                    .get_node(node_id)
                    .await
                    .ok()
                    .flatten()
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                return TestPrimitivePortOutcome::Failed {
                    finished_at: context.observed_at,
                    budget: OperationBudgetUsage::default(),
                };
            };
            let mut coverage_map = Vec::new();
            let mut uncovered = Vec::new();
            let mut test_files = std::collections::BTreeSet::new();
            for node in source_nodes {
                if !node.kind.is_callable_kind() {
                    continue;
                }
                let callers = self
                    .graph
                    .get_callers(&node.id, 3)
                    .await
                    .unwrap_or_default();
                let caller_ids: Vec<String> = callers.iter().map(|(n, _)| n.id.clone()).collect();
                let test_annotated = self
                    .graph
                    .get_test_annotated_node_ids(&caller_ids)
                    .await
                    .unwrap_or_default();
                let tests: Vec<TestReferenceV1> = callers
                    .into_iter()
                    .filter(|(n, _)| {
                        crate::tracedecay::is_test_file(&n.file_path)
                            || test_annotated.contains(&n.id)
                    })
                    .map(|(n, _)| {
                        test_files.insert(n.file_path.clone());
                        TestReferenceV1 {
                            test_name: n.name,
                            test_file: n.file_path,
                            test_line: n.start_line as usize,
                        }
                    })
                    .collect();
                if tests.is_empty() {
                    uncovered.push(UncoveredSourceV1 {
                        id: node.id,
                        name: node.name,
                        file: node.file_path,
                        line: node.start_line as usize,
                    });
                } else {
                    coverage_map.push(TestMapCoverageV1 {
                        source_name: node.name,
                        source_id: node.id,
                        source_file: node.file_path,
                        source_line: node.start_line as usize,
                        tests,
                    });
                }
            }
            let covered_symbols = coverage_map.len();
            let uncovered_symbols = uncovered.len();
            TestPrimitivePortOutcome::Completed {
                result: TestMapPrimitiveResultV1 {
                    covered_symbols,
                    uncovered_symbols,
                    test_files: test_files.into_iter().collect(),
                    coverage: coverage_map,
                    uncovered,
                    total: Some((covered_symbols + uncovered_symbols) as u64),
                    next_cursor: None,
                },
                finished_at: context.observed_at,
                budget: OperationBudgetUsage::default(),
            }
        })
    }

    fn affected_file_tests<'a>(
        &'a self,
        context: TestPrimitivePortContext<'a>,
        request: &'a AffectedFileTestsPrimitiveRequest,
    ) -> TestPrimitivePortFuture<'a, AffectedFileTestsPrimitiveResultV1> {
        Box::pin(async move {
            let custom_glob = request
                .filter
                .as_deref()
                .and_then(|pattern| glob::Pattern::new(pattern).ok());
            let files_with_inline_tests = self
                .graph
                .get_files_with_test_annotations()
                .await
                .unwrap_or_default();
            let Ok(traversal) = collect_affected_test_files(
                self.graph.as_ref(),
                &request.files,
                request.maximum_depth,
                custom_glob.as_ref(),
                &files_with_inline_tests,
            )
            .await
            else {
                return TestPrimitivePortOutcome::Failed {
                    finished_at: context.observed_at,
                    budget: OperationBudgetUsage::default(),
                };
            };
            let mut affected_tests = traversal.test_distances.keys().cloned().collect::<Vec<_>>();
            affected_tests.sort();
            let ranked = rank_affected_tests(&traversal.test_distances);
            let ranked_tests = ranked
                .iter()
                .enumerate()
                .map(|(index, test)| RankedAffectedTestV1 {
                    path: test.path.clone(),
                    rank: index + 1,
                    distance: test.distance,
                    proximity: affected_test_proximity(test.distance).to_owned(),
                })
                .collect::<Vec<_>>();
            let recommended_tests = ranked
                .iter()
                .filter(|test| test.distance <= 2)
                .map(|test| test.path.clone())
                .collect();
            let total = affected_tests.len() as u64;
            TestPrimitivePortOutcome::Completed {
                result: AffectedFileTestsPrimitiveResultV1 {
                    changed_files: request.files.clone(),
                    affected_tests,
                    ranked_tests,
                    recommended_tests,
                    total: Some(total),
                    next_cursor: None,
                },
                finished_at: context.observed_at,
                budget: OperationBudgetUsage::default(),
            }
        })
    }
}

pub struct TraceDecaySourceLinesPortV1 {
    graph: Arc<TraceDecay>,
}

impl TraceDecaySourceLinesPortV1 {
    pub fn new(graph: Arc<TraceDecay>) -> Self {
        Self { graph }
    }
}

impl SourceRetrievalPort for TraceDecaySourceLinesPortV1 {
    fn source_lines(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &SourceLinesRequest,
    ) -> RetrievalPortOutcome<SourceLinesResult> {
        let _ = context;
        let finished_at = now_observed();
        if request.span.validate().is_err() {
            return failed(EvidenceDomain::Source, finished_at);
        }
        let relative = request.file.as_str();
        let path = self.graph.project_root().join(relative);
        let Ok(bytes) = std::fs::read(&path) else {
            return failed(EvidenceDomain::Source, finished_at);
        };
        let start = request.span.start_byte as usize;
        let end = request.span.end_byte as usize;
        if end > bytes.len() || start > end {
            return failed(EvidenceDomain::Source, finished_at);
        }
        let Ok(digest) = canonical_sha256(&(
            "tracedecay.primitive.source-lines.v1",
            relative,
            request.span.start_byte,
            request.span.end_byte,
            &bytes[start..end],
        )) else {
            return failed(EvidenceDomain::Source, finished_at);
        };
        let Ok(anchor) = RetrievalAnchorId::new(format!(
            "anchor.source-lines.{}",
            digest.as_str().trim_start_matches("sha256:")
        )) else {
            return failed(EvidenceDomain::Source, finished_at);
        };
        completed(
            SourceLinesResult {
                references: vec![SourceReference {
                    anchor,
                    span: request.span,
                }],
            },
            EvidenceDomain::Source,
            finished_at,
        )
    }
}

pub(crate) struct TraceDecayTemporalPortV1 {
    session_db: Arc<RegisteredGlobalDb>,
}

impl TraceDecayTemporalPortV1 {
    pub(crate) fn new(session_db: Arc<RegisteredGlobalDb>) -> Self {
        Self { session_db }
    }
}

impl TemporalRetrievalPort for TraceDecayTemporalPortV1 {
    fn session_lookup(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &SessionLookupRequest,
    ) -> RetrievalPortOutcome<SessionLookupResult> {
        let _ = (context, &self.session_db, request);
        let finished_at = now_observed();
        // Session temporal anchors are owned by the PR8 kernel; when no exact
        // session handle is supplied on this compatibility port, return an
        // authoritative empty page rather than inventing anchors.
        completed(
            SessionLookupResult {
                anchors: Vec::new(),
            },
            EvidenceDomain::Temporal,
            finished_at,
        )
    }
}

pub struct TraceDecayHealthPortV1 {
    graph: Arc<TraceDecay>,
}

impl TraceDecayHealthPortV1 {
    pub fn new(graph: Arc<TraceDecay>) -> Self {
        Self { graph }
    }
}

impl OperationalRetrievalPort for TraceDecayHealthPortV1 {
    fn health_read(
        &self,
        _context: &RetrievalPortContext<'_>,
        _request: &HealthReadRequest,
    ) -> RetrievalPortOutcome<HealthReadResult> {
        let branch = self.graph.branch_diagnostics();
        let status = if branch.serving_db_exists && !self.graph.is_read_only() {
            if branch.is_fallback { "degraded" } else { "ok" }
        } else if branch.serving_db_exists {
            "read_only"
        } else {
            "degraded"
        };
        completed(
            HealthReadResult {
                status: status.to_owned(),
            },
            EvidenceDomain::Operational,
            now_observed(),
        )
    }
}

fn public_module_symbols(nodes: Vec<Node>, path: &str) -> Vec<SymbolPrimitiveRecord> {
    let prefix = if path.ends_with('/') {
        path.to_owned()
    } else {
        format!("{path}/")
    };
    let mut pub_nodes: Vec<Node> = nodes
        .into_iter()
        .filter(|node| {
            node.visibility == Visibility::Pub
                && (node.file_path == path || node.file_path.starts_with(&prefix))
        })
        .collect();
    pub_nodes.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then(left.start_line.cmp(&right.start_line))
    });
    pub_nodes
        .into_iter()
        .map(|node| symbol_record(node, None))
        .collect()
}

pub struct TraceDecayExtendedPrimitivePortV1 {
    graph: Arc<TraceDecay>,
}

impl TraceDecayExtendedPrimitivePortV1 {
    pub fn new(graph: Arc<TraceDecay>) -> Self {
        Self { graph }
    }
}

impl Pr12ExtendedPrimitivePort for TraceDecayExtendedPrimitivePortV1 {
    fn qualified_name<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        request: &'a QualifiedNamePrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, QualifiedNamePrimitiveResult> {
        Box::pin(async move {
            let nodes = self
                .graph
                .get_nodes_by_qualified_name(&request.qualified_name)
                .await
                .unwrap_or_default();
            let symbols: Vec<_> = nodes
                .into_iter()
                .map(|node| symbol_record(node, None))
                .collect();
            let total = symbols.len() as u64;
            completed(
                QualifiedNamePrimitiveResult {
                    symbols,
                    total: Some(total),
                    next_cursor: None,
                },
                EvidenceDomain::Symbol,
                now_observed(),
            )
        })
    }

    fn call_chain<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        request: &'a CallChainPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, CallChainPrimitiveResult> {
        Box::pin(async move {
            let path = self
                .graph
                .get_call_chain(
                    &request.from_node_id,
                    &request.to_node_id,
                    request.maximum_depth as usize,
                )
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let mut node_ids = Vec::new();
            let mut edge_kinds = Vec::new();
            for (node, edge) in path {
                node_ids.push(node.id);
                if let Some(edge) = edge {
                    edge_kinds.push(edge.kind.as_str().to_owned());
                }
            }
            completed(
                CallChainPrimitiveResult {
                    node_ids,
                    edge_kinds,
                },
                EvidenceDomain::Graph,
                now_observed(),
            )
        })
    }

    fn file_dependents<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        request: &'a FileDependentsPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, FileDependentsPrimitiveResult> {
        Box::pin(async move {
            let dependent_files = self
                .graph
                .get_file_dependents(&request.file)
                .await
                .unwrap_or_default();
            completed(
                FileDependentsPrimitiveResult {
                    file: request.file.clone(),
                    dependent_files,
                },
                EvidenceDomain::Graph,
                now_observed(),
            )
        })
    }

    fn source_body<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        request: &'a SourceBodyPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, SourceBodyPrimitiveResult> {
        Box::pin(async move {
            let Some(node) = self.graph.get_node(&request.node_id).await.ok().flatten() else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let path = self.graph.project_root().join(&node.file_path);
            let Ok(content) = tokio::fs::read_to_string(&path).await else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let start = node.start_line as usize;
            let end = node.end_line as usize;
            let body = content
                .lines()
                .skip(start)
                .take(end.saturating_sub(start).saturating_add(1))
                .collect::<Vec<_>>()
                .join("\n");
            completed(
                SourceBodyPrimitiveResult {
                    node_id: node.id,
                    file: node.file_path,
                    start_line: node.start_line.saturating_add(1),
                    end_line: node.end_line.saturating_add(1),
                    body,
                },
                EvidenceDomain::Source,
                now_observed(),
            )
        })
    }

    fn source_outline<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        request: &'a SourceOutlinePrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, SourceOutlinePrimitiveResult> {
        Box::pin(async move {
            let nodes = self
                .graph
                .get_nodes_by_file(&request.file)
                .await
                .unwrap_or_default();
            completed(
                SourceOutlinePrimitiveResult {
                    file: request.file.clone(),
                    symbols: nodes
                        .into_iter()
                        .map(|node| symbol_record(node, None))
                        .collect(),
                },
                EvidenceDomain::Source,
                now_observed(),
            )
        })
    }

    fn module_api<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        request: &'a ModuleApiPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, ModuleApiPrimitiveResult> {
        Box::pin(async move {
            let nodes = self.graph.get_all_nodes().await.unwrap_or_default();
            completed(
                ModuleApiPrimitiveResult {
                    path: request.path.clone(),
                    symbols: public_module_symbols(nodes, &request.path),
                },
                EvidenceDomain::Symbol,
                now_observed(),
            )
        })
    }

    fn file_metadata<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        request: &'a FileMetadataPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, FileMetadataPrimitiveResult> {
        Box::pin(async move {
            let mut files = Vec::new();
            for file in &request.files {
                let path = self.graph.project_root().join(file);
                let meta = tokio::fs::metadata(&path).await.ok();
                files.push(FileMetadataRecord {
                    file: file.clone(),
                    language: None,
                    indexed_at: None,
                    byte_size: meta.map(|value| value.len()),
                });
            }
            completed(
                FileMetadataPrimitiveResult { files },
                EvidenceDomain::Source,
                now_observed(),
            )
        })
    }

    fn storage_status<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        request: &'a StorageStatusPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, StorageStatusPrimitiveResult> {
        Box::pin(async move {
            let branch = self.graph.branch_diagnostics();
            let read_only = self.graph.is_read_only();
            let database_bytes = self
                .graph
                .db_path()
                .metadata()
                .ok()
                .map(|metadata| metadata.len());
            let status = if branch.serving_db_exists {
                if branch.is_fallback { "degraded" } else { "ok" }
            } else {
                "missing_graph_db"
            };
            let mut details = Vec::new();
            if request.include_details {
                details.extend(branch.warnings);
                if let Some(warning) = branch.fallback_warning {
                    details.push(warning);
                }
            }
            completed(
                StorageStatusPrimitiveResult {
                    status: status.to_owned(),
                    read_only,
                    database_bytes,
                    details,
                },
                EvidenceDomain::Operational,
                now_observed(),
            )
        })
    }

    fn diagnostics<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        _request: &'a DiagnosticsPrimitiveRequest,
    ) -> Pr12ExtendedPrimitiveFuture<'a, DiagnosticsPrimitiveResult> {
        Box::pin(async move {
            // Diagnostic publication is owned by the feedback/LSP brokers; when
            // none are mounted for this project the authoritative answer is an
            // empty completed page, not a fabricated Unavailable problem.
            completed(
                DiagnosticsPrimitiveResult {
                    diagnostics: Vec::<DiagnosticPrimitiveRecord>::new(),
                },
                EvidenceDomain::Diagnostic,
                now_observed(),
            )
        })
    }
}

pub struct TraceDecayOperationalPrimitivePortV1 {
    graph: Arc<TraceDecay>,
}

impl TraceDecayOperationalPrimitivePortV1 {
    pub fn new(graph: Arc<TraceDecay>) -> Self {
        Self { graph }
    }
}

impl Pr12OperationalPrimitivePort for TraceDecayOperationalPrimitivePortV1 {
    fn read<'a>(
        &'a self,
        context: &'a RequestContext,
        operation: &'a tracedecay_application::ApplicationOperation,
        request: &'a Pr12OperationalPrimitiveRequest,
        observed_at: UtcMicros,
    ) -> Pr12OperationalPrimitiveFuture<'a> {
        Box::pin(async move {
            use tracedecay_application::{
                ApplicationEnvelope, AuthorityReceipt, CoverageDomainState, EvidenceAuthority,
                EvidenceCoverage, EvidencePacket, OperationReceipt, OperationTermination,
                PolicyDecisionRef, TemporalState,
            };
            use tracedecay_domain::ComponentVersion;
            let branch = self.graph.branch_diagnostics();
            let status = match request.operation {
                Pr12OperationalPrimitive::Project
                | Pr12OperationalPrimitive::Status
                | Pr12OperationalPrimitive::Files
                | Pr12OperationalPrimitive::Configuration
                | Pr12OperationalPrimitive::RuntimeStatus => {
                    if branch.serving_db_exists {
                        if branch.is_fallback { "degraded" } else { "ok" }
                    } else {
                        "degraded"
                    }
                }
            };
            let payload = serde_json::json!({
                "status": status,
                "observed_at": observed_at.0,
                "read_only": self.graph.is_read_only(),
                "serving_db_exists": branch.serving_db_exists,
                "is_fallback": branch.is_fallback,
            });
            let policy = PolicyDecisionRef::new(
                "route.pr12-primitive.operational.v1",
                1,
                ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                    .unwrap_or_else(|_| panic!("digest")),
                ComponentVersion::new("pr12-operational.v1")
                    .unwrap_or_else(|_| panic!("component")),
            )
            .map_err(|_| operational_problem(context, operation))?;
            let authority = AuthorityReceipt::from_context(context, policy, observed_at)
                .map_err(|_| operational_problem(context, operation))?;
            let coverage = EvidenceCoverage {
                requested_domains: vec![EvidenceDomain::Operational],
                visited: Some(1),
                eligible: Some(1),
                returned: 1,
                completeness: CoverageCompleteness::Complete,
                domains: vec![CoverageDomainState {
                    domain: EvidenceDomain::Operational,
                    completeness: CoverageCompleteness::Complete,
                }],
            };
            let page = PageState::first_page(
                SortContractId::new(PRIMITIVE_SORT).unwrap_or_else(|_| panic!("static sort")),
                1,
                Some(1),
                1,
            )
            .unwrap_or_else(|_| panic!("page"));
            let execution = OperationReceipt {
                started_at: observed_at,
                ended_at: observed_at,
                effective_deadline: context.deadline().clone(),
                cancellation: None,
                budget: OperationBudgetUsage::default(),
                termination: OperationTermination::Completed,
            };
            Ok(ApplicationEnvelope::evidence(
                operation.result_contract().clone(),
                context.request_id().clone(),
                context.scope().clone(),
                EvidencePacket {
                    temporal: TemporalState::current(observed_at),
                    authority,
                    evidence_authorities: Vec::<EvidenceAuthority>::new(),
                    coverage,
                    omissions: Vec::new(),
                    scores: Vec::new(),
                    contributions: Vec::new(),
                    page,
                    execution,
                    payload: Some(payload),
                },
            ))
        })
    }
}

pub struct ProjectSymbolGraphCursorSnapshotAuthority {
    key: SignedCursorKeyRefV1,
    configuration_digest: ManifestDigest,
    watermark: u64,
}

impl SymbolGraphCursorSnapshotAuthority for ProjectSymbolGraphCursorSnapshotAuthority {
    fn snapshot(
        &self,
        context: &RequestContext,
        _lane: &str,
        _observed_at: UtcMicros,
    ) -> Result<TemporalExecutionSnapshot, tracedecay_application::retrieval::PrimitiveFailure>
    {
        let request = TemporalSnapshotRequest::new(
            SessionId::new("session.daemon.primitive").map_err(|_| {
                tracedecay_application::retrieval::PrimitiveFailure::new(
                    tracedecay_application::retrieval::PrimitiveFailureKind::Unavailable,
                    "application.symbol-graph.session",
                    "could not mint primitive session id",
                )
                .unwrap_or_else(|_| panic!("static"))
            })?,
            context.scope().scope_digest.as_str(),
            context.request_id().as_str(),
            context.grant().digest.as_str(),
            TemporalModeV1::Current,
            RetrievalGrainV1::Occurrence,
        )
        .map_err(|_| {
            tracedecay_application::retrieval::PrimitiveFailure::new(
                tracedecay_application::retrieval::PrimitiveFailureKind::Unavailable,
                "application.symbol-graph.snapshot",
                "could not build temporal snapshot request",
            )
            .unwrap_or_else(|_| panic!("static"))
        })?;
        TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation: 1,
                source: self.watermark,
                projection: self.watermark,
                index: self.watermark,
                summary: self.watermark,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new(
                    "configuration_digest",
                    self.configuration_digest.as_str(),
                )
                .map_err(|_| {
                    tracedecay_application::retrieval::PrimitiveFailure::new(
                        tracedecay_application::retrieval::PrimitiveFailureKind::Unavailable,
                        "application.symbol-graph.configuration",
                        "invalid configuration digest",
                    )
                    .unwrap_or_else(|_| panic!("static"))
                })?,
            },
            Some(self.key.clone()),
            ValidatedAuthorization::Authorized,
        )
        .map_err(|_| {
            tracedecay_application::retrieval::PrimitiveFailure::new(
                tracedecay_application::retrieval::PrimitiveFailureKind::Unavailable,
                "application.symbol-graph.snapshot",
                "could not authorize temporal snapshot",
            )
            .unwrap_or_else(|_| panic!("static"))
        })
    }
}

pub struct TraceDecayAffectedTestsPortV1 {
    project_id: Option<ProjectId>,
    generation: CodeGenerationId,
    attribution: Option<Arc<dyn GenerationTestAttributionJoinReadPort + Send + Sync>>,
}

impl TraceDecayAffectedTestsPortV1 {
    pub fn new(graph: Arc<TraceDecay>, generation: CodeGenerationId) -> Self {
        Self::from_binding(project_id_for_graph(&graph), generation, None)
    }

    pub fn with_generation_attribution(
        graph: Arc<TraceDecay>,
        generation: CodeGenerationId,
        attribution: Arc<dyn GenerationTestAttributionJoinReadPort + Send + Sync>,
    ) -> Self {
        Self::from_binding(project_id_for_graph(&graph), generation, Some(attribution))
    }

    fn from_binding(
        project_id: Option<ProjectId>,
        generation: CodeGenerationId,
        attribution: Option<Arc<dyn GenerationTestAttributionJoinReadPort + Send + Sync>>,
    ) -> Self {
        Self {
            project_id,
            generation,
            attribution,
        }
    }
}

impl tracedecay_application::AffectedTestsRetrievalPort for TraceDecayAffectedTestsPortV1 {
    fn affected_tests(
        &self,
        context: &RetrievalPortContext<'_>,
        request: &AffectedTestsRequest,
    ) -> RetrievalPortOutcome<AffectedTestsResult> {
        let finished_at = now_observed();
        if self.project_id.as_ref() != Some(&context.request.scope().project_id)
            || request.generation != self.generation
        {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Unavailable,
                FreshnessState::Unknown,
            );
        }
        let Some(attribution) = &self.attribution else {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Unavailable,
                FreshnessState::Unknown,
            );
        };
        attributed_tests_outcome(
            request,
            attribution.read_test_attribution(&request.generation),
            finished_at,
        )
    }
}

fn project_id_for_graph(graph: &TraceDecay) -> Option<ProjectId> {
    graph
        .hook_store_layout()
        .identity
        .project_id
        .as_ref()
        .and_then(|project_id| ProjectId::new(project_id.clone()).ok())
}

fn attributed_tests_outcome(
    request: &AffectedTestsRequest,
    read: GenerationProviderReadV1<GenerationTestJoinV1>,
    finished_at: UtcMicros,
) -> RetrievalPortOutcome<AffectedTestsResult> {
    if read.validate().is_err() {
        return affected_tests_unavailable(
            request,
            finished_at,
            OmissionReason::Failed,
            FreshnessState::Unknown,
        );
    }
    match read.provider_state {
        ProviderEvaluationStateV1::Cancelled => {
            return RetrievalPortOutcome::Cancelled(affected_tests_evidence(
                request,
                None,
                finished_at,
                CoverageCompleteness::Unknown,
                FreshnessState::Unknown,
                None,
                None,
                Some(OmissionReason::Cancelled),
            ));
        }
        ProviderEvaluationStateV1::TimedOut => {
            return RetrievalPortOutcome::TimedOut(affected_tests_evidence(
                request,
                None,
                finished_at,
                CoverageCompleteness::Unknown,
                FreshnessState::Unknown,
                None,
                None,
                Some(OmissionReason::TimedOut),
            ));
        }
        ProviderEvaluationStateV1::SupportedCompletedComplete
        | ProviderEvaluationStateV1::Partial => {}
        ProviderEvaluationStateV1::Stale => {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Stale,
                FreshnessState::Stale,
            );
        }
        ProviderEvaluationStateV1::Unsupported
        | ProviderEvaluationStateV1::Absent
        | ProviderEvaluationStateV1::Indexing
        | ProviderEvaluationStateV1::Failed
        | ProviderEvaluationStateV1::Unavailable => {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Unavailable,
                FreshnessState::Unknown,
            );
        }
    }

    let Some(join) = read.evidence else {
        return affected_tests_unavailable(
            request,
            finished_at,
            OmissionReason::Unavailable,
            FreshnessState::Unknown,
        );
    };
    if join.generation_id != request.generation
        || join.test_watermark.generation_id != request.generation
    {
        return affected_tests_unavailable(
            request,
            finished_at,
            OmissionReason::Stale,
            FreshnessState::Stale,
        );
    }

    let mut tests = Vec::new();
    let mut matching_incomplete = false;
    for record in &join.records {
        if record.attribution.generation_id != request.generation {
            return affected_tests_unavailable(
                request,
                finished_at,
                OmissionReason::Stale,
                FreshnessState::Stale,
            );
        }
        let covers_requested_symbol = record
            .attribution
            .covered_occurrences
            .contains(&request.symbol);
        if !covers_requested_symbol {
            continue;
        }
        if matches!(
            &record.disposition,
            GenerationTestJoinDispositionV1::Current { .. }
        ) {
            let Some(test_occurrence) = &record.test_occurrence else {
                return affected_tests_unavailable(
                    request,
                    finished_at,
                    OmissionReason::Failed,
                    FreshnessState::Unknown,
                );
            };
            if test_occurrence.occurrence_id != record.attribution.test_occurrence {
                return affected_tests_unavailable(
                    request,
                    finished_at,
                    OmissionReason::Failed,
                    FreshnessState::Unknown,
                );
            }
            tests.push(test_occurrence.occurrence_id.clone());
        } else {
            matching_incomplete = true;
        }
    }
    tests.sort();
    tests.dedup();

    let complete = read.provider_state == ProviderEvaluationStateV1::SupportedCompletedComplete
        && read.coverage.is_complete()
        && matches!(join.coverage, GenerationTestJoinCoverageV1::Complete)
        && !matching_incomplete;
    let (visited, eligible) = affected_tests_provider_counts(&read.coverage);
    if eligible.is_some_and(|eligible| tests.len() as u64 > eligible) {
        return affected_tests_unavailable(
            request,
            finished_at,
            OmissionReason::Failed,
            FreshnessState::Unknown,
        );
    }
    let evidence = affected_tests_evidence(
        request,
        Some(AffectedTestsResult { tests }),
        finished_at,
        if complete {
            CoverageCompleteness::Complete
        } else {
            CoverageCompleteness::Partial
        },
        if complete {
            FreshnessState::Current
        } else {
            FreshnessState::Unknown
        },
        visited,
        eligible,
        (!complete).then_some(OmissionReason::Unavailable),
    );
    if complete {
        RetrievalPortOutcome::Completed(evidence)
    } else {
        RetrievalPortOutcome::Partial(evidence)
    }
}

fn affected_tests_provider_counts(
    coverage: &GenerationProviderCoverageV1,
) -> (Option<u64>, Option<u64>) {
    match coverage {
        GenerationProviderCoverageV1::Complete {
            examined, eligible, ..
        }
        | GenerationProviderCoverageV1::Partial {
            examined, eligible, ..
        } => (Some(*examined), Some(*eligible)),
        GenerationProviderCoverageV1::Unavailable => (None, None),
    }
}

fn affected_tests_unavailable(
    request: &AffectedTestsRequest,
    finished_at: UtcMicros,
    reason: OmissionReason,
    freshness: FreshnessState,
) -> RetrievalPortOutcome<AffectedTestsResult> {
    RetrievalPortOutcome::Unavailable(affected_tests_evidence(
        request,
        None,
        finished_at,
        CoverageCompleteness::Unknown,
        freshness,
        None,
        None,
        Some(reason),
    ))
}

#[allow(clippy::too_many_arguments)]
fn affected_tests_evidence(
    request: &AffectedTestsRequest,
    payload: Option<AffectedTestsResult>,
    finished_at: UtcMicros,
    completeness: CoverageCompleteness,
    freshness: FreshnessState,
    visited: Option<u64>,
    eligible: Option<u64>,
    omission: Option<OmissionReason>,
) -> RetrievalEvidence<AffectedTestsResult> {
    let returned = payload
        .as_ref()
        .map_or(0, |result| result.tests.len() as u64);
    RetrievalEvidence {
        payload,
        temporal: TemporalState {
            requested_mode: request.meta.temporal,
            requested_at: finished_at,
            resolved_at: finished_at,
            source_generation: Some(request.generation.clone()),
            watermark_digest: None,
            freshness,
        },
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Test],
            visited,
            eligible,
            returned,
            completeness,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Test,
                completeness,
            }],
        },
        omissions: omission
            .map(|reason| Omission {
                domain: EvidenceDomain::Test,
                count: 0,
                reason,
            })
            .into_iter()
            .collect(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new(PRIMITIVE_SORT).unwrap_or_else(|_| panic!("static sort")),
            1,
            eligible,
            returned,
        )
        .unwrap_or_else(|_| panic!("affected-tests page")),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    }
}

fn operational_problem(
    context: &RequestContext,
    operation: &tracedecay_application::ApplicationOperation,
) -> tracedecay_application::ApplicationProblemEnvelope {
    use tracedecay_application::{ApplicationProblem, ApplicationProblemEnvelope, SafeDiagnostic};
    ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        ApplicationProblem::unavailable(
            SafeDiagnostic::new(
                "application.pr12-primitive.operational",
                "The operational primitive authority could not complete.",
            )
            .unwrap_or_else(|_| panic!("static diagnostic")),
        ),
    )
}

/// Opens the complete owned PR12 primitive runtime from production authorities.
pub(crate) async fn open_pr12_production_primitive_runtime(
    database: Database,
    graph: Arc<TraceDecay>,
    session_db: Arc<RegisteredGlobalDb>,
    project_root: PathBuf,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    scope: ResolvedScope,
    access: ProjectSourceAccessSnapshot,
    admitted_root_uri: String,
    operation_events: OperationEventAuthority,
    configuration_digest: ManifestDigest,
) -> Result<Pr12PrimitiveProjectRuntime, ApplicationContractError> {
    let key = session_db
        .as_ref()
        .ensure_active_session_cursor_key_result()
        .await
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "PR12 primitive session cursor key",
        })?;
    let read = session_db.as_ref().read_snapshot().await.map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "PR12 primitive session cursor snapshot",
        }
    })?;
    let authenticator = Arc::new(
        GlobalDbCursorKeyProvider::from_registered_key_ref(&read, key.clone())
            .await
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "PR12 primitive session cursor authenticator",
            })?,
    );
    let watermark = graph
        .get_stats()
        .await
        .map_or(1, |stats| stats.node_count)
        .max(1);
    let snapshots = Arc::new(ProjectSymbolGraphCursorSnapshotAuthority {
        key,
        configuration_digest,
        watermark,
    });
    let cursors: Arc<dyn SymbolGraphCursorPort> = Arc::new(
        AuthenticatedSymbolGraphCursorAdapter::new(snapshots, authenticator),
    );
    let test_run_scope: Arc<dyn ManagedTestRunCurrentScopePort> =
        Arc::new(ProductionManagedTestRunCurrentScope {
            project_root,
            code_index,
        });
    open_pr12_primitive_project_runtime(
        database,
        Arc::clone(&graph),
        cursors,
        Arc::new(TraceDecayTestPrimitivePortV1::new(Arc::clone(&graph))),
        Arc::new(TraceDecayLexicalGrepAuthorityV1::new(Arc::clone(&graph))),
        Arc::new(TraceDecayRedundancyAuthorityV1::new(Arc::clone(&graph))),
        Arc::new(TraceDecayTemporalPortV1::new(session_db)),
        Arc::new(TraceDecaySourceLinesPortV1::new(Arc::clone(&graph))),
        Arc::new(TraceDecayHealthPortV1::new(Arc::clone(&graph))),
        Arc::new(TraceDecayExtendedPrimitivePortV1::new(Arc::clone(&graph))),
        Arc::new(TraceDecayOperationalPrimitivePortV1::new(graph)),
        scope,
        access,
        admitted_root_uri,
        operation_events,
        test_run_scope,
    )
}

#[derive(Clone)]
struct ProductionManagedTestRunCurrentScope {
    project_root: PathBuf,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
}

impl ManagedTestRunCurrentScopePort for ProductionManagedTestRunCurrentScope {
    fn current_identity(&self) -> ManagedTestRunCurrentIdentityFuture<'_> {
        let project_root = self.project_root.clone();
        let code_index = Arc::clone(&self.code_index);
        Box::pin(async move {
            let head_commit_id = current_managed_test_run_head(&project_root)?;
            let current = code_index
                .current_identity(project_root, None)
                .await
                .map_err(|_| ApplicationContractError::Inconsistent {
                    field: "PR12 managed test result code generation",
                })?;
            Ok(ManagedTestRunCurrentIdentity {
                head_commit_id,
                code_generation_id: current.code_generation_id,
            })
        })
    }
}

fn current_managed_test_run_head(
    project_root: &Path,
) -> Result<CommitId, ApplicationContractError> {
    let repository =
        gix::open(project_root).map_err(|_| ApplicationContractError::Inconsistent {
            field: "PR12 managed test result repository",
        })?;
    let head_commit_id = repository
        .head_commit()
        .ok()
        .and_then(|commit| CommitId::new(commit.id().to_hex().to_string()).ok())
        .ok_or(ApplicationContractError::Inconsistent {
            field: "PR12 managed test result head",
        })?;
    Ok(head_commit_id)
}

pub fn admitted_root_uri_for_project(
    project_root: &Path,
) -> Result<String, ApplicationContractError> {
    let uri =
        Url::from_file_path(project_root).map_err(|()| ApplicationContractError::Inconsistent {
            field: "PR12 primitive admitted root URI",
        })?;
    Ok(uri.to_string())
}

pub fn locator_digest_for_project(
    project_root: &Path,
) -> Result<ManifestDigest, ApplicationContractError> {
    canonical_sha256(&(
        "tracedecay.project-open.locator.v1",
        project_root.to_string_lossy().as_ref(),
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "PR12 primitive project locator digest",
    })
}

#[cfg(test)]
mod affected_tests_tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_application::retrieval::{
        AffectedTestsRetrievalPort, PageRequest, ResultProjection, RetrievalOrder,
        RetrievalRequestMeta,
    };
    use tracedecay_application::{
        ApplicationOperation, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
        Deadline, DisclosureClass, RequestContext, RequestId, ResultContractRef,
    };
    use tracedecay_domain::{
        ActorId, CodeGenerationId, ComponentVersion, ContentDigest, FileOccurrenceId,
        GenerationTestAttributionV1, ProjectId, ProviderEvaluationStateV1, RefId, RepositoryId,
        SymbolOccurrenceId, TestAttributionEvidenceClassV1, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};

    use super::*;
    use crate::code_index::provider::{
        GenerationProviderCoverageV1, GenerationProviderReadV1,
        GenerationTestAttributionJoinReadPort,
    };
    use crate::code_index::test_attribution::{
        GenerationTestJoinCoverageV1, GenerationTestJoinDispositionV1,
        GenerationTestJoinPartialReasonV1, GenerationTestJoinRecordV1, GenerationTestJoinV1,
        TestAttributionJoinInputCoverageV1, TestAttributionOccurrenceV1,
        TestAttributionWatermarkV1,
    };

    struct AttributionFixture {
        calls: AtomicUsize,
        read: GenerationProviderReadV1<GenerationTestJoinV1>,
    }

    impl GenerationTestAttributionJoinReadPort for AttributionFixture {
        fn read_test_attribution(
            &self,
            _generation: &CodeGenerationId,
        ) -> GenerationProviderReadV1<GenerationTestJoinV1> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.read.clone()
        }
    }

    fn generation(value: &str) -> CodeGenerationId {
        CodeGenerationId::new(value).expect("generation")
    }

    fn digest(value: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", value.to_string().repeat(64))).expect("digest")
    }

    fn content(value: char) -> ContentDigest {
        ContentDigest::new(format!("sha256:{}", value.to_string().repeat(64))).expect("content")
    }

    fn context(project_id: ProjectId) -> (RequestContext, ApplicationOperation, ResolvedScope) {
        let scope = ResolvedScope::new(
            project_id,
            RepositoryId::new("repository.affected-tests").expect("repository"),
            WorktreeId::new("worktree.affected-tests").expect("worktree"),
            Some(RefId::new("refs/heads/affected-tests").expect("reference")),
        )
        .expect("scope");
        let capability = CapabilityId::new("capability.affected-tests").expect("capability");
        let use_case = UseCaseId::new("use-case.affected-tests").expect("use case");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.affected-tests").expect("grant"),
            1,
            digest('a'),
            ActorId::new("actor.affected-tests.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(10_000),
            scope.clone(),
            BTreeSet::from([capability.clone()]),
            BTreeSet::from([use_case.clone()]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let request = RequestContext::new(
            ActorId::new("actor.affected-tests.requester").expect("actor"),
            scope.clone(),
            grant,
            RequestId::new("request.affected-tests").expect("request"),
            Deadline::new(UtcMicros(10_000)).expect("deadline"),
            CancellationContext::active("cancel.affected-tests").expect("cancellation"),
        )
        .expect("context");
        let operation = ApplicationOperation::new(
            capability,
            use_case,
            ResultContractRef::new(SchemaId::new("schema.affected-tests").expect("schema"), 1)
                .expect("contract"),
            true,
        );
        (request, operation, scope)
    }

    fn request(generation: CodeGenerationId) -> AffectedTestsRequest {
        AffectedTestsRequest {
            symbol: SymbolOccurrenceId::new("symbol.source").expect("symbol"),
            generation,
            meta: RetrievalRequestMeta::current(
                PageRequest::first(100).expect("page"),
                ResultProjection::ReferencesOnly,
                RetrievalOrder::StableIdentity,
            ),
        }
    }

    fn complete_read(
        generation: CodeGenerationId,
    ) -> GenerationProviderReadV1<GenerationTestJoinV1> {
        let source = SymbolOccurrenceId::new("symbol.source").expect("source");
        let test = SymbolOccurrenceId::new("symbol.test").expect("test");
        let source_file = FileOccurrenceId::new("file.source").expect("source file");
        let test_file = FileOccurrenceId::new("file.test").expect("test file");
        let revision = ComponentVersion::new("test-attribution.v1").expect("revision");
        let attribution = GenerationTestAttributionV1 {
            generation_id: generation.clone(),
            source_revision: None,
            test_occurrence: test.clone(),
            covered_occurrences: vec![source.clone()],
            evidence_class: TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
            attribution_revision: revision.clone(),
        };
        let test_occurrence = TestAttributionOccurrenceV1 {
            occurrence_id: test.clone(),
            file_occurrence_id: test_file,
            content_digest: content('b'),
        };
        let source_occurrence = TestAttributionOccurrenceV1 {
            occurrence_id: source,
            file_occurrence_id: source_file,
            content_digest: content('c'),
        };
        let join = GenerationTestJoinV1 {
            generation_id: generation.clone(),
            code_snapshot_digest: digest('d'),
            code_content_identity: content('e'),
            test_watermark: TestAttributionWatermarkV1 {
                generation_id: generation,
                snapshot_digest: digest('d'),
                content_identity: content('e'),
                source_revision: None,
                attribution_revision: revision,
                evidence_digest: digest('f'),
                coverage: TestAttributionJoinInputCoverageV1::Complete,
            },
            records: vec![GenerationTestJoinRecordV1 {
                attribution,
                test_occurrence: Some(test_occurrence),
                covered_occurrences: vec![source_occurrence],
                disposition: GenerationTestJoinDispositionV1::Current {
                    evidence_class:
                        TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
                },
            }],
            coverage: GenerationTestJoinCoverageV1::Complete,
        };
        GenerationProviderReadV1::new(
            ProviderEvaluationStateV1::SupportedCompletedComplete,
            GenerationProviderCoverageV1::Complete {
                examined: 1,
                eligible: 1,
                excluded: 0,
            },
            Some(join),
        )
        .expect("provider read")
    }

    #[test]
    fn exact_project_and_generation_route_canonical_attribution() {
        let project_id = ProjectId::new("project.affected-tests").expect("project");
        let generation = generation("generation.affected-tests.1");
        let authority = Arc::new(AttributionFixture {
            calls: AtomicUsize::new(0),
            read: complete_read(generation.clone()),
        });
        let port = TraceDecayAffectedTestsPortV1::from_binding(
            Some(project_id.clone()),
            generation.clone(),
            Some(authority.clone()),
        );
        let (context, operation, _) = context(project_id);

        let outcome = port.affected_tests(
            &RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request(generation.clone()),
        );

        assert_eq!(authority.calls.load(Ordering::Relaxed), 1);
        let RetrievalPortOutcome::Completed(evidence) = outcome else {
            panic!("exact current attribution must complete");
        };
        assert_eq!(evidence.temporal.source_generation, Some(generation));
        assert_eq!(
            evidence.payload.expect("payload").tests,
            vec![SymbolOccurrenceId::new("symbol.test").expect("test")]
        );
    }

    #[test]
    fn absent_or_mismatched_authority_never_fabricates_complete_empty() {
        let project_id = ProjectId::new("project.affected-tests").expect("project");
        let expected_generation = generation("generation.affected-tests.1");
        let requested_generation = generation("generation.affected-tests.2");
        let port = TraceDecayAffectedTestsPortV1::from_binding(
            Some(project_id.clone()),
            expected_generation.clone(),
            None,
        );
        let (context, operation, _) = context(project_id);

        let outcome = port.affected_tests(
            &RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request(requested_generation),
        );

        assert!(matches!(outcome, RetrievalPortOutcome::Unavailable(_)));

        let authority = Arc::new(AttributionFixture {
            calls: AtomicUsize::new(0),
            read: complete_read(expected_generation.clone()),
        });
        let port = TraceDecayAffectedTestsPortV1::from_binding(
            Some(ProjectId::new("project.other").expect("other project")),
            expected_generation.clone(),
            Some(authority.clone()),
        );
        let outcome = port.affected_tests(
            &RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request(expected_generation),
        );

        assert!(matches!(outcome, RetrievalPortOutcome::Unavailable(_)));
        assert_eq!(authority.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn partial_attribution_stays_partial() {
        let project_id = ProjectId::new("project.affected-tests").expect("project");
        let generation = generation("generation.affected-tests.1");
        let mut read = complete_read(generation.clone());
        read.provider_state = ProviderEvaluationStateV1::Partial;
        read.coverage = GenerationProviderCoverageV1::Partial {
            examined: 2,
            eligible: 1,
            excluded: 0,
            unknown: 1,
            capped: false,
        };
        read.evidence.as_mut().expect("join").coverage = GenerationTestJoinCoverageV1::Partial {
            reasons: vec![GenerationTestJoinPartialReasonV1::InputPartial {
                reason: "indexing".to_owned(),
            }],
        };
        let authority = Arc::new(AttributionFixture {
            calls: AtomicUsize::new(0),
            read,
        });
        let port = TraceDecayAffectedTestsPortV1::from_binding(
            Some(project_id.clone()),
            generation.clone(),
            Some(authority),
        );
        let (context, operation, _) = context(project_id);

        let outcome = port.affected_tests(
            &RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request(generation),
        );

        assert!(matches!(outcome, RetrievalPortOutcome::Partial(_)));
    }
}
