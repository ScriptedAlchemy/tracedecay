use schemars::JsonSchema;
use tracedecay_domain::{
    ManifestDigest, WorkDuplicateAdjudicationCommandV1, WorkPlacementPreflightV1, WorkPlacementV1,
    WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1, WorkRunControlV1,
    canonical_sha256,
};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, CancellationContract, CancellationPoint,
    CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1, CatalogValidationError,
    CodecBindingKey, DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    IdempotencyContract, LifecycleClass, OperationId, PaginationContract, PrivacyClass, ProfileId,
    ReceiptContract, ReconciliationContract, RevalidationContract, RevalidationPoint,
    RouteExposureV1, RoutingContractV1, SchemaBodyAuthorityV1, SchemaId, SchemaRef, ScopeDimension,
    ScopeRequirement, ServiceId, StreamingContract, TerminalState, TerminalStateContract,
    UseCaseId,
};

use tracedecay_domain::WorkAttemptV1;

use crate::work_retry::{RetryWorkAttemptCommandV1, WorkRetryAttemptOutcomeV1};
use crate::{
    AcceptProposalCommand, AcceptTaskCommand, AdjudicateWorkLeakCommandV1, AdmitExecutionCommand,
    AdmitWorkPlacementCommand, AdmitWorkSynthesisCommand, AttachRuntimeEvidenceCommand,
    CancelWorkAttemptCommand, CreateWorkCommand, ExecutionTopologyMetricsRequestV1,
    ExecutionTopologyMetricsV1, ExecutionTopologyViewV1, GenerateProposalRequest,
    GeneratedWorkProposal, PauseWorkRunCommand, ReleaseWorkPlacementCommand,
    ReplanDependenciesCommand, ResumeWorkAttemptsCommand, ResumeWorkRunCommand,
    ReviewProposalRequestV1, StartWorkAttemptCommand, WorkArtifactHydrationRequestV1,
    WorkArtifactHydrationV1, WorkAttemptListRequestV1, WorkAttemptListV1,
    WorkAttemptRecoveryReportV1, WorkAttemptStatusRequestV1,
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkGraphReadRequestV1, WorkGraphReadV1,
    WorkLeakAdjudicationOutcomeV1, WorkPlacementPreflightRequestV1, WorkPlacementReadingV1,
    WorkPlacementStatusRequestV1, WorkProductMutationReceiptV1, WorkProductMutationRequestV1,
    WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
    WorkRetryTestBindingTokenOutcomeV1, WorkRetryTestBindingTokenRequestV1,
    WorkRunControlReadingV1, WorkRunControlRequestV1, WorkSynthesisAttemptV1,
    WorkTopologyViewRequestV1,
};

const WORK_SERVICE_ID: &str = "service.work";
pub const WORK_APPLICATION_OPERATION_IDS_V1: [(&str, &str, &str); 32] = [
    (
        "snapshot",
        "capability.work.snapshot",
        "use-case.work.snapshot",
    ),
    ("delta", "capability.work.delta", "use-case.work.delta"),
    (
        "generate_proposal",
        "capability.work.generate_proposal",
        "use-case.work.generate_proposal",
    ),
    ("create", "capability.work.create", "use-case.work.create"),
    (
        "replan_dependencies",
        "capability.work.replan_dependencies",
        "use-case.work.replan_dependencies",
    ),
    (
        "review_proposal",
        "capability.work.review_proposal",
        "use-case.work.review_proposal",
    ),
    (
        "accept_proposal",
        "capability.work.accept_proposal",
        "use-case.work.accept_proposal",
    ),
    (
        "admit_execution",
        "capability.work.admit_execution",
        "use-case.work.admit_execution",
    ),
    (
        "attach_runtime_evidence",
        "capability.work.attach_runtime_evidence",
        "use-case.work.attach_runtime_evidence",
    ),
    (
        "accept_task",
        "capability.work.accept_task",
        "use-case.work.accept_task",
    ),
    (
        "start_attempt",
        "capability.work.start_attempt",
        "use-case.work.start_attempt",
    ),
    (
        "synthesize",
        "capability.work.synthesize",
        "use-case.work.synthesize",
    ),
    (
        "attempt_status",
        "capability.work.attempt_status",
        "use-case.work.attempt_status",
    ),
    (
        "cancel_attempt",
        "capability.work.cancel_attempt",
        "use-case.work.cancel_attempt",
    ),
    (
        "resume_attempts",
        "capability.work.resume_attempts",
        "use-case.work.resume_attempts",
    ),
    (
        "retry_attempt",
        "capability.work.retry_attempt",
        "use-case.work.retry_attempt",
    ),
    (
        "mint_retry_test_binding",
        "capability.work.mint_retry_test_binding",
        "use-case.work.mint_retry_test_binding",
    ),
    (
        "list_attempts",
        "capability.work.list_attempts",
        "use-case.work.list_attempts",
    ),
    (
        "hydrate_artifacts",
        "capability.work.hydrate_artifacts",
        "use-case.work.hydrate_artifacts",
    ),
    ("views", "capability.work.views", "use-case.work.views"),
    (
        "mutate_graph",
        "capability.work.mutate_graph",
        "use-case.work.mutate_graph",
    ),
    (
        "topology",
        "capability.work.topology",
        "use-case.work.topology",
    ),
    (
        "topology_metrics",
        "capability.work.topology_metrics",
        "use-case.work.topology_metrics",
    ),
    (
        "adjudicate_duplicate",
        "capability.work.adjudicate_duplicate",
        "use-case.work.adjudicate_duplicate",
    ),
    (
        "adjudicate_leak",
        "capability.work.adjudicate_leak",
        "use-case.work.adjudicate_leak",
    ),
    (
        "pause_run",
        "capability.work.pause_run",
        "use-case.work.pause_run",
    ),
    (
        "resume_run",
        "capability.work.resume_run",
        "use-case.work.resume_run",
    ),
    (
        "run_control",
        "capability.work.run_control",
        "use-case.work.run_control",
    ),
    (
        "placement_preflight",
        "capability.work.placement_preflight",
        "use-case.work.placement_preflight",
    ),
    (
        "admit_placement",
        "capability.work.admit_placement",
        "use-case.work.admit_placement",
    ),
    (
        "placement_status",
        "capability.work.placement_status",
        "use-case.work.placement_status",
    ),
    (
        "release_placement",
        "capability.work.release_placement",
        "use-case.work.release_placement",
    ),
];

pub fn work_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    let bindings = vec![
        available::<WorkProjectionSnapshotRequestV1, WorkProjectionSnapshotV1>(
            "snapshot",
            "/application/work/snapshot",
            EffectClass::Read,
            "tracedecay_application::WorkProjectionSnapshotRequestV1",
            "tracedecay_domain::WorkProjectionSnapshotV1",
        )?,
        available::<WorkProjectionDeltaRequestV1, WorkProjectionDeltaV1>(
            "delta",
            "/application/work/delta",
            EffectClass::Read,
            "tracedecay_application::WorkProjectionDeltaRequestV1",
            "tracedecay_domain::WorkProjectionDeltaV1",
        )?,
        available::<GenerateProposalRequest, GeneratedWorkProposal>(
            "generate_proposal",
            "/application/work/generate-proposal",
            EffectClass::Read,
            "tracedecay_application::GenerateProposalRequest",
            "tracedecay_application::GeneratedWorkProposal",
        )?,
        available::<CreateWorkCommand, WorkProjection>(
            "create",
            "/application/work/create",
            EffectClass::Administrative,
            "tracedecay_application::CreateWorkCommand",
            "tracedecay_domain::WorkProjection",
        )?,
        available::<ReplanDependenciesCommand, WorkProjection>(
            "replan_dependencies",
            "/application/work/replan-dependencies",
            EffectClass::Administrative,
            "tracedecay_application::ReplanDependenciesCommand",
            "tracedecay_domain::WorkProjection",
        )?,
        available::<ReviewProposalRequestV1, WorkProjection>(
            "review_proposal",
            "/application/work/review-proposal",
            EffectClass::Administrative,
            "tracedecay_application::ReviewProposalRequestV1",
            "tracedecay_domain::WorkProjection",
        )?,
        available::<AcceptProposalCommand, WorkProjection>(
            "accept_proposal",
            "/application/work/accept-proposal",
            EffectClass::Administrative,
            "tracedecay_application::AcceptProposalCommand",
            "tracedecay_domain::WorkProjection",
        )?,
        available::<AdmitExecutionCommand, WorkProjection>(
            "admit_execution",
            "/application/work/admit-execution",
            EffectClass::Administrative,
            "tracedecay_application::AdmitExecutionCommand",
            "tracedecay_domain::WorkProjection",
        )?,
        available::<AttachRuntimeEvidenceCommand, WorkProjection>(
            "attach_runtime_evidence",
            "/application/work/attach-runtime-evidence",
            EffectClass::Administrative,
            "tracedecay_application::AttachRuntimeEvidenceCommand",
            "tracedecay_domain::WorkProjection",
        )?,
        available::<AcceptTaskCommand, WorkProjection>(
            "accept_task",
            "/application/work/accept-task",
            EffectClass::Administrative,
            "tracedecay_application::AcceptTaskCommand",
            "tracedecay_domain::WorkProjection",
        )?,
        available::<StartWorkAttemptCommand, WorkAttemptV1>(
            "start_attempt",
            "/application/work/start-attempt",
            EffectClass::Administrative,
            "tracedecay_application::StartWorkAttemptCommand",
            "tracedecay_domain::WorkAttemptV1",
        )?,
        available::<AdmitWorkSynthesisCommand, WorkSynthesisAttemptV1>(
            "synthesize",
            "/application/work/synthesize",
            EffectClass::Administrative,
            "tracedecay_application::AdmitWorkSynthesisCommand",
            "tracedecay_application::WorkSynthesisAttemptV1",
        )?,
        available::<WorkAttemptStatusRequestV1, WorkAttemptV1>(
            "attempt_status",
            "/application/work/attempt-status",
            EffectClass::Read,
            "tracedecay_application::WorkAttemptStatusRequestV1",
            "tracedecay_domain::WorkAttemptV1",
        )?,
        available::<CancelWorkAttemptCommand, WorkAttemptV1>(
            "cancel_attempt",
            "/application/work/cancel-attempt",
            EffectClass::Administrative,
            "tracedecay_application::CancelWorkAttemptCommand",
            "tracedecay_domain::WorkAttemptV1",
        )?,
        available::<ResumeWorkAttemptsCommand, WorkAttemptRecoveryReportV1>(
            "resume_attempts",
            "/application/work/resume-attempts",
            EffectClass::Administrative,
            "tracedecay_application::ResumeWorkAttemptsCommand",
            "tracedecay_application::WorkAttemptRecoveryReportV1",
        )?,
        available::<RetryWorkAttemptCommandV1, WorkRetryAttemptOutcomeV1>(
            "retry_attempt",
            "/application/work/retry-attempt",
            EffectClass::Administrative,
            "tracedecay_application::RetryWorkAttemptCommandV1",
            "tracedecay_application::WorkRetryAttemptOutcomeV1",
        )?,
        available::<WorkRetryTestBindingTokenRequestV1, WorkRetryTestBindingTokenOutcomeV1>(
            "mint_retry_test_binding",
            "/application/work/mint-retry-test-binding",
            EffectClass::Administrative,
            "tracedecay_application::WorkRetryTestBindingTokenRequestV1",
            "tracedecay_application::WorkRetryTestBindingTokenOutcomeV1",
        )?,
        available::<WorkAttemptListRequestV1, WorkAttemptListV1>(
            "list_attempts",
            "/application/work/list-attempts",
            EffectClass::Read,
            "tracedecay_application::WorkAttemptListRequestV1",
            "tracedecay_application::WorkAttemptListV1",
        )?,
        available::<WorkArtifactHydrationRequestV1, WorkArtifactHydrationV1>(
            "hydrate_artifacts",
            "/application/work/hydrate-artifacts",
            EffectClass::Read,
            "tracedecay_application::WorkArtifactHydrationRequestV1",
            "tracedecay_application::WorkArtifactHydrationV1",
        )?,
        available::<WorkGraphReadRequestV1, WorkGraphReadV1>(
            "views",
            "/application/work/views",
            EffectClass::Read,
            "tracedecay_application::WorkGraphReadRequestV1",
            "tracedecay_application::WorkGraphReadV1",
        )?,
        available::<WorkProductMutationRequestV1, WorkProductMutationReceiptV1>(
            "mutate_graph",
            "/application/work/mutate-graph",
            EffectClass::Administrative,
            "tracedecay_application::WorkProductMutationRequestV1",
            "tracedecay_application::WorkProductMutationReceiptV1",
        )?,
        available::<WorkTopologyViewRequestV1, ExecutionTopologyViewV1>(
            "topology",
            "/application/work/topology",
            EffectClass::Read,
            "tracedecay_application::WorkTopologyViewRequestV1",
            "tracedecay_application::ExecutionTopologyViewV1",
        )?,
        available::<ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1>(
            "topology_metrics",
            "/application/work/topology-metrics",
            EffectClass::Read,
            "tracedecay_application::ExecutionTopologyMetricsRequestV1",
            "tracedecay_application::ExecutionTopologyMetricsV1",
        )?,
        available::<WorkDuplicateAdjudicationCommandV1, WorkDuplicateAdjudicationAppendOutcomeV1>(
            "adjudicate_duplicate",
            "/application/work/adjudicate-duplicate",
            EffectClass::Administrative,
            "tracedecay_domain::WorkDuplicateAdjudicationCommandV1",
            "tracedecay_application::WorkDuplicateAdjudicationAppendOutcomeV1",
        )?,
        available::<AdjudicateWorkLeakCommandV1, WorkLeakAdjudicationOutcomeV1>(
            "adjudicate_leak",
            "/application/work/adjudicate-leak",
            EffectClass::Administrative,
            "tracedecay_application::AdjudicateWorkLeakCommandV1",
            "tracedecay_application::WorkLeakAdjudicationOutcomeV1",
        )?,
        available::<PauseWorkRunCommand, WorkRunControlV1>(
            "pause_run",
            "/application/work/pause-run",
            EffectClass::Administrative,
            "tracedecay_application::PauseWorkRunCommand",
            "tracedecay_domain::WorkRunControlV1",
        )?,
        available::<ResumeWorkRunCommand, WorkRunControlV1>(
            "resume_run",
            "/application/work/resume-run",
            EffectClass::Administrative,
            "tracedecay_application::ResumeWorkRunCommand",
            "tracedecay_domain::WorkRunControlV1",
        )?,
        available::<WorkRunControlRequestV1, WorkRunControlReadingV1>(
            "run_control",
            "/application/work/run-control",
            EffectClass::Read,
            "tracedecay_application::WorkRunControlRequestV1",
            "tracedecay_application::WorkRunControlReadingV1",
        )?,
        available::<WorkPlacementPreflightRequestV1, WorkPlacementPreflightV1>(
            "placement_preflight",
            "/application/work/placement-preflight",
            EffectClass::Read,
            "tracedecay_application::WorkPlacementPreflightRequestV1",
            "tracedecay_domain::WorkPlacementPreflightV1",
        )?,
        available::<AdmitWorkPlacementCommand, WorkPlacementV1>(
            "admit_placement",
            "/application/work/admit-placement",
            EffectClass::Administrative,
            "tracedecay_application::AdmitWorkPlacementCommand",
            "tracedecay_domain::WorkPlacementV1",
        )?,
        available::<WorkPlacementStatusRequestV1, WorkPlacementReadingV1>(
            "placement_status",
            "/application/work/placement-status",
            EffectClass::Read,
            "tracedecay_application::WorkPlacementStatusRequestV1",
            "tracedecay_application::WorkPlacementReadingV1",
        )?,
        available::<ReleaseWorkPlacementCommand, WorkPlacementV1>(
            "release_placement",
            "/application/work/release-placement",
            EffectClass::Administrative,
            "tracedecay_application::ReleaseWorkPlacementCommand",
            "tracedecay_domain::WorkPlacementV1",
        )?,
    ];
    ExecutableBindingRegistryV1::new(bindings)
}

/// Resolve one executable Work operation from the canonical registry.
///
/// Transport adapters use this lookup for lifecycle metadata instead of
/// reproducing the registry's effect, deadline, cancellation, or idempotency
/// contract beside their own name normalization.
pub fn work_executable_binding(
    operation_id: &OperationId,
) -> Result<Option<ExecutableBindingV1>, CatalogValidationError> {
    Ok(work_executable_binding_registry()?
        .get(operation_id)
        .and_then(|availability| availability.binding())
        .cloned())
}

pub fn work_executable_catalog_digest() -> Result<ManifestDigest, CatalogValidationError> {
    let registry = work_executable_binding_registry()?;
    canonical_sha256(&(
        "tracedecay.application.work-executable-catalog.v1",
        registry.iter().collect::<Vec<_>>(),
    ))
    .map_err(|_| CatalogValidationError::InvalidValue {
        field: "work executable catalog digest",
        reason: "canonical Work executable catalog could not be encoded",
    })
}

pub(crate) fn available<Request, Output>(
    operation: &str,
    route_path: &str,
    effect: EffectClass,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableBindingAvailabilityV1, CatalogValidationError>
where
    Request: JsonSchema,
    Output: JsonSchema,
{
    let manifest = work_manifest(operation, effect)?;
    let request_schema = SchemaBodyAuthorityV1::for_type_at_path::<Request>(
        manifest.request_schema().clone(),
        request_rust_type_path,
    )?;
    let result_schema = SchemaBodyAuthorityV1::for_type_at_path::<Output>(
        manifest.result_schema().clone(),
        result_rust_type_path,
    )?;
    let binding = ExecutableBindingV1::direct(
        &manifest,
        OperationId::new(format!("operation.work.{operation}"))
            .expect("static Work operation ID is valid"),
        ServiceId::new(WORK_SERVICE_ID).expect("static Work service ID is valid"),
        request_schema,
        result_schema,
        CodecBindingKey::new(format!("codec.work.{operation}.json.v1"))
            .expect("static Work codec ID is valid"),
        RouteExposureV1::Public {
            binding_id: BindingId::new(format!("binding.http.work.{operation}"))
                .expect("static Work binding ID is valid"),
            route_path: route_path.to_owned(),
        },
    )?;
    Ok(ExecutableBindingAvailabilityV1::available(binding))
}

fn work_manifest(
    operation: &str,
    effect: EffectClass,
) -> Result<CapabilityManifestV1, CatalogValidationError> {
    let read_only = effect.is_read_only();
    let binding_id = BindingId::new(format!("binding.http.work.{operation}"))
        .expect("static Work binding ID is valid");
    CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: CapabilityId::new(format!("capability.work.{operation}"))
            .expect("static Work capability ID is valid"),
        use_case_id: UseCaseId::new(format!("use-case.work.{operation}"))
            .expect("static Work use-case ID is valid"),
        routing: RoutingContractV1::new(
            1,
            format!("Work {operation}"),
            format!("Execute the canonical Work {operation} application use case."),
            vec![format!("Work {operation}")],
        )?,
        request_schema: schema_ref(format!("schema.work.{operation}.request"))?,
        result_schema: schema_ref(format!("schema.work.{operation}.result"))?,
        effect,
        scope: ScopeRequirement::new(vec![
            ScopeDimension::Project,
            ScopeDimension::Repository,
            ScopeDimension::Worktree,
        ])?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Stateless,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(if read_only {
            vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeRead,
                CancellationPoint::DuringRead,
            ]
        } else {
            vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeEffect,
                CancellationPoint::EffectInFlight,
                CancellationPoint::AfterCommit,
            ]
        })?,
        deadline: DeadlineContract::new(
            30_000,
            if read_only {
                DeadlineBehavior::ReturnOperationReceipt
            } else {
                DeadlineBehavior::ReturnEffectReceipt
            },
        )?,
        pagination: read_only.then(|| PaginationContract::new(100, 1_000, 60_000).unwrap()),
        idempotency: if read_only {
            IdempotencyContract::NotRequired
        } else {
            IdempotencyContract::Required
        },
        inverse: if read_only {
            tracedecay_tool_catalog::InverseContract::NotApplicable
        } else {
            tracedecay_tool_catalog::InverseContract::Unavailable {
                reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
            }
        },
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::ExpectedState,
        ])?,
        reconciliation: if read_only {
            ReconciliationContract::NotRequired
        } else {
            ReconciliationContract::Required
        },
        receipt: if read_only {
            ReceiptContract::Operation
        } else {
            ReceiptContract::DurableEffect
        },
        terminal_states: TerminalStateContract::new(terminal_states(read_only))?,
        availability: AvailabilityContract::Available,
        binding_ids: vec![binding_id],
        profile_eligibility: vec![
            ProfileId::new("profile.default").expect("static profile ID is valid"),
        ],
        required_features: Vec::new(),
    })
}

fn terminal_states(read_only: bool) -> Vec<TerminalState> {
    let mut states = vec![
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial,
    ];
    if !read_only {
        states.push(TerminalState::EffectUnknown);
    }
    states
}

fn schema_ref(id: String) -> Result<SchemaRef, CatalogValidationError> {
    let schema_id = SchemaId::new(id).map_err(|_| CatalogValidationError::InvalidValue {
        field: "work schema ID",
        reason: "must be a canonical catalog identifier",
    })?;
    SchemaRef::new(schema_id, 1)
}

#[cfg(test)]
mod tests {
    use tracedecay_tool_catalog::{CancellationPoint, RouteExposureV1};

    use super::{
        WORK_APPLICATION_OPERATION_IDS_V1, work_executable_binding,
        work_executable_binding_registry,
    };

    #[test]
    fn work_registry_advertises_only_mounted_application_operations() {
        let registry = work_executable_binding_registry().unwrap();
        let advertised = registry
            .iter()
            .filter_map(|availability| availability.binding())
            .collect::<Vec<_>>();
        let expected = WORK_APPLICATION_OPERATION_IDS_V1
            .iter()
            .map(|(operation, _, _)| format!("operation.work.{operation}"))
            .collect::<std::collections::BTreeSet<_>>();
        let actual = advertised
            .iter()
            .map(|binding| binding.operation_id().as_str().to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
        for binding in advertised {
            let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
                panic!("available Work binding must have a public route");
            };
            assert!(route_path.starts_with("/application/work/"));
            assert!(
                binding
                    .cancellation()
                    .observes(CancellationPoint::BeforeAdmission)
            );
            assert_ne!(
                binding.request_schema().body()["title"],
                serde_json::Value::String("Value".to_owned())
            );
        }
        let snapshot = registry
            .get(&tracedecay_tool_catalog::OperationId::new("operation.work.snapshot").unwrap())
            .unwrap()
            .binding()
            .unwrap();
        let delta = registry
            .get(&tracedecay_tool_catalog::OperationId::new("operation.work.delta").unwrap())
            .unwrap()
            .binding()
            .unwrap();
        assert_eq!(
            snapshot.result_schema().body()["title"],
            "WorkProjectionSnapshotV1"
        );
        assert_eq!(
            delta.result_schema().body()["title"],
            "WorkProjectionDeltaV1"
        );
    }

    #[test]
    fn operation_lookup_is_backed_by_the_executable_registry() {
        let operation =
            tracedecay_tool_catalog::OperationId::new("operation.work.topology").unwrap();
        let binding = work_executable_binding(&operation)
            .unwrap()
            .expect("topology is an executable Work operation");
        assert!(binding.effect().is_read_only());
        assert_eq!(binding.deadline().maximum_millis(), 30_000);
    }

    #[test]
    fn topology_metrics_binding_returns_the_canonical_read_model() {
        let operation =
            tracedecay_tool_catalog::OperationId::new("operation.work.topology_metrics").unwrap();
        let binding = work_executable_binding(&operation)
            .unwrap()
            .expect("topology metrics is an executable Work operation");

        assert_eq!(
            binding.request_schema().body()["title"],
            "ExecutionTopologyMetricsRequestV1"
        );
        assert_eq!(
            binding.result_schema().body()["title"],
            "ExecutionTopologyMetricsV1"
        );
        let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
            panic!("topology metrics must be publicly exposed");
        };
        assert_eq!(route_path, "/application/work/topology-metrics");
    }

    #[test]
    fn the_graph_views_binding_reads_the_work_product_graph_contract() {
        let registry = work_executable_binding_registry().unwrap();
        let views = registry
            .get(&tracedecay_tool_catalog::OperationId::new("operation.work.views").unwrap())
            .unwrap()
            .binding()
            .unwrap();
        // The views route serves the durable work-product graph authority, so it
        // must carry that authority's own request and result contracts rather
        // than a page-shaped mirror of the attempt list.
        assert_eq!(
            views.request_schema().body()["title"],
            "WorkGraphReadRequestV1"
        );
        assert_eq!(views.result_schema().body()["title"], "WorkGraphReadV1");
        let RouteExposureV1::Public { route_path, .. } = views.exposure() else {
            panic!("the Work views binding must be publicly routed");
        };
        assert_eq!(route_path, "/application/work/views");
        assert!(views.effect().is_read_only());
    }

    #[test]
    fn graph_mutation_binding_is_public_typed_and_effectful() {
        let registry = work_executable_binding_registry().unwrap();
        let mutation = registry
            .get(&tracedecay_tool_catalog::OperationId::new("operation.work.mutate_graph").unwrap())
            .unwrap()
            .binding()
            .unwrap();
        assert_eq!(
            mutation.request_schema().body()["title"],
            "WorkProductMutationRequestV1"
        );
        assert_eq!(
            mutation.result_schema().body()["title"],
            "WorkProductMutationReceiptV1"
        );
        assert_eq!(
            mutation.request_schema().rust_type_path(),
            "tracedecay_application::WorkProductMutationRequestV1"
        );
        assert_eq!(
            mutation.result_schema().rust_type_path(),
            "tracedecay_application::WorkProductMutationReceiptV1"
        );
        let RouteExposureV1::Public { route_path, .. } = mutation.exposure() else {
            panic!("the Work graph mutation binding must be publicly routed");
        };
        assert_eq!(route_path, "/application/work/mutate-graph");
        assert!(!mutation.effect().is_read_only());
    }
}
