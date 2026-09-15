//! Production application primitive owners over admitted graph and store authorities.

use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use tracedecay_contracts::retrieval::grep_analysis::PrimitiveCoverageV1;
use tracedecay_contracts::retrieval::{
    RetrievalPortOutcome, TemporalRetrievalPort, TestPrimitivePortContext, TestPrimitivePortOutcome,
};
use tracedecay_contracts::{
    ApplicationContractError, CoverageCompleteness, CoverageDomainState, EvidenceCoverage,
    EvidenceDomain, FreshnessState, Omission, OmissionReason, OpaqueCursor, OperationBudgetUsage,
    PageCursor, PageState, RequestAdmission, RequestContext, RetrievalEvidence, TemporalState,
    now_micros,
};
use tracedecay_domain::{
    CodeGenerationId, ManifestDigest, RetrievalGrainV1, SessionId, SignedCursorKeyRefV1,
    TemporalModeV1, UtcMicros, canonical_sha256,
};
use tracedecay_tool_catalog::SortContractId;
use url::Url;

use super::concrete::AuthenticatedSymbolGraphCursorAdapter;
use super::runtime::{
    DiagnosticPrimitiveRecord, DiagnosticsPrimitiveResult, ManagedTestRunCurrentIdentity,
    ManagedTestRunCurrentIdentityFuture, ManagedTestRunCurrentScopePort, PrimitiveProjectRuntime,
    open_primitive_project_runtime,
};
use super::symbol_graph::SymbolGraphCursorPort;
use crate::code_index::CodeIndexIgnoredDependencyAdmissionPortV1;
use crate::diagnostics_publication::CodeIndexPublicationIdentityPortV1;
use crate::diagnostics_query::DiagnosticQueryCursor;
use crate::lsp_runtime::LspCodeIndexProjectionIdentityPort;
use crate::operation_stream::OperationEventAuthority;
use crate::source_authorization::ProjectSourceAccessSnapshot;
use tracedecay_code_index::graph_projection::{
    CodeGraphInteractiveReader, CodeGraphSymbolSummaryV1,
};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_graph_query::SourceReadContext;
use tracedecay_graph_query::queries::{GraphQueryManager, is_test_marker};
use tracedecay_graph_query::{
    CodeGraphProjectionReadPort, CodeGraphReadError, CodeGraphReadRequest,
};
use tracedecay_session_temporal_store::SessionTemporalCursorKeyProvider;
use tracedecay_temporal_query::cursor::{
    CURSOR_LIFETIME_MICROS, StableSortKey, encode_cursor, verify_cursor,
};
use tracedecay_temporal_query::ports::{
    BindingDigest, KernelVersions, SessionCursorAuthenticator, TemporalExecutionSnapshot,
    TemporalSnapshotRequest, TemporalWatermarks,
};
use tracedecay_temporal_query::resolution::ValidatedAuthorization;

mod affected_tests;
#[cfg(test)]
mod affected_tests_tests;
mod extended_primitive;
mod lexical_grep;
mod managed_test_scope;
#[cfg(test)]
#[path = "production/managed_test_scope_tests.rs"]
mod managed_test_scope_tests;
mod retrieval_ports;
#[cfg(test)]
mod storage_table_detail_tests;
mod symbol_graph_snapshot;
mod test_primitive;
#[cfg(test)]
mod unavailable_evidence_tests;

pub use affected_tests::TraceDecayAffectedTestsPortV1;
pub use extended_primitive::TraceDecayExtendedPrimitivePortV1;
pub use lexical_grep::{TraceDecayLexicalGrepAuthorityV1, TraceDecayRedundancyAuthorityV1};
use managed_test_scope::ProductionManagedTestRunCurrentScope;
pub use retrieval_ports::{TraceDecayHealthPortV1, TraceDecaySourceLinesPortV1};
pub use symbol_graph_snapshot::ProjectSymbolGraphCursorSnapshotAuthority;
pub use test_primitive::TraceDecayTestPrimitivePortV1;

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

fn empty_primitive_page() -> PageState {
    PageState {
        sort_contract_id: PRIMITIVE_SORT_CONTRACT.clone(),
        sort_revision: 1,
        total: Some(0),
        returned: 0,
        cursor: None,
        expires_at: None,
    }
}

fn primitive_page(
    total: Option<u64>,
    returned: u64,
) -> Result<PageState, ApplicationContractError> {
    PageState::first_page(PRIMITIVE_SORT_CONTRACT.clone(), 1, total, returned)
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
        page: empty_primitive_page(),
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
        page: empty_primitive_page(),
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
    let mut page = match primitive_page(Some(total), returned) {
        Ok(page) => page,
        Err(_) => return failed(EvidenceDomain::Diagnostic, finished_at),
    };
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
    now_micros()
}

#[hotpath::measure(label = "usecases.primitives.open_graph", future = true)]
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

#[hotpath::measure(label = "usecases.primitives.graph_census")]
fn all_code_graph_symbols(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
) -> Result<Vec<CodeGraphSymbolSummaryV1>, ()> {
    const PAGE_SIZE: usize = 4_096;
    GraphQueryManager::new(graph, cancellation)
        .page_all_symbols(
            PAGE_SIZE,
            "verified symbol census exceeded its analytical budget",
        )
        .map_err(|_| ())
}

fn logical_file_symbols(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
    file: &str,
) -> Result<Vec<CodeGraphSymbolSummaryV1>, ()> {
    graph
        .symbols_in_logical_file(file, 100_000, cancellation)
        .map_err(|_| ())
}

fn symbol_at_line(
    symbols: &[CodeGraphSymbolSummaryV1],
    line_1based: u32,
) -> Result<Option<CodeGraphSymbolSummaryV1>, ()> {
    let line = line_1based.checked_sub(1).ok_or(())?;
    let mut enclosing = symbols
        .iter()
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
    Ok(enclosing.into_iter().next().cloned())
}

fn test_annotation_evidence(
    graph: &CodeGraphInteractiveReader,
    cancellation: Arc<dyn tracedecay_graph_db::GraphCancellation>,
    cache: &Mutex<
        Option<(
            CodeGenerationId,
            std::collections::HashSet<tracedecay_domain::SymbolOccurrenceId>,
        )>,
    >,
) -> Result<std::collections::HashSet<tracedecay_domain::SymbolOccurrenceId>, ()> {
    let generation = graph.generation().clone();
    if let Ok(guard) = cache.lock()
        && let Some((cached_generation, cached)) = &*guard
        && cached_generation == &generation
    {
        return Ok(cached.clone());
    }
    let symbols = all_code_graph_symbols(graph, Arc::clone(&cancellation))?;
    let occurrences = symbols
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    let markers = symbols
        .iter()
        .filter(|symbol| symbol.metadata.as_ref().is_some_and(is_test_marker))
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
    let evidence = edges
        .into_iter()
        .filter(|edge| markers.contains(&edge.edge.from_occurrence))
        .map(|edge| edge.edge.to_occurrence)
        .collect::<std::collections::HashSet<_>>();
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((generation, evidence.clone()));
    }
    Ok(evidence)
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

/// Owned authorities and admitted project state required to open the complete
/// application primitive runtime.
pub struct ProductionPrimitiveCodeAuthoritiesV1 {
    pub code_graph: Arc<dyn tracedecay_graph_query::CodeGraphProjectionReadPort>,
    pub ignored_dependency_admission: Option<Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1>>,
    pub code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    pub diagnostic_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
}

pub struct ProductionPrimitiveOpenRequestV1 {
    source_runtime: Arc<SourceReadContext>,
    code_graph: Arc<dyn tracedecay_graph_query::CodeGraphProjectionReadPort>,
    ignored_dependency_admission: Option<Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1>>,
    session_db: RegisteredGlobalDbLeaseV1,
    temporal: Arc<dyn TemporalRetrievalPort + Send + Sync>,
    code_index: Arc<dyn LspCodeIndexProjectionIdentityPort>,
    diagnostic_identity: Arc<dyn CodeIndexPublicationIdentityPortV1>,
    access: ProjectSourceAccessSnapshot,
    admitted_root_uri: String,
    operation_events: OperationEventAuthority,
}

impl ProductionPrimitiveOpenRequestV1 {
    pub fn new(
        source_runtime: Arc<SourceReadContext>,
        code: ProductionPrimitiveCodeAuthoritiesV1,
        session_db: RegisteredGlobalDbLeaseV1,
        temporal: Arc<dyn TemporalRetrievalPort + Send + Sync>,
        access: ProjectSourceAccessSnapshot,
        admitted_root_uri: String,
        operation_events: OperationEventAuthority,
    ) -> Self {
        Self {
            source_runtime,
            code_graph: code.code_graph,
            ignored_dependency_admission: code.ignored_dependency_admission,
            session_db,
            temporal,
            code_index: code.code_index,
            diagnostic_identity: code.diagnostic_identity,
            access,
            admitted_root_uri,
            operation_events,
        }
    }
}

/// Opens the complete owned application primitive runtime from production authorities.
#[hotpath::measure(label = "usecases.primitives.open", future = true)]
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
        SessionTemporalCursorKeyProvider::from_registered_key_ref(&read, key.clone())
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
        session_db.clone(),
        code_index,
        Arc::clone(&diagnostic_identity),
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
        Arc::new(TraceDecayRedundancyAuthorityV1),
        temporal,
        Arc::new(TraceDecaySourceLinesPortV1::new(
            Arc::clone(&source_runtime),
            Arc::clone(&diagnostic_identity),
        )),
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
    // The admitted root is published to clients and compared against the
    // spelling each one addresses it through, so it names the root's identity
    // rather than whichever alias the daemon happened to be handed.
    let identity = tracedecay_runtime_core::path_safety::canonical_root_identity(project_root);
    let uri = Url::from_file_path(&identity)
        .or_else(|()| Url::from_file_path(project_root))
        .map_err(|()| ApplicationContractError::Inconsistent {
            field: "application primitive admitted root URI",
        })?;
    Ok(uri.to_string())
}

pub fn locator_digest_for_project(
    project_root: &Path,
) -> Result<ManifestDigest, ApplicationContractError> {
    tracedecay_runtime_core::worktree::locator_digest_for_project(project_root).map_err(|_| {
        ApplicationContractError::Inconsistent {
            field: "application primitive project locator digest",
        }
    })
}
