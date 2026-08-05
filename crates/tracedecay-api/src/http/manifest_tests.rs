use std::collections::BTreeSet;

use tracedecay_application::{
    ApplicationOutcome, AuthorityReceipt, CancellationObservation, CancellationStage,
    CapabilityGrantId, Deadline, DisclosureClass, EvidenceCoverage, EvidenceDomain, EvidencePacket,
    OperationBudgetUsage, OperationReceipt, OperationTermination, PageState, PolicyDecisionRef,
    TemporalState,
};
use tracedecay_domain::{ComponentVersion, ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::{
    ApplicationHandlerDescriptorV1, AuthorityRequirement, AvailabilityContract, BindingId,
    BindingStatus, BindingSurface, CancellationContract, CancellationPoint, CapabilityId,
    CapabilityManifestInputV1, CapabilityManifestV1, CatalogContributionInputV1,
    CatalogContributionV1, CatalogSnapshotBuilderV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract, InverseContract,
    LifecycleClass, PaginationContract, PrivacyClass, ProfileBudget, ProfileDefinition,
    ProfileDefinitionInputV1, ProfileId, ProfileKind, ProtocolRevisionRange, ReceiptContract,
    ReconciliationContract, RevalidationContract, RoutingContractV1, RoutingFixtureExpectation,
    RoutingFixtureV1, SchemaId, SchemaRef, ScopeRequirement, SortContractId, StreamingContract,
    SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName, TerminalState,
    TerminalStateContract, UnavailabilityReason, UseCaseId,
};

use super::{HttpManifestContract, HttpManifestContractError};

fn binding(binding_id: &str, capability_id: &str, surface: BindingSurface) -> SurfaceBindingV1 {
    SurfaceBindingV1::new(SurfaceBindingInputV1 {
        binding_id: BindingId::new(binding_id).unwrap(),
        capability_id: CapabilityId::new(capability_id).unwrap(),
        surface,
        operation: SurfaceOperationName::new("source_read").unwrap(),
        protocol_revisions: ProtocolRevisionRange::new(1, 1).unwrap(),
        required_features: Vec::new(),
        status: BindingStatus::Current,
        alias_of: None,
    })
    .unwrap()
}

fn detached_contract(
    binding: &SurfaceBindingV1,
    capability: &CapabilityManifestV1,
) -> Result<HttpManifestContract, HttpManifestContractError> {
    HttpManifestContract::new_for_surface(binding, capability, BindingSurface::Http)
}

fn manifest(
    capability_id: &str,
    binding_ids: Vec<&str>,
    availability: AvailabilityContract,
    terminal_states: Vec<TerminalState>,
) -> CapabilityManifestV1 {
    manifest_with_lifecycle(
        capability_id,
        binding_ids,
        availability,
        terminal_states,
        DeadlineBehavior::ReturnOperationReceipt,
        Some(PaginationContract::new(12, 40, 60_000).unwrap()),
        CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::DuringRead,
        ])
        .unwrap(),
    )
}

fn manifest_with_lifecycle(
    capability_id: &str,
    binding_ids: Vec<&str>,
    availability: AvailabilityContract,
    terminal_states: Vec<TerminalState>,
    deadline_behavior: DeadlineBehavior,
    pagination: Option<PaginationContract>,
    cancellation: CancellationContract,
) -> CapabilityManifestV1 {
    CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: CapabilityId::new(capability_id).unwrap(),
        use_case_id: UseCaseId::new("source.read").unwrap(),
        routing: RoutingContractV1::new(
            1,
            "Read source",
            "Read bounded source evidence.",
            Vec::new(),
        )
        .unwrap(),
        request_schema: SchemaRef::new(SchemaId::new("source.read.request").unwrap(), 1).unwrap(),
        result_schema: SchemaRef::new(SchemaId::new("source.read.result").unwrap(), 1).unwrap(),
        effect: EffectClass::Read,
        scope: ScopeRequirement::none(),
        authority: AuthorityRequirement::None,
        denied_disclosure: DeniedDisclosurePolicy::Explicit,
        privacy: PrivacyClass::PublicMetadata,
        lifecycle: LifecycleClass::Stateless,
        streaming: StreamingContract::Unsupported,
        cancellation,
        deadline: DeadlineContract::new(2_500, deadline_behavior).unwrap(),
        pagination,
        idempotency: IdempotencyContract::NotRequired,
        inverse: InverseContract::NotApplicable,
        authority_revalidation: RevalidationContract::NotRequired,
        reconciliation: ReconciliationContract::NotRequired,
        receipt: ReceiptContract::Operation,
        terminal_states: TerminalStateContract::new(terminal_states).unwrap(),
        availability,
        binding_ids: binding_ids
            .into_iter()
            .map(|id| BindingId::new(id).unwrap())
            .collect(),
        profile_eligibility: vec![ProfileId::new("profile.default").unwrap()],
        required_features: Vec::new(),
    })
    .unwrap()
}

fn base_terminal_states() -> Vec<TerminalState> {
    vec![
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial,
    ]
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn evidence_outcome(execution: OperationReceipt) -> ApplicationOutcome<()> {
    ApplicationOutcome::Evidence(EvidencePacket {
        temporal: TemporalState::current(UtcMicros(100)),
        authority: AuthorityReceipt {
            grant_id: CapabilityGrantId::new("grant.fixture").unwrap(),
            grant_revision: 1,
            grant_digest: digest('a'),
            authorized_scope_digest: digest('b'),
            disclosure: DisclosureClass::Evidence,
            policy: PolicyDecisionRef::new(
                "policy.fixture",
                1,
                digest('c'),
                ComponentVersion::new("policy.fixture.v1").unwrap(),
            )
            .unwrap(),
            revalidated_at: UtcMicros(100),
        },
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Source], 1, 1, 1).unwrap(),
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.source.fixture").unwrap(),
            1,
            Some(1),
            1,
        )
        .unwrap(),
        execution,
        payload: Some(()),
    })
}

fn operation_receipt(
    termination: OperationTermination,
    cancellation: Option<CancellationObservation>,
) -> OperationReceipt {
    OperationReceipt {
        started_at: UtcMicros(100),
        ended_at: UtcMicros(200),
        effective_deadline: Deadline::new(UtcMicros(150)).unwrap(),
        cancellation,
        budget: OperationBudgetUsage::default(),
        termination,
    }
}

fn validate_outcome(
    contract: &HttpManifestContract,
    receipt: OperationReceipt,
) -> Result<(), HttpManifestContractError> {
    let admitted_deadline = receipt.effective_deadline.clone();
    contract.validate_outcome(&evidence_outcome(receipt), &admitted_deadline)
}

fn contract() -> HttpManifestContract {
    contract_with_lifecycle(
        DeadlineBehavior::ReturnOperationReceipt,
        Some(PaginationContract::new(12, 40, 60_000).unwrap()),
        CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::DuringRead,
        ])
        .unwrap(),
    )
}

fn contract_with_lifecycle(
    deadline_behavior: DeadlineBehavior,
    pagination: Option<PaginationContract>,
    cancellation: CancellationContract,
) -> HttpManifestContract {
    let binding = binding(
        "binding.http.source_read",
        "source.read",
        BindingSurface::Http,
    );
    let capability = manifest_with_lifecycle(
        "source.read",
        vec!["binding.http.source_read"],
        AvailabilityContract::Available,
        base_terminal_states(),
        deadline_behavior,
        pagination,
        cancellation,
    );
    detached_contract(&binding, &capability).unwrap()
}

fn catalog() -> (
    tracedecay_tool_catalog::CatalogSnapshotV1,
    ProfileId,
    SurfaceOperationName,
) {
    let profile_id = ProfileId::new("profile.default").unwrap();
    let binding = binding(
        "binding.http.source_read",
        "source.read",
        BindingSurface::Http,
    );
    let capability = manifest(
        "source.read",
        vec!["binding.http.source_read"],
        AvailabilityContract::Available,
        base_terminal_states(),
    );
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![capability.clone()],
        retrieval_primitives: Vec::new(),
        bindings: vec![binding],
    })
    .unwrap();
    let profile = ProfileDefinition::new(ProfileDefinitionInputV1 {
        profile_id: profile_id.clone(),
        kind: ProfileKind::Default,
        capability_ids: vec![capability.capability_id().clone()],
        enabled_surfaces: vec![BindingSurface::Http],
        requires_cli_mcp_pairing: false,
        budget: ProfileBudget::new(8, 2_000).unwrap(),
        routing_fixtures: vec![
            RoutingFixtureV1::new(
                "read source",
                RoutingFixtureExpectation::Select {
                    capability_id: capability.capability_id().clone(),
                },
            )
            .unwrap(),
            RoutingFixtureV1::new("do nothing", RoutingFixtureExpectation::Reject).unwrap(),
        ],
    })
    .unwrap();
    let handler = ApplicationHandlerDescriptorV1::new(
        capability.capability_id().clone(),
        capability.use_case_id().clone(),
        capability.request_schema().clone(),
        capability.result_schema().clone(),
    );
    let mut builder = CatalogSnapshotBuilderV1::new();
    builder
        .add_contribution(contribution)
        .add_profile(profile)
        .add_handler(handler);
    (
        builder.build().unwrap(),
        profile_id,
        SurfaceOperationName::new("source_read").unwrap(),
    )
}

#[test]
fn rejects_non_http_binding() {
    let binding = binding(
        "binding.cli.source_read",
        "source.read",
        BindingSurface::Cli,
    );
    let capability = manifest(
        "source.read",
        vec!["binding.cli.source_read"],
        AvailabilityContract::Available,
        base_terminal_states(),
    );

    assert_eq!(
        detached_contract(&binding, &capability).unwrap_err(),
        HttpManifestContractError::NotHttp
    );
}

#[test]
fn rejects_binding_with_a_different_capability_identity() {
    let binding = binding(
        "binding.http.source_read",
        "source.other",
        BindingSurface::Http,
    );
    let capability = manifest(
        "source.read",
        vec!["binding.http.source_read"],
        AvailabilityContract::Available,
        base_terminal_states(),
    );

    assert_eq!(
        detached_contract(&binding, &capability).unwrap_err(),
        HttpManifestContractError::CapabilityMismatch
    );
}

#[test]
fn rejects_binding_not_declared_by_the_capability() {
    let binding = binding(
        "binding.http.source_read",
        "source.read",
        BindingSurface::Http,
    );
    let capability = manifest(
        "source.read",
        vec!["binding.http.other"],
        AvailabilityContract::Available,
        base_terminal_states(),
    );

    assert_eq!(
        detached_contract(&binding, &capability).unwrap_err(),
        HttpManifestContractError::BindingNotDeclared
    );
}

#[test]
fn rejects_an_unavailable_capability() {
    let binding = binding(
        "binding.http.source_read",
        "source.read",
        BindingSurface::Http,
    );
    let capability = manifest(
        "source.read",
        vec!["binding.http.source_read"],
        AvailabilityContract::Unavailable {
            reason: UnavailabilityReason::NotImplemented,
        },
        base_terminal_states(),
    );

    assert_eq!(
        detached_contract(&binding, &capability).unwrap_err(),
        HttpManifestContractError::Unavailable
    );
}

#[test]
fn derives_lifecycle_constraints_from_the_capability_manifest() {
    let contract = contract();

    assert_eq!(contract.maximum_deadline_micros().unwrap(), 2_500_000);
    assert_eq!(
        contract.cancellation().points(),
        &[
            CancellationPoint::BeforeAdmission,
            CancellationPoint::DuringRead,
        ]
    );
    assert_eq!(contract.receipt(), ReceiptContract::Operation);

    let default_page = contract.page_request(None, None).unwrap().unwrap();
    assert_eq!(default_page.page_size, 12);
    let bounded_page = contract.page_request(Some(40), None).unwrap().unwrap();
    assert_eq!(bounded_page.page_size, 40);
    assert!(contract.page_request(Some(41), None).is_err());
}

#[test]
fn maps_every_operation_termination_to_the_manifest_terminal_state() {
    let binding = binding(
        "binding.http.source_read",
        "source.read",
        BindingSurface::Http,
    );
    let mut terminal_states = base_terminal_states();
    terminal_states.push(TerminalState::Unavailable);
    let capability = manifest(
        "source.read",
        vec!["binding.http.source_read"],
        AvailabilityContract::Available,
        terminal_states,
    );
    let contract = detached_contract(&binding, &capability).unwrap();

    let expectations = [
        (OperationTermination::Completed, true),
        (OperationTermination::Cancelled, true),
        (OperationTermination::TimedOut, true),
        (OperationTermination::Failed, true),
        (OperationTermination::Unavailable, true),
        (OperationTermination::Partial, true),
        (OperationTermination::EffectUnknown, false),
    ];
    for (termination, expected) in expectations {
        assert_eq!(
            contract.allows_termination(termination),
            expected,
            "unexpected contract decision for {termination:?}"
        );
    }
}

#[test]
fn resolve_returns_only_the_snapshot_selected_http_contract() {
    let (catalog, profile_id, operation) = catalog();
    let contract = HttpManifestContract::resolve(
        &catalog,
        &profile_id,
        BindingSurface::Http,
        &operation,
        1,
        &BTreeSet::new(),
    )
    .unwrap();

    assert_eq!(
        contract.binding().binding_id().as_str(),
        "binding.http.source_read"
    );
    assert_eq!(
        contract.capability().capability_id().as_str(),
        "source.read"
    );
    assert!(
        HttpManifestContract::resolve(
            &catalog,
            &ProfileId::new("profile.other").unwrap(),
            BindingSurface::Http,
            &operation,
            1,
            &BTreeSet::new(),
        )
        .is_none()
    );
}

#[test]
fn omitted_deadline_requires_caller_owned_admission_semantics() {
    let contract = contract();

    assert_eq!(
        contract.deadline_behavior(),
        DeadlineBehavior::ReturnOperationReceipt
    );
    assert_eq!(
        contract
            .effective_deadline(UtcMicros(100), None)
            .unwrap_err(),
        HttpManifestContractError::CallerDeadlineRequired
    );
}

#[test]
fn unsupported_pagination_rejects_http_page_controls() {
    let contract = contract_with_lifecycle(
        DeadlineBehavior::ReturnOperationReceipt,
        None,
        CancellationContract::NotCancellable,
    );

    assert_eq!(contract.page_request(None, None).unwrap(), None);
    assert_eq!(
        contract.page_request(Some(1), None).unwrap_err(),
        HttpManifestContractError::PaginationUnsupported
    );
}

#[test]
fn opaque_cursor_requires_authenticated_ttl_authority() {
    let contract = contract();

    assert_eq!(
        contract
            .page_request(
                Some(12),
                Some(tracedecay_application::OpaqueCursor::new("opaque.cursor").unwrap()),
            )
            .unwrap_err(),
        HttpManifestContractError::CursorAuthorityRequired {
            cursor_ttl_millis: 60_000,
        }
    );
}

#[test]
fn observed_receipt_family_must_match_the_manifest() {
    let contract = contract();

    assert_eq!(
        contract
            .validate_observed_receipt(
                ReceiptContract::DurableEffect,
                OperationTermination::Completed,
            )
            .unwrap_err(),
        HttpManifestContractError::ReceiptMismatch
    );
}

#[test]
fn validate_outcome_rejects_a_malformed_operation_receipt() {
    let contract = contract();
    let mut receipt = operation_receipt(OperationTermination::Completed, None);
    receipt.ended_at = UtcMicros(99);

    assert_eq!(
        validate_outcome(&contract, receipt).unwrap_err(),
        HttpManifestContractError::InvalidReceipt
    );
}

#[test]
fn validate_outcome_rejects_an_undeclared_cancellation_stage() {
    let contract = contract();
    let receipt = operation_receipt(
        OperationTermination::Completed,
        Some(CancellationObservation {
            stage: CancellationStage::BeforeRead,
            observed_at: UtcMicros(150),
        }),
    );

    assert_eq!(
        validate_outcome(&contract, receipt).unwrap_err(),
        HttpManifestContractError::CancellationStageNotDeclared
    );
}

#[test]
fn validate_outcome_rejects_an_out_of_range_cancellation_observation() {
    let contract = contract();
    let receipt = operation_receipt(
        OperationTermination::Cancelled,
        Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: UtcMicros(201),
        }),
    );

    assert_eq!(
        validate_outcome(&contract, receipt).unwrap_err(),
        HttpManifestContractError::InvalidReceipt
    );
}

#[test]
fn validate_outcome_rejects_an_effective_deadline_above_the_manifest_ceiling() {
    let contract = contract();
    let mut receipt = operation_receipt(OperationTermination::Completed, None);
    receipt.effective_deadline = Deadline::new(UtcMicros(2_500_101)).unwrap();

    assert_eq!(
        validate_outcome(&contract, receipt).unwrap_err(),
        HttpManifestContractError::EffectiveDeadlineExceedsManifest
    );
}

#[test]
fn validate_outcome_requires_the_exact_admitted_effective_deadline() {
    let contract = contract();
    let mut receipt = operation_receipt(OperationTermination::Completed, None);
    receipt.effective_deadline = Deadline::new(UtcMicros(149)).unwrap();
    let admitted_deadline = Deadline::new(UtcMicros(150)).unwrap();

    assert_eq!(
        contract
            .validate_outcome(&evidence_outcome(receipt), &admitted_deadline)
            .unwrap_err(),
        HttpManifestContractError::EffectiveDeadlineMismatch
    );
}

#[test]
fn validate_outcome_rejects_cancellation_for_a_non_cancellable_capability() {
    let contract = contract_with_lifecycle(
        DeadlineBehavior::ReturnOperationReceipt,
        None,
        CancellationContract::NotCancellable,
    );
    let receipt = operation_receipt(
        OperationTermination::Completed,
        Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: UtcMicros(150),
        }),
    );

    assert_eq!(
        validate_outcome(&contract, receipt).unwrap_err(),
        HttpManifestContractError::CancellationNotSupported
    );
}

#[test]
fn non_cancellable_capability_accepts_a_timed_out_deadline_observation() {
    let contract = contract_with_lifecycle(
        DeadlineBehavior::ReturnOperationReceipt,
        None,
        CancellationContract::NotCancellable,
    );
    let receipt = operation_receipt(
        OperationTermination::TimedOut,
        Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: UtcMicros(150),
        }),
    );

    assert!(validate_outcome(&contract, receipt).is_ok());
}

#[test]
fn reject_before_admission_cannot_surface_an_admitted_timeout_receipt() {
    let contract = contract_with_lifecycle(
        DeadlineBehavior::RejectBeforeAdmission,
        None,
        CancellationContract::cooperative(vec![CancellationPoint::DuringRead]).unwrap(),
    );
    let receipt = operation_receipt(
        OperationTermination::TimedOut,
        Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: UtcMicros(150),
        }),
    );

    assert_eq!(
        validate_outcome(&contract, receipt).unwrap_err(),
        HttpManifestContractError::DeadlineBehaviorMismatch
    );
}

#[test]
fn return_operation_receipt_preserves_an_already_elapsed_caller_deadline() {
    let contract = contract();
    let mut receipt = operation_receipt(
        OperationTermination::TimedOut,
        Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: UtcMicros(100),
        }),
    );
    receipt.effective_deadline = Deadline::new(UtcMicros(99)).unwrap();

    assert!(validate_outcome(&contract, receipt).is_ok());
}

#[test]
fn validate_outcome_accepts_a_well_formed_declared_receipt() {
    let contract = contract();

    assert!(
        validate_outcome(
            &contract,
            operation_receipt(OperationTermination::Completed, None),
        )
        .is_ok()
    );
}
