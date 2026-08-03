use schemars::JsonSchema;
use tracedecay_domain::{WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1};
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

use crate::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AttachRuntimeEvidenceCommand,
    CreateWorkCommand, ReplanDependenciesCommand, ReviewProposalRequestV1,
    WorkAttemptAcquireLeaseRequestV1, WorkAttemptCancelRequestV1, WorkAttemptFinishRequestV1,
    WorkAttemptPublishArtifactRequestV1, WorkAttemptPublishProgressRequestV1,
    WorkAttemptRecoverRequestV1, WorkAttemptRenewLeaseRequestV1, WorkAttemptResponseV1,
    WorkAttemptStartRequestV1, WorkAttemptTerminalizeRequestV1, WorkProjectionDeltaRequestV1,
    WorkProjectionSnapshotRequestV1,
};

const WORK_SERVICE_ID: &str = "service.work";
pub const WORK_APPLICATION_OPERATION_IDS_V1: [(&str, &str, &str); 9] = [
    (
        "snapshot",
        "capability.work.snapshot",
        "use-case.work.snapshot",
    ),
    ("delta", "capability.work.delta", "use-case.work.delta"),
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
];
pub const WORK_ATTEMPT_OPERATION_IDS_V1: [(&str, &str, &str); 9] = [
    (
        "attempt_acquire_lease",
        "capability.work.attempt_acquire_lease",
        "use-case.work.attempt_acquire_lease",
    ),
    (
        "attempt_renew_lease",
        "capability.work.attempt_renew_lease",
        "use-case.work.attempt_renew_lease",
    ),
    (
        "attempt_start",
        "capability.work.attempt_start",
        "use-case.work.attempt_start",
    ),
    (
        "attempt_publish_progress",
        "capability.work.attempt_publish_progress",
        "use-case.work.attempt_publish_progress",
    ),
    (
        "attempt_publish_artifact",
        "capability.work.attempt_publish_artifact",
        "use-case.work.attempt_publish_artifact",
    ),
    (
        "attempt_cancel",
        "capability.work.attempt_cancel",
        "use-case.work.attempt_cancel",
    ),
    (
        "attempt_recover",
        "capability.work.attempt_recover",
        "use-case.work.attempt_recover",
    ),
    (
        "attempt_finish",
        "capability.work.attempt_finish",
        "use-case.work.attempt_finish",
    ),
    (
        "attempt_terminalize",
        "capability.work.attempt_terminalize",
        "use-case.work.attempt_terminalize",
    ),
];

pub fn work_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    let mut bindings = vec![
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
    ];
    bindings.extend([
        available::<WorkAttemptAcquireLeaseRequestV1, WorkAttemptResponseV1>(
            "attempt_acquire_lease",
            "/application/work/attempt/acquire-lease",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptRenewLeaseRequestV1, WorkAttemptResponseV1>(
            "attempt_renew_lease",
            "/application/work/attempt/renew-lease",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptStartRequestV1, WorkAttemptResponseV1>(
            "attempt_start",
            "/application/work/attempt/start",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptPublishProgressRequestV1, WorkAttemptResponseV1>(
            "attempt_publish_progress",
            "/application/work/attempt/publish-progress",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptPublishArtifactRequestV1, WorkAttemptResponseV1>(
            "attempt_publish_artifact",
            "/application/work/attempt/publish-artifact",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptCancelRequestV1, WorkAttemptResponseV1>(
            "attempt_cancel",
            "/application/work/attempt/cancel",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptRecoverRequestV1, WorkAttemptResponseV1>(
            "attempt_recover",
            "/application/work/attempt/recover",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptFinishRequestV1, WorkAttemptResponseV1>(
            "attempt_finish",
            "/application/work/attempt/finish",
            EffectClass::Administrative,
        )?,
        available::<WorkAttemptTerminalizeRequestV1, WorkAttemptResponseV1>(
            "attempt_terminalize",
            "/application/work/attempt/terminalize",
            EffectClass::Administrative,
        )?,
    ]);
    ExecutableBindingRegistryV1::new(bindings)
}

fn available<Request, Output>(
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

    #[test]
    fn runtime_attempt_operations_use_versioned_public_bindings() {
        let registry = work_executable_binding_registry().unwrap();
        for (operation, request_title) in [
            ("attempt_acquire_lease", "WorkAttemptAcquireLeaseRequestV1"),
            ("attempt_renew_lease", "WorkAttemptRenewLeaseRequestV1"),
            ("attempt_start", "WorkAttemptStartRequestV1"),
            (
                "attempt_publish_progress",
                "WorkAttemptPublishProgressRequestV1",
            ),
            (
                "attempt_publish_artifact",
                "WorkAttemptPublishArtifactRequestV1",
            ),
            ("attempt_cancel", "WorkAttemptCancelRequestV1"),
            ("attempt_recover", "WorkAttemptRecoverRequestV1"),
            ("attempt_finish", "WorkAttemptFinishRequestV1"),
            ("attempt_terminalize", "WorkAttemptTerminalizeRequestV1"),
        ] {
            let operation =
                tracedecay_tool_catalog::OperationId::new(format!("operation.work.{operation}"))
                    .unwrap();
            let binding = registry.get(&operation).unwrap().binding().unwrap();
            assert_eq!(binding.request_schema().body()["title"], request_title);
            assert_eq!(
                binding.result_schema().body()["title"],
                "WorkAttemptResponseV1"
            );
            assert!(matches!(
                binding.exposure(),
                RouteExposureV1::Public { route_path, .. }
                    if route_path.starts_with("/application/work/attempt/")
            ));
        }
    }
}
