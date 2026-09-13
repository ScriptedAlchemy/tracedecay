use std::sync::LazyLock;

use schemars::JsonSchema;
use tracedecay_domain::{
    ManifestDigest, WorkDuplicateAdjudicationCommandV1, WorkPlacementPreflightV1, WorkPlacementV1,
    WorkRunControlV1, canonical_sha256,
};
use tracedecay_tool_catalog::{
    AvailabilityContract, BindingId, CancellationContract, CancellationPoint, CapabilityId,
    CapabilityManifestV1, CatalogValidationError, CodecBindingKey, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, ExecutableBindingAvailabilityV1,
    ExecutableBindingRegistryV1, ExecutableBindingV1, LifecycleClass, OperationId,
    PaginationContract, PrivacyClass, ProfileId, RevalidationContract, RevalidationPoint,
    RouteExposureV1, RoutingContractV1, SchemaBodyAuthorityV1, SchemaId, SchemaRef, ScopeDimension,
    ScopeRequirement, ServiceId, StreamingContract, TerminalState, TerminalStateContract,
    UseCaseId,
};

use tracedecay_domain::WorkAttemptV1;

use crate::capability_manifest::{
    ApplicationCapabilityManifestInput, application_capability_manifest,
};
use crate::work_retry::{RetryWorkAttemptCommandV1, WorkRetryAttemptOutcomeV1};
use crate::{
    AcceptWorkProposalRequestV1, AdjudicateWorkLeakCommandV1, AdmitWorkExecutionRequestV1,
    AdmitWorkPlacementCommand, AdmitWorkSynthesisCommand, AdmittedWorkExecutionV1,
    CancelWorkAttemptCommand, CreateWorkTaskRequestV1, ExecutionTopologyMetricsRequestV1,
    ExecutionTopologyMetricsV1, ExecutionTopologyViewV1, GenerateProposalRequest,
    GeneratedWorkProposal, PauseWorkRunCommand, PrepareWorkDuplicateAdjudicationRequestV1,
    PrepareWorkProductMutationRequestV1, ReleaseWorkPlacementCommand, ResumeWorkAttemptsCommand,
    ResumeWorkRunCommand, ReviewWorkProposalRequestV1, StartWorkAttemptCommand,
    WorkArtifactHydrationRequestV1, WorkArtifactHydrationV1, WorkAttemptListRequestV1,
    WorkAttemptListV1, WorkAttemptRecoveryReportV1, WorkAttemptStatusRequestV1,
    WorkDuplicateAdjudicationAppendOutcomeV1, WorkEvidenceRetrievalV1,
    WorkEvidenceRetrieveRequestV1, WorkExecutionHistoryV1, WorkExperienceRequestV1,
    WorkExperienceV1, WorkGraphReadRequestV1, WorkGraphReadV1, WorkLeakAdjudicationOutcomeV1,
    WorkPlacementPreflightRequestV1, WorkPlacementReadingV1, WorkPlacementStatusRequestV1,
    WorkProductMutationReceiptV1, WorkProductMutationRequestV1, WorkProposalComparisonRequestV1,
    WorkProposalComparisonV1, WorkRunControlReadingV1, WorkRunControlRequestV1,
    WorkSynthesisAttemptV1, WorkTopologyViewRequestV1,
};

const WORK_SERVICE_ID: &str = "service.work";
pub const WORK_APPLICATION_OPERATION_IDS_V1: [(&str, &str, &str); 32] = [
    (
        "generate_proposal",
        "capability.work.generate_proposal",
        "use-case.work.generate_proposal",
    ),
    ("create", "capability.work.create", "use-case.work.create"),
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
        "list_attempts",
        "capability.work.list_attempts",
        "use-case.work.list_attempts",
    ),
    (
        "execution_history",
        "capability.work.execution_history",
        "use-case.work.execution_history",
    ),
    (
        "hydrate_artifacts",
        "capability.work.hydrate_artifacts",
        "use-case.work.hydrate_artifacts",
    ),
    (
        "retrieve_evidence",
        "capability.work.evidence.read",
        "use-case.work.evidence.read",
    ),
    ("views", "capability.work.views", "use-case.work.views"),
    (
        "experience",
        "capability.work.experience",
        "use-case.work.experience",
    ),
    (
        "compare_proposal",
        "capability.work.compare_proposal",
        "use-case.work.compare_proposal",
    ),
    (
        "prepare_graph_mutation",
        "capability.work.prepare_graph_mutation",
        "use-case.work.prepare_graph_mutation",
    ),
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
        "prepare_duplicate_adjudication",
        "capability.work.prepare_duplicate_adjudication",
        "use-case.work.prepare_duplicate_adjudication",
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

/// Every input to the registry is process-static (operation tables and
/// schemars-derived schemas), but each binding regenerates and cross-validates
/// its request/result schemas, so assembling it is expensive. Per-operation
/// lookups such as [`work_executable_binding`] used to rebuild the whole
/// registry per call, which made every consumer that iterates the operation
/// table quadratic in schema generations and added hundreds of megabytes of
/// allocator churn to first global MCP catalog construction. The registry is
/// immutable, so callers borrow the one process-lifetime authority instead of
/// cloning every schema body.
pub fn work_executable_binding_registry()
-> Result<&'static ExecutableBindingRegistryV1, CatalogValidationError> {
    static REGISTRY: LazyLock<Result<ExecutableBindingRegistryV1, CatalogValidationError>> =
        LazyLock::new(build_work_executable_binding_registry);
    REGISTRY.as_ref().map_err(Clone::clone)
}

fn build_work_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    let bindings = vec![
        available::<GenerateProposalRequest, GeneratedWorkProposal>(
            "generate_proposal",
            "/application/work/generate-proposal",
            EffectClass::Read,
            "tracedecay_contracts::GenerateProposalRequest",
            "tracedecay_contracts::GeneratedWorkProposal",
        )?,
        available::<CreateWorkTaskRequestV1, WorkProductMutationReceiptV1>(
            "create",
            "/application/work/create",
            EffectClass::Administrative,
            "tracedecay_contracts::CreateWorkTaskRequestV1",
            "tracedecay_contracts::WorkProductMutationReceiptV1",
        )?,
        available::<ReviewWorkProposalRequestV1, WorkProductMutationReceiptV1>(
            "review_proposal",
            "/application/work/review-proposal",
            EffectClass::Administrative,
            "tracedecay_contracts::ReviewWorkProposalRequestV1",
            "tracedecay_contracts::WorkProductMutationReceiptV1",
        )?,
        available::<AcceptWorkProposalRequestV1, WorkProductMutationReceiptV1>(
            "accept_proposal",
            "/application/work/accept-proposal",
            EffectClass::Administrative,
            "tracedecay_contracts::AcceptWorkProposalRequestV1",
            "tracedecay_contracts::WorkProductMutationReceiptV1",
        )?,
        available::<AdmitWorkExecutionRequestV1, AdmittedWorkExecutionV1>(
            "admit_execution",
            "/application/work/admit-execution",
            EffectClass::Administrative,
            "tracedecay_contracts::AdmitWorkExecutionRequestV1",
            "tracedecay_contracts::AdmittedWorkExecutionV1",
        )?,
        available::<StartWorkAttemptCommand, WorkAttemptV1>(
            "start_attempt",
            "/application/work/start-attempt",
            EffectClass::Administrative,
            "tracedecay_contracts::StartWorkAttemptCommand",
            "tracedecay_domain::WorkAttemptV1",
        )?,
        available::<AdmitWorkSynthesisCommand, WorkSynthesisAttemptV1>(
            "synthesize",
            "/application/work/synthesize",
            EffectClass::Administrative,
            "tracedecay_contracts::AdmitWorkSynthesisCommand",
            "tracedecay_contracts::WorkSynthesisAttemptV1",
        )?,
        available::<WorkAttemptStatusRequestV1, WorkAttemptV1>(
            "attempt_status",
            "/application/work/attempt-status",
            EffectClass::Read,
            "tracedecay_contracts::WorkAttemptStatusRequestV1",
            "tracedecay_domain::WorkAttemptV1",
        )?,
        available::<CancelWorkAttemptCommand, WorkAttemptV1>(
            "cancel_attempt",
            "/application/work/cancel-attempt",
            EffectClass::Administrative,
            "tracedecay_contracts::CancelWorkAttemptCommand",
            "tracedecay_domain::WorkAttemptV1",
        )?,
        available::<ResumeWorkAttemptsCommand, WorkAttemptRecoveryReportV1>(
            "resume_attempts",
            "/application/work/resume-attempts",
            EffectClass::Administrative,
            "tracedecay_contracts::ResumeWorkAttemptsCommand",
            "tracedecay_contracts::WorkAttemptRecoveryReportV1",
        )?,
        available::<RetryWorkAttemptCommandV1, WorkRetryAttemptOutcomeV1>(
            "retry_attempt",
            "/application/work/retry-attempt",
            EffectClass::Administrative,
            "tracedecay_contracts::RetryWorkAttemptCommandV1",
            "tracedecay_contracts::WorkRetryAttemptOutcomeV1",
        )?,
        available::<WorkAttemptListRequestV1, WorkAttemptListV1>(
            "list_attempts",
            "/application/work/list-attempts",
            EffectClass::Read,
            "tracedecay_contracts::WorkAttemptListRequestV1",
            "tracedecay_contracts::WorkAttemptListV1",
        )?,
        available::<WorkAttemptListRequestV1, WorkExecutionHistoryV1>(
            "execution_history",
            "/application/work/execution-history",
            EffectClass::Read,
            "tracedecay_contracts::WorkAttemptListRequestV1",
            "tracedecay_contracts::WorkExecutionHistoryV1",
        )?,
        available::<WorkArtifactHydrationRequestV1, WorkArtifactHydrationV1>(
            "hydrate_artifacts",
            "/application/work/hydrate-artifacts",
            EffectClass::Read,
            "tracedecay_contracts::WorkArtifactHydrationRequestV1",
            "tracedecay_contracts::WorkArtifactHydrationV1",
        )?,
        available::<WorkEvidenceRetrieveRequestV1, WorkEvidenceRetrievalV1>(
            "retrieve_evidence",
            "/application/work/retrieve-evidence",
            EffectClass::Read,
            "tracedecay_contracts::WorkEvidenceRetrieveRequestV1",
            "tracedecay_contracts::WorkEvidenceRetrievalV1",
        )?,
        available::<WorkGraphReadRequestV1, WorkGraphReadV1>(
            "views",
            "/application/work/views",
            EffectClass::Read,
            "tracedecay_contracts::WorkGraphReadRequestV1",
            "tracedecay_contracts::WorkGraphReadV1",
        )?,
        available::<WorkExperienceRequestV1, WorkExperienceV1>(
            "experience",
            "/application/work/experience",
            EffectClass::Read,
            "tracedecay_contracts::WorkExperienceRequestV1",
            "tracedecay_contracts::WorkExperienceV1",
        )?,
        available::<WorkProposalComparisonRequestV1, WorkProposalComparisonV1>(
            "compare_proposal",
            "/application/work/compare-proposal",
            EffectClass::Read,
            "tracedecay_contracts::WorkProposalComparisonRequestV1",
            "tracedecay_contracts::WorkProposalComparisonV1",
        )?,
        available::<PrepareWorkProductMutationRequestV1, WorkProductMutationRequestV1>(
            "prepare_graph_mutation",
            "/application/work/prepare-graph-mutation",
            EffectClass::Read,
            "tracedecay_contracts::PrepareWorkProductMutationRequestV1",
            "tracedecay_contracts::WorkProductMutationRequestV1",
        )?,
        available::<WorkProductMutationRequestV1, WorkProductMutationReceiptV1>(
            "mutate_graph",
            "/application/work/mutate-graph",
            EffectClass::Administrative,
            "tracedecay_contracts::WorkProductMutationRequestV1",
            "tracedecay_contracts::WorkProductMutationReceiptV1",
        )?,
        available::<WorkTopologyViewRequestV1, ExecutionTopologyViewV1>(
            "topology",
            "/application/work/topology",
            EffectClass::Read,
            "tracedecay_contracts::WorkTopologyViewRequestV1",
            "tracedecay_contracts::ExecutionTopologyViewV1",
        )?,
        available::<ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1>(
            "topology_metrics",
            "/application/work/topology-metrics",
            EffectClass::Read,
            "tracedecay_contracts::ExecutionTopologyMetricsRequestV1",
            "tracedecay_contracts::ExecutionTopologyMetricsV1",
        )?,
        available::<PrepareWorkDuplicateAdjudicationRequestV1, WorkDuplicateAdjudicationCommandV1>(
            "prepare_duplicate_adjudication",
            "/application/work/prepare-duplicate-adjudication",
            EffectClass::Read,
            "tracedecay_contracts::PrepareWorkDuplicateAdjudicationRequestV1",
            "tracedecay_domain::WorkDuplicateAdjudicationCommandV1",
        )?,
        available::<WorkDuplicateAdjudicationCommandV1, WorkDuplicateAdjudicationAppendOutcomeV1>(
            "adjudicate_duplicate",
            "/application/work/adjudicate-duplicate",
            EffectClass::Administrative,
            "tracedecay_domain::WorkDuplicateAdjudicationCommandV1",
            "tracedecay_contracts::WorkDuplicateAdjudicationAppendOutcomeV1",
        )?,
        available::<AdjudicateWorkLeakCommandV1, WorkLeakAdjudicationOutcomeV1>(
            "adjudicate_leak",
            "/application/work/adjudicate-leak",
            EffectClass::Administrative,
            "tracedecay_contracts::AdjudicateWorkLeakCommandV1",
            "tracedecay_contracts::WorkLeakAdjudicationOutcomeV1",
        )?,
        available::<PauseWorkRunCommand, WorkRunControlV1>(
            "pause_run",
            "/application/work/pause-run",
            EffectClass::Administrative,
            "tracedecay_contracts::PauseWorkRunCommand",
            "tracedecay_domain::WorkRunControlV1",
        )?,
        available::<ResumeWorkRunCommand, WorkRunControlV1>(
            "resume_run",
            "/application/work/resume-run",
            EffectClass::Administrative,
            "tracedecay_contracts::ResumeWorkRunCommand",
            "tracedecay_domain::WorkRunControlV1",
        )?,
        available::<WorkRunControlRequestV1, WorkRunControlReadingV1>(
            "run_control",
            "/application/work/run-control",
            EffectClass::Read,
            "tracedecay_contracts::WorkRunControlRequestV1",
            "tracedecay_contracts::WorkRunControlReadingV1",
        )?,
        available::<WorkPlacementPreflightRequestV1, WorkPlacementPreflightV1>(
            "placement_preflight",
            "/application/work/placement-preflight",
            EffectClass::Read,
            "tracedecay_contracts::WorkPlacementPreflightRequestV1",
            "tracedecay_domain::WorkPlacementPreflightV1",
        )?,
        available::<AdmitWorkPlacementCommand, WorkPlacementV1>(
            "admit_placement",
            "/application/work/admit-placement",
            EffectClass::Administrative,
            "tracedecay_contracts::AdmitWorkPlacementCommand",
            "tracedecay_domain::WorkPlacementV1",
        )?,
        available::<WorkPlacementStatusRequestV1, WorkPlacementReadingV1>(
            "placement_status",
            "/application/work/placement-status",
            EffectClass::Read,
            "tracedecay_contracts::WorkPlacementStatusRequestV1",
            "tracedecay_contracts::WorkPlacementReadingV1",
        )?,
        available::<ReleaseWorkPlacementCommand, WorkPlacementV1>(
            "release_placement",
            "/application/work/release-placement",
            EffectClass::Administrative,
            "tracedecay_contracts::ReleaseWorkPlacementCommand",
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
) -> Result<Option<&'static ExecutableBindingV1>, CatalogValidationError> {
    Ok(work_executable_binding_registry()?
        .get(operation_id)
        .and_then(|availability| availability.binding()))
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
        OperationId::new(format!("operation.work.{operation}")).map_err(|_| {
            invalid_identity(
                "operation_id",
                "work operation name does not form a canonical operation ID",
            )
        })?,
        ServiceId::new(WORK_SERVICE_ID)
            .map_err(|_| invalid_identity("service_id", "work service ID is not canonical"))?,
        request_schema,
        result_schema,
        CodecBindingKey::new(format!("codec.work.{operation}.json.v1")).map_err(|_| {
            invalid_identity(
                "codec_binding_key",
                "work operation name does not form a canonical codec key",
            )
        })?,
        RouteExposureV1::Public {
            binding_id: BindingId::new(format!("binding.http.work.{operation}")).map_err(|_| {
                invalid_identity(
                    "binding_id",
                    "work operation name does not form a canonical binding ID",
                )
            })?,
            route_path: route_path.to_owned(),
        },
    )?;
    Ok(ExecutableBindingAvailabilityV1::available(binding))
}

fn invalid_identity(field: &'static str, reason: &'static str) -> CatalogValidationError {
    CatalogValidationError::InvalidValue { field, reason }
}

fn work_manifest(
    operation: &str,
    effect: EffectClass,
) -> Result<CapabilityManifestV1, CatalogValidationError> {
    let read_only = effect.is_read_only();
    let binding_id = BindingId::new(format!("binding.http.work.{operation}")).map_err(|_| {
        invalid_identity(
            "binding_id",
            "work operation name does not form a canonical binding ID",
        )
    })?;
    application_capability_manifest(ApplicationCapabilityManifestInput {
        capability_id: CapabilityId::new(format!("capability.work.{operation}")).map_err(|_| {
            invalid_identity(
                "capability_id",
                "work operation name does not form a canonical capability ID",
            )
        })?,
        use_case_id: UseCaseId::new(format!("use-case.work.{operation}")).map_err(|_| {
            invalid_identity(
                "use_case_id",
                "work operation name does not form a canonical use-case ID",
            )
        })?,
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
        pagination: read_only
            .then(|| PaginationContract::new(100, 1_000, 60_000))
            .transpose()?,
        inverse: None,
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::ExpectedState,
        ])?,
        terminal_states: TerminalStateContract::new(terminal_states(read_only))?,
        availability: AvailabilityContract::Available,
        binding_ids: vec![binding_id],
        profile_eligibility: vec![
            ProfileId::new("profile.default")
                .map_err(|_| invalid_identity("profile_id", "work profile ID is not canonical"))?,
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
        for retired in [
            "operation.work.snapshot",
            "operation.work.delta",
            "operation.work.replan_dependencies",
            "operation.work.accept_task",
        ] {
            assert!(
                registry
                    .get(&tracedecay_tool_catalog::OperationId::new(retired).unwrap())
                    .is_none(),
                "retired operation {retired} must not be advertised"
            );
        }
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
            "tracedecay_contracts::WorkProductMutationRequestV1"
        );
        assert_eq!(
            mutation.result_schema().rust_type_path(),
            "tracedecay_contracts::WorkProductMutationReceiptV1"
        );
        let RouteExposureV1::Public { route_path, .. } = mutation.exposure() else {
            panic!("the Work graph mutation binding must be publicly routed");
        };
        assert_eq!(route_path, "/application/work/mutate-graph");
        assert!(!mutation.effect().is_read_only());
    }

    #[test]
    fn create_binding_uses_the_canonical_work_product_authority() {
        let registry = work_executable_binding_registry().unwrap();
        let create = registry
            .get(&tracedecay_tool_catalog::OperationId::new("operation.work.create").unwrap())
            .unwrap()
            .binding()
            .unwrap();
        assert_eq!(
            create.request_schema().body()["title"],
            "CreateWorkTaskRequestV1"
        );
        assert_eq!(
            create.result_schema().body()["title"],
            "WorkProductMutationReceiptV1"
        );
        assert_eq!(
            create.request_schema().rust_type_path(),
            "tracedecay_contracts::CreateWorkTaskRequestV1"
        );
        assert_eq!(
            create.result_schema().rust_type_path(),
            "tracedecay_contracts::WorkProductMutationReceiptV1"
        );

        let admit = registry
            .get(
                &tracedecay_tool_catalog::OperationId::new("operation.work.admit_execution")
                    .unwrap(),
            )
            .unwrap()
            .binding()
            .unwrap();
        assert_eq!(
            admit.request_schema().rust_type_path(),
            "tracedecay_contracts::AdmitWorkExecutionRequestV1"
        );
        assert_eq!(
            admit.result_schema().rust_type_path(),
            "tracedecay_contracts::WorkProductMutationReceiptV1"
        );
    }
}
