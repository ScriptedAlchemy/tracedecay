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

use crate::{
    TaskHandoffGrant, TaskHandoffIssueRequest, TaskHandoffRedeemRequest, TaskHandoffRedeemed,
    WorkflowActivation, WorkflowDefinitionActivateRequest, WorkflowDefinitionDiff,
    WorkflowDefinitionDiffRequest, WorkflowDefinitionGetRequest, WorkflowDefinitionHistoryRequest,
    WorkflowDefinitionListRequest, WorkflowDefinitionRegisterRequest,
    WorkflowDefinitionRetireRequest, WorkflowDefinitionValidateRequest,
    WorkflowDefinitionValidation, WorkflowFanOutRequest, WorkflowRetirement,
};

const WORKFLOW_SERVICE_ID: &str = "service.workflow";

pub const WORKFLOW_APPLICATION_OPERATION_IDS: [(&str, &str, &str); 11] = [
    (
        "register_definition",
        "capability.workflow.register_definition",
        "use-case.workflow.register_definition",
    ),
    (
        "validate_definition",
        "capability.workflow.validate_definition",
        "use-case.workflow.validate_definition",
    ),
    (
        "get_definition",
        "capability.workflow.get_definition",
        "use-case.workflow.get_definition",
    ),
    (
        "list_definitions",
        "capability.workflow.list_definitions",
        "use-case.workflow.list_definitions",
    ),
    (
        "definition_history",
        "capability.workflow.definition_history",
        "use-case.workflow.definition_history",
    ),
    (
        "diff_definition",
        "capability.workflow.diff_definition",
        "use-case.workflow.diff_definition",
    ),
    (
        "activate_definition",
        "capability.workflow.activate_definition",
        "use-case.workflow.activate_definition",
    ),
    (
        "retire_definition",
        "capability.workflow.retire_definition",
        "use-case.workflow.retire_definition",
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
    ExecutableBindingRegistryV1::new(
        WORKFLOW_APPLICATION_OPERATION_IDS
            .iter()
            .map(|(operation, _, _)| match *operation {
                "register_definition" => {
                    available::<
                        WorkflowDefinitionRegisterRequest,
                        tracedecay_domain::WorkflowDefinition,
                    >(operation, "/application/workflow/register-definition")
                }
                "validate_definition" => available::<
                    WorkflowDefinitionValidateRequest,
                    WorkflowDefinitionValidation,
                >(operation, "/application/workflow/validate-definition"),
                "get_definition" => available::<
                    WorkflowDefinitionGetRequest,
                    tracedecay_domain::WorkflowDefinition,
                >(operation, "/application/workflow/get-definition"),
                "list_definitions" => available::<
                    WorkflowDefinitionListRequest,
                    Vec<tracedecay_domain::WorkflowDefinition>,
                >(operation, "/application/workflow/list-definitions"),
                "definition_history" => available::<
                    WorkflowDefinitionHistoryRequest,
                    Vec<tracedecay_domain::WorkflowDefinition>,
                >(operation, "/application/workflow/definition-history"),
                "diff_definition" => available::<
                    WorkflowDefinitionDiffRequest,
                    WorkflowDefinitionDiff,
                >(operation, "/application/workflow/diff-definition"),
                "activate_definition" => {
                    available::<WorkflowDefinitionActivateRequest, WorkflowActivation>(
                        operation,
                        "/application/workflow/activate-definition",
                    )
                }
                "retire_definition" => {
                    available::<WorkflowDefinitionRetireRequest, WorkflowRetirement>(
                        operation,
                        "/application/workflow/retire-definition",
                    )
                }
                "execute_fan_out" => {
                    available::<WorkflowFanOutRequest, tracedecay_domain::WorkflowRunProjection>(
                        operation,
                        "/application/workflow/execute-fan-out",
                    )
                }
                "handoff_issue" => available::<TaskHandoffIssueRequest, TaskHandoffGrant>(
                    operation,
                    "/application/workflow/handoff-issue",
                ),
                "handoff_redeem" => available::<TaskHandoffRedeemRequest, TaskHandoffRedeemed>(
                    operation,
                    "/application/workflow/handoff-redeem",
                ),
                _ => unreachable!("static Workflow operation is exhaustive"),
            })
            .collect::<Result<Vec<_>, _>>()?,
    )
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
    let read_only = matches!(
        operation,
        "validate_definition"
            | "get_definition"
            | "list_definitions"
            | "definition_history"
            | "diff_definition"
    );
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
        effect: if read_only {
            EffectClass::Read
        } else {
            EffectClass::Administrative
        },
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
        pagination: None::<PaginationContract>,
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
        terminal_states: TerminalStateContract::new({
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
        })?,
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
    fn workflow_registry_advertises_every_mounted_application_route() {
        let registry = workflow_executable_binding_registry().unwrap();
        assert_eq!(registry.iter().count(), 11);
        let advertised = registry
            .iter()
            .filter_map(|availability| availability.binding())
            .collect::<Vec<_>>();
        assert_eq!(advertised.len(), 11);
        for binding in advertised {
            let tracedecay_tool_catalog::RouteExposureV1::Public { route_path, .. } =
                binding.exposure()
            else {
                panic!("mounted Workflow operation must have a public route");
            };
            assert!(route_path.starts_with("/application/workflow/"));
        }
    }
}
