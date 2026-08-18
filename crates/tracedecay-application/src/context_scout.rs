//! Canonical Context Scout operations for CLI/MCP/HTTP surfaces.
//!
//! The application crate owns operation identity and catalog metadata only.
//! Exact-address authorization and durable mutation remain daemon authorities.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{ConfigurationIdempotencyKey, ConfigurationRevisionId};
use tracedecay_domain::{CodeGenerationId, ManifestDigest, RetrievalAnchorId, UtcMicros};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, CodecBindingKey, ContributionId,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    ExecutableSchemaAuthority, IdempotencyContract, LifecycleClass, OperationId,
    PaginationContract, PrivacyClass, ProfileId, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RouteExposureV1, RoutingContractV1, SchemaId,
    SchemaRef, ScopeDimension, ScopeRequirement, ServiceId, StreamingContract, TerminalState,
    TerminalStateContract, UseCaseId,
};

use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::{IdempotencyKey, ResultContractRef};
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;

const SCOUT_SURFACES: [BindingSurface; 3] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
];

#[derive(Clone, Copy)]
struct ContextScoutOperationSpec {
    operation: &'static str,
    summary: &'static str,
    description: &'static str,
    effect: EffectClass,
    paginated: bool,
}

const CONTEXT_SCOUT_SPECS: [ContextScoutOperationSpec; 11] = [
    read_spec("context_scout_status", "Read Context Scout status"),
    read_spec("context_scout_recent", "Read recent Context Scout state"),
    read_spec("context_scout_explain", "Explain Context Scout state"),
    read_spec("context_scout_capability", "Read Context Scout capability"),
    read_spec("context_scout_budget", "Read Context Scout budget"),
    configuration_control_spec("context_scout_pause", "Pause Context Scout"),
    configuration_control_spec("context_scout_resume", "Resume Context Scout"),
    control_spec("context_scout_cancel", "Cancel Context Scout work"),
    control_spec("context_scout_claim", "Claim a Context Scout delivery"),
    control_spec("context_scout_delivery", "Record a Context Scout delivery"),
    control_spec("context_scout_feedback", "Record Context Scout feedback"),
];

/// Exact opaque destination for one public Context Scout operation.
///
/// The daemon converts this transport-neutral address into its runtime value
/// only after catalog admission. Keeping the public wire here makes CLI, MCP,
/// HTTP, and both SDKs share one generated schema authority.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutAddressV1 {
    pub profile_id: [u8; 16],
    pub provider_id: [u8; 16],
    pub protected_session_id: [u8; 32],
    pub thread_id: [u8; 16],
    pub turn_id: [u8; 16],
    pub agent_id: [u8; 16],
    pub logical_message_id: [u8; 16],
    pub project_id: [u8; 16],
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutClaimWindowV1 {
    IdleWindow,
    OnRequest,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutDeliveryWindowV1 {
    Immediate,
    NextBoundary,
    IdleWindow,
    OnRequest,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutCategoryV1 {
    Retrieval,
    Diagnostic,
    Coordination,
    Verification,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutRouteV1 {
    Deterministic,
    ModelAssisted,
    DeterministicFallback,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutModelBackendV1 {
    Disabled,
    CodexAppServer,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutModelOutcomeV1 {
    NotRequested,
    Succeeded,
    Disabled,
    Unavailable,
    Denied,
    Disconnected,
    Cancelled,
    DeadlineExceeded,
    TokenBudgetExceeded,
    InvalidOutput,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutServiceStateV1 {
    Active,
    Paused,
    Disabled,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutRuntimeModeV1 {
    Deterministic,
    ConfiguredModel,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutSuppressionV1 {
    Disabled,
    Paused,
    DirtyOverlay,
    QuietOrUnreceptive,
    NoEligibleCandidate,
    Expired,
    Duplicate,
    Cancelled,
    ModelOutputInvalid,
    EvidencePartial,
    EvidenceStale,
    EvidenceUnavailable,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutEvidenceAvailabilityV1 {
    Complete,
    Partial,
    Stale,
    Cancelled,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutDeliveryOutcomeV1 {
    Attempted,
    Delayed,
    Displayed,
    Expanded,
    ExplicitlyAccepted,
    ExplicitlyRejected,
    Dismissed,
    ExpiredUnseen,
    Corrected,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutFeedbackKindV1 {
    ExplicitlyAccepted,
    ExplicitlyRejected,
    Dismissed,
    Corrected,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutStoreOutcomeV1 {
    Stored,
    Duplicate,
    Superseded,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutLimitsV1 {
    pub max_candidates: usize,
    pub max_evidence: usize,
    pub max_text_bytes: usize,
    pub max_model_input_tokens: usize,
    pub max_model_output_tokens: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutModelReceiptV1 {
    pub requested_backend: ContextScoutModelBackendV1,
    pub actual_provider: Option<String>,
    pub actual_model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub estimated_cost_microusd: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutWorkV1 {
    pub address: ContextScoutAddressV1,
    pub generation: u64,
    pub input_watermark: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutEvidenceProjectionV1 {
    pub content_generation: CodeGenerationId,
    pub availability: ContextScoutEvidenceAvailabilityV1,
    pub anchor_ids: Vec<RetrievalAnchorId>,
    pub claim_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutSuggestionProjectionV1 {
    pub work: ContextScoutWorkV1,
    pub envelope_id: [u8; 16],
    pub configuration_revision: [u8; 32],
    pub delivery_window: ContextScoutDeliveryWindowV1,
    pub route: ContextScoutRouteV1,
    pub model_outcome: ContextScoutModelOutcomeV1,
    pub model_receipt: Option<ContextScoutModelReceiptV1>,
    pub dedupe_key: [u8; 32],
    pub category: ContextScoutCategoryV1,
    pub relevance_score: u16,
    pub suggestion_text: String,
    pub evidence: ContextScoutEvidenceProjectionV1,
    pub expires_at: UtcMicros,
}

/// Public lease proof. It carries only opaque identities; the daemon resolves
/// it back to the durable queue row before recording delivery.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutClaimHandleV1 {
    pub work: ContextScoutWorkV1,
    pub envelope_id: [u8; 16],
    pub lease_id: [u8; 16],
    pub lease_expires_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum ContextScoutClaimResultV1 {
    Claimed {
        claim: Box<ContextScoutClaimHandleV1>,
        suggestion: Box<ContextScoutSuggestionProjectionV1>,
    },
    Empty,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutDeliveryReceiptV1 {
    pub receipt_id: [u8; 16],
    pub envelope_id: [u8; 16],
    pub delivered_at: UtcMicros,
    pub outcome: ContextScoutDeliveryOutcomeV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutFeedbackV1 {
    pub receipt_id: [u8; 16],
    pub kind: ContextScoutFeedbackKindV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutRecentDeliveryV1 {
    pub suggestion: ContextScoutSuggestionProjectionV1,
    pub receipt: ContextScoutDeliveryReceiptV1,
    pub feedback: Option<ContextScoutFeedbackV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutRecentResultV1 {
    pub configuration_revision: [u8; 32],
    pub observed_at: UtcMicros,
    pub pending: Vec<ContextScoutSuggestionProjectionV1>,
    pub deliveries: Vec<ContextScoutRecentDeliveryV1>,
    pub omitted: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutStatusResultV1 {
    pub configuration_revision: [u8; 32],
    pub state: ContextScoutServiceStateV1,
    pub mode: ContextScoutRuntimeModeV1,
    pub model_path: Option<ContextScoutModelBackendV1>,
    pub limits: ContextScoutLimitsV1,
    pub active_suggestions: usize,
    pub last_route: Option<ContextScoutRouteV1>,
    pub last_suppression: Option<ContextScoutSuppressionV1>,
    pub last_model_outcome: Option<ContextScoutModelOutcomeV1>,
    pub last_model_receipt: Option<ContextScoutModelReceiptV1>,
    pub last_delivery_outcome: Option<ContextScoutDeliveryOutcomeV1>,
    pub last_feedback: Option<ContextScoutFeedbackKindV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutExplanationResultV1 {
    pub status: ContextScoutStatusResultV1,
    pub recent: ContextScoutRecentResultV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutCapabilityResultV1 {
    pub state: ContextScoutServiceStateV1,
    pub mode: ContextScoutRuntimeModeV1,
    pub deterministic_available: bool,
    pub configured_model: Option<ContextScoutModelBackendV1>,
    pub configured_model_available: bool,
    pub last_model_outcome: Option<ContextScoutModelOutcomeV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutBudgetResultV1 {
    pub limits: ContextScoutLimitsV1,
    pub last_model_outcome: Option<ContextScoutModelOutcomeV1>,
    pub exhausted: bool,
    pub last_input_tokens: Option<u64>,
    pub last_output_tokens: Option<u64>,
    pub last_estimated_cost_microusd: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutMutationResultV1 {
    pub outcome: ContextScoutStoreOutcomeV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutExactAddressRequestV1 {
    pub address: ContextScoutAddressV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutRecentRequestV1 {
    pub address: ContextScoutAddressV1,
    #[serde(default = "default_context_scout_recent_limit")]
    pub limit: usize,
}

const fn default_context_scout_recent_limit() -> usize {
    8
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutControlRequestV1 {
    pub address: ContextScoutAddressV1,
    pub expected_revision: ConfigurationRevisionId,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutCancelRequestV1 {
    pub address: ContextScoutAddressV1,
    pub work: ContextScoutWorkV1,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutClaimRequestV1 {
    pub address: ContextScoutAddressV1,
    pub window: ContextScoutClaimWindowV1,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutDeliveryRequestV1 {
    pub address: ContextScoutAddressV1,
    pub claim: ContextScoutClaimHandleV1,
    pub receipt: ContextScoutDeliveryReceiptV1,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutFeedbackRequestV1 {
    pub address: ContextScoutAddressV1,
    pub receipt: ContextScoutDeliveryReceiptV1,
    pub feedback: ContextScoutFeedbackV1,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "operation", content = "request")]
pub enum ContextScoutSurfaceRequestV1 {
    Status(ContextScoutExactAddressRequestV1),
    Recent(ContextScoutRecentRequestV1),
    Explain(ContextScoutRecentRequestV1),
    Capability(ContextScoutExactAddressRequestV1),
    Budget(ContextScoutExactAddressRequestV1),
    Pause(ContextScoutControlRequestV1),
    Resume(ContextScoutControlRequestV1),
    Cancel(ContextScoutCancelRequestV1),
    Claim(ContextScoutClaimRequestV1),
    Delivery(ContextScoutDeliveryRequestV1),
    Feedback(ContextScoutFeedbackRequestV1),
}

impl ContextScoutSurfaceRequestV1 {
    pub const fn address(&self) -> ContextScoutAddressV1 {
        match self {
            Self::Status(request) | Self::Capability(request) | Self::Budget(request) => {
                request.address
            }
            Self::Recent(request) | Self::Explain(request) => request.address,
            Self::Pause(request) | Self::Resume(request) => request.address,
            Self::Cancel(request) => request.address,
            Self::Claim(request) => request.address,
            Self::Delivery(request) => request.address,
            Self::Feedback(request) => request.address,
        }
    }

    pub fn matches_operation(&self, operation: &str) -> bool {
        matches!(
            (self, operation),
            (Self::Status(_), "context_scout_status")
                | (Self::Recent(_), "context_scout_recent")
                | (Self::Explain(_), "context_scout_explain")
                | (Self::Capability(_), "context_scout_capability")
                | (Self::Budget(_), "context_scout_budget")
                | (Self::Pause(_), "context_scout_pause")
                | (Self::Resume(_), "context_scout_resume")
                | (Self::Cancel(_), "context_scout_cancel")
                | (Self::Claim(_), "context_scout_claim")
                | (Self::Delivery(_), "context_scout_delivery")
                | (Self::Feedback(_), "context_scout_feedback")
        )
    }
}

const fn read_spec(operation: &'static str, summary: &'static str) -> ContextScoutOperationSpec {
    ContextScoutOperationSpec {
        operation,
        summary,
        description: "Execute the exact-address Context Scout read through the daemon-owned application authority.",
        effect: EffectClass::Read,
        paginated: false,
    }
}

const fn control_spec(operation: &'static str, summary: &'static str) -> ContextScoutOperationSpec {
    ContextScoutOperationSpec {
        operation,
        summary,
        description: "Execute the exact-address Context Scout control through the daemon-owned application authority.",
        effect: EffectClass::Administrative,
        paginated: false,
    }
}

const fn configuration_control_spec(
    operation: &'static str,
    summary: &'static str,
) -> ContextScoutOperationSpec {
    ContextScoutOperationSpec {
        operation,
        summary,
        description: "Persist the exact-address Context Scout state through the canonical configuration authority.",
        effect: EffectClass::ConfigurationWrite,
        paginated: false,
    }
}

pub fn context_scout_surface_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(CONTEXT_SCOUT_SPECS.len());
    let mut bindings = Vec::with_capacity(CONTEXT_SCOUT_SPECS.len() * SCOUT_SURFACES.len());
    for spec in &CONTEXT_SCOUT_SPECS {
        let is_effect = spec.effect.is_effect();
        let capability_id = capability_id(spec)?;
        let (spec_bindings, binding_ids) =
            current_bindings(&capability_id, spec.operation, SCOUT_SURFACES)?;
        bindings.extend(spec_bindings);
        capabilities.push(CapabilityManifestV1::new(CapabilityManifestInputV1 {
            capability_id,
            use_case_id: use_case_id(spec)?,
            routing: RoutingContractV1::new(
                1,
                spec.summary,
                spec.description,
                vec![format!("{} for this exact address", spec.summary)],
            )?,
            request_schema: request_schema(spec)?,
            result_schema: result_schema(spec)?,
            effect: spec.effect,
            scope: ScopeRequirement::new(vec![
                ScopeDimension::Project,
                ScopeDimension::Worktree,
                ScopeDimension::Session,
                ScopeDimension::Resource,
            ])?,
            authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
            denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
            privacy: PrivacyClass::ScopedMetadata,
            lifecycle: LifecycleClass::Resumable,
            streaming: StreamingContract::Unsupported,
            cancellation: if is_effect {
                CancellationContract::NotCancellable
            } else {
                CancellationContract::cooperative(vec![
                    CancellationPoint::BeforeAdmission,
                    CancellationPoint::BeforeRead,
                    CancellationPoint::DuringRead,
                ])?
            },
            deadline: DeadlineContract::new(
                15_000,
                if is_effect {
                    DeadlineBehavior::ReturnEffectReceipt
                } else {
                    DeadlineBehavior::ReturnOperationReceipt
                },
            )?,
            pagination: spec
                .paginated
                .then(|| PaginationContract::new(8, 32, 60_000))
                .transpose()?,
            idempotency: if is_effect {
                IdempotencyContract::Required
            } else {
                IdempotencyContract::NotRequired
            },
            inverse: if is_effect {
                tracedecay_tool_catalog::InverseContract::Unavailable {
                    reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
                }
            } else {
                tracedecay_tool_catalog::InverseContract::NotApplicable
            },
            authority_revalidation: RevalidationContract::required(vec![
                RevalidationPoint::Authority,
                RevalidationPoint::Scope,
                RevalidationPoint::Policy,
                RevalidationPoint::Configuration,
                RevalidationPoint::ExpectedState,
            ])?,
            reconciliation: if is_effect {
                ReconciliationContract::Required
            } else {
                ReconciliationContract::NotRequired
            },
            receipt: if is_effect {
                ReceiptContract::DurableEffect
            } else {
                ReceiptContract::Operation
            },
            terminal_states: TerminalStateContract::new(if is_effect {
                // Effect-class Scout operations are NotCancellable, and the
                // manifest contract requires the cancelled terminal to match
                // the cancellation contract exactly.
                vec![
                    TerminalState::Completed,
                    TerminalState::TimedOut,
                    TerminalState::Failed,
                    TerminalState::EffectUnknown,
                    TerminalState::Partial,
                ]
            } else {
                vec![
                    TerminalState::Completed,
                    TerminalState::Cancelled,
                    TerminalState::TimedOut,
                    TerminalState::Failed,
                    TerminalState::Partial,
                ]
            })?,
            availability: AvailabilityContract::Available,
            binding_ids,
            profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
            required_features: Vec::new(),
        })?);
    }
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.context-scout-surface")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?;
    let schemas = context_scout_executable_schemas(&contribution)?;
    Ok(contribution.with_executable_schemas(schemas)?)
}

/// Daemon-owned public HTTP bindings for every shipped Scout operation.
pub fn context_scout_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, ApplicationContractError> {
    let contribution = context_scout_surface_catalog_contribution()?;
    let service_id = ServiceId::new("service.application.context-scout")?;
    let mut bindings = Vec::with_capacity(CONTEXT_SCOUT_SPECS.len());
    for spec in &CONTEXT_SCOUT_SPECS {
        let capability_id = capability_id(spec)?;
        let manifest = contribution
            .capabilities()
            .iter()
            .find(|manifest| manifest.capability_id() == &capability_id)
            .ok_or(ApplicationContractError::Inconsistent {
                field: "Context Scout executable capability",
            })?;
        let schema = contribution.executable_schema(&capability_id).ok_or(
            ApplicationContractError::Inconsistent {
                field: "Context Scout executable schema",
            },
        )?;
        let http_binding = contribution
            .bindings()
            .iter()
            .find(|binding| {
                binding.capability_id() == &capability_id
                    && binding.surface() == BindingSurface::Http
            })
            .ok_or(ApplicationContractError::Inconsistent {
                field: "Context Scout HTTP binding",
            })?;
        bindings.push(ExecutableBindingAvailabilityV1::available(
            ExecutableBindingV1::daemon_owned(
                manifest,
                OperationId::new(format!("operation.application.{}", spec.operation))?,
                service_id.clone(),
                schema.request_schema().clone(),
                schema.result_schema().clone(),
                CodecBindingKey::new(format!(
                    "codec.application.context-scout.{}.json.v1",
                    spec.operation
                ))?,
                RouteExposureV1::Public {
                    binding_id: http_binding.binding_id().clone(),
                    route_path: format!("/application/context-scout/{}", spec.operation),
                },
            )?,
        ));
    }
    Ok(ExecutableBindingRegistryV1::new(bindings)?)
}

fn context_scout_executable_schemas(
    contribution: &CatalogContributionV1,
) -> Result<Vec<ExecutableSchemaAuthority>, ApplicationContractError> {
    let mut schemas = Vec::with_capacity(CONTEXT_SCOUT_SPECS.len());
    macro_rules! add {
        ($operation:literal, $request:ty, crate::configuration::$result:ident) => {
            schemas.push(context_scout_executable_schema::<
                $request,
                crate::configuration::$result,
            >(
                contribution,
                $operation,
                concat!(
                    "tracedecay_application::context_scout::",
                    stringify!($request)
                ),
                concat!(
                    "tracedecay_application::configuration::",
                    stringify!($result)
                ),
            )?)
        };
        ($operation:literal, $request:ty, $result:ty) => {
            schemas.push(context_scout_executable_schema::<$request, $result>(
                contribution,
                $operation,
                concat!(
                    "tracedecay_application::context_scout::",
                    stringify!($request)
                ),
                concat!(
                    "tracedecay_application::context_scout::",
                    stringify!($result)
                ),
            )?)
        };
    }
    add!(
        "context_scout_status",
        ContextScoutExactAddressRequestV1,
        ContextScoutStatusResultV1
    );
    add!(
        "context_scout_recent",
        ContextScoutRecentRequestV1,
        ContextScoutRecentResultV1
    );
    add!(
        "context_scout_explain",
        ContextScoutRecentRequestV1,
        ContextScoutExplanationResultV1
    );
    add!(
        "context_scout_capability",
        ContextScoutExactAddressRequestV1,
        ContextScoutCapabilityResultV1
    );
    add!(
        "context_scout_budget",
        ContextScoutExactAddressRequestV1,
        ContextScoutBudgetResultV1
    );
    add!(
        "context_scout_pause",
        ContextScoutControlRequestV1,
        crate::configuration::ConfigurationMutationReceipt
    );
    add!(
        "context_scout_resume",
        ContextScoutControlRequestV1,
        crate::configuration::ConfigurationMutationReceipt
    );
    add!(
        "context_scout_cancel",
        ContextScoutCancelRequestV1,
        ContextScoutMutationResultV1
    );
    add!(
        "context_scout_claim",
        ContextScoutClaimRequestV1,
        ContextScoutClaimResultV1
    );
    add!(
        "context_scout_delivery",
        ContextScoutDeliveryRequestV1,
        ContextScoutMutationResultV1
    );
    add!(
        "context_scout_feedback",
        ContextScoutFeedbackRequestV1,
        ContextScoutMutationResultV1
    );
    Ok(schemas)
}

fn context_scout_executable_schema<Request, Response>(
    contribution: &CatalogContributionV1,
    operation: &str,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableSchemaAuthority, ApplicationContractError>
where
    Request: JsonSchema,
    Response: JsonSchema,
{
    let spec = CONTEXT_SCOUT_SPECS
        .iter()
        .find(|spec| spec.operation == operation)
        .ok_or(ApplicationContractError::Inconsistent {
            field: "Context Scout schema operation",
        })?;
    let capability_id = capability_id(spec)?;
    let manifest = contribution
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == &capability_id)
        .ok_or(ApplicationContractError::Inconsistent {
            field: "Context Scout schema capability",
        })?;
    Ok(ExecutableSchemaAuthority::for_types_at_paths::<
        Request,
        Response,
    >(
        manifest, request_rust_type_path, result_rust_type_path
    )?)
}

pub fn context_scout_surface_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    CONTEXT_SCOUT_SPECS
        .iter()
        .map(|spec| {
            ApplicationHandlerDescriptor::new(
                context_scout_surface_operation(spec.operation)?.ok_or(
                    ApplicationContractError::Inconsistent {
                        field: "Context Scout operation spec",
                    },
                )?,
                request_schema(spec)?,
                result_schema(spec)?,
            )
        })
        .collect()
}

pub fn context_scout_surface_operation(
    name: &str,
) -> Result<Option<ApplicationOperation>, ApplicationContractError> {
    CONTEXT_SCOUT_SPECS
        .iter()
        .find(|spec| spec.operation == name)
        .map(|spec| {
            Ok(ApplicationOperation::new(
                capability_id(spec)?,
                use_case_id(spec)?,
                ResultContractRef::from_schema(&result_schema(spec)?),
                true,
            ))
        })
        .transpose()
}

fn capability_id(
    spec: &ContextScoutOperationSpec,
) -> Result<CapabilityId, ApplicationContractError> {
    Ok(CapabilityId::new(format!(
        "capability.application.{}",
        spec.operation.replace('_', "-")
    ))?)
}

fn use_case_id(spec: &ContextScoutOperationSpec) -> Result<UseCaseId, ApplicationContractError> {
    Ok(UseCaseId::new(format!(
        "use-case.application.{}",
        spec.operation.replace('_', "-")
    ))?)
}

fn request_schema(spec: &ContextScoutOperationSpec) -> Result<SchemaRef, ApplicationContractError> {
    schema(spec, "request")
}

fn result_schema(spec: &ContextScoutOperationSpec) -> Result<SchemaRef, ApplicationContractError> {
    schema(spec, "result")
}

fn schema(
    spec: &ContextScoutOperationSpec,
    suffix: &str,
) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!(
            "schema.application.{}.{}",
            spec.operation.replace('_', "-"),
            suffix
        ))?,
        1,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_exposes_every_scout_operation_on_cli_mcp_and_http_only() {
        let contribution = context_scout_surface_catalog_contribution().unwrap();
        assert_eq!(contribution.capabilities().len(), CONTEXT_SCOUT_SPECS.len());
        let routing_examples = contribution
            .capabilities()
            .iter()
            .flat_map(|capability| capability.routing().examples())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(routing_examples.len(), CONTEXT_SCOUT_SPECS.len());
        for spec in CONTEXT_SCOUT_SPECS {
            let capability = contribution
                .capabilities()
                .iter()
                .find(|capability| capability.capability_id() == &capability_id(&spec).unwrap())
                .expect("every Scout operation has one capability");
            assert_eq!(capability.effect(), spec.effect);
            if spec.effect.is_effect() {
                assert_eq!(capability.receipt(), ReceiptContract::DurableEffect);
                assert_eq!(capability.idempotency(), IdempotencyContract::Required);
                assert_eq!(
                    capability.reconciliation(),
                    ReconciliationContract::Required
                );
                assert_eq!(
                    capability.deadline().behavior(),
                    DeadlineBehavior::ReturnEffectReceipt
                );
                assert_eq!(
                    capability.cancellation(),
                    &CancellationContract::NotCancellable
                );
                assert_eq!(capability.deadline().maximum_millis(), 15_000);
                assert!(
                    capability
                        .terminal_states()
                        .states()
                        .contains(&TerminalState::EffectUnknown)
                );
            } else {
                assert_eq!(capability.receipt(), ReceiptContract::Operation);
                assert_eq!(capability.idempotency(), IdempotencyContract::NotRequired);
                assert_eq!(
                    capability.reconciliation(),
                    ReconciliationContract::NotRequired
                );
                assert_eq!(
                    capability.deadline().behavior(),
                    DeadlineBehavior::ReturnOperationReceipt
                );
                assert!(
                    !capability
                        .terminal_states()
                        .states()
                        .contains(&TerminalState::EffectUnknown)
                );
            }
            let surfaces = contribution
                .bindings()
                .iter()
                .filter(|binding| binding.operation().as_str() == spec.operation)
                .map(|binding| binding.surface())
                .collect::<Vec<_>>();
            assert_eq!(surfaces.len(), SCOUT_SURFACES.len());
            for expected in SCOUT_SURFACES {
                assert!(surfaces.contains(&expected));
            }
        }
    }

    #[test]
    fn application_catalog_and_handlers_reach_every_scout_operation() {
        let contributions = crate::application_catalog_contributions().unwrap();
        let handlers = crate::application_handler_descriptors().unwrap();
        handlers.validate_against(&contributions).unwrap();

        for spec in CONTEXT_SCOUT_SPECS {
            let operation = context_scout_surface_operation(spec.operation)
                .unwrap()
                .expect("Scout operation is application-reachable");
            let handler = handlers
                .get(operation.use_case_id())
                .expect("Scout operation has one canonical handler");
            assert_eq!(handler.operation(), &operation);
        }
    }

    /// Regression: the Scout family used to publish callable CLI/MCP/HTTP
    /// bindings while withholding every executable schema body. The SDK then
    /// truthfully marked all eleven operations `schema_unavailable`, leaving
    /// an advertised product family with no typed public client journey.
    #[test]
    fn every_context_scout_operation_owns_an_executable_schema() {
        let contribution = context_scout_surface_catalog_contribution().unwrap();
        for capability in contribution.capabilities() {
            assert!(
                contribution
                    .executable_schema(capability.capability_id())
                    .is_some(),
                "{} must own its canonical request/result schema bodies",
                capability.capability_id().as_str()
            );
        }
    }

    #[test]
    fn pause_and_resume_publish_configuration_effect_settlement_metadata() {
        let contribution = context_scout_surface_catalog_contribution().unwrap();
        for operation in ["context_scout_pause", "context_scout_resume"] {
            let spec = CONTEXT_SCOUT_SPECS
                .iter()
                .find(|spec| spec.operation == operation)
                .unwrap();
            let capability = contribution
                .capabilities()
                .iter()
                .find(|capability| capability.capability_id() == &capability_id(spec).unwrap())
                .unwrap();
            assert_eq!(capability.effect(), EffectClass::ConfigurationWrite);
            assert_eq!(
                capability.cancellation(),
                &CancellationContract::NotCancellable
            );
            assert_eq!(capability.deadline().maximum_millis(), 15_000);
            assert_eq!(capability.receipt(), ReceiptContract::DurableEffect);
            assert_eq!(capability.idempotency(), IdempotencyContract::Required);
            assert_eq!(
                capability.reconciliation(),
                ReconciliationContract::Required
            );
        }
    }
}
