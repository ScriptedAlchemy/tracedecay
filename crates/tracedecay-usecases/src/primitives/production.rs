//! Production application primitive owners over admitted graph and store authorities.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use sha2::{Digest, Sha256};
use tracedecay_application::retrieval::grep_analysis::{
    GrepAnalysisProblemV1, GrepHitV1, GrepRequestV1, GrepResultV1, LexicalGrepAuthorityV1,
    PrimitiveCoverageV1, PrimitiveFutureV1, PrimitiveOutcomeV1, PrimitivePageV1,
    PrimitivePortContextV1, RedundancyAuthorityV1, RedundancyRequestV1, RedundancyResultV1,
};
use tracedecay_application::retrieval::{
    AffectedFileTestsPrimitiveRequest, AffectedFileTestsPrimitiveResultV1,
    AffectedTestAttributionV1, AffectedTestsRequest, AffectedTestsResult, HealthDeltaRequest,
    HealthDeltaResult, HealthReadRequest, HealthReadResult, OperationalRetrievalPort,
    RankedAffectedTestV1, RetrievalPortContext, RetrievalPortOutcome, SourceLinesRequest,
    SourceLinesResult, SourceReference, SourceRetrievalPort, SymbolPrimitiveRecord,
    TemporalRetrievalPort, TestMapCoverageV1, TestMapPrimitiveRequest, TestMapPrimitiveResultV1,
    TestPrimitivePort, TestPrimitivePortContext, TestPrimitivePortFuture, TestPrimitivePortOutcome,
    TestReferenceV1, UncoveredSourceV1,
};
use tracedecay_application::{
    ApplicationContractError, CoverageCompleteness, CoverageDomainState, EvidenceAuthority,
    EvidenceCoverage, EvidenceDomain, EvidenceIdentity, FreshnessState, Omission, OmissionReason,
    OpaqueCursor, OperationBudgetUsage, PageCursor, PageState, RequestAdmission, RequestContext,
    ResolvedScope, RetrievalEvidence, TemporalState,
};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, ProjectId, ProviderEvaluationStateV1, RetrievalAnchorId,
    RetrievalGrainV1, SessionId, SignedCursorKeyRefV1, TemporalModeV1, UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::SortContractId;
use url::Url;

use super::concrete::{
    AuthenticatedSymbolGraphCursorAdapter, SymbolGraphCursorSnapshot,
    SymbolGraphCursorSnapshotAuthority,
};
use super::runtime::{
    CallChainPrimitiveRequest, CallChainPrimitiveResult, DiagnosticPrimitiveRecord,
    DiagnosticsPrimitiveRequest, DiagnosticsPrimitiveResult, ExtendedPrimitiveFuture,
    ExtendedPrimitivePort, FileDependentsPrimitiveRequest, FileDependentsPrimitiveResult,
    FileMetadataPrimitiveRequest, FileMetadataPrimitiveResult, FileMetadataRecord,
    ManagedTestRunCurrentIdentity, ManagedTestRunCurrentIdentityFuture,
    ManagedTestRunCurrentScopePort, ModuleApiPrimitiveRequest, ModuleApiPrimitiveResult,
    PrimitiveProjectRuntime, QualifiedNamePrimitiveRequest, QualifiedNamePrimitiveResult,
    SourceBodyPrimitiveRequest, SourceBodyPrimitiveResult, SourceOutlinePrimitiveRequest,
    SourceOutlinePrimitiveResult, StorageStatusHistoryPointV1, StorageStatusPrimitiveRequest,
    StorageStatusPrimitiveResult, open_primitive_project_runtime,
};
use super::support::{
    BoundedSourceSearch, affected_test_proximity, collect_affected_test_files, rank_affected_tests,
    run_bounded_source_search,
};
use super::symbol_graph::{SymbolGraphCursorPort, symbol_record};
use crate::code_index::CodeIndexIgnoredDependencyAdmissionPortV1;
use crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1;
use crate::diagnostics_query::{
    DiagnosticPageRequest, DiagnosticQueryCoverage, DiagnosticQueryCursor, DiagnosticsQuery,
};
use crate::graph::health_delta::compute_verified_health_delta;
use crate::graph::queries::GraphQueryManager;
use crate::graph::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadRequest,
    request_graph_cancellation,
};
use crate::lsp_runtime::LspCodeIndexProjectionIdentityPort;
use crate::operation_stream::OperationEventAuthority;
use crate::source_authorization::ProjectSourceAccessSnapshot;
use crate::tracedecay::SourceReadRuntime;
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1,
};
use tracedecay_code_index::grep_search::{
    GrepSearchQuery, search_tree_with_cancel as lexical_search_tree_with_cancel,
};
use tracedecay_code_index::provider::{
    GenerationProviderCoverageV1, GenerationProviderReadV1, GenerationTestAttributionJoinReadPort,
};
use tracedecay_code_index::test_attribution::{
    GenerationTestJoinCoverageV1, GenerationTestJoinDispositionV1, GenerationTestJoinV1,
};
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_global_db::session_temporal::GlobalDbCursorKeyProvider;
use tracedecay_runtime_core::db::Database;
use tracedecay_temporal_query::cursor::{
    CURSOR_LIFETIME_MICROS, StableSortKey, encode_cursor, verify_cursor,
};
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, SessionCursorAuthenticator, TemporalExecutionSnapshot,
    TemporalSnapshotRequest, TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

mod managed_test_scope;
#[cfg(test)]
#[path = "production/managed_test_scope_tests.rs"]
mod managed_test_scope_tests;
use managed_test_scope::ProductionManagedTestRunCurrentScope;

const PRIMITIVE_SORT: &str = "sort.application.primitive.v1";

/// Validated once per process; every page in this module shares the same
/// static sort contract, so the identifier check does not belong on the
/// per-call path.
static PRIMITIVE_SORT_CONTRACT: LazyLock<SortContractId> =
    LazyLock::new(|| SortContractId::new(PRIMITIVE_SORT).unwrap_or_else(|_| panic!("static sort")));

fn completed<T>(
    payload: T,
    domain: EvidenceDomain,
    finished_at: UtcMicros,
) -> RetrievalPortOutcome<T> {
    let Ok(coverage) = EvidenceCoverage::complete(vec![domain], 1, 1, 1) else {
        return failed(domain, finished_at);
    };
    let Ok(page) = PageState::first_page(PRIMITIVE_SORT_CONTRACT.clone(), 1, Some(1), 1) else {
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
            domains: vec![CoverageDomainState {
                domain,
                completeness: CoverageCompleteness::Unknown,
            }],
        },
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(PRIMITIVE_SORT_CONTRACT.clone(), 1, Some(0), 0)
            .unwrap_or_else(|_| panic!("empty page")),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    })
}

/// Reports a test primitive read that could not be served.
///
/// A graph read that fails leaves the port with no measurement at all. The
/// empty default would be returned as `Completed`, so a store failure would
/// claim a tested function has no tests, which is worse than reporting
/// nothing. Whole-read failures land here; per-symbol failures keep the
/// symbols that were read and report `Partial`.
fn test_primitive_failed<T>(context: TestPrimitivePortContext<'_>) -> TestPrimitivePortOutcome<T> {
    TestPrimitivePortOutcome::Failed {
        finished_at: context.observed_at,
        budget: OperationBudgetUsage::default(),
    }
}

fn diagnostics_unavailable(
    finished_at: UtcMicros,
    reason: OmissionReason,
) -> RetrievalPortOutcome<DiagnosticsPrimitiveResult> {
    evidence_unavailable(EvidenceDomain::Diagnostic, finished_at, reason, 0)
}

fn omitted_evidence<T>(
    domain: EvidenceDomain,
    finished_at: UtcMicros,
    reason: OmissionReason,
    omitted: u64,
) -> RetrievalEvidence<T> {
    RetrievalEvidence {
        payload: None,
        temporal: TemporalState {
            freshness: if reason == OmissionReason::Stale {
                FreshnessState::Stale
            } else {
                FreshnessState::Unknown
            },
            ..TemporalState::current(finished_at)
        },
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage {
            requested_domains: vec![domain],
            visited: None,
            eligible: None,
            returned: 0,
            completeness: CoverageCompleteness::Unknown,
            domains: vec![CoverageDomainState {
                domain,
                completeness: CoverageCompleteness::Unknown,
            }],
        },
        omissions: vec![Omission {
            domain,
            count: omitted,
            reason,
        }],
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(PRIMITIVE_SORT_CONTRACT.clone(), 1, None, 0)
            .unwrap_or_else(|_| panic!("diagnostic unavailable page")),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    }
}

fn evidence_unavailable<T>(
    domain: EvidenceDomain,
    finished_at: UtcMicros,
    reason: OmissionReason,
    omitted: u64,
) -> RetrievalPortOutcome<T> {
    RetrievalPortOutcome::Unavailable(omitted_evidence(domain, finished_at, reason, omitted))
}

fn graph_read_outcome<T>(
    error: &CodeGraphReadError,
    domain: EvidenceDomain,
    finished_at: UtcMicros,
) -> RetrievalPortOutcome<T> {
    let reason = match error {
        CodeGraphReadError::Cancelled => OmissionReason::Cancelled,
        CodeGraphReadError::TimedOut => OmissionReason::TimedOut,
        CodeGraphReadError::Stale { .. } => OmissionReason::Stale,
        _ => OmissionReason::Unavailable,
    };
    let evidence = omitted_evidence(domain, finished_at, reason, 0);
    match error {
        CodeGraphReadError::Cancelled => RetrievalPortOutcome::Cancelled(evidence),
        CodeGraphReadError::TimedOut => RetrievalPortOutcome::TimedOut(evidence),
        _ => RetrievalPortOutcome::Unavailable(evidence),
    }
}

fn diagnostics_result(
    generation_id: CodeGenerationId,
    watermark_digest: ManifestDigest,
    diagnostics: Vec<DiagnosticPrimitiveRecord>,
    total: u64,
    next_cursor: Option<OpaqueCursor>,
    finished_at: UtcMicros,
) -> RetrievalPortOutcome<DiagnosticsPrimitiveResult> {
    let returned = diagnostics.len() as u64;
    let next_cursor_text = next_cursor
        .as_ref()
        .map(|cursor| cursor.as_str().to_owned());
    let mut page = PageState::first_page(PRIMITIVE_SORT_CONTRACT.clone(), 1, Some(total), returned)
        .unwrap_or_else(|_| panic!("diagnostic result page"));
    page.cursor = next_cursor.map(|cursor| PageCursor::Opaque { cursor });
    page.expires_at = page.cursor.as_ref().and_then(|_| {
        finished_at
            .0
            .checked_add(CURSOR_LIFETIME_MICROS)
            .map(UtcMicros)
    });
    let evidence = RetrievalEvidence {
        payload: Some(DiagnosticsPrimitiveResult {
            generation_id: generation_id.clone(),
            clean_generation: true,
            findings_cleared: total == 0,
            diagnostics,
            next_cursor: next_cursor_text,
        }),
        temporal: TemporalState {
            source_generation: Some(generation_id),
            watermark_digest: Some(watermark_digest),
            ..TemporalState::current(finished_at)
        },
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Diagnostic],
            visited: Some(total),
            eligible: Some(total),
            returned,
            completeness: CoverageCompleteness::Complete,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Diagnostic,
                completeness: CoverageCompleteness::Complete,
            }],
        },
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page,
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    };
    RetrievalPortOutcome::Completed(evidence)
}

const DIAGNOSTIC_CURSOR_LANE_WORKSPACE: &str = "workspace";

struct AuthenticatedDiagnosticCursorAuthorityV1 {
    key: SignedCursorKeyRefV1,
    configuration_digest: ManifestDigest,
    authenticator: Arc<dyn SessionCursorAuthenticator>,
}

impl AuthenticatedDiagnosticCursorAuthorityV1 {
    fn snapshot(
        &self,
        context: &RequestContext,
        generation: &CodeGenerationId,
        lane: &str,
    ) -> Result<TemporalExecutionSnapshot, ()> {
        if context.validate().is_err()
            || context.admission_at(now_observed()) != RequestAdmission::Admitted
        {
            return Err(());
        }
        let request_digest = canonical_sha256(&(
            "tracedecay.diagnostics.cursor.v1",
            context.actor(),
            context.grant().revision,
            &context.grant().digest,
            &context.grant().issuer,
            &context.grant().allowed_capabilities,
            &context.grant().allowed_use_cases,
            context.grant().disclosure,
            generation.as_str(),
            lane,
        ))
        .map_err(|_| ())?;
        let request = TemporalSnapshotRequest::new(
            SessionId::new("session.daemon.diagnostics").map_err(|_| ())?,
            context.scope().scope_digest.as_str(),
            request_digest.as_str(),
            context.grant().digest.as_str(),
            TemporalModeV1::Current,
            RetrievalGrainV1::Occurrence,
        )
        .map_err(|_| ())?;
        TemporalExecutionSnapshot::new_authorized(
            request,
            TemporalWatermarks {
                generation: 1,
                source: 1,
                projection: 1,
                index: 1,
                summary: 1,
            },
            KernelVersions {
                schema: 1,
                ranking: 1,
                configuration_digest: BindingDigest::new(
                    "configuration_digest",
                    self.configuration_digest.as_str(),
                )
                .map_err(|_| ())?,
            },
            Some(self.key.clone()),
            ValidatedAuthorization::Authorized,
        )
        .map_err(|_| ())
    }

    fn decode(
        &self,
        encoded: &str,
        context: &RequestContext,
        generation: &CodeGenerationId,
        lane: &str,
    ) -> Result<DiagnosticQueryCursor, ()> {
        let snapshot = self.snapshot(context, generation, lane)?;
        let sort_key =
            verify_cursor(encoded, &snapshot, self.authenticator.as_ref()).map_err(|_| ())?;
        if sort_key.normalized_score_micros != 0 || sort_key.knowledge_at_micros != 0 {
            return Err(());
        }
        DiagnosticQueryCursor::decode(&format!("dq1:{}", sort_key.stable_id)).map_err(|_| ())
    }

    fn encode(
        &self,
        cursor: &DiagnosticQueryCursor,
        context: &RequestContext,
        generation: &CodeGenerationId,
        lane: &str,
    ) -> Result<OpaqueCursor, ()> {
        let snapshot = self.snapshot(context, generation, lane)?;
        let encoded = encode_cursor(
            &snapshot,
            &StableSortKey {
                normalized_score_micros: 0,
                knowledge_at_micros: 0,
                stable_id: cursor.anchor().to_owned(),
            },
            self.authenticator.as_ref(),
        )
        .map_err(|_| ())?;
        OpaqueCursor::new(encoded).map_err(|_| ())
    }
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

async fn open_code_graph(
    port: &dyn CodeGraphProjectionReadPort,
    context: &RequestContext,
    observed_at: UtcMicros,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
) -> Result<CodeGraphInteractiveReader, CodeGraphReadError> {
    port.open(CodeGraphReadRequest::new(
        context,
        observed_at,
        Arc::clone(&cancellation),
    ))
    .await?
    .reader_with_cancellation(context, observed_at, cancellation)
}

fn all_code_graph_symbols(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
) -> Result<Vec<CodeGraphSymbolSummaryV1>, ()> {
    const MAX_SYMBOLS: usize = 500_000;
    const PAGE_SIZE: usize = 4_096;
    let mut after = None;
    let mut symbols = Vec::new();
    loop {
        let page = graph
            .symbols_page(after.as_ref(), PAGE_SIZE, Arc::clone(&cancellation))
            .map_err(|_| ())?;
        if symbols.len().saturating_add(page.symbols.len()) > MAX_SYMBOLS {
            return Err(());
        }
        after = page.symbols.last().map(|symbol| symbol.occurrence.clone());
        symbols.extend(page.symbols);
        if !page.has_more {
            return Ok(symbols);
        }
    }
}

fn symbol_at_location(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
    file: &str,
    line_1based: u32,
) -> Result<Option<CodeGraphSymbolSummaryV1>, ()> {
    let line = line_1based.checked_sub(1).ok_or(())?;
    let mut enclosing = graph
        .symbols_in_logical_file(file, 100_000, cancellation)
        .map_err(|_| ())?
        .into_iter()
        .filter(|symbol| {
            symbol.metadata.as_ref().is_some_and(|metadata| {
                metadata.line_span > 0
                    && metadata.start_line <= line
                    && metadata
                        .start_line
                        .checked_add(metadata.line_span)
                        .is_some_and(|end| line < end)
            })
        })
        .collect::<Vec<_>>();
    enclosing.sort_by(|left, right| {
        left.metadata
            .as_ref()
            .map(|metadata| metadata.line_span)
            .cmp(&right.metadata.as_ref().map(|metadata| metadata.line_span))
            .then(left.occurrence.cmp(&right.occurrence))
    });
    Ok(enclosing.into_iter().next())
}

fn test_annotation_evidence(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
) -> Result<std::collections::HashSet<tracedecay_domain::SymbolOccurrenceId>, ()> {
    let symbols = all_code_graph_symbols(graph, Arc::clone(&cancellation))?;
    let occurrences = symbols
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    let markers = symbols
        .iter()
        .filter(|symbol| {
            symbol.metadata.as_ref().is_some_and(|metadata| {
                metadata.kind == "annotation_usage"
                    && matches!(
                        metadata.simple_name.as_str(),
                        "test" | "wasm_bindgen_test" | "rstest" | "parameterized"
                    )
            })
        })
        .map(|symbol| symbol.occurrence.clone())
        .collect::<std::collections::HashSet<_>>();
    let edges = graph
        .edges_among(
            &occurrences,
            &[tracedecay_domain::RelationEdgeKindV1::Annotates],
            2_000_000,
            cancellation,
        )
        .map_err(|_| ())?;
    Ok(edges
        .into_iter()
        .filter(|edge| markers.contains(&edge.edge.from_occurrence))
        .map(|edge| edge.edge.to_occurrence)
        .collect())
}

fn files_for_occurrences(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
    occurrences: &std::collections::HashSet<tracedecay_domain::SymbolOccurrenceId>,
) -> Result<std::collections::HashSet<String>, ()> {
    occurrences
        .iter()
        .map(|occurrence| {
            graph
                .symbol_summary(occurrence, Arc::clone(&cancellation))
                .map_err(|_| ())?
                .and_then(|symbol| symbol.binding?.logical_path)
                .ok_or(())
        })
        .collect()
}

fn relation_kind_name(kind: tracedecay_domain::RelationEdgeKindV1) -> &'static str {
    match kind {
        tracedecay_domain::RelationEdgeKindV1::Calls => "calls",
        tracedecay_domain::RelationEdgeKindV1::Uses => "uses",
        tracedecay_domain::RelationEdgeKindV1::TypeOf => "type_of",
        tracedecay_domain::RelationEdgeKindV1::Contains => "contains",
        tracedecay_domain::RelationEdgeKindV1::Implements => "implements",
        tracedecay_domain::RelationEdgeKindV1::Extends => "extends",
        tracedecay_domain::RelationEdgeKindV1::Annotates => "annotates",
        tracedecay_domain::RelationEdgeKindV1::Returns => "returns",
        tracedecay_domain::RelationEdgeKindV1::Receives => "receives",
    }
}

pub struct TraceDecayLexicalGrepAuthorityV1 {
    source_runtime: Arc<SourceReadRuntime>,
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
}

impl TraceDecayLexicalGrepAuthorityV1 {
    pub fn new(
        source_runtime: Arc<SourceReadRuntime>,
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
        Box::pin(async move {
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
            // A graph read that fails cannot distinguish "this line is in no
            // symbol" from "the enclosing symbol could not be read", so the
            // page reports itself incomplete rather than attributing the hit
            // to nothing.
            let mut unread_enclosing_symbols = false;
            for hit in scan.hits {
                if context.request.cancellation().is_cancelled() {
                    return PrimitiveOutcomeV1::Cancelled;
                }
                let enclosing = match symbol_at_location(
                    &reader,
                    Arc::clone(&graph_cancellation),
                    &hit.file,
                    hit.line,
                ) {
                    Ok(enclosing) => enclosing,
                    Err(_) => {
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
        })
    }
}

pub struct TraceDecayRedundancyAuthorityV1 {
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
}

impl TraceDecayRedundancyAuthorityV1 {
    pub fn new(code_graph: Arc<dyn CodeGraphProjectionReadPort>) -> Self {
        Self { code_graph }
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
            let _ = (&self.code_graph, request, context.scope_prefix);
            PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::AuthorityFailed(
                "the verified graph generation does not publish redundancy fingerprints".to_owned(),
            ))
        })
    }
}

pub struct TraceDecayTestPrimitivePortV1 {
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
}

impl TraceDecayTestPrimitivePortV1 {
    pub fn new(code_graph: Arc<dyn CodeGraphProjectionReadPort>) -> Self {
        Self { code_graph }
    }
}

impl TestPrimitivePort for TraceDecayTestPrimitivePortV1 {
    fn test_map<'a>(
        &'a self,
        context: TestPrimitivePortContext<'a>,
        request: &'a TestMapPrimitiveRequest,
    ) -> TestPrimitivePortFuture<'a, TestMapPrimitiveResultV1> {
        Box::pin(async move {
            let cancellation = request_graph_cancellation(context.request);
            let Ok(reader) = open_code_graph(
                self.code_graph.as_ref(),
                context.request,
                context.observed_at,
                Arc::clone(&cancellation),
            )
            .await
            else {
                return test_primitive_failed(context);
            };
            let source_nodes = if let Some(file) = request.file.as_deref() {
                let Ok(nodes) =
                    reader.symbols_in_logical_file(file, 100_000, Arc::clone(&cancellation))
                else {
                    return test_primitive_failed(context);
                };
                nodes
            } else if let Some(node_id) = request.node_id.as_deref() {
                let Ok(occurrence) = tracedecay_domain::SymbolOccurrenceId::new(node_id.to_owned())
                else {
                    return test_primitive_failed(context);
                };
                let Ok(node) = reader.symbol_summary(&occurrence, Arc::clone(&cancellation)) else {
                    return test_primitive_failed(context);
                };
                node.into_iter().collect::<Vec<_>>()
            } else {
                return test_primitive_failed(context);
            };
            let Ok(test_evidence) = test_annotation_evidence(&reader, Arc::clone(&cancellation))
            else {
                return test_primitive_failed(context);
            };
            let mut coverage_map = Vec::new();
            let mut uncovered = Vec::new();
            let mut test_files = std::collections::BTreeSet::new();
            let mut unread_symbols = false;
            for node in source_nodes {
                let Some(metadata) = node.metadata.as_ref() else {
                    unread_symbols = true;
                    continue;
                };
                let Some(file_path) = node
                    .binding
                    .as_ref()
                    .and_then(|binding| binding.logical_path.as_deref())
                else {
                    unread_symbols = true;
                    continue;
                };
                if !NodeKind::from_str(&metadata.kind).is_some_and(|kind| kind.is_callable_kind()) {
                    continue;
                }
                // A caller or annotation read that fails leaves this symbol
                // unmeasured. Listing it as uncovered would report a tested
                // function as untested, so it is omitted and the page reports
                // itself partial.
                let Ok(callers) = reader.impact(
                    std::slice::from_ref(&node.occurrence),
                    &[tracedecay_domain::RelationEdgeKindV1::Calls],
                    3,
                    50_000,
                    200_000,
                    Arc::clone(&cancellation),
                ) else {
                    unread_symbols = true;
                    continue;
                };
                if !callers.complete {
                    unread_symbols = true;
                    continue;
                }
                let tests: Vec<TestReferenceV1> = callers
                    .impacted
                    .into_iter()
                    .filter_map(|caller| {
                        let caller_metadata = caller.summary.metadata.as_ref()?.clone();
                        let caller_file = caller
                            .summary
                            .binding
                            .as_ref()
                            .and_then(|binding| binding.logical_path.clone())?;
                        (tracedecay_code_index::is_test_file(&caller_file)
                            || test_evidence.contains(&caller.summary.occurrence))
                        .then_some((caller_metadata, caller_file))
                    })
                    .map(|(metadata, caller_file)| {
                        test_files.insert(caller_file.clone());
                        TestReferenceV1 {
                            test_name: metadata.simple_name,
                            test_file: caller_file,
                            test_line: metadata.start_line as usize,
                        }
                    })
                    .collect();
                if tests.is_empty() {
                    uncovered.push(UncoveredSourceV1 {
                        id: node.occurrence.as_str().to_owned(),
                        name: metadata.simple_name.clone(),
                        file: file_path.to_owned(),
                        line: metadata.start_line as usize,
                    });
                } else {
                    coverage_map.push(TestMapCoverageV1 {
                        source_name: metadata.simple_name.clone(),
                        source_id: node.occurrence.as_str().to_owned(),
                        source_file: file_path.to_owned(),
                        source_line: metadata.start_line as usize,
                        tests,
                    });
                }
            }
            let covered_symbols = coverage_map.len();
            let uncovered_symbols = uncovered.len();
            let result = TestMapPrimitiveResultV1 {
                covered_symbols,
                uncovered_symbols,
                test_files: test_files.into_iter().collect(),
                coverage: coverage_map,
                uncovered,
                total: Some((covered_symbols + uncovered_symbols) as u64),
                next_cursor: None,
            };
            let finished_at = context.observed_at;
            let budget = OperationBudgetUsage::default();
            if unread_symbols {
                TestPrimitivePortOutcome::Partial {
                    result,
                    finished_at,
                    budget,
                }
            } else {
                TestPrimitivePortOutcome::Completed {
                    result,
                    finished_at,
                    budget,
                }
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
            let cancellation = request_graph_cancellation(context.request);
            let Ok(verified) = self
                .code_graph
                .open(CodeGraphReadRequest::new(
                    context.request,
                    context.observed_at,
                    Arc::clone(&cancellation),
                ))
                .await
            else {
                return test_primitive_failed(context);
            };
            let Ok(reader) = verified.reader_with_cancellation(
                context.request,
                context.observed_at,
                Arc::clone(&cancellation),
            ) else {
                return test_primitive_failed(context);
            };
            let Ok(test_annotations) = test_annotation_evidence(&reader, Arc::clone(&cancellation))
            else {
                return test_primitive_failed(context);
            };
            let Ok(files_with_inline_tests) =
                files_for_occurrences(&reader, Arc::clone(&cancellation), &test_annotations)
            else {
                return test_primitive_failed(context);
            };
            let graph = GraphQueryManager::new(&reader, cancellation);
            let Ok(traversal) = collect_affected_test_files(
                &graph,
                &request.files,
                request.maximum_depth,
                custom_glob.as_ref(),
                &files_with_inline_tests,
            )
            .await
            else {
                return test_primitive_failed(context);
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
    source_runtime: Arc<SourceReadRuntime>,
}

impl TraceDecaySourceLinesPortV1 {
    pub fn new(source_runtime: Arc<SourceReadRuntime>) -> Self {
        Self { source_runtime }
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
        let path = self.source_runtime.project_root().join(relative);
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

pub struct TraceDecayHealthPortV1 {
    source_runtime: Arc<SourceReadRuntime>,
}

impl TraceDecayHealthPortV1 {
    pub fn new(source_runtime: Arc<SourceReadRuntime>) -> Self {
        Self { source_runtime }
    }
}

impl OperationalRetrievalPort for TraceDecayHealthPortV1 {
    fn health_read(
        &self,
        _context: &RetrievalPortContext<'_>,
        _request: &HealthReadRequest,
    ) -> RetrievalPortOutcome<HealthReadResult> {
        let serving_db_exists = self.source_runtime.db().canonical_database_path().is_file();
        let status = if serving_db_exists && !self.source_runtime.is_read_only() {
            "ok"
        } else if serving_db_exists {
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

fn public_module_symbols(
    nodes: Vec<CodeGraphSymbolSummaryV1>,
    path: &str,
) -> Result<Vec<SymbolPrimitiveRecord>, ()> {
    let prefix = if path.ends_with('/') {
        path.to_owned()
    } else {
        format!("{path}/")
    };
    let mut pub_nodes: Vec<CodeGraphSymbolSummaryV1> = nodes
        .into_iter()
        .filter(|node| {
            let Some(metadata) = node.metadata.as_ref() else {
                return false;
            };
            let Some(file_path) = node
                .binding
                .as_ref()
                .and_then(|binding| binding.logical_path.as_deref())
            else {
                return false;
            };
            metadata.visibility == "public" && (file_path == path || file_path.starts_with(&prefix))
        })
        .collect();
    pub_nodes.sort_by(|left, right| {
        let left_path = left
            .binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_deref());
        let right_path = right
            .binding
            .as_ref()
            .and_then(|binding| binding.logical_path.as_deref());
        left_path.cmp(&right_path).then(
            left.metadata
                .as_ref()
                .map(|metadata| metadata.start_line)
                .cmp(&right.metadata.as_ref().map(|metadata| metadata.start_line)),
        )
    });
    pub_nodes
        .into_iter()
        .map(|node| symbol_record(node, None))
        .collect()
}

pub struct TraceDecayExtendedPrimitivePortV1 {
    source_runtime: Arc<SourceReadRuntime>,
    code_graph: Arc<dyn CodeGraphProjectionReadPort>,
    database: Database,
    observation_database: Arc<RegisteredGlobalDb>,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    diagnostic_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
    diagnostic_cursors: AuthenticatedDiagnosticCursorAuthorityV1,
}

impl TraceDecayExtendedPrimitivePortV1 {
    fn new(
        source_runtime: Arc<SourceReadRuntime>,
        code_graph: Arc<dyn CodeGraphProjectionReadPort>,
        database: Database,
        observation_database: Arc<RegisteredGlobalDb>,
        code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
        diagnostic_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
        diagnostic_cursors: AuthenticatedDiagnosticCursorAuthorityV1,
    ) -> Self {
        Self {
            source_runtime,
            code_graph,
            database,
            observation_database,
            code_index,
            diagnostic_identity,
            diagnostic_cursors,
        }
    }
}

const STORAGE_STATUS_HISTORY_REVISION_V1: u32 = 1;
const MAX_STORAGE_STATUS_HISTORY_POINTS: usize = 128;

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct DurableStorageStatusHistoryV1 {
    revision: u32,
    project_id: Option<String>,
    store_path: String,
    samples: Vec<StorageStatusHistoryPointV1>,
}

fn storage_status_history_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn storage_status_history_path(
    data_root: &Path,
    project_id: Option<&str>,
    store_path: &str,
) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(project_id.unwrap_or_default().as_bytes());
    digest.update([0]);
    digest.update(store_path.as_bytes());
    data_root
        .join("storage-status-history-v1")
        .join(format!("{}.json", hex::encode(digest.finalize())))
}

fn update_storage_status_history(
    history_path: &Path,
    project_id: Option<String>,
    store_path: String,
    database_bytes: u64,
    observed_at: i64,
) -> (Vec<StorageStatusHistoryPointV1>, String) {
    let Ok(_guard) = storage_status_history_lock().lock() else {
        return (
            vec![StorageStatusHistoryPointV1 {
                observed_at,
                database_bytes,
            }],
            "current_sample_only_history_lock_failed".to_owned(),
        );
    };
    let stored = std::fs::read(history_path).ok();
    let restored = stored
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<DurableStorageStatusHistoryV1>(bytes).ok())
        .filter(|durable| {
            durable.revision == STORAGE_STATUS_HISTORY_REVISION_V1
                && durable.project_id == project_id
                && durable.store_path == store_path
                && durable
                    .samples
                    .windows(2)
                    .all(|pair| pair[0].observed_at <= pair[1].observed_at)
        });
    let invalid_stored_history = stored.is_some() && restored.is_none();
    let mut history = restored.map_or_else(Vec::new, |durable| durable.samples);
    let clock_regressed = history
        .last()
        .is_some_and(|sample| sample.observed_at > observed_at);
    if clock_regressed {
        history.clear();
    }
    // The series records store-size changes, not reads. Re-observing the same
    // size adds no information, and appending per read would both amplify
    // writes and make this evidence operation non-idempotent, so the same
    // authorized status read could never agree across CLI, MCP, and HTTP.
    let recorded = history
        .last()
        .is_none_or(|sample| sample.database_bytes != database_bytes);
    if recorded {
        history.push(StorageStatusHistoryPointV1 {
            observed_at,
            database_bytes,
        });
        if history.len() > MAX_STORAGE_STATUS_HISTORY_POINTS {
            history.drain(..history.len() - MAX_STORAGE_STATUS_HISTORY_POINTS);
        }
    }
    let persisted = !recorded
        || serde_json::to_vec_pretty(&DurableStorageStatusHistoryV1 {
            revision: STORAGE_STATUS_HISTORY_REVISION_V1,
            project_id,
            store_path,
            samples: history.clone(),
        })
        .ok()
        .and_then(|bytes| {
            std::fs::create_dir_all(history_path.parent().unwrap_or_else(|| Path::new(".")))
                .ok()?;
            let temp =
                history_path.with_extension(format!("tmp-{}-{observed_at}", std::process::id()));
            tracedecay_runtime_core::storage::PrivateStoreIo::write_file_atomically(
                history_path,
                &temp,
                &bytes,
            )
            .ok()
        })
        .is_some();
    let coverage = if !persisted {
        "current_sample_only_history_persistence_failed"
    } else if invalid_stored_history {
        "durable_project_store_history_reset_invalid"
    } else if clock_regressed {
        "durable_project_store_history_reset_clock_regression"
    } else {
        "durable_project_store_history"
    };
    (history, coverage.to_owned())
}

/// Canonical storage-status owner used by the application operation and its
/// dashboard projection. History is durable and scope-bound, so growth does
/// not reset when the daemon or dashboard restarts.
pub(crate) async fn canonical_storage_status(
    database: &Database,
    source_runtime: &SourceReadRuntime,
    project_id: &ProjectId,
    include_details: bool,
) -> StorageStatusPrimitiveResult {
    let read_only = source_runtime.is_read_only();
    let database_path = database.canonical_database_path();
    let store_path = database_path.display().to_string();
    let file_bytes = database_path.metadata().ok().map(|metadata| metadata.len());
    let page_counts = database.storage_page_counts().await.ok();
    let page_size_bytes = page_counts.and_then(|(page_size, _, _)| u32::try_from(page_size).ok());
    let page_count = page_counts.map(|(_, page_count, _)| page_count);
    let freelist_pages = page_counts.map(|(_, _, freelist_pages)| freelist_pages);
    let database_bytes = page_size_bytes
        .zip(page_count)
        .map(|(page_size, pages)| u64::from(page_size).saturating_mul(pages))
        .or(file_bytes);
    let project_id = Some(project_id.as_str().to_owned());
    let status = if database_path.is_file() {
        if read_only { "read_only" } else { "ok" }
    } else {
        "missing_graph_db"
    };
    let details = if include_details && page_counts.is_none() {
        vec!["project store page telemetry unavailable".to_owned()]
    } else {
        Vec::new()
    };
    let history_path = storage_status_history_path(
        database_path.parent().unwrap_or_else(|| Path::new(".")),
        project_id.as_deref(),
        &store_path,
    );
    let (history, history_coverage) = database_bytes.map_or_else(
        || (Vec::new(), "current_sample_unavailable".to_owned()),
        |bytes| {
            update_storage_status_history(
                &history_path,
                project_id.clone(),
                store_path.clone(),
                bytes,
                now_observed().0,
            )
        },
    );
    StorageStatusPrimitiveResult {
        status: status.to_owned(),
        read_only,
        database_bytes,
        page_size_bytes,
        page_count,
        freelist_pages,
        details,
        project_id,
        store_path: Some(store_path),
        history,
        history_coverage: Some(history_coverage),
    }
}

impl ExtendedPrimitivePort for TraceDecayExtendedPrimitivePortV1 {
    fn qualified_name<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a QualifiedNamePrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, QualifiedNamePrimitiveResult> {
        Box::pin(async move {
            let cancellation = request_graph_cancellation(context.request);
            let Ok(reader) = open_code_graph(
                self.code_graph.as_ref(),
                context.request,
                now_observed(),
                Arc::clone(&cancellation),
            )
            .await
            else {
                return failed(EvidenceDomain::Symbol, now_observed());
            };
            let Ok(nodes) =
                reader.resolve_qualified_name(&request.qualified_name, None, 10_000, cancellation)
            else {
                return failed(EvidenceDomain::Symbol, now_observed());
            };
            let symbols = nodes
                .into_iter()
                .map(|node| symbol_record(node, None))
                .collect::<Result<Vec<_>, _>>();
            let Ok(symbols) = symbols else {
                return failed(EvidenceDomain::Symbol, now_observed());
            };
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
        context: RetrievalPortContext<'a>,
        request: &'a CallChainPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, CallChainPrimitiveResult> {
        Box::pin(async move {
            let cancellation = request_graph_cancellation(context.request);
            let Ok(reader) = open_code_graph(
                self.code_graph.as_ref(),
                context.request,
                now_observed(),
                Arc::clone(&cancellation),
            )
            .await
            else {
                return failed(EvidenceDomain::Graph, now_observed());
            };
            let Ok(from) = tracedecay_domain::SymbolOccurrenceId::new(request.from_node_id.clone())
            else {
                return failed(EvidenceDomain::Graph, now_observed());
            };
            let Ok(to) = tracedecay_domain::SymbolOccurrenceId::new(request.to_node_id.clone())
            else {
                return failed(EvidenceDomain::Graph, now_observed());
            };
            let Ok(path) = reader.shortest_path(
                &from,
                &to,
                &[tracedecay_domain::RelationEdgeKindV1::Calls],
                request.maximum_depth,
                100_000,
                cancellation,
            ) else {
                return failed(EvidenceDomain::Graph, now_observed());
            };
            if !path.complete {
                return evidence_unavailable(
                    EvidenceDomain::Graph,
                    now_observed(),
                    OmissionReason::Unavailable,
                    0,
                );
            }
            let edges = path.path.unwrap_or_default();
            let mut node_ids = vec![from.as_str().to_owned()];
            node_ids.extend(
                edges
                    .iter()
                    .map(|edge| edge.to_occurrence.as_str().to_owned()),
            );
            let edge_kinds = edges
                .into_iter()
                .map(|edge| relation_kind_name(edge.kind).to_owned())
                .collect();
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
        context: RetrievalPortContext<'a>,
        request: &'a FileDependentsPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, FileDependentsPrimitiveResult> {
        Box::pin(async move {
            let observed_at = now_observed();
            let cancellation = request_graph_cancellation(context.request);
            let verified = match self
                .code_graph
                .open(CodeGraphReadRequest::new(
                    context.request,
                    observed_at,
                    Arc::clone(&cancellation),
                ))
                .await
            {
                Ok(verified) => verified,
                Err(error) => {
                    return graph_read_outcome(&error, EvidenceDomain::Graph, observed_at);
                }
            };
            let reader = match verified.reader_with_cancellation(
                context.request,
                observed_at,
                Arc::clone(&cancellation),
            ) {
                Ok(reader) => reader,
                Err(error) => {
                    return graph_read_outcome(&error, EvidenceDomain::Graph, observed_at);
                }
            };
            let query = GraphQueryManager::new(&reader, cancellation);
            let Ok(dependent_files) = query.get_file_dependents(&request.file).await else {
                return evidence_unavailable(
                    EvidenceDomain::Graph,
                    now_observed(),
                    OmissionReason::Unavailable,
                    0,
                );
            };
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
        context: RetrievalPortContext<'a>,
        request: &'a SourceBodyPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, SourceBodyPrimitiveResult> {
        Box::pin(async move {
            let cancellation = request_graph_cancellation(context.request);
            let Ok(reader) = open_code_graph(
                self.code_graph.as_ref(),
                context.request,
                now_observed(),
                Arc::clone(&cancellation),
            )
            .await
            else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let Ok(occurrence) =
                tracedecay_domain::SymbolOccurrenceId::new(request.node_id.clone())
            else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let Ok(Some(node)) = reader.symbol_summary(&occurrence, cancellation) else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let Some(metadata) = node.metadata else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let Some(file) = node.binding.and_then(|binding| binding.logical_path) else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let Some(line_span) = metadata.line_span.checked_sub(1) else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let Some(end_line) = metadata.start_line.checked_add(line_span) else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let path = self.source_runtime.project_root().join(&file);
            let Ok(content) = tokio::fs::read_to_string(&path).await else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let start = metadata.start_line as usize;
            let end = end_line as usize;
            let body = content
                .lines()
                .skip(start)
                .take(end.saturating_sub(start).saturating_add(1))
                .collect::<Vec<_>>()
                .join("\n");
            completed(
                SourceBodyPrimitiveResult {
                    node_id: occurrence.as_str().to_owned(),
                    file,
                    start_line: metadata.start_line.saturating_add(1),
                    end_line: end_line.saturating_add(1),
                    body,
                },
                EvidenceDomain::Source,
                now_observed(),
            )
        })
    }

    fn source_outline<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a SourceOutlinePrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, SourceOutlinePrimitiveResult> {
        Box::pin(async move {
            let cancellation = request_graph_cancellation(context.request);
            let Ok(reader) = open_code_graph(
                self.code_graph.as_ref(),
                context.request,
                now_observed(),
                Arc::clone(&cancellation),
            )
            .await
            else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let Ok(nodes) = reader.symbols_in_logical_file(&request.file, 100_000, cancellation)
            else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            let symbols = nodes
                .into_iter()
                .map(|node| symbol_record(node, None))
                .collect::<Result<Vec<_>, _>>();
            let Ok(symbols) = symbols else {
                return failed(EvidenceDomain::Source, now_observed());
            };
            completed(
                SourceOutlinePrimitiveResult {
                    file: request.file.clone(),
                    symbols,
                },
                EvidenceDomain::Source,
                now_observed(),
            )
        })
    }

    fn module_api<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a ModuleApiPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, ModuleApiPrimitiveResult> {
        Box::pin(async move {
            let cancellation = request_graph_cancellation(context.request);
            let Ok(reader) = open_code_graph(
                self.code_graph.as_ref(),
                context.request,
                now_observed(),
                Arc::clone(&cancellation),
            )
            .await
            else {
                return failed(EvidenceDomain::Symbol, now_observed());
            };
            let Ok(nodes) = all_code_graph_symbols(&reader, cancellation) else {
                return failed(EvidenceDomain::Symbol, now_observed());
            };
            let Ok(symbols) = public_module_symbols(nodes, &request.path) else {
                return failed(EvidenceDomain::Symbol, now_observed());
            };
            completed(
                ModuleApiPrimitiveResult {
                    path: request.path.clone(),
                    symbols,
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
    ) -> ExtendedPrimitiveFuture<'a, FileMetadataPrimitiveResult> {
        Box::pin(async move {
            let mut files = Vec::new();
            for file in &request.files {
                let path = self.source_runtime.project_root().join(file);
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

    fn health_delta<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a HealthDeltaRequest,
    ) -> ExtendedPrimitiveFuture<'a, HealthDeltaResult> {
        Box::pin(async move {
            let observed_at = now_observed();
            let cancellation = request_graph_cancellation(context.request);
            let verified = match self
                .code_graph
                .open(CodeGraphReadRequest::new(
                    context.request,
                    observed_at,
                    Arc::clone(&cancellation),
                ))
                .await
            {
                Ok(verified) => verified,
                Err(error) => {
                    return graph_read_outcome(&error, EvidenceDomain::Operational, observed_at);
                }
            };
            let reader = match verified.reader_with_cancellation(
                context.request,
                observed_at,
                Arc::clone(&cancellation),
            ) {
                Ok(reader) => reader,
                Err(error) => {
                    return graph_read_outcome(&error, EvidenceDomain::Operational, observed_at);
                }
            };
            let query = GraphQueryManager::new(&reader, cancellation);
            match compute_verified_health_delta(
                Some(context.request.scope().project_id.as_str().to_owned()),
                &query,
                self.observation_database.as_ref(),
                request.before_cursor.as_deref(),
                request.path_prefix.as_deref(),
            )
            .await
            {
                Ok(result) => completed(result, EvidenceDomain::Operational, now_observed()),
                Err(_) => evidence_unavailable(
                    EvidenceDomain::Operational,
                    observed_at,
                    OmissionReason::Unavailable,
                    0,
                ),
            }
        })
    }

    fn storage_status<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a StorageStatusPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, StorageStatusPrimitiveResult> {
        Box::pin(async move {
            completed(
                canonical_storage_status(
                    &self.database,
                    self.source_runtime.as_ref(),
                    &context.request.scope().project_id,
                    request.include_details,
                )
                .await,
                EvidenceDomain::Operational,
                now_observed(),
            )
        })
    }

    fn diagnostics<'a>(
        &'a self,
        context: RetrievalPortContext<'a>,
        request: &'a DiagnosticsPrimitiveRequest,
    ) -> ExtendedPrimitiveFuture<'a, DiagnosticsPrimitiveResult> {
        Box::pin(async move {
            let finished_at = now_observed();
            if !(1..=1_000).contains(&request.maximum_diagnostics) {
                return diagnostics_unavailable(finished_at, OmissionReason::Unsupported);
            }
            let Some(identity) = self
                .diagnostic_identity
                .resolve(self.source_runtime.project_root().to_path_buf())
                .await
            else {
                return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
            };
            let scope = context.request.scope();
            if identity.repository() != &scope.repository_id
                || identity.worktree() != Some(&scope.worktree_id)
                || identity.reference() != scope.reference.as_ref()
            {
                return diagnostics_unavailable(finished_at, OmissionReason::Stale);
            }
            let document_path = match &request.scope {
                super::runtime::DiagnosticsPrimitiveScope::Workspace => None,
                super::runtime::DiagnosticsPrimitiveScope::File(path) => {
                    let Some(path) = crate::diagnostics_publication::code_index_logical_path(
                        self.source_runtime.project_root(),
                        path,
                    ) else {
                        return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                    };
                    if identity.file(&path).is_none() {
                        return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                    }
                    Some(path)
                }
                super::runtime::DiagnosticsPrimitiveScope::Package(_) => {
                    return diagnostics_unavailable(finished_at, OmissionReason::Unsupported);
                }
            };
            let current_index = match self
                .code_index
                .current_identity(
                    self.source_runtime.project_root().to_path_buf(),
                    document_path.clone(),
                )
                .await
            {
                Ok(identity) => identity,
                Err(_) => {
                    return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                }
            };
            if current_index.code_generation_id != *identity.generation_id() {
                return diagnostics_unavailable(finished_at, OmissionReason::Stale);
            }
            let query = DiagnosticsQuery::new(self.database.conn());
            let current = query.current_generation().await;
            let Some(current_generation) = current.generation else {
                return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
            };
            if !matches!(current.coverage, DiagnosticQueryCoverage::Complete) {
                return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
            }
            if current_generation != *identity.generation_id() {
                return diagnostics_unavailable(finished_at, OmissionReason::Stale);
            }
            let selected_file = document_path
                .as_deref()
                .and_then(|path| identity.file(path).map(|(file, _)| file));
            let cursor_lane = selected_file.map_or(
                DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
                tracedecay_domain::FileOccurrenceId::as_str,
            );
            let cursor = match request.cursor.as_deref() {
                Some(cursor) => match self.diagnostic_cursors.decode(
                    cursor,
                    context.request,
                    &current_generation,
                    cursor_lane,
                ) {
                    Ok(cursor) => Some(cursor),
                    Err(()) => {
                        return diagnostics_unavailable(finished_at, OmissionReason::Unsupported);
                    }
                },
                None => None,
            };
            let page_request =
                DiagnosticPageRequest::new(request.maximum_diagnostics as usize, cursor);
            let page = match selected_file {
                Some(file) => {
                    query
                        .current_by_file(&current_generation, file, &page_request)
                        .await
                }
                None => {
                    query
                        .current_by_generation(&current_generation, &page_request)
                        .await
                }
            };
            let Ok(page) = page else {
                return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
            };
            match page.coverage {
                DiagnosticQueryCoverage::Complete | DiagnosticQueryCoverage::Truncated => {}
                DiagnosticQueryCoverage::StoreUnavailable { .. } => {
                    return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
                }
            }
            let next_cursor = page
                .next_cursor
                .as_ref()
                .map(|cursor| {
                    self.diagnostic_cursors.encode(
                        cursor,
                        context.request,
                        &current_generation,
                        cursor_lane,
                    )
                })
                .transpose();
            let Ok(next_cursor) = next_cursor else {
                return diagnostics_unavailable(finished_at, OmissionReason::Unavailable);
            };
            let mut diagnostics = Vec::new();
            for diagnostic in page.records {
                if diagnostic.repository != *identity.repository()
                    || diagnostic.worktree.as_ref() != identity.worktree()
                    || diagnostic.reference.as_ref() != identity.reference()
                    || diagnostic.source_revision.as_ref() != identity.source_revision()
                    || diagnostic.generation_id != *identity.generation_id()
                    || !diagnostic.is_current()
                {
                    return diagnostics_unavailable(finished_at, OmissionReason::Stale);
                }
                let Some((logical_path, expected_digest)) = identity
                    .logical_path(&diagnostic.file_occurrence_id)
                    .and_then(|path| identity.file(path).map(|(_, digest)| (path, digest)))
                else {
                    return diagnostics_unavailable(finished_at, OmissionReason::Stale);
                };
                if expected_digest != &diagnostic.content_digest {
                    return diagnostics_unavailable(finished_at, OmissionReason::Stale);
                }
                if selected_file.is_none_or(|file| file == &diagnostic.file_occurrence_id) {
                    diagnostics.push(DiagnosticPrimitiveRecord {
                        logical_path: logical_path.to_owned(),
                        diagnostic,
                    });
                }
            }
            diagnostics_result(
                identity.generation_id().clone(),
                current_index.snapshot_digest,
                diagnostics,
                page.total as u64,
                next_cursor,
                finished_at,
            )
        })
    }
}

/// Derives symbol-graph cursor snapshots from the code index's *current*
/// published generation.
///
/// The generation is resolved per call rather than captured when the runtime
/// mounts. A cached watermark would give two different graph states one cursor
/// identity, so a cursor minted before a publication would keep verifying
/// against the rows that replaced it — the page-set would change underneath the
/// caller with nothing in the answer saying so.
pub struct ProjectSymbolGraphCursorSnapshotAuthority {
    key: SignedCursorKeyRefV1,
    configuration_digest: ManifestDigest,
    project_root: PathBuf,
    scope: ResolvedScope,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
}

fn symbol_graph_snapshot_failure(
    code: &str,
    message: &str,
) -> tracedecay_application::retrieval::PrimitiveFailure {
    tracedecay_application::retrieval::PrimitiveFailure::new(
        tracedecay_application::retrieval::PrimitiveFailureKind::Unavailable,
        code,
        message,
    )
    .unwrap_or_else(|_| panic!("static"))
}

impl SymbolGraphCursorSnapshotAuthority for ProjectSymbolGraphCursorSnapshotAuthority {
    fn snapshot<'a>(
        &'a self,
        context: &'a RequestContext,
        lane: &'a str,
        _observed_at: UtcMicros,
    ) -> super::concrete::SymbolGraphCursorSnapshotFuture<'a> {
        Box::pin(async move {
            let graph_identity = self
                .code_index
                .current_identity(self.project_root.clone(), None)
                .await
                .map_err(|_| {
                    symbol_graph_snapshot_failure(
                        "application.symbol-graph.identity",
                        "could not read the current symbol-graph identity",
                    )
                })?
                .admit_for_scope(&self.scope)
                .map_err(|failure| {
                    symbol_graph_snapshot_failure(
                        "application.symbol-graph.scope",
                        &format!(
                            "the current symbol-graph identity was not admitted \
                             for the scope: {}",
                            failure.class()
                        ),
                    )
                })?;
            let code_generation_id = graph_identity.code_generation_id.clone();
            // Every component of the published generation's address folds into
            // the identity, so any republication — even one that leaves the
            // generation sequence alone — produces a different snapshot and
            // therefore refuses cursors minted before it.
            //
            // Where each part is bound decides how a refusal is *typed*, and
            // the cursor codec checks the request binding before the
            // watermarks. Binding the generation into the request digest would
            // therefore report an ordinary publication as a wrong request —
            // indistinguishable from a forged cursor — so the generation rides
            // the watermarks (a mismatch there is already typed stale) and only
            // the finer content digests ride the configuration binding, which
            // is checked last and so speaks only for a republication that left
            // the generation sequence unmoved.
            let graph_snapshot_digest = canonical_sha256(&(
                "tracedecay.symbol-graph.snapshot.v1",
                graph_identity.head_commit_id.as_str(),
                graph_identity.code_generation_id.as_str(),
                graph_identity.snapshot_digest.as_str(),
                graph_identity.invalidation_digest.as_str(),
                graph_identity.snapshot_content_digest.as_str(),
            ))
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.identity",
                    "could not derive the current symbol-graph snapshot digest",
                )
            })?;
            // The snapshot identity is what a cursor is verified against on the
            // next request, so it is derived from the authorization and lane that
            // must still hold at resume time. A per-request correlation id would
            // both fail the digest binding and make every resume a different
            // request.
            let request_digest = canonical_sha256(&(
                "tracedecay.symbol-graph.cursor.v1",
                context.actor(),
                context.grant().revision,
                &context.grant().digest,
                &context.grant().issuer,
                &context.grant().allowed_capabilities,
                &context.grant().allowed_use_cases,
                context.grant().disclosure,
                lane,
            ))
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.request",
                    "could not derive the symbol-graph cursor request digest",
                )
            })?;
            let request = TemporalSnapshotRequest::new(
                SessionId::new("session.daemon.primitive").map_err(|_| {
                    symbol_graph_snapshot_failure(
                        "application.symbol-graph.session",
                        "could not mint primitive session id",
                    )
                })?,
                context.scope().scope_digest.as_str(),
                request_digest.as_str(),
                context.grant().digest.as_str(),
                TemporalModeV1::Current,
                RetrievalGrainV1::Occurrence,
            )
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.snapshot",
                    "could not build temporal snapshot request",
                )
            })?;
            let watermark = graph_identity.generation.max(1);
            let temporal = TemporalExecutionSnapshot::new_authorized(
                request,
                TemporalWatermarks {
                    generation: 1,
                    source: watermark,
                    projection: watermark,
                    index: watermark,
                    summary: watermark,
                },
                KernelVersions {
                    schema: 1,
                    ranking: 1,
                    configuration_digest: BindingDigest::new(
                        "configuration_digest",
                        canonical_sha256(&(
                            "tracedecay.symbol-graph.cursor-configuration.v1",
                            self.configuration_digest.as_str(),
                            graph_snapshot_digest.as_str(),
                            code_generation_id.as_str(),
                        ))
                        .map_err(|_| {
                            symbol_graph_snapshot_failure(
                                "application.symbol-graph.configuration",
                                "could not bind the symbol-graph snapshot configuration",
                            )
                        })?
                        .as_str(),
                    )
                    .map_err(|_| {
                        symbol_graph_snapshot_failure(
                            "application.symbol-graph.configuration",
                            "invalid configuration digest",
                        )
                    })?,
                },
                Some(self.key.clone()),
                ValidatedAuthorization::Authorized,
            )
            .map_err(|_| {
                symbol_graph_snapshot_failure(
                    "application.symbol-graph.snapshot",
                    "could not authorize temporal snapshot",
                )
            })?;
            Ok(SymbolGraphCursorSnapshot::new(temporal, code_generation_id))
        })
    }
}

pub struct TraceDecayAffectedTestsPortV1 {
    project_id: Option<ProjectId>,
    attribution: Option<Arc<dyn GenerationTestAttributionJoinReadPort + Send + Sync>>,
}

impl TraceDecayAffectedTestsPortV1 {
    pub fn new(project_id: ProjectId, generation: CodeGenerationId) -> Self {
        Self::from_binding(Some(project_id), generation, None)
    }

    pub fn with_generation_attribution(
        project_id: ProjectId,
        generation: CodeGenerationId,
        attribution: Arc<dyn GenerationTestAttributionJoinReadPort + Send + Sync>,
    ) -> Self {
        Self::from_binding(Some(project_id), generation, Some(attribution))
    }

    fn from_binding(
        project_id: Option<ProjectId>,
        _generation: CodeGenerationId,
        attribution: Option<Arc<dyn GenerationTestAttributionJoinReadPort + Send + Sync>>,
    ) -> Self {
        Self {
            project_id,
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
        if self.project_id.as_ref() != Some(&context.request.scope().project_id) {
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
            context.request.scope().clone(),
            attribution.read_test_attribution(&request.generation),
            finished_at,
        )
    }
}

/// How many tables the storage detail lines name before summarising the rest.
#[cfg(test)]
const STORAGE_TABLE_DETAIL_LIMIT: usize = 10;

/// Renders per-table byte attribution for the graph store.
///
/// Without this, a store total is one opaque number and no claim about which
/// table holds the bytes can be reproduced through the product. A read the
/// runtime cannot serve reports that it could not be sampled, never an absent
/// or zero line that would read as "no table holds any bytes".
#[cfg(test)]
fn largest_table_details(
    tables: tracedecay_runtime_core::errors::Result<Vec<(String, u64)>>,
) -> Vec<String> {
    let mut tables = match tables {
        Ok(tables) => tables,
        Err(error) => return vec![format!("table sizes could not be sampled: {error}")],
    };
    if tables.is_empty() {
        return vec!["table sizes reported no tables".to_owned()];
    }
    tables.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let total: u64 = tables.iter().map(|(_, bytes)| bytes).sum();
    let remainder = tables.len().saturating_sub(STORAGE_TABLE_DETAIL_LIMIT);
    let mut details = vec![format!(
        "table bytes total {total} across {} tables",
        tables.len()
    )];
    details.extend(
        tables
            .iter()
            .take(STORAGE_TABLE_DETAIL_LIMIT)
            .map(|(table, bytes)| format!("table {table} holds {bytes} bytes")),
    );
    if remainder > 0 {
        details.push(format!("{remainder} smaller tables not listed"));
    }
    details
}

fn attributed_tests_outcome(
    request: &AffectedTestsRequest,
    scope: ResolvedScope,
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
    let mut attributions = Vec::new();
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
        if let GenerationTestJoinDispositionV1::Current { evidence_class } = &record.disposition {
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
            attributions.push(AffectedTestAttributionV1 {
                test: test_occurrence.occurrence_id.clone(),
                evidence_class: *evidence_class,
            });
        } else {
            matching_incomplete = true;
            if matches!(
                record.disposition,
                GenerationTestJoinDispositionV1::StaleEvidence
                    | GenerationTestJoinDispositionV1::UnknownUnsupported
            ) && record.test_occurrence.as_ref().is_some_and(|occurrence| {
                occurrence.occurrence_id == record.attribution.test_occurrence
            }) {
                attributions.push(AffectedTestAttributionV1 {
                    test: record.attribution.test_occurrence.clone(),
                    evidence_class: record.attribution.evidence_class,
                });
            }
        }
    }
    tests.sort();
    tests.dedup();
    attributions.sort_by(|left, right| {
        (&left.test, left.evidence_class).cmp(&(&right.test, right.evidence_class))
    });
    attributions.dedup();

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
        Some(AffectedTestsResult {
            tests,
            attributions,
        }),
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
        Some(EvidenceAuthority {
            evidence_id: EvidenceIdentity::new(format!(
                "evidence.test-attribution.{}",
                join.test_watermark
                    .evidence_digest
                    .as_str()
                    .trim_start_matches("sha256:")
            ))
            .unwrap_or_else(|_| panic!("validated attribution digest yields evidence identity")),
            source_kind: "test_attribution".to_owned(),
            producer: "code_index".to_owned(),
            scope,
            revision: join.test_watermark.attribution_revision.clone(),
            horizon: None,
        }),
        Some(join.test_watermark.evidence_digest.clone()),
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
    evidence_authority: Option<EvidenceAuthority>,
    watermark_digest: Option<ManifestDigest>,
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
            watermark_digest,
            freshness,
        },
        evidence_authorities: evidence_authority.into_iter().collect(),
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
        page: PageState::first_page(PRIMITIVE_SORT_CONTRACT.clone(), 1, eligible, returned)
            .unwrap_or_else(|_| panic!("affected-tests page")),
        finished_at,
        budget: OperationBudgetUsage::default(),
        cancellation: None,
    }
}

/// Owned authorities and admitted project state required to open the complete
/// application primitive runtime.
pub struct ProductionPrimitiveOpenRequestV1 {
    source_runtime: Arc<SourceReadRuntime>,
    code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
    ignored_dependency_admission: Option<Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1>>,
    session_db: Arc<RegisteredGlobalDb>,
    temporal: Arc<dyn TemporalRetrievalPort + Send + Sync>,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    diagnostic_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
    access: ProjectSourceAccessSnapshot,
    admitted_root_uri: String,
    operation_events: OperationEventAuthority,
}

impl ProductionPrimitiveOpenRequestV1 {
    pub fn new(
        source_runtime: Arc<SourceReadRuntime>,
        code_graph: Arc<dyn crate::graph::CodeGraphProjectionReadPort>,
        ignored_dependency_admission: Option<Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1>>,
        session_db: Arc<RegisteredGlobalDb>,
        temporal: Arc<dyn TemporalRetrievalPort + Send + Sync>,
        code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
        diagnostic_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
        access: ProjectSourceAccessSnapshot,
        admitted_root_uri: String,
        operation_events: OperationEventAuthority,
    ) -> Self {
        Self {
            source_runtime,
            code_graph,
            ignored_dependency_admission,
            session_db,
            temporal,
            code_index,
            diagnostic_identity,
            access,
            admitted_root_uri,
            operation_events,
        }
    }
}

/// Opens the complete owned application primitive runtime from production authorities.
pub async fn open_production_primitive_runtime(
    request: ProductionPrimitiveOpenRequestV1,
) -> Result<PrimitiveProjectRuntime, ApplicationContractError> {
    let ProductionPrimitiveOpenRequestV1 {
        source_runtime,
        code_graph,
        ignored_dependency_admission,
        session_db,
        temporal,
        code_index,
        diagnostic_identity,
        access,
        admitted_root_uri,
        operation_events,
    } = request;
    let database = source_runtime.db().clone();
    let project_root = source_runtime.project_root().to_path_buf();
    let scope = access.scope.clone();
    let configuration_digest = access.configuration_digest.clone();
    let key = session_db
        .as_ref()
        .ensure_active_session_cursor_key_result()
        .await
        .map_err(|_| ApplicationContractError::Inconsistent {
            field: "application primitive session cursor key",
        })?;
    let read = session_db.as_ref().read_snapshot().await.map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "application primitive session cursor snapshot",
        }
    })?;
    let authenticator = Arc::new(
        GlobalDbCursorKeyProvider::from_registered_key_ref(&read, key.clone())
            .await
            .map_err(|_| ApplicationContractError::Inconsistent {
                field: "application primitive session cursor authenticator",
            })?,
    );
    let snapshots = Arc::new(ProjectSymbolGraphCursorSnapshotAuthority {
        key: key.clone(),
        configuration_digest: configuration_digest.clone(),
        project_root: project_root.clone(),
        scope: scope.clone(),
        code_index: Arc::clone(&code_index),
    });
    let cursors: Arc<dyn SymbolGraphCursorPort> = Arc::new(
        AuthenticatedSymbolGraphCursorAdapter::new(snapshots, Arc::clone(&authenticator)),
    );
    let test_run_scope: Arc<dyn ManagedTestRunCurrentScopePort> =
        Arc::new(ProductionManagedTestRunCurrentScope::new(
            project_root,
            scope.clone(),
            Arc::clone(&code_index),
        ));
    let extended = Arc::new(TraceDecayExtendedPrimitivePortV1::new(
        Arc::clone(&source_runtime),
        Arc::clone(&code_graph),
        database.clone(),
        Arc::clone(&session_db),
        code_index,
        diagnostic_identity,
        AuthenticatedDiagnosticCursorAuthorityV1 {
            key,
            configuration_digest,
            authenticator,
        },
    ));
    open_primitive_project_runtime(
        database,
        Arc::clone(&source_runtime),
        Arc::clone(&code_graph),
        cursors,
        ignored_dependency_admission,
        Arc::new(TraceDecayTestPrimitivePortV1::new(Arc::clone(&code_graph))),
        Arc::new(TraceDecayLexicalGrepAuthorityV1::new(
            Arc::clone(&source_runtime),
            Arc::clone(&code_graph),
        )),
        Arc::new(TraceDecayRedundancyAuthorityV1::new(Arc::clone(
            &code_graph,
        ))),
        temporal,
        Arc::new(TraceDecaySourceLinesPortV1::new(Arc::clone(
            &source_runtime,
        ))),
        Arc::new(TraceDecayHealthPortV1::new(Arc::clone(&source_runtime))),
        extended,
        scope,
        access,
        admitted_root_uri,
        operation_events,
        test_run_scope,
    )
}

pub fn admitted_root_uri_for_project(
    project_root: &Path,
) -> Result<String, ApplicationContractError> {
    let uri =
        Url::from_file_path(project_root).map_err(|()| ApplicationContractError::Inconsistent {
            field: "application primitive admitted root URI",
        })?;
    Ok(uri.to_string())
}

pub fn locator_digest_for_project(
    project_root: &Path,
) -> Result<ManifestDigest, ApplicationContractError> {
    // Linked worktrees share one retained project/configuration authority.
    // Bind that authority to their canonical Git common directory, while
    // independent clones retain distinct locators. Non-Git projects fall back
    // to their canonical root.
    let repository_locator = tracedecay_runtime_core::worktree::git_common_dir(project_root)
        .unwrap_or_else(|| {
            project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.to_path_buf())
        });
    canonical_sha256(&(
        "tracedecay.project-open.repository-locator.v2",
        repository_locator.to_string_lossy().as_ref(),
    ))
    .map_err(|_| ApplicationContractError::Inconsistent {
        field: "application primitive project locator digest",
    })
}

#[cfg(test)]
mod locator_digest_tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "TraceDecay Test")
            .env("GIT_AUTHOR_EMAIL", "test@tracedecay.local")
            .env("GIT_COMMITTER_NAME", "TraceDecay Test")
            .env("GIT_COMMITTER_EMAIL", "test@tracedecay.local")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn linked_worktrees_share_repository_locator_but_independent_repositories_do_not() {
        let temporary = TempDir::new().expect("temporary root");
        let primary = temporary.path().join("primary");
        let linked = temporary.path().join("linked");
        let independent = temporary.path().join("independent");
        std::fs::create_dir_all(&primary).expect("primary root");
        std::fs::create_dir_all(&independent).expect("independent root");

        git(&primary, &["init", "-b", "main", "--quiet"]);
        std::fs::write(primary.join("README.md"), "primary\n").expect("fixture");
        git(&primary, &["add", "README.md"]);
        git(&primary, &["commit", "-m", "fixture", "--quiet"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "feature/linked",
                linked.to_str().expect("linked path"),
                "HEAD",
            ],
        );
        git(&independent, &["init", "-b", "main", "--quiet"]);

        let primary_digest = locator_digest_for_project(&primary).expect("primary locator digest");
        let linked_digest = locator_digest_for_project(&linked).expect("linked locator digest");
        let independent_digest =
            locator_digest_for_project(&independent).expect("independent locator digest");

        assert_eq!(linked_digest, primary_digest);
        assert_ne!(independent_digest, primary_digest);
    }
}

#[cfg(test)]
mod unavailable_evidence_tests {
    use super::*;

    #[test]
    fn generic_failure_preserves_unknown_domain_coverage() {
        let outcome: RetrievalPortOutcome<()> = failed(EvidenceDomain::Graph, UtcMicros(1));
        let coverage = &outcome.evidence().coverage;

        assert!(coverage.validate().is_ok());
        assert_eq!(
            coverage.domains,
            vec![CoverageDomainState {
                domain: EvidenceDomain::Graph,
                completeness: CoverageCompleteness::Unknown,
            }]
        );
    }

    #[test]
    fn diagnostic_unavailability_preserves_unknown_domain_coverage() {
        let outcome = diagnostics_unavailable(UtcMicros(1), OmissionReason::Unavailable);
        let coverage = &outcome.evidence().coverage;

        assert!(coverage.validate().is_ok());
        assert_eq!(
            coverage.domains,
            vec![CoverageDomainState {
                domain: EvidenceDomain::Diagnostic,
                completeness: CoverageCompleteness::Unknown,
            }]
        );
    }
}

#[cfg(test)]
mod storage_table_detail_tests {
    use super::{STORAGE_TABLE_DETAIL_LIMIT, largest_table_details};

    #[test]
    fn tables_are_ranked_by_bytes_and_the_tail_is_counted() {
        let tables = (0..STORAGE_TABLE_DETAIL_LIMIT + 3)
            .map(|index| (format!("t{index:02}"), (index as u64 + 1) * 100))
            .collect();

        let details = largest_table_details(Ok(tables));

        assert_eq!(
            details.first().map(String::as_str),
            Some("table bytes total 9100 across 13 tables")
        );
        assert_eq!(
            details.get(1).map(String::as_str),
            Some("table t12 holds 1300 bytes"),
            "the largest table must lead"
        );
        assert_eq!(
            details.last().map(String::as_str),
            Some("3 smaller tables not listed")
        );
    }

    #[test]
    fn an_unsampled_store_says_so_instead_of_reporting_no_bytes() {
        let details = largest_table_details(Err(
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                message: "reader lease timed out".to_owned(),
                operation: "sample graph-store table sizes".to_owned(),
            },
        ));

        assert_eq!(details.len(), 1);
        assert!(
            details[0].starts_with("table sizes could not be sampled: "),
            "unexpected detail: {}",
            details[0]
        );
    }

    #[test]
    fn a_store_with_no_tables_is_distinct_from_an_unsampled_store() {
        assert_eq!(
            largest_table_details(Ok(Vec::new())),
            vec!["table sizes reported no tables".to_owned()]
        );
    }
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
        SessionCursorKeyIdV1, SessionCursorVersionV1, SymbolOccurrenceId,
        TestAttributionEvidenceClassV1, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, SchemaId, UseCaseId};

    use super::*;
    use tracedecay_code_index::provider::{
        GenerationProviderCoverageV1, GenerationProviderReadV1,
        GenerationTestAttributionJoinReadPort,
    };
    use tracedecay_code_index::test_attribution::{
        GenerationTestJoinCoverageV1, GenerationTestJoinDispositionV1,
        GenerationTestJoinPartialReasonV1, GenerationTestJoinRecordV1, GenerationTestJoinV1,
        TestAttributionJoinInputCoverageV1, TestAttributionOccurrenceV1,
        TestAttributionWatermarkV1,
    };
    use tracedecay_temporal_query::ports::InMemoryCursorAuthenticator;

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

    struct GenerationSwitchingFixture {
        current: CodeGenerationId,
        read: GenerationProviderReadV1<GenerationTestJoinV1>,
    }

    impl GenerationTestAttributionJoinReadPort for GenerationSwitchingFixture {
        fn read_test_attribution(
            &self,
            generation: &CodeGenerationId,
        ) -> GenerationProviderReadV1<GenerationTestJoinV1> {
            if generation == &self.current {
                self.read.clone()
            } else {
                GenerationProviderReadV1::new(
                    ProviderEvaluationStateV1::Unavailable,
                    GenerationProviderCoverageV1::Unavailable,
                    None,
                )
                .expect("unavailable provider read")
            }
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

    fn cursor_context(project: &str) -> RequestContext {
        let scope = ResolvedScope::new(
            ProjectId::new(project).expect("project"),
            RepositoryId::new("repository.diagnostics").expect("repository"),
            WorktreeId::new("worktree.diagnostics").expect("worktree"),
            Some(RefId::new("refs/heads/diagnostics").expect("reference")),
        )
        .expect("scope");
        let capability = CapabilityId::new("capability.diagnostics").expect("capability");
        let use_case = UseCaseId::new("use-case.diagnostics").expect("use case");
        let expires_at = UtcMicros(i64::MAX / 2);
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.diagnostics").expect("grant"),
            1,
            digest('a'),
            ActorId::new("actor.diagnostics.issuer").expect("issuer"),
            UtcMicros(1),
            expires_at,
            scope.clone(),
            BTreeSet::from([capability]),
            BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        RequestContext::new(
            ActorId::new("actor.diagnostics.requester").expect("actor"),
            scope,
            grant,
            RequestId::new("request.diagnostics").expect("request"),
            Deadline::new(expires_at).expect("deadline"),
            CancellationContext::active("cancel.diagnostics").expect("cancellation"),
        )
        .expect("context")
    }

    #[test]
    fn diagnostic_cursor_binds_scope_generation_and_lane() {
        let key = SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new("cursor.diagnostics").expect("key"),
            version: SessionCursorVersionV1::new(1).expect("version"),
        };
        let authenticator =
            InMemoryCursorAuthenticator::new(key.clone(), vec![7_u8; 32]).expect("authenticator");
        let authority = AuthenticatedDiagnosticCursorAuthorityV1 {
            key,
            configuration_digest: digest('c'),
            authenticator: Arc::new(authenticator),
        };
        let context = cursor_context("project.diagnostics");
        let current_generation = generation("generation.diagnostics.1");
        let query_cursor =
            DiagnosticQueryCursor::decode("dq1:anchor.diagnostic.1").expect("query cursor");
        let encoded = authority
            .encode(
                &query_cursor,
                &context,
                &current_generation,
                DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
            )
            .expect("encode");

        assert_eq!(
            authority
                .decode(
                    encoded.as_str(),
                    &context,
                    &current_generation,
                    DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
                )
                .expect("decode"),
            query_cursor
        );
        assert!(
            authority
                .decode(
                    encoded.as_str(),
                    &cursor_context("project.diagnostics.other"),
                    &current_generation,
                    DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
                )
                .is_err()
        );
        assert!(
            authority
                .decode(
                    encoded.as_str(),
                    &context,
                    &generation("generation.diagnostics.2"),
                    DIAGNOSTIC_CURSOR_LANE_WORKSPACE,
                )
                .is_err()
        );
        assert!(
            authority
                .decode(
                    encoded.as_str(),
                    &context,
                    &current_generation,
                    "file.diagnostics",
                )
                .is_err()
        );
    }

    fn symbol_graph_context(request_id: RequestId) -> RequestContext {
        let scope = ResolvedScope::new(
            ProjectId::new("project.symbol-graph").expect("project"),
            RepositoryId::new("repository.symbol-graph").expect("repository"),
            WorktreeId::new("worktree.symbol-graph").expect("worktree"),
            Some(RefId::new("refs/heads/symbol-graph").expect("reference")),
        )
        .expect("scope");
        let capability = CapabilityId::new("capability.symbol-graph").expect("capability");
        let use_case = UseCaseId::new("use-case.symbol-graph").expect("use case");
        let expires_at = UtcMicros(i64::MAX / 2);
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.symbol-graph").expect("grant"),
            1,
            digest('a'),
            ActorId::new("actor.symbol-graph.issuer").expect("issuer"),
            UtcMicros(1),
            expires_at,
            scope.clone(),
            BTreeSet::from([capability]),
            BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        RequestContext::new(
            ActorId::new("actor.symbol-graph.requester").expect("actor"),
            scope,
            grant,
            request_id,
            Deadline::new(expires_at).expect("deadline"),
            CancellationContext::active("cancel.symbol-graph").expect("cancellation"),
        )
        .expect("context")
    }

    /// Stands in for the daemon-owned code index, publishing whichever
    /// generation the test has most recently made current.
    struct PublishedCodeIndexIdentity {
        scope: ResolvedScope,
        published: Mutex<(CodeGenerationId, ManifestDigest)>,
    }

    impl PublishedCodeIndexIdentity {
        fn publish(&self, generation: &str, snapshot: char) {
            *self.published.lock().expect("published") = (
                CodeGenerationId::new(generation).expect("generation"),
                digest(snapshot),
            );
        }
    }

    impl LspCodeIndexProjectionIdentityPort for PublishedCodeIndexIdentity {
        fn current_identity(
            &self,
            _project_root: PathBuf,
            _document_relative_path: Option<String>,
        ) -> tracedecay_lsp::LspRuntimeFuture<
            Result<
                crate::lsp_runtime::LspCodeIndexProjectionIdentity,
                tracedecay_lsp::LspRuntimeFailure,
            >,
        > {
            let (code_generation_id, snapshot_digest) =
                self.published.lock().expect("published").clone();
            let identity = crate::lsp_runtime::LspCodeIndexProjectionIdentity {
                project: self.scope.project_id.clone(),
                repository: self.scope.repository_id.clone(),
                worktree: Some(self.scope.worktree_id.clone()),
                reference: self.scope.reference.clone(),
                source_revision: Some(
                    tracedecay_domain::CommitId::new("a".repeat(40)).expect("commit"),
                ),
                code_generation_id,
                snapshot_digest,
                invalidation_digest: digest('e'),
                snapshot_content_digest: content('f'),
                document_content_digest: None,
            };
            Box::pin(async move { Ok(identity) })
        }
    }

    fn symbol_graph_scope() -> ResolvedScope {
        ResolvedScope::new(
            ProjectId::new("project.symbol-graph").expect("project"),
            RepositoryId::new("repository.symbol-graph").expect("repository"),
            WorktreeId::new("worktree.symbol-graph").expect("worktree"),
            Some(RefId::new("refs/heads/symbol-graph").expect("reference")),
        )
        .expect("scope")
    }

    fn symbol_graph_cursor_authority(
        key: SignedCursorKeyRefV1,
    ) -> (
        Arc<PublishedCodeIndexIdentity>,
        ProjectSymbolGraphCursorSnapshotAuthority,
    ) {
        let scope = symbol_graph_scope();
        let code_index = Arc::new(PublishedCodeIndexIdentity {
            scope: scope.clone(),
            published: Mutex::new((
                CodeGenerationId::new("generation.symbol-graph.code.11").expect("generation"),
                digest('d'),
            )),
        });
        let authority = ProjectSymbolGraphCursorSnapshotAuthority {
            key,
            configuration_digest: digest('c'),
            project_root: PathBuf::from("/symbol-graph"),
            scope,
            code_index: Arc::clone(&code_index) as Arc<dyn LspCodeIndexProjectionIdentityPort>,
        };
        (code_index, authority)
    }

    /// Production never mints a `sha256:`-prefixed request id, so a snapshot
    /// bound to one could not be built, and a snapshot bound to the
    /// correlation id could never be resumed by the next request. Both
    /// contexts here carry ids minted by the real production surfaces.
    #[tokio::test]
    async fn symbol_graph_cursors_resume_across_production_minted_request_ids() {
        let key = SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new("cursor.symbol-graph").expect("key"),
            version: SessionCursorVersionV1::new(1).expect("version"),
        };
        let authenticator = Arc::new(
            InMemoryCursorAuthenticator::new(key.clone(), vec![9_u8; 32]).expect("authenticator"),
        );
        let (_code_index, snapshots) = symbol_graph_cursor_authority(key);
        let adapter =
            AuthenticatedSymbolGraphCursorAdapter::new(Arc::new(snapshots), authenticator);

        let issuing = symbol_graph_context(
            crate::request_identity::mcp_connection_request_id(
                &serde_json::json!(1),
                "connection.symbol-graph",
            )
            .expect("mcp connection request id"),
        );
        let resuming = symbol_graph_context(
            crate::request_identity::mint_global_request_id(
                crate::request_identity::GlobalRequestSurface::McpFallback,
            )
            .expect("mcp fallback request id"),
        );
        assert_ne!(
            issuing.request_id().as_str(),
            resuming.request_id().as_str(),
            "each request carries its own correlation id"
        );

        let observed_at = now_observed();
        let claim = adapter
            .claim_page(&issuing, "search", None, observed_at)
            .await
            .expect("a production request must be able to claim a page");
        let cursor = adapter
            .finish_page(&issuing, "search", &claim, 3, 8, true, observed_at)
            .await
            .expect("a production request must be able to issue a page cursor")
            .expect("a page with more to serve mints a continuation");
        assert_eq!(
            adapter
                .claim_page(&resuming, "search", Some(&cursor), observed_at)
                .await
                .expect("the next production request must resume the page")
                .offset(),
            3
        );
        assert!(
            adapter
                .claim_page(&resuming, "callers", Some(&cursor), observed_at)
                .await
                .is_err(),
            "a cursor must not resume into another lane"
        );
    }

    /// The whole point of resolving the generation per request: once the code
    /// index publishes a new generation, a cursor minted under the old one is
    /// refused rather than quietly indexing into the replacement's rows.
    #[tokio::test]
    async fn a_cursor_minted_before_a_publication_does_not_resume_after_it() {
        let key = SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new("cursor.symbol-graph").expect("key"),
            version: SessionCursorVersionV1::new(1).expect("version"),
        };
        let authenticator = Arc::new(
            InMemoryCursorAuthenticator::new(key.clone(), vec![9_u8; 32]).expect("authenticator"),
        );
        let (code_index, snapshots) = symbol_graph_cursor_authority(key);
        let adapter =
            AuthenticatedSymbolGraphCursorAdapter::new(Arc::new(snapshots), authenticator);
        let context = symbol_graph_context(
            crate::request_identity::mint_global_request_id(
                crate::request_identity::GlobalRequestSurface::McpFallback,
            )
            .expect("mcp fallback request id"),
        );

        let observed_at = now_observed();
        let claim = adapter
            .claim_page(&context, "search", None, observed_at)
            .await
            .expect("claim page");
        let cursor = adapter
            .finish_page(&context, "search", &claim, 3, 8, true, observed_at)
            .await
            .expect("finish page")
            .expect("continuation");
        assert_eq!(
            adapter
                .claim_page(&context, "search", Some(&cursor), observed_at)
                .await
                .expect("the unchanged generation still resumes")
                .offset(),
            3
        );

        code_index.publish("generation.symbol-graph.code.12", 'd');
        let failure = adapter
            .claim_page(&context, "search", Some(&cursor), observed_at)
            .await
            .expect_err("a superseded generation must refuse the cursor");
        assert_eq!(
            failure.kind,
            tracedecay_application::retrieval::PrimitiveFailureKind::Stale,
            "a cursor from a superseded generation is stale, not a different page"
        );

        // A re-index of the same commit republishes under the same generation
        // sequence with different content. The rows behind the cursor are still
        // gone, so the cursor must still be refused — the sequence is not the
        // whole identity.
        code_index.publish("generation.symbol-graph.code.11", '9');
        assert!(
            adapter
                .claim_page(&context, "search", Some(&cursor), observed_at)
                .await
                .is_err(),
            "a republication at the same sequence must not serve the old page-set"
        );
    }

    #[test]
    fn diagnostic_continuation_is_complete_coverage_not_partial_evidence() {
        let cursor = OpaqueCursor::new("opaque.diagnostics.next").expect("cursor");
        let outcome = diagnostics_result(
            generation("generation.diagnostics.1"),
            digest('b'),
            Vec::new(),
            2,
            Some(cursor.clone()),
            UtcMicros(100),
        );
        let RetrievalPortOutcome::Completed(evidence) = outcome else {
            panic!("bounded pagination must complete");
        };
        assert_eq!(
            evidence.coverage.completeness,
            CoverageCompleteness::Complete
        );
        assert_eq!(evidence.coverage.eligible, Some(2));
        assert_eq!(evidence.page.cursor, Some(PageCursor::from(cursor)));
        assert!(evidence.page.expires_at.is_some());
        assert!(!evidence.payload.expect("payload").findings_cleared);
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
        assert_eq!(evidence.temporal.watermark_digest, Some(digest('f')));
        assert_eq!(evidence.evidence_authorities.len(), 1);
        assert_eq!(
            evidence.evidence_authorities[0].source_kind,
            "test_attribution"
        );
        let payload = evidence.payload.expect("payload");
        assert_eq!(
            payload.tests,
            vec![SymbolOccurrenceId::new("symbol.test").expect("test")]
        );
        assert_eq!(
            payload.attributions,
            vec![AffectedTestAttributionV1 {
                test: SymbolOccurrenceId::new("symbol.test").expect("test"),
                evidence_class: TestAttributionEvidenceClassV1::ConservativeDependencyCandidates,
            }]
        );
    }

    #[test]
    fn attribution_class_is_preserved_without_inference() {
        for evidence_class in [
            TestAttributionEvidenceClassV1::ObservedCoverageCandidates,
            TestAttributionEvidenceClassV1::PredictiveRankedCandidates,
        ] {
            let project_id = ProjectId::new("project.affected-tests").expect("project");
            let generation = generation("generation.affected-tests.1");
            let mut read = complete_read(generation.clone());
            let record = &mut read.evidence.as_mut().expect("join").records[0];
            record.attribution.evidence_class = evidence_class;
            record.disposition = GenerationTestJoinDispositionV1::Current { evidence_class };
            let port = TraceDecayAffectedTestsPortV1::from_binding(
                Some(project_id.clone()),
                generation.clone(),
                Some(Arc::new(AttributionFixture {
                    calls: AtomicUsize::new(0),
                    read,
                })),
            );
            let (context, operation, _) = context(project_id);

            let RetrievalPortOutcome::Completed(evidence) = port.affected_tests(
                &RetrievalPortContext {
                    request: &context,
                    operation: &operation,
                },
                &request(generation),
            ) else {
                panic!("current attribution must complete");
            };
            assert_eq!(
                evidence.payload.expect("payload").attributions[0].evidence_class,
                evidence_class
            );
        }
    }

    #[test]
    fn unknown_attribution_remains_typed_partial() {
        let project_id = ProjectId::new("project.affected-tests").expect("project");
        let generation = generation("generation.affected-tests.1");
        let mut read = complete_read(generation.clone());
        read.provider_state = ProviderEvaluationStateV1::Partial;
        read.coverage = GenerationProviderCoverageV1::Partial {
            examined: 1,
            eligible: 0,
            excluded: 0,
            unknown: 1,
            capped: false,
        };
        let join = read.evidence.as_mut().expect("join");
        join.coverage = GenerationTestJoinCoverageV1::Partial {
            reasons: vec![GenerationTestJoinPartialReasonV1::UnknownUnsupported {
                test_occurrence: SymbolOccurrenceId::new("symbol.test").expect("test"),
            }],
        };
        join.records[0].attribution.evidence_class =
            TestAttributionEvidenceClassV1::UnknownUnsupported;
        join.records[0].disposition = GenerationTestJoinDispositionV1::UnknownUnsupported;
        let port = TraceDecayAffectedTestsPortV1::from_binding(
            Some(project_id.clone()),
            generation.clone(),
            Some(Arc::new(AttributionFixture {
                calls: AtomicUsize::new(0),
                read,
            })),
        );
        let (context, operation, _) = context(project_id);

        let RetrievalPortOutcome::Partial(evidence) = port.affected_tests(
            &RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request(generation),
        ) else {
            panic!("unknown attribution must stay partial");
        };
        let payload = evidence.payload.expect("payload");
        assert!(payload.tests.is_empty());
        assert_eq!(
            payload.attributions[0].evidence_class,
            TestAttributionEvidenceClassV1::UnknownUnsupported
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
    fn port_routes_each_current_generation_instead_of_pinning_open_generation() {
        let project_id = ProjectId::new("project.affected-tests").expect("project");
        let opened_generation = generation("generation.affected-tests.1");
        let current_generation = generation("generation.affected-tests.2");
        let port = TraceDecayAffectedTestsPortV1::from_binding(
            Some(project_id.clone()),
            opened_generation,
            Some(Arc::new(GenerationSwitchingFixture {
                current: current_generation.clone(),
                read: complete_read(current_generation.clone()),
            })),
        );
        let (context, operation, _) = context(project_id);

        let outcome = port.affected_tests(
            &RetrievalPortContext {
                request: &context,
                operation: &operation,
            },
            &request(current_generation),
        );

        assert!(matches!(outcome, RetrievalPortOutcome::Completed(_)));
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

    #[test]
    fn storage_status_history_is_reloaded_from_durable_scope_file() {
        let directory = tempfile::tempdir().expect("history tempdir");
        let history_path = directory.path().join("storage-status-history-v1.json");
        let project_id = Some("project.storage-status".to_owned());
        let store_path = "/project/.tracedecay/graph.db".to_owned();

        let (first, first_coverage) = update_storage_status_history(
            &history_path,
            project_id.clone(),
            store_path.clone(),
            4096,
            1,
        );
        assert_eq!(first.len(), 1);
        assert_eq!(first_coverage, "durable_project_store_history");
        assert!(history_path.is_file());

        let (second, second_coverage) =
            update_storage_status_history(&history_path, project_id, store_path, 8192, 2);
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].database_bytes, 4096);
        assert_eq!(second[1].database_bytes, 8192);
        assert_eq!(second_coverage, "durable_project_store_history");
    }

    #[test]
    fn storage_status_history_records_changes_not_reads() {
        let directory = tempfile::tempdir().expect("history tempdir");
        let history_path = directory.path().join("storage-status-history-v1.json");
        let project_id = Some("project.storage-status".to_owned());
        let store_path = "/project/.tracedecay/graph.db".to_owned();

        let (first, _) = update_storage_status_history(
            &history_path,
            project_id.clone(),
            store_path.clone(),
            4096,
            1,
        );
        let (repeated, repeated_coverage) = update_storage_status_history(
            &history_path,
            project_id.clone(),
            store_path.clone(),
            4096,
            2,
        );

        assert_eq!(first, repeated, "an unchanged store must read idempotently");
        assert_eq!(repeated.len(), 1);
        assert_eq!(repeated[0].observed_at, 1);
        assert_eq!(repeated_coverage, "durable_project_store_history");

        let (changed, _) =
            update_storage_status_history(&history_path, project_id, store_path, 8192, 3);
        assert_eq!(changed.len(), 2);
        assert_eq!(changed[1].database_bytes, 8192);
        assert_eq!(changed[1].observed_at, 3);
    }

    #[test]
    fn storage_status_history_paths_are_store_scope_isolated() {
        let root = Path::new("/profile/projects/project.storage-status");
        let first = storage_status_history_path(
            root,
            Some("project.storage-status"),
            "/project/branches/main/graph.db",
        );
        let second = storage_status_history_path(
            root,
            Some("project.storage-status"),
            "/project/branches/topic/graph.db",
        );

        assert_ne!(first, second);
        assert_eq!(first.parent(), second.parent());
    }

    #[test]
    fn invalid_storage_status_history_is_reset_without_claiming_full_history() {
        let directory = tempfile::tempdir().expect("history tempdir");
        let history_path = directory.path().join("storage-status-history-v1.json");
        std::fs::write(&history_path, b"{not-json").expect("invalid history");

        let (history, coverage) = update_storage_status_history(
            &history_path,
            Some("project.storage-status".to_owned()),
            "/project/.tracedecay/graph.db".to_owned(),
            4096,
            1,
        );

        assert_eq!(history.len(), 1);
        assert_eq!(coverage, "durable_project_store_history_reset_invalid");
    }
}
