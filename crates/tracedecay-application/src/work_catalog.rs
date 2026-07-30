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
    WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1,
};

const WORK_SERVICE_ID: &str = "service.work";

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
        available::<ReviewProposalCommand, WorkProjection>(
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
        unavailable("attempt_acquire_lease"),
        unavailable("attempt_renew_lease"),
        unavailable("attempt_start"),
        unavailable("attempt_publish_progress"),
        unavailable("attempt_publish_artifact"),
        unavailable("attempt_cancel"),
        unavailable("attempt_recover"),
        unavailable("attempt_terminalize"),
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

fn unavailable(operation: &str) -> ExecutableBindingAvailabilityV1 {
    ExecutableBindingAvailabilityV1::Unavailable {
        operation_id: OperationId::new(format!("operation.work.{operation}"))
            .expect("static Work operation IDs are valid"),
        disposition: ExecutableUnavailableDispositionV1::ServiceNotRegistered,
    }
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
        let registry = work_executable_binding_registry().unwrap();
        let advertised = registry
            .iter()
            .filter_map(|availability| availability.binding())
            .collect::<Vec<_>>();
        assert_eq!(advertised.len(), 9);
        let actual = advertised
            .iter()
            .map(|binding| {
                let RouteExposureV1::Public { route_path, .. } = binding.exposure() else {
                    panic!("available Work binding must have a public route");
                };
                (binding.operation_id().as_str(), route_path.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            vec![
                (
                    "operation.work.accept_proposal",
                    "/application/work/accept-proposal"
                ),
                (
                    "operation.work.accept_task",
                    "/application/work/accept-task"
                ),
                (
                    "operation.work.admit_execution",
                    "/application/work/admit-execution"
                ),
                (
                    "operation.work.attach_runtime_evidence",
                    "/application/work/attach-runtime-evidence"
                ),
                ("operation.work.create", "/application/work/create"),
                ("operation.work.delta", "/application/work/delta"),
                (
                    "operation.work.replan_dependencies",
                    "/application/work/replan-dependencies"
                ),
                (
                    "operation.work.review_proposal",
                    "/application/work/review-proposal"
                ),
                ("operation.work.snapshot", "/application/work/snapshot"),
            ]
        );
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
    fn runtime_attempt_operations_remain_typed_unavailable() {
        let registry = work_executable_binding_registry().unwrap();
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
            let availability = registry.get(&operation).unwrap();
            assert!(availability.binding().is_none());
        }
    }
}
