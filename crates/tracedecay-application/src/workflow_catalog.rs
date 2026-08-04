use schemars::JsonSchema;
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

use tracedecay_domain::WorkflowDefinitionV1;

use crate::{
    TaskHandoffGrantV1, TaskHandoffIssueRequestV1, TaskHandoffRedeemRequestV1,
    TaskHandoffRedeemedV1, WorkflowActivationV1, WorkflowDefinitionActivateRequestV1,
    WorkflowDefinitionRegisterRequestV1, WorkflowExecutionTruthV1, WorkflowFanOutRequestV1,
};

const WORKFLOW_SERVICE_ID: &str = "service.workflow";

pub const WORKFLOW_APPLICATION_OPERATION_IDS_V1: [(&str, &str, &str); 5] = [
    (
        "register_definition",
        "capability.workflow.register_definition",
        "use-case.workflow.register_definition",
    ),
    (
        "activate_definition",
        "capability.workflow.activate_definition",
        "use-case.workflow.activate_definition",
    ),
    (
        "execute_fan_out",
        "capability.workflow.execute_fan_out",
        "use-case.workflow.execute_fan_out",
    ),
    (
        "handoff_issue",
        "capability.workflow.handoff_issue",
        "use-case.workflow.handoff_issue",
    ),
    (
        "handoff_redeem",
        "capability.workflow.handoff_redeem",
        "use-case.workflow.handoff_redeem",
    ),
];

pub fn workflow_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    ExecutableBindingRegistryV1::new(vec![
        available::<WorkflowDefinitionRegisterRequestV1, WorkflowDefinitionV1>(
            "register_definition",
            "/application/workflow/register-definition",
        )?,
        available::<WorkflowDefinitionActivateRequestV1, WorkflowActivationV1>(
            "activate_definition",
            "/application/workflow/activate-definition",
        )?,
        available::<WorkflowFanOutRequestV1, WorkflowExecutionTruthV1>(
            "execute_fan_out",
            "/application/workflow/execute-fan-out",
        )?,
        available::<TaskHandoffIssueRequestV1, TaskHandoffGrantV1>(
            "handoff_issue",
            "/application/workflow/handoff-issue",
        )?,
        available::<TaskHandoffRedeemRequestV1, TaskHandoffRedeemedV1>(
            "handoff_redeem",
            "/application/workflow/handoff-redeem",
        )?,
    ])
}

fn available<Request, Output>(
    operation: &str,
    route_path: &str,
) -> Result<ExecutableBindingAvailabilityV1, CatalogValidationError>
where
    Request: JsonSchema,
    Output: JsonSchema,
{
    let manifest = workflow_manifest(operation)?;
    let request_schema =
        SchemaBodyAuthorityV1::for_type::<Request>(manifest.request_schema().clone())?;
    let result_schema =
        SchemaBodyAuthorityV1::for_type::<Output>(manifest.result_schema().clone())?;
    let binding = ExecutableBindingV1::direct(
        &manifest,
        OperationId::new(format!("operation.workflow.{operation}"))
            .expect("static Workflow operation ID is valid"),
        ServiceId::new(WORKFLOW_SERVICE_ID).expect("static Workflow service ID is valid"),
        request_schema,
        result_schema,
        CodecBindingKey::new(format!("codec.workflow.{operation}.json.v1"))
            .expect("static Workflow codec ID is valid"),
        RouteExposureV1::Public {
            binding_id: BindingId::new(format!("binding.http.workflow.{operation}"))
                .expect("static Workflow binding ID is valid"),
            route_path: route_path.to_owned(),
        },
    )?;
    Ok(ExecutableBindingAvailabilityV1::available(binding))
}

fn workflow_manifest(operation: &str) -> Result<CapabilityManifestV1, CatalogValidationError> {
    let binding_id = BindingId::new(format!("binding.http.workflow.{operation}"))
        .expect("static Workflow binding ID is valid");
    CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: CapabilityId::new(format!("capability.workflow.{operation}"))
            .expect("static Workflow capability ID is valid"),
        use_case_id: UseCaseId::new(format!("use-case.workflow.{operation}"))
            .expect("static Workflow use-case ID is valid"),
        routing: RoutingContractV1::new(
            1,
            format!("Workflow {operation}"),
            format!("Execute the canonical Workflow {operation} application use case."),
            vec![format!("Workflow {operation}")],
        )?,
        request_schema: schema_ref(format!("schema.workflow.{operation}.request"))?,
        result_schema: schema_ref(format!("schema.workflow.{operation}.result"))?,
        effect: EffectClass::Administrative,
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
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeEffect,
            CancellationPoint::EffectInFlight,
            CancellationPoint::AfterCommit,
        ])?,
        deadline: DeadlineContract::new(30_000, DeadlineBehavior::ReturnEffectReceipt)?,
        pagination: None::<PaginationContract>,
        idempotency: IdempotencyContract::Required,
        inverse: tracedecay_tool_catalog::InverseContract::Unavailable {
            reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
        },
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::ExpectedState,
        ])?,
        reconciliation: ReconciliationContract::Required,
        receipt: ReceiptContract::DurableEffect,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Partial,
            TerminalState::EffectUnknown,
        ])?,
        availability: AvailabilityContract::Available,
        binding_ids: vec![binding_id],
        profile_eligibility: vec![
            ProfileId::new("profile.default").expect("static profile ID is valid"),
        ],
        required_features: Vec::new(),
    })
}

fn schema_ref(id: String) -> Result<SchemaRef, CatalogValidationError> {
    SchemaRef::new(
        SchemaId::new(id).expect("static Workflow schema ID is valid"),
        1,
    )
}

#[cfg(test)]
mod tests {
    use super::workflow_executable_binding_registry;

    #[test]
    fn workflow_registry_advertises_every_daemon_owned_route() {
        let registry = workflow_executable_binding_registry().unwrap();
        assert_eq!(registry.iter().count(), 5);
        let advertised = registry
            .iter()
            .filter_map(|availability| availability.binding())
            .collect::<Vec<_>>();
        assert_eq!(advertised.len(), 5);
        assert_eq!(
            advertised
                .iter()
                .map(|binding| binding.operation_id().as_str())
                .collect::<Vec<_>>(),
            vec![
                "operation.workflow.activate_definition",
                "operation.workflow.execute_fan_out",
                "operation.workflow.handoff_issue",
                "operation.workflow.handoff_redeem",
                "operation.workflow.register_definition",
            ]
        );
        assert_eq!(
            advertised
                .iter()
                .map(|binding| match binding.exposure() {
                    tracedecay_tool_catalog::RouteExposureV1::Public { route_path, .. } =>
                        route_path.as_str(),
                    _ => panic!("daemon-owned Workflow operations must have public routes"),
                })
                .collect::<Vec<_>>(),
            vec![
                "/application/workflow/activate-definition",
                "/application/workflow/execute-fan-out",
                "/application/workflow/handoff-issue",
                "/application/workflow/handoff-redeem",
                "/application/workflow/register-definition",
            ]
        );
    }
}
