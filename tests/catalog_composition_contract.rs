use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use tracedecay::catalog_composition::{
    CatalogCompositionError, build_application_catalog_snapshot, compose_application_catalog,
    validate_application_catalog,
};
use tracedecay_application::handlers::CanonicalApplicationDispatcher;
use tracedecay_application::{
    ApplicationContractError, ApplicationHandlerDescriptor, ApplicationHandlerDescriptors,
    ApplicationOperation, ApplicationResult, AuthorizationPort, AuthorizationPortOutcome,
    AuthorizationRequest, AuthorizationService, CancellationContext, CapabilityGrantId,
    CapabilityGrantSnapshot, Deadline, DisclosureClass, EvidenceCoverage, EvidenceDomain,
    PageRequest, PageState, RequestContext, RequestId, ResolvedScope, ResultContractRef,
    ResultProjection, RetrievalEvidence, RetrievalOrder, RetrievalPortContext,
    RetrievalPortOutcome, SourceAuthorizationSnapshot, SymbolRetrievalPort, SymbolSearchRequest,
    SymbolSearchResult, SymbolSearchService, TemporalState, application_catalog_contributions,
    application_handler_descriptors, retrieval::catalog::symbol_search_contribution,
};
use tracedecay_domain::{
    ActorId, EphemeralSanitizedQueryViewV1, FallbackSubpayloadDigest, FusionProfileId,
    ManifestDigest, Pr9FallbackSubpayload, ProjectId, PublicRetrieverStatus,
    QueryNormalizationRevision, RefId, RepositoryId, RetrieverKind, SanitizerRevision, UtcMicros,
    WorktreeId,
};
use tracedecay_policy::authorization::{
    SourceAuthorizationEvaluatorV1, SourceAuthorizationInputV1, SourceAuthorizationTruthTableV1,
};
use tracedecay_tool_catalog::{
    CapabilityId, ProfileBudget, ProfileId, ProfileKind, SchemaId, SchemaRef, SortContractId,
    UseCaseId,
};

const SHA256_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SOURCE_AUTHORIZATION_TRUTH_TABLES: &str =
    include_str!("../crates/tracedecay-policy/tests/fixtures/source_authorization/core.json");

#[test]
fn root_snapshot_validates_every_application_contribution_against_declared_descriptors() {
    let contributions = application_catalog_contributions().unwrap();
    let handlers = application_handler_descriptors().unwrap();
    let snapshot = build_application_catalog_snapshot().unwrap();

    let contributed_capabilities = contributions
        .iter()
        .flat_map(|contribution| contribution.capabilities())
        .count();
    assert_eq!(contributed_capabilities, handlers.iter().count());
    assert_eq!(snapshot.capabilities().count(), contributed_capabilities);

    for contribution in &contributions {
        for capability in contribution.capabilities() {
            let handler = handlers
                .get(capability.use_case_id())
                .expect("every declared capability has one callable handler descriptor");
            assert_eq!(
                handler.operation().capability_id(),
                capability.capability_id()
            );
            assert_eq!(handler.operation().use_case_id(), capability.use_case_id());
            assert_eq!(handler.request_schema(), capability.request_schema());
            assert_eq!(handler.result_schema(), capability.result_schema());
            assert_eq!(
                capability.availability().is_callable(),
                !capability.profile_eligibility().is_empty(),
                "{} availability and profile eligibility disagree",
                capability.capability_id()
            );
        }
    }

    let symbol_search = CapabilityId::new("capability.retrieval.symbol-search").unwrap();
    assert!(snapshot.capability(&symbol_search).is_some());

    let default_profile = ProfileId::new("profile.default").unwrap();
    assert!(snapshot.profile(&default_profile).is_some());
    assert_eq!(
        snapshot
            .visible_capabilities(&default_profile, &BTreeSet::new())
            .into_iter()
            .map(|capability| capability.capability_id().as_str())
            .collect::<Vec<_>>(),
        vec![
            "capability.application.code-query.callees",
            "capability.application.code-query.exact-occurrence",
            "capability.application.code-query.phrase-search",
            "capability.application.configuration.audit",
            "capability.application.configuration.batch",
            "capability.application.configuration.explain",
            "capability.application.configuration.get",
            "capability.application.configuration.list",
            "capability.application.configuration.observed_state",
            "capability.application.configuration.protected_apply",
            "capability.application.configuration.protected_preview",
            "capability.application.configuration.rollback_apply",
            "capability.application.configuration.rollback_preview",
            "capability.application.configuration.set",
            "capability.application.configuration.unset",
            "capability.application.configuration.write_credential",
            "capability.application.context-scout-budget",
            "capability.application.context-scout-cancel",
            "capability.application.context-scout-capability",
            "capability.application.context-scout-claim",
            "capability.application.context-scout-delivery",
            "capability.application.context-scout-explain",
            "capability.application.context-scout-feedback",
            "capability.application.context-scout-pause",
            "capability.application.context-scout-recent",
            "capability.application.context-scout-resume",
            "capability.application.context-scout-status",
            "capability.application.feedback.affected-tests",
            "capability.application.feedback.ci-failure-localize",
            "capability.application.feedback.diagnostics",
            "capability.application.feedback.expand",
            "capability.application.feedback.get",
            "capability.application.feedback.github-review-ingest",
            "capability.application.feedback.impact",
            "capability.application.feedback.list",
            "capability.application.feedback.proximity",
            "capability.application.feedback.test-results",
            "capability.application.git.apply",
            "capability.application.git.blame",
            "capability.application.git.diff",
            "capability.application.git.history",
            "capability.application.git.hunks",
            "capability.application.git.preview",
            "capability.application.git.status",
            "capability.application.primitive.call-chain",
            "capability.application.primitive.code-callers",
            "capability.application.primitive.code-implementations",
            "capability.application.primitive.code-signature-search",
            "capability.application.primitive.code-type-hierarchy",
            "capability.application.primitive.diagnostics-read",
            "capability.application.primitive.file-dependents",
            "capability.application.primitive.file-metadata",
            "capability.application.primitive.health-read",
            "capability.application.primitive.module-api",
            "capability.application.primitive.qualified-name",
            "capability.application.primitive.session-lookup",
            "capability.application.primitive.source-body",
            "capability.application.primitive.source-lines",
            "capability.application.primitive.source-outline",
            "capability.application.primitive.storage-status",
            "capability.application.retained.fact-feedback",
            "capability.application.retained.fact-store",
            "capability.application.retained.lcm-compress",
            "capability.application.retained.lcm-describe",
            "capability.application.retained.lcm-doctor",
            "capability.application.retained.lcm-expand",
            "capability.application.retained.lcm-expand-query",
            "capability.application.retained.lcm-grep",
            "capability.application.retained.lcm-load-session",
            "capability.application.retained.lcm-preflight",
            "capability.application.retained.lcm-session-boundary",
            "capability.application.retained.lcm-status",
            "capability.application.retained.memory-status",
            "capability.application.retained.message-search",
            "capability.application.retained.session-end",
            "capability.application.retained.session-refresh",
            "capability.application.retained.session-start",
            "capability.application.retained.sessions-for",
            "capability.application.retained.workflows",
            "capability.application.source-edit.ast-grep-rewrite",
            "capability.application.source-edit.insert-at",
            "capability.application.source-edit.insert-at-symbol",
            "capability.application.source-edit.move-symbol",
            "capability.application.source-edit.multi-str-replace",
            "capability.application.source-edit.reconcile",
            "capability.application.source-edit.replace-symbol",
            "capability.application.source-edit.str-replace",
            "capability.retrieval.symbol-search",
        ]
    );
}

#[test]
fn root_snapshot_composes_every_explicit_profile_without_widening_eligibility() {
    let snapshot = build_application_catalog_snapshot().unwrap();
    let expected_profiles = [
        (
            "profile.default",
            ProfileKind::Default,
            ProfileBudget::new(256, 80_000_000, 18_000).unwrap(),
        ),
        (
            "profile.compact",
            ProfileKind::Compact,
            ProfileBudget::COMPACT,
        ),
        (
            "profile.administrative",
            ProfileKind::Administrative,
            ProfileBudget::ADMINISTRATIVE,
        ),
        (
            "profile.host-limited",
            ProfileKind::HostLimited,
            ProfileBudget::HOST_LIMITED,
        ),
    ];

    assert_eq!(snapshot.profiles().count(), expected_profiles.len());
    for (profile_id, kind, budget) in expected_profiles {
        let profile_id = ProfileId::new(profile_id).unwrap();
        let profile = snapshot
            .profile(&profile_id)
            .expect("every explicit application profile is composed");
        let eligible_capability_ids = snapshot
            .capabilities()
            .filter(|capability| {
                capability.availability().is_callable()
                    && capability.profile_eligibility().contains(&profile_id)
            })
            .map(|capability| capability.capability_id().clone())
            .collect::<Vec<_>>();

        assert_eq!(profile.kind(), kind);
        assert_eq!(profile.budget(), budget);
        assert_eq!(profile.capability_ids(), eligible_capability_ids);
        assert_eq!(
            snapshot
                .visible_capabilities(&profile_id, &BTreeSet::new())
                .into_iter()
                .map(|capability| capability.capability_id().clone())
                .collect::<Vec<_>>(),
            eligible_capability_ids,
        );
    }
}

#[test]
fn root_composition_invokes_the_canonical_typed_application_handler() {
    let operation = application_handler_descriptors()
        .unwrap()
        .get(&UseCaseId::new("use-case.retrieval.symbol-search").unwrap())
        .unwrap()
        .operation()
        .clone();
    let context = request_context(&operation);
    let calls = Rc::new(Cell::new(0));
    let service = SymbolSearchService::new(
        RecordingSymbolPort {
            calls: Rc::clone(&calls),
        },
        AuthorizationService::new(
            StaticAuthorizationPort,
            SourceAuthorizationEvaluatorV1::default(),
        ),
        operation.clone(),
    );
    let composition = compose_application_catalog(CanonicalSymbolSearchDispatcher {
        expected_operation: operation,
        service,
        context,
    })
    .unwrap();
    let handler = composition
        .handler(&UseCaseId::new("use-case.retrieval.symbol-search").unwrap())
        .expect("composed catalog retains a handler bound to its dispatcher");

    let result = handler
        .invoke(symbol_search_request())
        .expect("canonical symbol-search service returns evidence");

    assert_eq!(calls.get(), 1);
    assert_eq!(
        result.contract,
        handler.operation().result_contract().clone()
    );
}

#[test]
fn registered_capability_does_not_require_a_catalog_surface_binding() {
    assert_eq!(
        validate_application_catalog(
            &[symbol_search_contribution().unwrap()],
            &ApplicationHandlerDescriptors::new([descriptor_with_contract(
                "capability.retrieval.symbol-search",
                "use-case.retrieval.symbol-search",
                symbol_request_schema(),
                symbol_result_schema(),
            )])
            .unwrap(),
        ),
        Ok(())
    );
}

#[test]
fn mismatched_descriptor_schema_is_rejected() {
    let contribution = symbol_search_contribution().unwrap();
    let cases = [
        (
            descriptor_with_contract(
                "capability.retrieval.symbol-search",
                "use-case.retrieval.symbol-search",
                schema("schema.test.drifted-request", 384),
                symbol_result_schema(),
            ),
            "application capability schema mapping",
        ),
        (
            descriptor_with_contract(
                "capability.retrieval.symbol-search",
                "use-case.retrieval.symbol-search",
                symbol_request_schema(),
                schema("schema.test.drifted-result", 1_024),
            ),
            "application capability schema mapping",
        ),
    ];

    for (descriptor, field) in cases {
        let handlers = ApplicationHandlerDescriptors::new([descriptor]).unwrap();
        assert_eq!(
            validate_application_catalog(std::slice::from_ref(&contribution), &handlers),
            inconsistent(field),
            "descriptor mismatch for {field} must be rejected"
        );
    }
}

#[test]
fn mismatched_descriptor_capability_is_rejected() {
    let contribution = symbol_search_contribution().unwrap();
    let handlers = ApplicationHandlerDescriptors::new([descriptor_with_contract(
        "capability.retrieval.wrong-symbol-search",
        "use-case.retrieval.symbol-search",
        symbol_request_schema(),
        symbol_result_schema(),
    )])
    .unwrap();

    assert_eq!(
        validate_application_catalog(std::slice::from_ref(&contribution), &handlers),
        inconsistent("application capability/use-case mapping")
    );
}

#[test]
fn capability_without_descriptor_is_rejected() {
    assert_eq!(
        validate_application_catalog(
            &[symbol_search_contribution().unwrap()],
            &ApplicationHandlerDescriptors::default(),
        ),
        inconsistent("application capability handler mapping")
    );
}

#[test]
fn orphan_handler_descriptor_is_rejected() {
    let mut descriptors: Vec<_> = application_handler_descriptors()
        .unwrap()
        .iter()
        .cloned()
        .collect();
    descriptors.push(descriptor_with_contract(
        "capability.application.orphan",
        "use-case.application.orphan",
        symbol_request_schema(),
        symbol_result_schema(),
    ));
    let handlers = ApplicationHandlerDescriptors::new(descriptors).unwrap();

    assert_eq!(
        validate_application_catalog(&application_catalog_contributions().unwrap(), &handlers),
        inconsistent("application handler use case")
    );
}

#[test]
fn root_composition_is_deterministic() {
    let first = build_application_catalog_snapshot().unwrap();
    let second = build_application_catalog_snapshot().unwrap();

    assert_eq!(first, second);
    assert_eq!(first.digest(), second.digest());
}

fn descriptor_with_contract(
    capability_id: &str,
    use_case_id: &str,
    request_schema: SchemaRef,
    result_schema: SchemaRef,
) -> ApplicationHandlerDescriptor {
    ApplicationHandlerDescriptor::new(
        ApplicationOperation::new(
            CapabilityId::new(capability_id).unwrap(),
            UseCaseId::new(use_case_id).unwrap(),
            ResultContractRef::from_schema(&result_schema),
            true,
        ),
        request_schema,
        result_schema,
    )
    .unwrap()
}

fn symbol_request_schema() -> SchemaRef {
    schema("schema.application.symbol-search.request", 384)
}

fn symbol_result_schema() -> SchemaRef {
    schema("schema.application.symbol-search.result", 1_024)
}

fn schema(id: &str, maximum_bytes: u32) -> SchemaRef {
    SchemaRef::new(SchemaId::new(id).unwrap(), 1, maximum_bytes).unwrap()
}

fn inconsistent(field: &'static str) -> Result<(), CatalogCompositionError> {
    Err(CatalogCompositionError::Application(
        ApplicationContractError::Inconsistent { field },
    ))
}

struct RecordingSymbolPort {
    calls: Rc<Cell<usize>>,
}

impl SymbolRetrievalPort for RecordingSymbolPort {
    fn symbol_search(
        &self,
        _context: &RetrievalPortContext<'_>,
        _request: &SymbolSearchRequest,
    ) -> RetrievalPortOutcome<SymbolSearchResult> {
        self.calls.set(self.calls.get() + 1);
        RetrievalPortOutcome::Completed(RetrievalEvidence {
            payload: Some(SymbolSearchResult {
                pr9_fallback: pr9_fallback(),
            }),
            temporal: TemporalState::current(UtcMicros(2)),
            evidence_authorities: Vec::new(),
            coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Symbol], 1, 1, 1).unwrap(),
            omissions: Vec::new(),
            scores: Vec::new(),
            contributions: Vec::new(),
            page: PageState::first_page(
                SortContractId::new("sort.symbol.catalog-dispatch.v1").unwrap(),
                1,
                Some(1),
                1,
            )
            .unwrap(),
            finished_at: UtcMicros(3),
            budget: Default::default(),
            cancellation: None,
        })
    }
}

struct StaticAuthorizationPort;

impl AuthorizationPort for StaticAuthorizationPort {
    fn source_authorization_snapshot(
        &self,
        _request: &AuthorizationRequest<'_>,
    ) -> AuthorizationPortOutcome {
        AuthorizationPortOutcome::Snapshot(Box::new(SourceAuthorizationSnapshot::new(
            authorized_source_input(),
            true,
        )))
    }
}

struct CanonicalSymbolSearchDispatcher {
    expected_operation: ApplicationOperation,
    service: SymbolSearchService<
        RecordingSymbolPort,
        StaticAuthorizationPort,
        SourceAuthorizationEvaluatorV1,
    >,
    context: RequestContext,
}

impl CanonicalApplicationDispatcher<SymbolSearchRequest> for CanonicalSymbolSearchDispatcher {
    type Output = ApplicationResult<SymbolSearchResult>;

    fn invoke(
        &self,
        operation: &ApplicationOperation,
        request: SymbolSearchRequest,
    ) -> Self::Output {
        assert_eq!(operation, &self.expected_operation);
        self.service.execute(&self.context, request, UtcMicros(2))
    }
}

fn symbol_search_request() -> SymbolSearchRequest {
    let query = EphemeralSanitizedQueryViewV1::sanitize(
        "symbol fixture",
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

fn request_context(operation: &ApplicationOperation) -> RequestContext {
    let scope = ResolvedScope::new(
        ProjectId::new("project.catalog-dispatch").unwrap(),
        RepositoryId::new("repository.catalog-dispatch").unwrap(),
        WorktreeId::new("worktree.catalog-dispatch").unwrap(),
        Some(RefId::new("refs/heads/main").unwrap()),
    )
    .unwrap();
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.catalog-dispatch").unwrap(),
        1,
        ManifestDigest::new(SHA256_A).unwrap(),
        ActorId::new("actor.issuer").unwrap(),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    RequestContext::new(
        ActorId::new("actor.requester").unwrap(),
        scope,
        grant,
        RequestId::new("request.catalog-dispatch").unwrap(),
        Deadline::new(UtcMicros(500)).unwrap(),
        CancellationContext::active("cancel.catalog-dispatch").unwrap(),
    )
    .unwrap()
}

fn authorized_source_input() -> SourceAuthorizationInputV1 {
    serde_json::from_str::<Vec<SourceAuthorizationTruthTableV1>>(SOURCE_AUTHORIZATION_TRUTH_TABLES)
        .unwrap()
        .into_iter()
        .find(|row| row.name == "project_authorized_live")
        .unwrap()
        .input
}

fn pr9_fallback() -> Pr9FallbackSubpayload {
    let mut fallback = Pr9FallbackSubpayload {
        profile_id: FusionProfileId::new("profile.pr9.catalog-dispatch").unwrap(),
        ordered_candidates: Vec::new(),
        public_pr9_lane_coverage: BTreeMap::from([
            (RetrieverKind::ExactLiteral, PublicRetrieverStatus::Complete),
            (RetrieverKind::Lexical, PublicRetrieverStatus::Complete),
            (RetrieverKind::Graph, PublicRetrieverStatus::Complete),
        ]),
        freshness: Vec::new(),
        cursor: None,
        digest: FallbackSubpayloadDigest::new(SHA256_A).unwrap(),
    };
    fallback.digest = fallback.compute_digest().unwrap();
    fallback
}
