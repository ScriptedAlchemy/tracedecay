use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_application::retrieval::{
    PrimitiveFailure, PrimitiveFailureKind, SymbolGraphPortOutcome, SymbolGraphPrimitivePort,
};
use tracedecay_application::{OpaqueCursor, RequestAdmission, RequestContext};
use tracedecay_code_index::graph_projection::CodeGraphProjectionError;
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::{GraphCancellation, GraphProperty, GraphPropertyName, NeverCancelled};

use super::{
    CanonicalSymbolGraphAdapter, CodeGraphProjectionReadPort, CodeGraphProjectionStore,
    CodeGraphReadError, CodeGraphReadFuture, CodeGraphReadRequest,
    CodeIndexIgnoredDependencyAdmissionPortV1, RecordingIgnoredDependencyAdmission, ResolvedScope,
    SymbolGraphCursorFuture, SymbolGraphCursorPort, SymbolGraphPageClaim, VerifiedCodeGraphRead,
    adapter, assert_completed_names, assert_failure, current_generation, exact_request, fixture,
    next_generation, port_context, projection_fixture, projection_manifest, search_request,
    store_for_manifest,
};

struct CancelledNow;

impl GraphCancellation for CancelledNow {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[derive(Clone)]
struct CountingProjection {
    scope: ResolvedScope,
    store: Arc<CodeGraphProjectionStore>,
    opens: Arc<AtomicUsize>,
}

impl CodeGraphProjectionReadPort for CountingProjection {
    fn open<'a>(&'a self, request: CodeGraphReadRequest<'a>) -> CodeGraphReadFuture<'a> {
        Box::pin(async move {
            self.opens.fetch_add(1, Ordering::SeqCst);
            if request.context.scope() != &self.scope {
                return Err(CodeGraphReadError::Denied);
            }
            if request.cancellation.is_cancelled() {
                return Err(CodeGraphReadError::Cancelled);
            }
            match request.context.admission_at(request.observed_at) {
                RequestAdmission::Admitted => {}
                RequestAdmission::Cancelled => return Err(CodeGraphReadError::Cancelled),
                RequestAdmission::TimedOut => return Err(CodeGraphReadError::TimedOut),
            }
            VerifiedCodeGraphRead::new(self.scope.clone(), Arc::clone(&self.store))
        })
    }
}

#[tokio::test]
async fn zero_symbol_and_import_candidates_complete_empty_without_a_scheduler() {
    let fixture = fixture();
    let absent_adapter = adapter(&fixture, None);

    let exact = absent_adapter
        .exact_symbol(
            port_context(&fixture),
            &exact_request("MissingCandidate", true, Some("src/client")),
        )
        .await;
    assert_completed_names(exact, &[]);

    let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Ok(
        next_generation(),
    )));
    let search_adapter = adapter(&fixture, Some(scheduler.clone()));
    let request = search_request("MissingCandidate", true, Some("src/client"));
    let search = search_adapter
        .symbol_search(port_context(&fixture), &request)
        .await;
    assert_completed_names(search, &[]);
    assert!(scheduler.calls().is_empty());
}

#[tokio::test]
async fn same_generation_scheduler_success_is_typed_not_advanced() {
    let fixture = fixture();
    let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Ok(fixture
        .generation
        .clone())));
    let adapter = adapter(&fixture, Some(scheduler.clone()));

    let outcome = adapter
        .exact_symbol(
            port_context(&fixture),
            &exact_request("ExternalWidget", true, Some("src/client")),
        )
        .await;

    assert_failure(
        outcome,
        PrimitiveFailureKind::Unavailable,
        "application.symbol-graph.ignored-dependency-generation-not-advanced",
    );
    assert_eq!(scheduler.calls().len(), 1);
}

#[tokio::test]
async fn multiple_candidates_submit_only_the_first_canonical_scoped_row() {
    let fixture = fixture();
    let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
    let verified = fixture
        .graph
        .open(CodeGraphReadRequest::new(
            &fixture.context,
            super::NOW,
            Arc::clone(&cancellation),
        ))
        .await
        .expect("verified fixture graph");
    let reader = verified
        .reader_with_cancellation(&fixture.context, super::NOW, Arc::clone(&cancellation))
        .expect("fixture reader");
    let candidates = reader
        .external_type_import_candidates(
            "ExternalWidget",
            Some("src/client"),
            2,
            Arc::clone(&cancellation),
        )
        .expect("candidate rows");
    assert_eq!(
        candidates.len(),
        2,
        "an out-of-scope row sorted first must not consume the limit"
    );
    assert_eq!(candidates[0], fixture.expected_import);
    assert_eq!(candidates[1].start_line, 15);

    let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Ok(
        next_generation(),
    )));
    let outcome = adapter(&fixture, Some(scheduler.clone()))
        .exact_symbol(
            port_context(&fixture),
            &exact_request("ExternalWidget", true, Some("src/client")),
        )
        .await;

    assert_failure(
        outcome,
        PrimitiveFailureKind::Stale,
        "application.symbol-graph.ignored-dependency-generation-advanced",
    );
    let calls = scheduler.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].imports, vec![fixture.expected_import.clone()]);
}

#[test]
fn candidate_read_failures_preserve_stable_typed_semantics() {
    let generation = current_generation();
    let (store, _) = projection_fixture(&generation);
    let reader = store
        .interactive_reader_with_cancellation(&generation, Arc::new(NeverCancelled))
        .expect("valid reader");
    let cancelled = reader
        .external_type_import_candidates(
            "ExternalWidget",
            Some("src/client"),
            1,
            Arc::new(CancelledNow),
        )
        .expect_err("cancelled candidate read");
    let stale = store
        .interactive_reader_with_cancellation(&next_generation(), Arc::new(NeverCancelled))
        .expect_err("generation mismatch");

    let corrupt_store = corrupt_store(&generation);
    let corrupt_reader = corrupt_store
        .interactive_reader_with_cancellation(&generation, Arc::new(NeverCancelled))
        .expect("corrupt payload is detected by the interactive catalog");
    let corrupt = corrupt_reader
        .external_type_import_candidates(
            "ExternalWidget",
            Some("src/client"),
            1,
            Arc::new(NeverCancelled),
        )
        .expect_err("corrupt candidate read");

    let cases = [
        (
            cancelled,
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-cancelled",
        ),
        (
            CodeGraphProjectionError::DeadlineExceeded,
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-timed-out",
        ),
        (
            stale,
            PrimitiveFailureKind::Stale,
            "application.symbol-graph.ignored-dependency-candidate-generation-stale",
        ),
        (
            corrupt,
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-corrupt",
        ),
        (
            CodeGraphProjectionError::Unavailable("fixture offline".to_owned()),
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-unavailable",
        ),
        (
            CodeGraphProjectionError::BudgetExhausted {
                budget: "read".to_owned(),
                limit: 1,
            },
            PrimitiveFailureKind::Unavailable,
            "application.symbol-graph.ignored-dependency-candidate-read-budget-exhausted",
        ),
    ];

    for (error, expected_kind, expected_code) in cases {
        let failure = super::super::symbol_graph::ignored_dependency_candidate_failure(error);
        assert_eq!(failure.kind, expected_kind);
        assert_eq!(failure.code, expected_code);
    }
}

#[derive(Clone)]
struct MismatchedClaimCursor {
    snapshot: tracedecay_temporal_query::ports::TemporalExecutionSnapshot,
    source_generation: tracedecay_domain::CodeGenerationId,
    finishes: Arc<AtomicUsize>,
}

impl SymbolGraphCursorPort for MismatchedClaimCursor {
    fn claim_page<'a>(
        &'a self,
        _context: &'a RequestContext,
        _lane: &'a str,
        _cursor: Option<&'a OpaqueCursor>,
        _observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, SymbolGraphPageClaim> {
        Box::pin(async move {
            Ok(SymbolGraphPageClaim::new(
                self.snapshot.clone(),
                self.source_generation.clone(),
                0,
            ))
        })
    }

    fn finish_page<'a>(
        &'a self,
        _context: &'a RequestContext,
        _lane: &'a str,
        _claim: &'a SymbolGraphPageClaim,
        _next_offset: usize,
        _total: usize,
        _has_more: bool,
        _observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, Option<OpaqueCursor>> {
        Box::pin(async move {
            self.finishes.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    }
}

#[tokio::test]
async fn mismatched_claim_and_reader_generation_fails_stale_before_query_or_admission() {
    let fixture = fixture();
    let opens = Arc::new(AtomicUsize::new(0));
    let finishes = Arc::new(AtomicUsize::new(0));
    // The corrupt catalog is a query sentinel: any ordinary graph query would
    // fail as corruption instead of producing the required claim-stale result.
    let graph: Arc<dyn CodeGraphProjectionReadPort> = Arc::new(CountingProjection {
        scope: fixture.scope.clone(),
        store: corrupt_store(&fixture.generation),
        opens: Arc::clone(&opens),
    });
    let cursor = MismatchedClaimCursor {
        snapshot: fixture.cursor.snapshot.clone(),
        source_generation: next_generation(),
        finishes: Arc::clone(&finishes),
    };
    let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Ok(
        next_generation(),
    )));
    let scheduler_port: Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1> = scheduler.clone();
    let adapter = CanonicalSymbolGraphAdapter::new(graph, cursor, Some(scheduler_port));

    let outcome = adapter
        .exact_symbol(
            port_context(&fixture),
            &exact_request("ExternalWidget", true, Some("src/client")),
        )
        .await;

    assert_failure(
        outcome,
        PrimitiveFailureKind::Stale,
        "application.symbol-graph.claim-generation-stale",
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1);
    assert_eq!(finishes.load(Ordering::SeqCst), 0);
    assert!(scheduler.calls().is_empty());
}

#[derive(Clone)]
struct AdvancingFinishCursor {
    snapshot: tracedecay_temporal_query::ports::TemporalExecutionSnapshot,
    source_generation: tracedecay_domain::CodeGenerationId,
    finishes: Arc<AtomicUsize>,
}

impl SymbolGraphCursorPort for AdvancingFinishCursor {
    fn claim_page<'a>(
        &'a self,
        _context: &'a RequestContext,
        _lane: &'a str,
        _cursor: Option<&'a OpaqueCursor>,
        _observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, SymbolGraphPageClaim> {
        Box::pin(async move {
            Ok(SymbolGraphPageClaim::new(
                self.snapshot.clone(),
                self.source_generation.clone(),
                0,
            ))
        })
    }

    fn finish_page<'a>(
        &'a self,
        _context: &'a RequestContext,
        _lane: &'a str,
        _claim: &'a SymbolGraphPageClaim,
        _next_offset: usize,
        _total: usize,
        _has_more: bool,
        _observed_at: UtcMicros,
    ) -> SymbolGraphCursorFuture<'a, Option<OpaqueCursor>> {
        Box::pin(async move {
            self.finishes.fetch_add(1, Ordering::SeqCst);
            Err(PrimitiveFailure::new(
                PrimitiveFailureKind::Stale,
                "application.symbol-graph.generation-changed",
                "the claimed graph generation advanced before lazy indexing",
            )
            .expect("failure"))
        })
    }
}

#[tokio::test]
async fn stale_claim_finish_prevents_lazy_scheduler_mutation() {
    let fixture = fixture();
    let finishes = Arc::new(AtomicUsize::new(0));
    let cursor = AdvancingFinishCursor {
        snapshot: fixture.cursor.snapshot.clone(),
        source_generation: fixture.generation.clone(),
        finishes: Arc::clone(&finishes),
    };
    let scheduler = Arc::new(RecordingIgnoredDependencyAdmission::new(Ok(
        next_generation(),
    )));
    let scheduler_port: Arc<dyn CodeIndexIgnoredDependencyAdmissionPortV1> = scheduler.clone();
    let adapter =
        CanonicalSymbolGraphAdapter::new(fixture.graph.clone(), cursor, Some(scheduler_port));

    let outcome = adapter
        .exact_symbol(
            port_context(&fixture),
            &exact_request("ExternalWidget", true, Some("src/client")),
        )
        .await;

    assert_stale_claim(outcome);
    assert_eq!(finishes.load(Ordering::SeqCst), 1);
    assert!(scheduler.calls().is_empty());
}

fn assert_stale_claim<T>(outcome: SymbolGraphPortOutcome<T>) {
    let SymbolGraphPortOutcome::Failed { failure, .. } = outcome else {
        panic!("stale claim must fail before lazy indexing")
    };
    assert_eq!(failure.kind, PrimitiveFailureKind::Stale);
    assert_eq!(failure.code, "application.symbol-graph.generation-changed");
}

fn corrupt_store(
    generation: &tracedecay_domain::CodeGenerationId,
) -> Arc<CodeGraphProjectionStore> {
    let (mut manifest, _) = projection_manifest(generation);
    let property = GraphPropertyName::new("import-record").expect("property");
    let import = manifest
        .entities
        .iter_mut()
        .find(|entity| entity.properties.contains_key(&property))
        .expect("import entity");
    import
        .properties
        .insert(property, GraphProperty::Bytes(b"not-json".to_vec()));
    store_for_manifest(manifest, generation)
}
