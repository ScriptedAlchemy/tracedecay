use schemars::JsonSchema;
use tracedecay_domain::{
    ManifestDigest, WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1,
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

use crate::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AttachRuntimeEvidenceCommand,
    CancelWorkAttemptCommand, CreateWorkCommand, GenerateProposalRequest, GeneratedWorkProposal,
    ReplanDependenciesCommand, ResumeWorkAttemptsCommand, ReviewProposalRequestV1,
    StartWorkAttemptCommand, WorkAttemptListRequestV1, WorkAttemptListV1,
    WorkAttemptRecoveryReportV1, WorkAttemptStatusRequestV1, WorkProjectionDeltaRequestV1,
    WorkProjectionSnapshotRequestV1,
};

const WORK_SERVICE_ID: &str = "service.work";
pub const WORK_APPLICATION_OPERATION_IDS_V1: [(&str, &str, &str); 15] = [
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
        "list_attempts",
        "capability.work.list_attempts",
        "use-case.work.list_attempts",
    ),
];

pub fn work_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    let bindings = vec![
        available::<WorkProjectionSnapshotRequestV1, WorkProjectionSnapshotV1>(
            "snapshot",
            "/application/work/snapshot",
            EffectClass::Read,
        )?,
        available::<WorkProjectionDeltaRequestV1, WorkProjectionDeltaV1>(
            "delta",
            "/application/work/delta",
            EffectClass::Read,
        )?,
        available::<GenerateProposalRequest, GeneratedWorkProposal>(
            "generate_proposal",
            "/application/work/generate-proposal",
            EffectClass::Read,
        )?,
        available::<CreateWorkCommand, WorkProjection>(
            "create",
            "/application/work/create",
            EffectClass::Administrative,
        )?,
        available::<ReplanDependenciesCommand, WorkProjection>(
            "replan_dependencies",
            "/application/work/replan-dependencies",
            EffectClass::Administrative,
        )?,
        available::<ReviewProposalRequestV1, WorkProjection>(
            "review_proposal",
            "/application/work/review-proposal",
            EffectClass::Administrative,
        )?,
        available::<AcceptProposalCommand, WorkProjection>(
            "accept_proposal",
            "/application/work/accept-proposal",
            EffectClass::Administrative,
        )?,
        available::<AdmitExecutionCommand, WorkProjection>(
            "admit_execution",
            "/application/work/admit-execution",
            EffectClass::Administrative,
        )?,
        available::<AttachRuntimeEvidenceCommand, WorkProjection>(
            "attach_runtime_evidence",
            "/application/work/attach-runtime-evidence",
            EffectClass::Administrative,
        )?,
        available::<AcceptTaskCommand, WorkProjection>(
            "accept_task",
            "/application/work/accept-task",
            EffectClass::Administrative,
        )?,
        available::<StartWorkAttemptCommand, WorkAttemptV1>(
            "start_attempt",
            "/application/work/start-attempt",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptStatusRequestV1, WorkAttemptV1>(
            "attempt_status",
            "/application/work/attempt-status",
            EffectClass::Read,
        )?,
        available::<CancelWorkAttemptCommand, WorkAttemptV1>(
            "cancel_attempt",
            "/application/work/cancel-attempt",
            EffectClass::Administrative,
        )?,
        available::<ResumeWorkAttemptsCommand, WorkAttemptRecoveryReportV1>(
            "resume_attempts",
            "/application/work/resume-attempts",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptListRequestV1, WorkAttemptListV1>(
            "list_attempts",
            "/application/work/list-attempts",
            EffectClass::Read,
        )?,
    ];
    ExecutableBindingRegistryV1::new(bindings)
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
) -> Result<ExecutableBindingAvailabilityV1, CatalogValidationError>
where
    Request: JsonSchema,
    Output: JsonSchema,
{
    let manifest = work_manifest(operation, effect)?;
    let request_schema =
        SchemaBodyAuthorityV1::for_type::<Request>(manifest.request_schema().clone())?;
    let result_schema =
        SchemaBodyAuthorityV1::for_type::<Output>(manifest.result_schema().clone())?;
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

    use super::work_executable_binding_registry;

    #[test]
    fn work_registry_advertises_only_mounted_application_operations() {
        let registry = work_executable_binding_registry().unwrap();
        let advertised = registry
            .iter()
            .filter_map(|availability| availability.binding())
            .collect::<Vec<_>>();
        assert!(!advertised.is_empty());
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
}
