use schemars::JsonSchema;
use tracedecay_domain::{WorkProjection, WorkProjectionDeltaV1, WorkProjectionSnapshotV1};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, CancellationContract, CancellationPoint,
    CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1, CatalogValidationError,
    CodecBindingKey, DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    ExecutableUnavailableDispositionV1, IdempotencyContract, LifecycleClass, OperationId,
    PaginationContract, PrivacyClass, ProfileId, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RouteExposureV1, RoutingContractV1,
    SchemaBodyAuthorityV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement, ServiceId,
    StreamingContract, TerminalState, TerminalStateContract, UseCaseId,
};

use crate::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AttachRuntimeEvidenceCommand,
    CreateWorkCommand, ReplanDependenciesCommand, ReviewProposalCommand,
    WorkAttemptAcquireLeaseRequestV1, WorkAttemptCancelRequestV1,
    WorkAttemptPublishArtifactRequestV1, WorkAttemptPublishProgressRequestV1,
    WorkAttemptRecoverRequestV1, WorkAttemptRenewLeaseRequestV1, WorkAttemptResponseV1,
    WorkAttemptStartRequestV1, WorkAttemptTerminalizeRequestV1, WorkProjectionDeltaRequestV1,
    WorkProjectionSnapshotRequestV1,
};

const WORK_SERVICE_ID: &str = "service.work";
pub const WORK_ATTEMPT_OPERATION_IDS_V1: [(&str, &str, &str); 8] = [
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
        "attempt_terminalize",
        "capability.work.attempt_terminalize",
        "use-case.work.attempt_terminalize",
    ),
];

pub fn work_executable_binding_registry(
    application_routes_mounted: bool,
    attempt_routes_mounted: bool,
) -> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    let mut bindings = vec![
        route_binding::<WorkProjectionSnapshotRequestV1, WorkProjectionSnapshotV1>(
            application_routes_mounted,
            "snapshot",
            "/application/work/snapshot",
            EffectClass::Read,
        )?,
        route_binding::<WorkProjectionDeltaRequestV1, WorkProjectionDeltaV1>(
            application_routes_mounted,
            "delta",
            "/application/work/delta",
            EffectClass::Read,
        )?,
        route_binding::<CreateWorkCommand, WorkProjection>(
            application_routes_mounted,
            "create",
            "/application/work/create",
            EffectClass::Administrative,
        )?,
        route_binding::<ReplanDependenciesCommand, WorkProjection>(
            application_routes_mounted,
            "replan_dependencies",
            "/application/work/replan-dependencies",
            EffectClass::Administrative,
        )?,
        route_binding::<ReviewProposalCommand, WorkProjection>(
            application_routes_mounted,
            "review_proposal",
            "/application/work/review-proposal",
            EffectClass::Administrative,
        )?,
        route_binding::<AcceptProposalCommand, WorkProjection>(
            application_routes_mounted,
            "accept_proposal",
            "/application/work/accept-proposal",
            EffectClass::Administrative,
        )?,
        route_binding::<AdmitExecutionCommand, WorkProjection>(
            application_routes_mounted,
            "admit_execution",
            "/application/work/admit-execution",
            EffectClass::Administrative,
        )?,
        route_binding::<AttachRuntimeEvidenceCommand, WorkProjection>(
            application_routes_mounted,
            "attach_runtime_evidence",
            "/application/work/attach-runtime-evidence",
            EffectClass::Administrative,
        )?,
        route_binding::<AcceptTaskCommand, WorkProjection>(
            application_routes_mounted,
            "accept_task",
            "/application/work/accept-task",
            EffectClass::Administrative,
        )?,
    ];
    bindings.extend([
        route_binding::<WorkAttemptAcquireLeaseRequestV1, WorkAttemptResponseV1>(
            attempt_routes_mounted,
            "attempt_acquire_lease",
            "/application/work/attempt/acquire-lease",
            EffectClass::Administrative,
        )?,
        route_binding::<WorkAttemptRenewLeaseRequestV1, WorkAttemptResponseV1>(
            attempt_routes_mounted,
            "attempt_renew_lease",
            "/application/work/attempt/renew-lease",
            EffectClass::Administrative,
        )?,
        route_binding::<WorkAttemptStartRequestV1, WorkAttemptResponseV1>(
            attempt_routes_mounted,
            "attempt_start",
            "/application/work/attempt/start",
            EffectClass::Administrative,
        )?,
        route_binding::<WorkAttemptPublishProgressRequestV1, WorkAttemptResponseV1>(
            attempt_routes_mounted,
            "attempt_publish_progress",
            "/application/work/attempt/publish-progress",
            EffectClass::Administrative,
        )?,
        route_binding::<WorkAttemptPublishArtifactRequestV1, WorkAttemptResponseV1>(
            attempt_routes_mounted,
            "attempt_publish_artifact",
            "/application/work/attempt/publish-artifact",
            EffectClass::Administrative,
        )?,
        route_binding::<WorkAttemptCancelRequestV1, WorkAttemptResponseV1>(
            attempt_routes_mounted,
            "attempt_cancel",
            "/application/work/attempt/cancel",
            EffectClass::Administrative,
        )?,
        route_binding::<WorkAttemptRecoverRequestV1, WorkAttemptResponseV1>(
            attempt_routes_mounted,
            "attempt_recover",
            "/application/work/attempt/recover",
            EffectClass::Administrative,
        )?,
        route_binding::<WorkAttemptTerminalizeRequestV1, WorkAttemptResponseV1>(
            attempt_routes_mounted,
            "attempt_terminalize",
            "/application/work/attempt/terminalize",
            EffectClass::Administrative,
        )?,
    ]);
    ExecutableBindingRegistryV1::new(bindings)
}

fn route_binding<Request, Output>(
    mounted: bool,
    operation: &str,
    route_path: &str,
    effect: EffectClass,
) -> Result<ExecutableBindingAvailabilityV1, CatalogValidationError>
where
    Request: JsonSchema,
    Output: JsonSchema,
{
    if mounted {
        available::<Request, Output>(operation, route_path, effect)
    } else {
        Ok(ExecutableBindingAvailabilityV1::Unavailable {
            operation_id: OperationId::new(format!("operation.work.{operation}"))
                .expect("static Work operation ID is valid"),
            disposition: ExecutableUnavailableDispositionV1::RouteUnavailable,
        })
    }
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
    Ok(SchemaRef::new(
        SchemaId::new(id).expect("static Work schema ID is valid"),
        1,
    )?)
}

#[cfg(test)]
mod tests {
    use tracedecay_tool_catalog::{CancellationPoint, RouteExposureV1};

    use super::work_executable_binding_registry;

    #[test]
    fn work_registry_advertises_only_mounted_application_operations() {
        let registry = work_executable_binding_registry(true, true).unwrap();
        let advertised = registry
            .iter()
            .filter_map(|availability| availability.binding())
            .collect::<Vec<_>>();
        assert_eq!(advertised.len(), 17);
        let actual = advertised
            .iter()
            .map(|binding| {
                let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
                    panic!("available Work binding must have a public route");
                };
                (binding.operation_id().as_str(), route_path.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), 17);
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
        let registry = work_executable_binding_registry(false, true).unwrap();
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

    #[test]
    fn runtime_attempt_operations_remain_unavailable_without_mounted_routes() {
        let registry = work_executable_binding_registry(false, false).unwrap();
        for operation in [
            "attempt_acquire_lease",
            "attempt_renew_lease",
            "attempt_start",
            "attempt_publish_progress",
            "attempt_publish_artifact",
            "attempt_cancel",
            "attempt_recover",
            "attempt_terminalize",
        ] {
            let operation =
                tracedecay_tool_catalog::OperationId::new(format!("operation.work.{operation}"))
                    .unwrap();
            assert!(registry.get(&operation).unwrap().binding().is_none());
        }
    }
}
