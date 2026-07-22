use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, CancellationContract, CancellationPoint,
    CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1, CatalogContributionInputV1,
    CatalogContributionV1, ContributionId, DeadlineBehavior, DeadlineContract,
    DeniedDisclosurePolicy, EffectClass, IdempotencyContract, LifecycleClass, PaginationContract,
    PrivacyClass, ProfileId, ReceiptContract, ReconciliationContract, RevalidationContract,
    RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement,
    StreamingContract, TerminalState, TerminalStateContract, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;

use super::callable_code::{
    CALLABLE_CODE_OPERATION_COUNT, CallableCodeOperationKind, CallableCodeOperations,
};
use super::catalog::APPLICATION_DEFAULT_PROFILE_ID;

const REQUEST_MAXIMUM_BYTES: u32 = 65_536;
const RESULT_MAXIMUM_BYTES: u32 = 1_048_576;

pub fn callable_code_request_schema(
    kind: CallableCodeOperationKind,
) -> Result<SchemaRef, ApplicationContractError> {
    code_query_schema(kind, "request", REQUEST_MAXIMUM_BYTES)
}

pub fn callable_code_result_schema(
    kind: CallableCodeOperationKind,
) -> Result<SchemaRef, ApplicationContractError> {
    code_query_schema(kind, "result", RESULT_MAXIMUM_BYTES)
}

fn code_query_schema(
    kind: CallableCodeOperationKind,
    suffix: &str,
    maximum_bytes: u32,
) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!(
            "schema.application.code-query.{}.{}",
            kind.as_str().replace('_', "-"),
            suffix
        ))?,
        1,
        maximum_bytes,
    )?)
}

pub fn callable_code_operation(
    kind: CallableCodeOperationKind,
) -> Result<ApplicationOperation, ApplicationContractError> {
    let operation = kind.as_str().replace('_', "-");
    let result_schema = callable_code_result_schema(kind)?;
    Ok(ApplicationOperation::new(
        CapabilityId::new(format!("capability.application.code-query.{operation}"))?,
        UseCaseId::new(format!("use-case.application.code-query.{operation}"))?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

pub fn callable_code_operations() -> Result<CallableCodeOperations, ApplicationContractError> {
    CallableCodeOperations::new(
        CallableCodeOperationKind::ALL
            .into_iter()
            .map(|kind| callable_code_operation(kind).map(|operation| (kind, operation)))
            .collect::<Result<Vec<_>, _>>()?,
    )
}

pub fn callable_code_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    CallableCodeOperationKind::ALL
        .into_iter()
        .map(|kind| {
            ApplicationHandlerDescriptor::new(
                callable_code_operation(kind)?,
                callable_code_request_schema(kind)?,
                callable_code_result_schema(kind)?,
            )
        })
        .collect()
}

/// Inert application contribution for the generation-bound PR9 callable
/// query family. Bindings remain empty until the root catalog owner wires the
/// typed application operations to CLI, MCP, HTTP, or LSP surfaces.
pub fn callable_code_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let capabilities = CallableCodeOperationKind::ALL
        .into_iter()
        .map(code_query_capability)
        .collect::<Result<Vec<_>, _>>()?;
    debug_assert_eq!(capabilities.len(), CALLABLE_CODE_OPERATION_COUNT);
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.callable-code-query")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })?)
}

fn code_query_capability(
    kind: CallableCodeOperationKind,
) -> Result<CapabilityManifestV1, ApplicationContractError> {
    let operation = kind.as_str();
    let readable_name = operation.replace('_', " ");
    Ok(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: CapabilityId::new(format!(
            "capability.application.code-query.{}",
            operation.replace('_', "-")
        ))?,
        use_case_id: UseCaseId::new(format!(
            "use-case.application.code-query.{}",
            operation.replace('_', "-")
        ))?,
        routing: RoutingContractV1::new(
            1,
            format!("Read {readable_name}"),
            format!(
                "Invoke the generation-bound PR9 {readable_name} query without replacing its owning kernel."
            ),
            vec![format!("Read {readable_name}")],
        )?,
        request_schema: callable_code_request_schema(kind)?,
        result_schema: callable_code_result_schema(kind)?,
        effect: EffectClass::Read,
        scope: code_query_scope()?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Resumable,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead,
            CancellationPoint::DuringRead,
        ])?,
        deadline: DeadlineContract::new(10_000, DeadlineBehavior::ReturnOperationReceipt)?,
        pagination: Some(PaginationContract::new(25, 1_000, 60_000)?),
        idempotency: IdempotencyContract::NotRequired,
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::Configuration,
        ])?,
        reconciliation: ReconciliationContract::NotRequired,
        receipt: ReceiptContract::Operation,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Partial,
        ])?,
        availability: AvailabilityContract::Available,
        binding_ids: Vec::new(),
        profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
        required_features: Vec::new(),
    })?)
}

fn code_query_scope() -> Result<ScopeRequirement, ApplicationContractError> {
    Ok(ScopeRequirement::new(vec![
        ScopeDimension::Project,
        ScopeDimension::Repository,
        ScopeDimension::Worktree,
        ScopeDimension::Resource,
    ])?)
}
