mod common;

use std::cell::Cell;
use std::rc::Rc;

use tracedecay_application::{
    ApplicationOutcome, ApplicationProblemKind, AuthorizationService, CancellationContext,
    CancellationObservation, CancellationStage, Deadline, OperationTermination, PageRequest,
    ResultProjection, RetrievalOrder, RetrievalPortOutcome, SymbolRetrievalPort,
    SymbolSearchRequest, SymbolSearchResult, SymbolSearchService,
};
use tracedecay_domain::{
    EphemeralSanitizedQueryViewV1, QueryNormalizationRevision, SanitizerRevision, UtcMicros,
};
use tracedecay_policy::authorization::SourceAuthorizationEvaluatorV1;

struct CountedPort {
    calls: Rc<Cell<usize>>,
}

struct DuringReadCancellationPort;

impl SymbolRetrievalPort for DuringReadCancellationPort {
    fn symbol_search(
        &self,
        _context: &tracedecay_application::RetrievalPortContext<'_>,
        _request: &SymbolSearchRequest,
    ) -> RetrievalPortOutcome<SymbolSearchResult> {
        let mut evidence = common::evidence(SymbolSearchResult {
            pr9_fallback: superfluous_fallback(),
        });
        evidence.cancellation = Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: UtcMicros(3),
        });
        RetrievalPortOutcome::Cancelled(evidence)
    }
}

struct SlowCompletedPort;

impl SymbolRetrievalPort for SlowCompletedPort {
    fn symbol_search(
        &self,
        _context: &tracedecay_application::RetrievalPortContext<'_>,
        _request: &SymbolSearchRequest,
    ) -> RetrievalPortOutcome<SymbolSearchResult> {
        RetrievalPortOutcome::Completed(common::evidence(SymbolSearchResult {
            pr9_fallback: superfluous_fallback(),
        }))
    }
}

fn superfluous_fallback() -> tracedecay_domain::Pr9FallbackSubpayload {
    let mut fallback = tracedecay_domain::Pr9FallbackSubpayload {
        profile_id: common::id("profile.pr9.fixture"),
        ordered_candidates: Vec::new(),
        public_pr9_lane_coverage: std::collections::BTreeMap::new(),
        freshness: Vec::new(),
        cursor: None,
        digest: common::id(common::SHA256_A),
    };
    fallback.digest = fallback.compute_digest().unwrap();
    fallback
}

impl SymbolRetrievalPort for CountedPort {
    fn symbol_search(
        &self,
        _context: &tracedecay_application::RetrievalPortContext<'_>,
        _request: &SymbolSearchRequest,
    ) -> RetrievalPortOutcome<SymbolSearchResult> {
        self.calls.set(self.calls.get() + 1);
        panic!("pre-admission termination must not invoke the retrieval port")
    }
}

fn request() -> SymbolSearchRequest {
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "cancel fixture",
        SanitizerRevision::new("sanitizer.fixture.v1").unwrap(),
        QueryNormalizationRevision::new("normalization.fixture.v1").unwrap(),
    )
    .unwrap();
    SymbolSearchRequest::new(
        query,
        PageRequest::first(10).unwrap(),
        ResultProjection::Evidence,
        RetrievalOrder::Relevance,
    )
    .unwrap()
}

#[test]
fn cancellation_before_admission_returns_a_problem_without_new_work() {
    let operation = common::operation();
    let context = common::context(&operation)
        .with_cancellation(CancellationContext::cancelled("cancel.fixture", UtcMicros(1)).unwrap());
    let calls = Rc::new(Cell::new(0));
    let service = SymbolSearchService::new(
        CountedPort {
            calls: calls.clone(),
        },
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation,
    );

    let problem = service
        .execute(&context, request(), UtcMicros(2))
        .unwrap_err();

    assert_eq!(calls.get(), 0);
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Cancelled);
    assert!(problem.problem.is_pre_admission());
}

#[test]
fn elapsed_deadline_precedes_port_admission() {
    let operation = common::operation();
    let context = common::context(&operation).with_deadline(Deadline::new(UtcMicros(1)).unwrap());
    let calls = Rc::new(Cell::new(0));
    let service = SymbolSearchService::new(
        CountedPort {
            calls: calls.clone(),
        },
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation,
    );

    let problem = service
        .execute(&context, request(), UtcMicros(2))
        .unwrap_err();

    assert_eq!(calls.get(), 0);
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::TimedOut);
    assert!(problem.problem.is_pre_admission());
}

#[test]
fn during_read_cancellation_suppresses_late_payload() {
    let operation = common::operation();
    let context = common::context(&operation);
    let service = SymbolSearchService::new(
        DuringReadCancellationPort,
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation,
    );

    let result = service.execute(&context, request(), UtcMicros(2)).unwrap();

    let ApplicationOutcome::Evidence(packet) = result.outcome else {
        panic!("admitted cancellation must return terminal evidence");
    };
    assert_eq!(
        packet.execution.termination,
        OperationTermination::Cancelled
    );
    assert!(packet.payload.is_none());
}

#[test]
fn deadline_elapsed_during_read_returns_timed_out_receipt() {
    let operation = common::operation();
    let context = common::context(&operation).with_deadline(Deadline::new(UtcMicros(3)).unwrap());
    let service = SymbolSearchService::new(
        SlowCompletedPort,
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation,
    );

    let result = service.execute(&context, request(), UtcMicros(2)).unwrap();

    let ApplicationOutcome::Evidence(packet) = result.outcome else {
        panic!("admitted timeout must return terminal evidence");
    };
    assert_eq!(packet.execution.termination, OperationTermination::TimedOut);
    assert!(packet.payload.is_none());
}
