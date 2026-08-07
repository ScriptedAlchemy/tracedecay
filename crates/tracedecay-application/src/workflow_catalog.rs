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
    WorkflowDefinitionDiff, WorkflowDefinitionDiffRequest, WorkflowDefinitionGetRequest,
    WorkflowDefinitionHistoryRequest, WorkflowDefinitionListRequest,
    WorkflowDefinitionRegisterRequest, WorkflowDefinitionValidateRequest,
    WorkflowDefinitionValidation,
};

const WORKFLOW_SERVICE_ID: &str = "service.workflow";

pub const WORKFLOW_APPLICATION_OPERATION_IDS: [(&str, &str, &str); 8] = [
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
            .map(|(operation, _, _)| workflow_binding(operation))
            .collect::<Result<Vec<_>, _>>()?,
    )
}

fn workflow_binding(
    operation: &str,
) -> Result<ExecutableBindingAvailabilityV1, CatalogValidationError> {
    match operation {
        "register_definition" => available::<
            WorkflowDefinitionRegisterRequest,
            tracedecay_domain::WorkflowDefinition,
        >(operation, "/application/workflow/register-definition"),
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
        "diff_definition" => available::<WorkflowDefinitionDiffRequest, WorkflowDefinitionDiff>(
            operation,
            "/application/workflow/diff-definition",
        ),
        "handoff_issue" => available::<TaskHandoffIssueRequest, TaskHandoffGrant>(
            operation,
            "/application/workflow/handoff-issue",
        ),
        "handoff_redeem" => available::<TaskHandoffRedeemRequest, TaskHandoffRedeemed>(
            operation,
            "/application/workflow/handoff-redeem",
        ),
        _ => Err(invalid_catalog_value(
            "workflow operation",
            "operation has no executable binding",
        )),
    }
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
            .map_err(|_| invalid_catalog_value("workflow operation ID", "ID is invalid"))?,
        ServiceId::new(WORKFLOW_SERVICE_ID)
            .map_err(|_| invalid_catalog_value("workflow service ID", "ID is invalid"))?,
        request_schema,
        result_schema,
        CodecBindingKey::new(format!("codec.workflow.{operation}.json.v1"))
            .map_err(|_| invalid_catalog_value("workflow codec ID", "ID is invalid"))?,
        RouteExposureV1::Public {
            binding_id: BindingId::new(format!("binding.http.workflow.{operation}"))
                .map_err(|_| invalid_catalog_value("workflow binding ID", "ID is invalid"))?,
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
        .map_err(|_| invalid_catalog_value("workflow binding ID", "ID is invalid"))?;
    CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: CapabilityId::new(format!("capability.workflow.{operation}"))
            .map_err(|_| invalid_catalog_value("workflow capability ID", "ID is invalid"))?,
        use_case_id: UseCaseId::new(format!("use-case.workflow.{operation}"))
            .map_err(|_| invalid_catalog_value("workflow use-case ID", "ID is invalid"))?,
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
        lifecycle: if read_only {
            LifecycleClass::Stateless
        } else {
            LifecycleClass::Resumable
        },
        streaming: StreamingContract::Unsupported,
        cancellation: if read_only {
            CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeRead,
                CancellationPoint::DuringRead,
            ])?
        } else {
            CancellationContract::NotCancellable
        },
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
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::Partial,
            ];
            if read_only {
                states.push(TerminalState::Cancelled);
            } else {
                states.push(TerminalState::EffectUnknown);
            }
            states
        })?,
        availability: AvailabilityContract::Available,
        binding_ids: vec![binding_id],
        profile_eligibility: vec![
            ProfileId::new("profile.default")
                .map_err(|_| invalid_catalog_value("workflow profile ID", "ID is invalid"))?,
        ],
        required_features: Vec::new(),
    })
}

fn schema_ref(id: String) -> Result<SchemaRef, CatalogValidationError> {
    SchemaRef::new(
        SchemaId::new(id)
            .map_err(|_| invalid_catalog_value("workflow schema ID", "ID is invalid"))?,
        1,
    )
}

const fn invalid_catalog_value(
    field: &'static str,
    reason: &'static str,
) -> CatalogValidationError {
    CatalogValidationError::InvalidValue { field, reason }
}

#[cfg(test)]
mod tests {
    use tracedecay_tool_catalog::{
        CancellationContract, CancellationPoint, DeadlineBehavior, EffectClass,
        IdempotencyContract, LifecycleClass, ReceiptContract, ReconciliationContract,
        TerminalState,
    };

    use super::{workflow_executable_binding_registry, workflow_manifest};

    #[test]
    fn workflow_registry_advertises_every_mounted_application_route() {
        let registry = workflow_executable_binding_registry().unwrap();
        assert_eq!(registry.iter().count(), 8);
        let advertised = registry
            .iter()
            .filter_map(|availability| availability.binding())
            .collect::<Vec<_>>();
        assert_eq!(advertised.len(), 8);
        for binding in advertised {
            let tracedecay_tool_catalog::RouteExposureV1::Public { route_path, .. } =
                binding.exposure()
            else {
                panic!("mounted Workflow operation must have a public route");
            };
            assert!(route_path.starts_with("/application/workflow/"));
        }
    }

    #[test]
    fn workflow_mutations_declare_durable_effect_semantics() {
        for operation in ["register_definition", "handoff_issue", "handoff_redeem"] {
            let manifest = workflow_manifest(operation).unwrap();
            assert_eq!(manifest.effect(), EffectClass::Administrative);
            assert_eq!(manifest.lifecycle(), LifecycleClass::Resumable);
            assert_eq!(manifest.idempotency(), IdempotencyContract::Required);
            assert_eq!(manifest.reconciliation(), ReconciliationContract::Required);
            assert_eq!(manifest.receipt(), ReceiptContract::DurableEffect);
            assert_eq!(
                manifest.deadline().behavior(),
                DeadlineBehavior::ReturnEffectReceipt
            );
            assert!(
                manifest
                    .terminal_states()
                    .contains(TerminalState::EffectUnknown)
            );
            assert!(
                !manifest
                    .terminal_states()
                    .contains(TerminalState::Cancelled)
            );
            assert!(matches!(
                manifest.cancellation(),
                CancellationContract::NotCancellable
            ));
        }
    }

    #[test]
    fn workflow_queries_declare_read_semantics() {
        for operation in [
            "validate_definition",
            "get_definition",
            "list_definitions",
            "definition_history",
            "diff_definition",
        ] {
            let manifest = workflow_manifest(operation).unwrap();
            assert_eq!(manifest.effect(), EffectClass::Read);
            assert_eq!(manifest.lifecycle(), LifecycleClass::Stateless);
            assert_eq!(manifest.idempotency(), IdempotencyContract::NotRequired);
            assert_eq!(
                manifest.reconciliation(),
                ReconciliationContract::NotRequired
            );
            assert_eq!(manifest.receipt(), ReceiptContract::Operation);
            assert_eq!(
                manifest.deadline().behavior(),
                DeadlineBehavior::ReturnOperationReceipt
            );
            assert!(
                !manifest
                    .terminal_states()
                    .contains(TerminalState::EffectUnknown)
            );
            for point in [
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeRead,
                CancellationPoint::DuringRead,
            ] {
                assert!(manifest.cancellation().observes(point));
            }
        }
    }
}
