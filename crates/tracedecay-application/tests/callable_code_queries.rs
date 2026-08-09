mod common;

use std::future::Future;
use std::task::{Context, Poll, Waker};

use tracedecay_application::retrieval::{
    CodeFacetRecord, CodeFacetRequest, CodeLexicalField, CodeLexicalFieldFilter,
    CodeNavigationRequest, CodeTimelineRecord, CodeTimelineRequest, SymbolPrimitiveRecord,
    SymbolRelationRecord, TypeHierarchyRecord,
};
use tracedecay_application::{
    ApplicationOperation, ApplicationOutcome, ApplicationProblem, ApplicationProblemKind,
    AuthorityReceipt, AuthorizationService, CALLABLE_CODE_OPERATION_COUNT,
    CallableCodeAuthorizationAdmission, CallableCodeAuthorizationFuture,
    CallableCodeAuthorizationPort, CallableCodeOperationKind, CallableCodeQueryFuture,
    CallableCodeQueryPort, CallableCodeQueryService, CodeHierarchyRequest, CodeImpactRequest,
    CodeImplementationsRequest, CodeQueryPage, CodeQueryScope, CodeRelationRequest,
    CodeSignatureRequest, CodeSymbolSearchRequest, CoverageCompleteness, ExactOccurrenceRecord,
    ExactOccurrenceRequest, LexicalOccurrenceRecord, ModuleApiRequest, OpaqueCursor, PageRequest,
    PhraseSearchRequest, QualifiedNameRequest, RequestContext, ResultProjection, RetrievalOrder,
    RetrievalPortContext, RetrievalPortOutcome, RetrievalRequestMeta, SourceMetadataRecord,
    SourceMetadataRequest, callable_code_catalog_contribution, callable_code_handler_descriptors,
    callable_code_operations,
};
use tracedecay_domain::{
    CodeGenerationId, EphemeralSanitizedQueryViewV1, PublicRetrieverStatus,
    QueryFallbackSubpayload, QueryNormalizationRevision, RetrieverKind, SanitizerRevision,
    TemporalModeV1, UtcMicros,
};
use tracedecay_policy::authorization::SourceAuthorizationEvaluatorV1;
use tracedecay_tool_catalog::{
    AuthorityRequirement, BindingStatus, BindingSurface, LifecycleClass,
};

fn meta() -> RetrievalRequestMeta {
    RetrievalRequestMeta::current(
        PageRequest::first(25).unwrap(),
        ResultProjection::Evidence,
        RetrievalOrder::Relevance,
    )
}

fn scope() -> CodeQueryScope {
    scope_for("generation.fixture")
}

fn scope_for(generation: &str) -> CodeQueryScope {
    CodeQueryScope::new(
        common::id::<CodeGenerationId>(generation),
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

fn fallback() -> QueryFallbackSubpayload {
    let mut fallback = QueryFallbackSubpayload {
        profile_id: common::id("profile.query.fixture"),
        ordered_candidates: Vec::new(),
        public_fallback_lane_coverage: [
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
    ValidCursor,
    UnexpectedCursor,
    ResolvedGeneration,
    MissingGeneration,
    UnavailableWithoutGeneration,
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
        let generation = if matches!(self.scenario, ExactPortScenario::ResolvedGeneration) {
            common::id::<CodeGenerationId>("generation.resolved")
        } else {
            generation.clone()
        };
        let mut query_fallback = fallback();
        if matches!(self.scenario, ExactPortScenario::InvalidFallback) {
            query_fallback.digest = common::id(common::SHA256_B);
        }
        let next_cursor = matches!(
            self.scenario,
            ExactPortScenario::ValidCursor | ExactPortScenario::UnexpectedCursor
        )
        .then(|| OpaqueCursor::new("cursor.generation.fixture.page-2").unwrap());
        let total = u64::from(matches!(self.scenario, ExactPortScenario::ValidCursor));
        let page = CodeQueryPage {
            generation: generation.clone(),
            items: Vec::new(),
            total: Some(total),
            next_cursor: next_cursor.clone(),
            query_fallback: Some(query_fallback),
        };
        let mut evidence = common::evidence(page);
        evidence.temporal.source_generation =
            (!matches!(self.scenario, ExactPortScenario::MissingGeneration))
                .then(|| generation.clone());
        if matches!(
            self.scenario,
            ExactPortScenario::UnavailableWithoutGeneration
        ) {
            evidence.payload = None;
            evidence.temporal.source_generation = None;
            return RetrievalPortOutcome::Unavailable(evidence);
        }
        if matches!(self.scenario, ExactPortScenario::WrongTemporalMode) {
            evidence.temporal.requested_mode = TemporalModeV1::AsOf {
                cutoff: UtcMicros(1),
            };
        }
        evidence.coverage.visited = Some(total);
        evidence.coverage.eligible = Some(total);
        evidence.coverage.returned = 0;
        evidence.page.total = Some(total);
        evidence.page.returned = u64::from(matches!(
            self.scenario,
            ExactPortScenario::MismatchedPageCounts
        ));
        evidence.page.cursor = next_cursor;
        if matches!(self.scenario, ExactPortScenario::ValidCursor) {
            evidence.page.expires_at = Some(UtcMicros(10));
        }
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
    unused_callable_port_method!(facets, CodeFacetRequest, CodeFacetRecord);
    unused_callable_port_method!(timeline, CodeTimelineRequest, CodeTimelineRecord);
    unused_callable_port_method!(declaration, CodeNavigationRequest, SymbolPrimitiveRecord);
    unused_callable_port_method!(definition, CodeNavigationRequest, SymbolPrimitiveRecord);
    unused_callable_port_method!(
        type_definition,
        CodeNavigationRequest,
        SymbolPrimitiveRecord
    );
    unused_callable_port_method!(references, CodeNavigationRequest, SymbolRelationRecord);
}

struct RoutedAuthorization;

impl CallableCodeAuthorizationPort for RoutedAuthorization {
    fn admit<'a>(
        &'a self,
        context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        _observed_at: UtcMicros,
    ) -> CallableCodeAuthorizationFuture<
        'a,
        Result<CallableCodeAuthorizationAdmission, ApplicationProblem>,
    > {
        Box::pin(async move {
            Ok(CallableCodeAuthorizationAdmission::Routed(
                common::authority(context),
            ))
        })
    }

    fn recheck_publication<'a>(
        &'a self,
        context: &'a RequestContext,
        _operation: &'a ApplicationOperation,
        admission: &'a CallableCodeAuthorizationAdmission,
        observed_at: UtcMicros,
    ) -> CallableCodeAuthorizationFuture<'a, Result<AuthorityReceipt, ApplicationProblem>> {
        Box::pin(async move {
            let CallableCodeAuthorizationAdmission::Routed(admission) = admission else {
                panic!("routed authorization admission remains opaque");
            };
            let mut current = common::authority(context);
            assert_eq!(admission.policy, current.policy);
            current.revalidated_at = observed_at;
            Ok(current)
        })
    }
}

fn execute_exact(
    scenario: ExactPortScenario,
) -> tracedecay_application::ApplicationResult<CodeQueryPage<ExactOccurrenceRecord>> {
    execute_exact_in_scope(scenario, scope())
}

fn execute_exact_in_scope(
    scenario: ExactPortScenario,
    scope: CodeQueryScope,
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
        ExactOccurrenceRequest::new("ApplicationOperation", None, scope, meta()).unwrap(),
        UtcMicros(2),
    ))
    .expect("typed callable-code envelope construction succeeds")
}

#[test]
fn callable_code_service_accepts_route_owned_authorization() {
    let operations = callable_code_operations().unwrap();
    let context = common::context(operations.get(CallableCodeOperationKind::ExactOccurrence));
    let service = CallableCodeQueryService::new(
        ExactOnlyPort {
            scenario: ExactPortScenario::Valid,
        },
        RoutedAuthorization,
        operations,
    );

    let result = block_on(service.exact_occurrence(
        &context,
        ExactOccurrenceRequest::new("ApplicationOperation", None, scope(), meta()).unwrap(),
        UtcMicros(2),
    ))
    .expect("typed callable-code envelope construction succeeds")
    .unwrap();
    let ApplicationOutcome::Evidence(packet) = result.outcome else {
        panic!("route-authorized callable query returns evidence");
    };
    assert_eq!(packet.authority.revalidated_at, UtcMicros(3));
}

#[test]
fn callable_code_requests_are_generation_bound_and_bounded() {
    let exact = ExactOccurrenceRequest::new("ApplicationOperation", None, scope(), meta()).unwrap();
    assert_eq!(exact.scope.generation.as_str(), "generation.fixture");

    let phrase = PhraseSearchRequest::new(
        query("application operation"),
        vec!["application operation".to_owned()],
        Vec::new(),
        0,
        scope(),
        meta(),
    )
    .unwrap();
    assert_eq!(phrase.phrases, vec!["application operation".to_owned()]);
    assert_eq!(phrase.fuzzy_budget, 0);

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
    assert!(
        PhraseSearchRequest::new(
            query("empty phrases"),
            Vec::new(),
            Vec::new(),
            0,
            scope(),
            meta()
        )
        .is_err()
    );
    assert!(SourceMetadataRequest::new(Vec::new(), scope(), meta()).is_err());
    assert!(
        PhraseSearchRequest::new(
            query("duplicate fields"),
            vec!["duplicate fields".to_owned()],
            vec![
                CodeLexicalFieldFilter {
                    field: CodeLexicalField::Path,
                    include: true,
                },
                CodeLexicalFieldFilter {
                    field: CodeLexicalField::Path,
                    include: false,
                },
            ],
            0,
            scope(),
            meta(),
        )
        .is_err()
    );
    assert!(
        PhraseSearchRequest::new(
            query("fuzzy bound"),
            vec!["fuzzy bound".to_owned()],
            Vec::new(),
            65,
            scope(),
            meta(),
        )
        .is_err()
    );
}

#[test]
fn callable_code_service_reauthorizes_then_delegates_cursor_to_port() {
    let operations = callable_code_operations().unwrap();
    let context = common::context(operations.get(CallableCodeOperationKind::ExactOccurrence));
    let service = CallableCodeQueryService::new(
        ExactOnlyPort {
            scenario: ExactPortScenario::Valid,
        },
        RoutedAuthorization,
        operations,
    );
    let cursor = OpaqueCursor::new("cursor.unsupported").unwrap();
    let request = ExactOccurrenceRequest::new(
        "ApplicationOperation",
        None,
        scope(),
        RetrievalRequestMeta::current(
            PageRequest::new(25, Some(cursor)).unwrap(),
            ResultProjection::Evidence,
            RetrievalOrder::Relevance,
        ),
    )
    .unwrap();

    let result = block_on(service.exact_occurrence(&context, request, UtcMicros(2)))
        .expect("typed callable-code envelope construction succeeds")
        .unwrap();
    assert!(matches!(result.outcome, ApplicationOutcome::Evidence(_)));
}

#[test]
fn callable_code_service_requires_generation_bound_temporal_evidence() {
    let problem = execute_exact(ExactPortScenario::MissingGeneration).unwrap_err();
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Stale);
}

#[test]
fn callable_code_service_accepts_concrete_generation_for_unpinned_marker() {
    let result = execute_exact_in_scope(
        ExactPortScenario::ResolvedGeneration,
        scope_for("code-generation:unpinned-latest.v1"),
    )
    .unwrap();
    let ApplicationOutcome::Evidence(packet) = result.outcome else {
        panic!("resolved unpinned query must return evidence");
    };
    assert_eq!(
        packet.temporal.source_generation.as_ref().unwrap().as_str(),
        "generation.resolved"
    );
    assert_eq!(
        packet.payload.unwrap().generation.as_str(),
        "generation.resolved"
    );
}

#[test]
fn callable_code_service_rejects_unresolved_unpinned_marker_outcome() {
    let problem = execute_exact_in_scope(
        ExactPortScenario::Valid,
        scope_for("code-generation:unpinned-latest.v1"),
    )
    .unwrap_err();
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Stale);
}

#[test]
fn callable_code_service_preserves_unavailable_when_unpinned_has_no_generation() {
    let result = execute_exact_in_scope(
        ExactPortScenario::UnavailableWithoutGeneration,
        scope_for("code-generation:unpinned-latest.v1"),
    )
    .unwrap();
    let ApplicationOutcome::Evidence(packet) = result.outcome else {
        panic!("unavailable callable code state remains typed evidence");
    };
    assert_eq!(
        packet.execution.termination,
        tracedecay_application::OperationTermination::Unavailable
    );
    assert!(packet.temporal.source_generation.is_none());
    assert!(packet.payload.is_none());
}

#[test]
fn callable_code_service_preserves_exact_equality_for_pinned_request() {
    let problem = execute_exact(ExactPortScenario::ResolvedGeneration).unwrap_err();
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
fn callable_code_service_preserves_generation_coverage_and_fallback() {
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
    assert!(page.next_cursor.is_none());
    page.query_fallback.as_ref().unwrap().validate().unwrap();
}

#[test]
fn callable_code_service_rejects_an_unresumable_port_cursor() {
    let problem = execute_exact(ExactPortScenario::UnexpectedCursor).unwrap_err();
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Unavailable);
    assert_eq!(
        problem.problem.diagnostic.as_ref().unwrap().code,
        "application.code-query.invalid-port-evidence"
    );
}

#[test]
fn callable_code_service_accepts_a_bounded_unexpired_port_cursor() {
    let result = execute_exact(ExactPortScenario::ValidCursor).unwrap();
    let ApplicationOutcome::Evidence(packet) = result.outcome else {
        panic!("valid continuation returns evidence");
    };
    assert_eq!(packet.page.expires_at, Some(UtcMicros(10)));
    assert_eq!(
        packet.page.cursor.as_ref().map(OpaqueCursor::as_str),
        Some("cursor.generation.fixture.page-2")
    );
}

#[test]
fn callable_code_page_preserves_generation_cursor_and_query_fallback() {
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
    page.query_fallback.as_ref().unwrap().validate().unwrap();

    let outcome = RetrievalPortOutcome::Completed(common::evidence(page));
    assert_eq!(
        outcome.evidence().coverage.completeness,
        CoverageCompleteness::Complete
    );
    assert!(outcome.evidence().payload.is_some());
}

#[test]
fn callable_code_catalog_exposes_only_production_owned_transport_bindings() {
    let contribution = callable_code_catalog_contribution().unwrap();
    let descriptors = callable_code_handler_descriptors().unwrap();
    let operations = callable_code_operations().unwrap();

    assert_eq!(
        CallableCodeOperationKind::ALL.len(),
        CALLABLE_CODE_OPERATION_COUNT
    );
    let canonical_equivalents = [
        CallableCodeOperationKind::SymbolSearch,
        CallableCodeOperationKind::QualifiedName,
        CallableCodeOperationKind::SignatureSearch,
        CallableCodeOperationKind::Implementations,
        CallableCodeOperationKind::TypeHierarchy,
        CallableCodeOperationKind::Callers,
        CallableCodeOperationKind::Impact,
        CallableCodeOperationKind::ModuleApi,
        CallableCodeOperationKind::SourceMetadata,
    ];
    let callable_catalog_count = CALLABLE_CODE_OPERATION_COUNT - canonical_equivalents.len();
    assert_eq!(contribution.capabilities().len(), callable_catalog_count);
    assert_eq!(descriptors.len(), callable_catalog_count);
    assert_eq!(operations.iter().count(), CALLABLE_CODE_OPERATION_COUNT);
    for kind in canonical_equivalents {
        let capability_id = format!(
            "capability.application.code-query.{}",
            kind.as_str().replace('_', "-")
        );
        assert!(
            contribution
                .capabilities()
                .iter()
                .all(|capability| capability.capability_id().as_str() != capability_id),
            "{kind:?} is owned by its canonical application surface"
        );
    }
    let reachable = [
        ("exact_occurrence", "code_exact_occurrence"),
        ("phrase_search", "code_phrase_search"),
        ("callees", "code_callees"),
        ("facets", "code_facets"),
        ("timeline", "code_timeline"),
        ("declaration", "code_declaration"),
        ("definition", "code_definition"),
        ("type_definition", "code_type_definition"),
        ("references", "code_references"),
    ];
    let expected_lsp_bindings = 3;
    assert_eq!(
        contribution.bindings().len(),
        reachable.len() * 3 + expected_lsp_bindings
    );
    for capability in contribution.capabilities() {
        assert_eq!(
            capability.authority(),
            AuthorityRequirement::CapabilityGrantWithRevalidation
        );
        assert_eq!(capability.lifecycle(), LifecycleClass::Resumable);
        let pagination = capability
            .pagination()
            .expect("direct callable code query is resumable");
        assert_eq!(pagination.default_page_size(), 10);
        assert_eq!(pagination.maximum_page_size(), 1_000);
        assert_eq!(pagination.cursor_ttl_millis(), 15 * 60 * 1_000);
        let kind = CallableCodeOperationKind::ALL
            .into_iter()
            .find(|kind| {
                capability.capability_id().as_str()
                    == format!(
                        "capability.application.code-query.{}",
                        kind.as_str().replace('_', "-")
                    )
            })
            .expect("capability maps to one callable-code operation");
        let Some((_, surface_operation)) = reachable
            .iter()
            .find(|(operation, _)| *operation == kind.as_str())
        else {
            panic!("{kind:?} must be owned by a canonical equivalent or a callable binding");
        };
        assert!(capability.availability().is_callable());
        assert_eq!(
            capability.profile_eligibility(),
            &[tracedecay_tool_catalog::ProfileId::new("profile.default").unwrap()]
        );
        let expected_binding_count = match kind {
            CallableCodeOperationKind::ExactOccurrence => 5,
            CallableCodeOperationKind::Callees => 4,
            _ => 3,
        };
        assert_eq!(capability.binding_ids().len(), expected_binding_count);
        for surface in [
            BindingSurface::Cli,
            BindingSurface::Mcp,
            BindingSurface::Http,
        ] {
            let surface_name = match surface {
                BindingSurface::Cli => "cli",
                BindingSurface::Mcp => "mcp",
                BindingSurface::Http => "http",
                BindingSurface::Lsp => "lsp",
                BindingSurface::Dashboard => "dashboard",
            };
            let binding = contribution
                .bindings()
                .iter()
                .find(|binding| {
                    binding.capability_id() == capability.capability_id()
                        && binding.surface() == surface
                })
                .expect("reachable operation has one binding per transport");
            assert_eq!(
                binding.binding_id().as_str(),
                format!("binding.{surface_name}.{surface_operation}.v1")
            );
            assert_eq!(binding.operation().as_str(), *surface_operation);
            assert_eq!(binding.status(), &BindingStatus::Current);
            assert!(binding.protocol_revisions().contains(1));
            assert!(!binding.protocol_revisions().contains(2));
            assert!(binding.required_features().is_empty());
            assert!(!binding.is_alias());
            assert!(capability.binding_ids().contains(binding.binding_id()));
        }
    }

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
