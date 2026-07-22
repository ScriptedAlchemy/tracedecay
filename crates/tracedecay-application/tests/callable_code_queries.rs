mod common;

use std::future::Future;
use std::task::{Context, Poll, Waker};

use tracedecay_application::retrieval::{
    SymbolPrimitiveRecord, SymbolRelationRecord, TypeHierarchyRecord,
};
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblemKind, AuthorizationService,
    CALLABLE_CODE_OPERATION_COUNT, CallableCodeOperationKind, CallableCodeQueryFuture,
    CallableCodeQueryPort, CallableCodeQueryService, CodeHierarchyRequest, CodeImpactRequest,
    CodeImplementationsRequest, CodeQueryPage, CodeQueryScope, CodeRelationRequest,
    CodeSignatureRequest, CodeSymbolSearchRequest, CoverageCompleteness, ExactOccurrenceRecord,
    ExactOccurrenceRequest, LexicalOccurrenceRecord, ModuleApiRequest, OpaqueCursor, PageRequest,
    PhraseSearchRequest, QualifiedNameRequest, ResultProjection, RetrievalOrder,
    RetrievalPortContext, RetrievalPortOutcome, RetrievalRequestMeta, SourceMetadataRecord,
    SourceMetadataRequest, callable_code_catalog_contribution, callable_code_handler_descriptors,
    callable_code_operations,
};
use tracedecay_domain::{
    CodeGenerationId, EphemeralSanitizedQueryViewV1, Pr9FallbackSubpayload, PublicRetrieverStatus,
    QueryNormalizationRevision, RetrieverKind, SanitizerRevision, TemporalModeV1, UtcMicros,
};
use tracedecay_policy::authorization::SourceAuthorizationEvaluatorV1;

fn meta() -> RetrievalRequestMeta {
    RetrievalRequestMeta::current(
        PageRequest::first(25).unwrap(),
        ResultProjection::Evidence,
        RetrievalOrder::Relevance,
    )
}

fn scope() -> CodeQueryScope {
    CodeQueryScope::new(
        common::id::<CodeGenerationId>("generation.fixture"),
        Some("crates/tracedecay-application".to_owned()),
    )
    .unwrap()
}

fn query(text: &str) -> EphemeralSanitizedQueryViewV1 {
    EphemeralSanitizedQueryViewV1::sanitize(
        text,
        SanitizerRevision::new("sanitizer.fixture.v1").unwrap(),
        QueryNormalizationRevision::new("normalization.fixture.v1").unwrap(),
    )
    .unwrap()
}

fn fallback() -> Pr9FallbackSubpayload {
    let mut fallback = Pr9FallbackSubpayload {
        profile_id: common::id("profile.pr9.fixture"),
        ordered_candidates: Vec::new(),
        public_pr9_lane_coverage: [
            (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
            (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
            (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
        ]
        .into_iter()
        .collect(),
        freshness: Vec::new(),
        cursor: None,
        digest: common::id(common::SHA256_A),
    };
    fallback.digest = fallback.compute_digest().unwrap();
    fallback
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = Box::pin(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("callable code fixture futures must complete immediately"),
    }
}

#[derive(Clone, Copy)]
enum ExactPortScenario {
    Valid,
    MissingGeneration,
    MismatchedPageCounts,
    InvalidFallback,
    WrongTemporalMode,
}

struct ExactOnlyPort {
    scenario: ExactPortScenario,
}

impl ExactOnlyPort {
    fn outcome(
        &self,
        generation: &CodeGenerationId,
    ) -> RetrievalPortOutcome<CodeQueryPage<ExactOccurrenceRecord>> {
        let mut pr9_fallback = fallback();
        if matches!(self.scenario, ExactPortScenario::InvalidFallback) {
            pr9_fallback.digest = common::id(common::SHA256_B);
        }
        let next_cursor = matches!(self.scenario, ExactPortScenario::Valid)
            .then(|| OpaqueCursor::new("cursor.generation.fixture.page-2").unwrap());
        let page = CodeQueryPage {
            generation: generation.clone(),
            items: Vec::new(),
            total: Some(0),
            next_cursor: next_cursor.clone(),
            pr9_fallback: Some(pr9_fallback),
        };
        let mut evidence = common::evidence(page);
        evidence.temporal.source_generation =
            (!matches!(self.scenario, ExactPortScenario::MissingGeneration))
                .then(|| generation.clone());
        if matches!(self.scenario, ExactPortScenario::WrongTemporalMode) {
            evidence.temporal.requested_mode = TemporalModeV1::AsOf {
                cutoff: UtcMicros(1),
            };
        }
        evidence.coverage.visited = Some(0);
        evidence.coverage.eligible = Some(0);
        evidence.coverage.returned = 0;
        evidence.page.total = Some(0);
        evidence.page.returned = u64::from(matches!(
            self.scenario,
            ExactPortScenario::MismatchedPageCounts
        ));
        evidence.page.cursor = next_cursor;
        RetrievalPortOutcome::Completed(evidence)
    }
}

macro_rules! unused_callable_port_method {
    ($name:ident, $request:ty, $item:ty) => {
        fn $name<'a>(
            &'a self,
            _context: RetrievalPortContext<'a>,
            _request: &'a $request,
        ) -> CallableCodeQueryFuture<'a, $item> {
            panic!("unused callable code fixture method")
        }
    };
}

impl CallableCodeQueryPort for ExactOnlyPort {
    fn exact_occurrence<'a>(
        &'a self,
        _context: RetrievalPortContext<'a>,
        request: &'a ExactOccurrenceRequest,
    ) -> CallableCodeQueryFuture<'a, ExactOccurrenceRecord> {
        let outcome = self.outcome(&request.scope.generation);
        Box::pin(async move { outcome })
    }

    unused_callable_port_method!(phrase_search, PhraseSearchRequest, LexicalOccurrenceRecord);
    unused_callable_port_method!(
        symbol_search,
        CodeSymbolSearchRequest,
        SymbolPrimitiveRecord
    );
    unused_callable_port_method!(qualified_name, QualifiedNameRequest, SymbolPrimitiveRecord);
    unused_callable_port_method!(
        signature_search,
        CodeSignatureRequest,
        SymbolPrimitiveRecord
    );
    unused_callable_port_method!(
        implementations,
        CodeImplementationsRequest,
        SymbolRelationRecord
    );
    unused_callable_port_method!(type_hierarchy, CodeHierarchyRequest, TypeHierarchyRecord);
    unused_callable_port_method!(callers, CodeRelationRequest, SymbolRelationRecord);
    unused_callable_port_method!(callees, CodeRelationRequest, SymbolRelationRecord);
    unused_callable_port_method!(impact, CodeImpactRequest, SymbolPrimitiveRecord);
    unused_callable_port_method!(module_api, ModuleApiRequest, SymbolPrimitiveRecord);
    unused_callable_port_method!(source_metadata, SourceMetadataRequest, SourceMetadataRecord);
}

fn execute_exact(
    scenario: ExactPortScenario,
) -> tracedecay_application::ApplicationResult<CodeQueryPage<ExactOccurrenceRecord>> {
    let operations = callable_code_operations().unwrap();
    let context = common::context(operations.get(CallableCodeOperationKind::ExactOccurrence));
    let service = CallableCodeQueryService::new(
        ExactOnlyPort { scenario },
        AuthorizationService::new(
            common::StaticAuthorizationPort::authorized(),
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operations,
    );
    block_on(service.exact_occurrence(
        &context,
        ExactOccurrenceRequest::new("ApplicationOperation", None, scope(), meta()).unwrap(),
        UtcMicros(2),
    ))
}

#[test]
fn callable_code_requests_are_generation_bound_and_bounded() {
    let exact = ExactOccurrenceRequest::new("ApplicationOperation", None, scope(), meta()).unwrap();
    assert_eq!(exact.scope.generation.as_str(), "generation.fixture");

    let phrase = PhraseSearchRequest::new(
        query("application operation"),
        vec!["application operation".to_owned()],
        scope(),
        meta(),
    )
    .unwrap();
    assert_eq!(phrase.phrases, vec!["application operation".to_owned()]);

    assert!(
        CodeQueryScope::new(
            common::id::<CodeGenerationId>("generation.fixture"),
            Some("../outside".to_owned()),
        )
        .is_err()
    );
    assert!(
        CodeQueryScope::new(
            common::id::<CodeGenerationId>("generation.fixture"),
            Some("x".repeat(4_097)),
        )
        .is_err()
    );
    assert!(PhraseSearchRequest::new(query("empty phrases"), Vec::new(), scope(), meta()).is_err());
    assert!(SourceMetadataRequest::new(Vec::new(), scope(), meta()).is_err());
}

#[test]
fn callable_code_service_requires_generation_bound_temporal_evidence() {
    let problem = execute_exact(ExactPortScenario::MissingGeneration).unwrap_err();
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Stale);
}

#[test]
fn callable_code_service_rejects_page_evidence_count_mismatch() {
    let problem = execute_exact(ExactPortScenario::MismatchedPageCounts).unwrap_err();
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Unavailable);
}

#[test]
fn callable_code_service_classifies_invalid_payload_as_unavailable() {
    let problem = execute_exact(ExactPortScenario::InvalidFallback).unwrap_err();
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Unavailable);
}

#[test]
fn callable_code_service_rejects_non_current_temporal_evidence() {
    let problem = execute_exact(ExactPortScenario::WrongTemporalMode).unwrap_err();
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Unavailable);
}

#[test]
fn callable_code_service_preserves_generation_coverage_cursor_and_fallback() {
    let result = execute_exact(ExactPortScenario::Valid).unwrap();
    let ApplicationOutcome::Evidence(packet) = result.outcome else {
        panic!("callable code query must return evidence");
    };
    assert_eq!(
        packet.temporal.source_generation.as_ref().unwrap().as_str(),
        "generation.fixture"
    );
    assert_eq!(packet.coverage.completeness, CoverageCompleteness::Complete);
    assert_eq!(packet.coverage.returned, 0);
    let page = packet.payload.unwrap();
    assert_eq!(
        page.next_cursor.as_ref().unwrap().as_str(),
        "cursor.generation.fixture.page-2"
    );
    page.pr9_fallback.as_ref().unwrap().validate().unwrap();
}

#[test]
fn callable_code_page_preserves_generation_cursor_and_pr9_fallback() {
    let cursor = OpaqueCursor::new("cursor.generation.fixture.page-2").unwrap();
    let page = CodeQueryPage::<String>::new(
        scope().generation,
        Vec::new(),
        Some(0),
        Some(cursor),
        Some(fallback()),
    )
    .unwrap();

    assert_eq!(page.generation.as_str(), "generation.fixture");
    assert_eq!(page.total, Some(0));
    assert_eq!(
        page.next_cursor.as_ref().unwrap().as_str(),
        "cursor.generation.fixture.page-2"
    );
    page.pr9_fallback.as_ref().unwrap().validate().unwrap();

    let outcome = RetrievalPortOutcome::Completed(common::evidence(page));
    assert_eq!(
        outcome.evidence().coverage.completeness,
        CoverageCompleteness::Complete
    );
    assert!(outcome.evidence().payload.is_some());
}

#[test]
fn callable_code_catalog_is_complete_and_inert_until_root_binding() {
    let contribution = callable_code_catalog_contribution().unwrap();
    let descriptors = callable_code_handler_descriptors().unwrap();
    let operations = callable_code_operations().unwrap();

    assert_eq!(
        CallableCodeOperationKind::ALL.len(),
        CALLABLE_CODE_OPERATION_COUNT
    );
    assert_eq!(
        contribution.capabilities().len(),
        CALLABLE_CODE_OPERATION_COUNT
    );
    assert_eq!(descriptors.len(), CALLABLE_CODE_OPERATION_COUNT);
    assert_eq!(operations.iter().count(), CALLABLE_CODE_OPERATION_COUNT);
    assert!(contribution.bindings().is_empty());
    assert!(
        contribution
            .capabilities()
            .iter()
            .all(|capability| capability.binding_ids().is_empty())
    );

    let declared: Vec<_> = operations
        .iter()
        .map(|(kind, operation)| {
            (
                kind.as_str().to_owned(),
                operation.use_case_id().as_str().to_owned(),
            )
        })
        .collect();
    let expected: Vec<_> = CallableCodeOperationKind::ALL
        .into_iter()
        .map(|kind| {
            let name = kind.as_str();
            (
                name.to_owned(),
                format!("use-case.application.code-query.{}", name.replace('_', "-")),
            )
        })
        .collect();
    assert_eq!(declared, expected);
}
