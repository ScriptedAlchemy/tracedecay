use schemars::JsonSchema;
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, CancellationContract, CancellationPoint,
    CapabilityManifestInputV1, CapabilityManifestV1, CatalogValidationError, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, ExecutableBindingAvailabilityV1,
    ExecutableBindingRegistryV1, ExecutableBindingV1, IdempotencyContract, IdentifierError,
    LifecycleClass, PaginationContract, PrivacyClass, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RouteExposureV1, RoutingContractV1,
    SchemaBodyAuthorityV1, SchemaRef, ScopeDimension, ScopeRequirement, StreamingContract,
    TerminalState, TerminalStateContract,
};

use crate::{
    OpenInvestigationHandoffRequestV1, OpenInvestigationHandoffResultV1, OpenTaskHandoffRequestV1,
    OpenTaskHandoffResultV1,
};

const HANDOFF_SERVICE_ID: &str = "service.handoff";

pub const HANDOFF_APPLICATION_OPERATION_IDS_V1: [(&str, &str, &str); 2] = [
    (
        "open_investigation_handoff",
        "capability.handoff.open_investigation_handoff",
        "use-case.handoff.open_investigation_handoff",
    ),
    (
        "open_task_handoff",
        "capability.handoff.open_task_handoff",
        "use-case.handoff.open_task_handoff",
    ),
];

pub fn handoff_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, CatalogValidationError> {
    ExecutableBindingRegistryV1::new(vec![
        available::<OpenInvestigationHandoffRequestV1, OpenInvestigationHandoffResultV1>(
            "open_investigation_handoff",
            "/application/handoff/open-investigation",
        )?,
        available::<OpenTaskHandoffRequestV1, OpenTaskHandoffResultV1>(
            "open_task_handoff",
            "/application/handoff/open-task",
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
    let manifest = handoff_manifest(operation)?;
    let request_schema =
        SchemaBodyAuthorityV1::for_type::<Request>(manifest.request_schema().clone())?;
    let result_schema =
        SchemaBodyAuthorityV1::for_type::<Output>(manifest.result_schema().clone())?;
    let binding = ExecutableBindingV1::direct(
        &manifest,
        identifier(
            format!("operation.handoff.{operation}"),
            "handoff operation ID",
        )?,
        identifier(HANDOFF_SERVICE_ID.to_owned(), "handoff service ID")?,
        request_schema,
        result_schema,
        identifier(
            format!("codec.handoff.{operation}.json.v1"),
            "handoff codec ID",
        )?,
        RouteExposureV1::Public {
            binding_id: identifier(
                format!("binding.http.handoff.{operation}"),
                "handoff binding ID",
            )?,
            route_path: route_path.to_owned(),
        },
    )?;
    Ok(ExecutableBindingAvailabilityV1::available(binding))
}

fn handoff_manifest(operation: &str) -> Result<CapabilityManifestV1, CatalogValidationError> {
    let binding_id = identifier(
        format!("binding.http.handoff.{operation}"),
        "handoff binding ID",
    )?;
    CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: identifier(
            format!("capability.handoff.{operation}"),
            "handoff capability ID",
        )?,
        use_case_id: identifier(
            format!("use-case.handoff.{operation}"),
            "handoff use-case ID",
        )?,
        routing: RoutingContractV1::new(
            1,
            format!("Open {operation}"),
            format!("Consume a single-use daemon handoff for {operation}."),
            vec![format!("Open {operation}")],
        )?,
        request_schema: schema_ref(format!("schema.handoff.{operation}.request"))?,
        result_schema: schema_ref(format!("schema.handoff.{operation}.result"))?,
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
        profile_eligibility: vec![identifier(
            "profile.default".to_owned(),
            "handoff profile ID",
        )?],
        required_features: Vec::new(),
    })
}

fn schema_ref(id: String) -> Result<SchemaRef, CatalogValidationError> {
    SchemaRef::new(identifier(id, "handoff schema ID")?, 1)
}

fn identifier<T>(value: String, field: &'static str) -> Result<T, CatalogValidationError>
where
    T: TryFrom<String, Error = IdentifierError>,
{
    T::try_from(value).map_err(|_| CatalogValidationError::InvalidValue {
        field,
        reason: "must be a canonical catalog identifier",
    })
}
