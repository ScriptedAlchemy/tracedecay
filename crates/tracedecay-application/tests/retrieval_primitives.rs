mod common;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

use tracedecay_application::{
    ApplicationOutcome, AuthorizationService, CoverageCompleteness, OmissionReason,
    OperationTermination, PageRequest, ResultProjection, RetrievalOrder, RetrievalPortOutcome,
    SymbolRetrievalPort, SymbolSearchRequest, SymbolSearchResult, SymbolSearchService,
};
use tracedecay_domain::{
    EphemeralSanitizedQueryViewV1, Pr9FallbackSubpayload, PublicRetrieverStatus,
    QueryNormalizationRevision, RetrieverKind, SanitizerRevision, UtcMicros,
};
use tracedecay_policy::authorization::SourceAuthorizationEvaluatorV1;

struct RecordingSymbolPort {
    calls: Rc<Cell<usize>>,
}

impl SymbolRetrievalPort for RecordingSymbolPort {
    fn symbol_search(
        &self,
        _context: &tracedecay_application::RetrievalPortContext<'_>,
        _request: &SymbolSearchRequest,
    ) -> RetrievalPortOutcome<SymbolSearchResult> {
        self.calls.set(self.calls.get() + 1);
        RetrievalPortOutcome::Completed(common::evidence(SymbolSearchResult {
            pr9_fallback: pr9_fallback(),
        }))
    }
}

fn pr9_fallback() -> Pr9FallbackSubpayload {
    let mut fallback = Pr9FallbackSubpayload {
        profile_id: common::id("profile.pr9.fixture"),
        ordered_candidates: Vec::new(),
        public_pr9_lane_coverage: BTreeMap::from([
            (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
            (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
            (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
        ]),
        freshness: Vec::new(),
        cursor: None,
        digest: common::id(common::SHA256_A),
    };
    fallback.digest = fallback.compute_digest().unwrap();
    fallback
}

#[test]
fn symbol_search_calls_one_port_and_preserves_pr9_fallback_bytes() {
    let operation = common::operation();
    let context = common::context(&operation);
    let calls = Rc::new(Cell::new(0));
    let service = SymbolSearchService::new(
        RecordingSymbolPort {
            calls: calls.clone(),
        },
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation,
    );
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "symbol fixture",
        SanitizerRevision::new("sanitizer.fixture.v1").unwrap(),
        QueryNormalizationRevision::new("normalization.fixture.v1").unwrap(),
    )
    .unwrap();
    let request = SymbolSearchRequest::new(
        query,
        PageRequest::first(10).unwrap(),
        ResultProjection::Evidence,
        RetrievalOrder::Relevance,
    )
    .unwrap();

    let expected_fallback_bytes =
        serde_json::to_vec(&pr9_fallback()).expect("serialize expected PR9 fallback");
    let result = service.execute(&context, request, UtcMicros(2)).unwrap();

    assert_eq!(calls.get(), 1);
    let ApplicationOutcome::Evidence(packet) = result.outcome else {
        panic!("symbol search must return evidence");
    };
    assert_eq!(packet.authority.revalidated_at, UtcMicros(3));
    let payload = packet.payload.expect("completed read has a payload");
    assert_eq!(
        serde_json::to_vec(&payload.pr9_fallback).expect("serialize returned PR9 fallback"),
        expected_fallback_bytes
    );
    payload.pr9_fallback.validate().unwrap();
    assert_eq!(
        payload.pr9_fallback.public_pr9_lane_coverage.len(),
        RetrieverKind::PR9_FALLBACK_LANES.len()
    );
}

#[test]
fn pre_publication_revocation_suppresses_retrieved_evidence() {
    let operation = common::operation();
    let context = common::context(&operation);
    let calls = Rc::new(Cell::new(0));
    let service = SymbolSearchService::new(
        RecordingSymbolPort {
            calls: calls.clone(),
        },
        AuthorizationService::new(
            common::SequencedAuthorizationPort::snapshots([
                common::source_snapshot(common::authorized_source_input()),
                common::source_snapshot(common::source_authorization_input(
                    "project_owner_mismatch",
                )),
            ]),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation,
    );
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "revocation fixture",
        SanitizerRevision::new("sanitizer.fixture.v1").unwrap(),
        QueryNormalizationRevision::new("normalization.fixture.v1").unwrap(),
    )
    .unwrap();
    let request = SymbolSearchRequest::new(
        query,
        PageRequest::first(10).unwrap(),
        ResultProjection::Evidence,
        RetrievalOrder::Relevance,
    )
    .unwrap();

    let result = service.execute(&context, request, UtcMicros(2)).unwrap();

    assert_eq!(calls.get(), 1);
    let ApplicationOutcome::Evidence(packet) = result.outcome else {
        panic!("revoked read must return terminal evidence");
    };
    assert_eq!(packet.execution.termination, OperationTermination::Failed);
    assert_eq!(packet.coverage.completeness, CoverageCompleteness::Unknown);
    assert!(packet.payload.is_none());
    assert!(
        packet
            .omissions
            .iter()
            .all(|omission| omission.reason == OmissionReason::Redacted)
    );
}
