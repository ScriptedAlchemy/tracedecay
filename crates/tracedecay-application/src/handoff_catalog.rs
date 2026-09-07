use schemars::JsonSchema;
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, CancellationContract, CapabilityManifestInputV1,
    CapabilityManifestV1, CatalogValidationError, DeadlineBehavior, DeadlineContract,
    DeniedDisclosurePolicy, EffectClass, ExecutableBindingAvailabilityV1,
    ExecutableBindingRegistryV1, ExecutableBindingV1, IdempotencyContract, IdentifierError,
    LifecycleClass, PaginationContract, PrivacyClass, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RouteExposureV1, RoutingContractV1,
    SchemaBodyAuthorityV1, SchemaRef, ScopeDimension, ScopeRequirement, StreamingContract,
    TerminalState, TerminalStateContract,
};

use crate::{
    IssueTaskHandoffRequestV1, IssueTaskHandoffResultV1, ListTaskHandoffsRequestV1,
    ListTaskHandoffsResultV1, OpenInvestigationHandoffRequestV1, OpenInvestigationHandoffResultV1,
    OpenTaskHandoffRequestV1, OpenTaskHandoffResultV1,
};

const HANDOFF_SERVICE_ID: &str = "service.handoff";

pub const HANDOFF_APPLICATION_OPERATION_IDS_V1: [(&str, &str, &str); 4] = [
    (
        "issue_task_handoff",
        "capability.handoff.issue_task_handoff",
        "use-case.handoff.issue_task_handoff",
    ),
    (
        "list_task_handoffs",
        "capability.handoff.list_task_handoffs",
        "use-case.handoff.list_task_handoffs",
    ),
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
        available::<IssueTaskHandoffRequestV1, IssueTaskHandoffResultV1>(
            "issue_task_handoff",
            "/application/handoff/issue-task",
            "tracedecay_application::handoff::IssueTaskHandoffRequestV1",
            "tracedecay_application::handoff::IssueTaskHandoffResultV1",
        )?,
        available::<ListTaskHandoffsRequestV1, ListTaskHandoffsResultV1>(
            "list_task_handoffs",
            "/application/handoff/list-task",
            "tracedecay_application::handoff::ListTaskHandoffsRequestV1",
            "tracedecay_application::handoff::ListTaskHandoffsResultV1",
        )?,
        available::<OpenInvestigationHandoffRequestV1, OpenInvestigationHandoffResultV1>(
            "open_investigation_handoff",
            "/application/handoff/open-investigation",
            "tracedecay_application::handoff::OpenInvestigationHandoffRequestV1",
            "tracedecay_application::handoff::OpenInvestigationHandoffResultV1",
        )?,
        available::<OpenTaskHandoffRequestV1, OpenTaskHandoffResultV1>(
            "open_task_handoff",
            "/application/handoff/open-task",
            "tracedecay_application::handoff::OpenTaskHandoffRequestV1",
            "tracedecay_application::handoff::OpenTaskHandoffResultV1",
        )?,
    ])
}

fn available<Request, Output>(
    operation: &str,
    route_path: &str,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableBindingAvailabilityV1, CatalogValidationError>
where
    Request: JsonSchema,
    Output: JsonSchema,
{
    let manifest = handoff_manifest(operation)?;
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

/// Whether this operation reads the grant store or commits against it.
///
/// The three token operations issue or consume a grant; the enumeration only
/// looks. Declaring a pure read through the effect-shaped branch below would
/// catalogue it as a durable administrative effect with a required idempotency
/// key and an effect receipt — a contract the operation cannot honour, since it
/// mints no effect to reconcile and nothing to be idempotent about.
const fn is_read_operation(operation: &str) -> bool {
    matches!(operation.as_bytes(), b"list_task_handoffs")
}

fn handoff_manifest(operation: &str) -> Result<CapabilityManifestV1, CatalogValidationError> {
    let binding_id = identifier(
        format!("binding.http.handoff.{operation}"),
        "handoff binding ID",
    )?;
    let reads = is_read_operation(operation);
    let routing = if reads {
        RoutingContractV1::new(
            1,
            format!("List {operation}"),
            "Enumerate the daemon handoff tokens this caller could redeem, by digest and never by bearer.".to_owned(),
            vec![format!("List {operation}")],
        )?
    } else {
        RoutingContractV1::new(
            1,
            format!("Open {operation}"),
            format!("Consume a single-use daemon handoff for {operation}."),
            vec![format!("Open {operation}")],
        )?
    };
    CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: identifier(
            format!("capability.handoff.{operation}"),
            "handoff capability ID",
        )?,
        use_case_id: identifier(
            format!("use-case.handoff.{operation}"),
            "handoff use-case ID",
        )?,
        routing,
        request_schema: schema_ref(format!("schema.handoff.{operation}.request"))?,
        result_schema: schema_ref(format!("schema.handoff.{operation}.result"))?,
        effect: if reads {
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
        // Consuming the token is a single atomic authority commit. A caller
        // may withdraw before admission, but once admitted there is no safe
        // cancellable interval or rollback to advertise.
        cancellation: CancellationContract::NotCancellable,
        // A read has no effect to receipt, so its deadline returns the
        // operation receipt instead.
        deadline: DeadlineContract::new(
            30_000,
            if reads {
                DeadlineBehavior::ReturnOperationReceipt
            } else {
                DeadlineBehavior::ReturnEffectReceipt
            },
        )?,
        // The enumeration is CAPPED, not paged: it has a ceiling and reports
        // reaching it on the result, but serves no cursor. Declaring a
        // pagination contract would promise a continuation that does not exist.
        pagination: None::<PaginationContract>,
        idempotency: if reads {
            IdempotencyContract::NotRequired
        } else {
            IdempotencyContract::Required
        },
        // A read has nothing to undo, so an inverse is not merely unshipped but
        // meaningless — and the catalog validator refuses a read-only
        // capability that advertises one.
        inverse: if reads {
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
        reconciliation: if reads {
            ReconciliationContract::NotRequired
        } else {
            ReconciliationContract::Required
        },
        receipt: if reads {
            ReceiptContract::Operation
        } else {
            ReceiptContract::DurableEffect
        },
        // `Partial` is retained for the read: hitting the enumeration ceiling
        // is exactly a partial answer. `EffectUnknown` is not — it is the state
        // of an effect whose commit is in doubt, and an operation that writes
        // nothing can never leave a commit in doubt.
        terminal_states: if reads {
            TerminalStateContract::new(vec![
                TerminalState::Completed,
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::Partial,
            ])?
        } else {
            TerminalStateContract::new(vec![
                TerminalState::Completed,
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::Partial,
                TerminalState::EffectUnknown,
            ])?
        },
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
