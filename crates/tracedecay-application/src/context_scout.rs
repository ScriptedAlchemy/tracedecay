//! Canonical Context Scout operations for CLI/MCP/HTTP surfaces.
//!
//! Exact-address authorization and durable mutation remain daemon authorities;
//! this module owns the shared request and result values that cross those
//! application boundaries.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::UtcMicros;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract, LifecycleClass,
    PaginationContract, PrivacyClass, ProfileId, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef,
    ScopeDimension, ScopeRequirement, StreamingContract, TerminalState, TerminalStateContract,
    UseCaseId,
};

use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;

pub const MAX_SCOUT_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_SCOUT_CANDIDATES: usize = 32;
pub const MAX_SCOUT_EVIDENCE: usize = 16;
pub const MAX_SCOUT_RECENT_DELIVERIES: usize = 32;
pub const MAX_SCOUT_ACTIVE_ADDRESSES: usize = 32;
pub const MAX_SCOUT_MODEL_INPUT_TOKENS: usize = 2_048;
pub const MAX_SCOUT_MODEL_OUTPUT_TOKENS: usize = 256;

/// Exact destination for one advisory suggestion. Every field is opaque and
/// fixed-size so a host integration cannot persist prompt/source/path data.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
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

impl ContextScoutAddressV1 {
    pub fn validate(self) -> Result<(), ContextScoutErrorV1> {
        if self.profile_id == [0; 16]
            || self.provider_id == [0; 16]
            || self.protected_session_id == [0; 32]
            || self.thread_id == [0; 16]
            || self.turn_id == [0; 16]
            || self.agent_id == [0; 16]
            || self.logical_message_id == [0; 16]
            || self.project_id == [0; 16]
        {
            return Err(ContextScoutErrorV1::InvalidAddress);
        }
        Ok(())
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutEvidenceGenerationV1 {
    SavedContent,
    CleanGeneration,
    DirtyOverlay,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct ContextScoutEvidenceBindingV1 {
    pub anchor_id: [u8; 16],
    pub content_identity: [u8; 32],
    pub generation: ContextScoutEvidenceGenerationV1,
}

impl ContextScoutEvidenceBindingV1 {
    pub fn validate(self) -> Result<(), ContextScoutErrorV1> {
        if self.anchor_id == [0; 16] || self.content_identity == [0; 32] {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        Ok(())
    }

    pub const fn durable(self) -> bool {
        matches!(
            self.generation,
            ContextScoutEvidenceGenerationV1::SavedContent
                | ContextScoutEvidenceGenerationV1::CleanGeneration
        )
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutCategoryV1 {
    Retrieval,
    Diagnostic,
    Coordination,
    Verification,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutCandidateV1 {
    pub dedupe_key: [u8; 32],
    pub category: ContextScoutCategoryV1,
    pub relevance_score: u16,
    pub suggestion_text: String,
    pub evidence: Vec<ContextScoutEvidenceBindingV1>,
    pub expires_at: UtcMicros,
}

impl ContextScoutCandidateV1 {
    pub fn validate(&self, limits: ContextScoutLimitsV1) -> Result<(), ContextScoutErrorV1> {
        if self.dedupe_key == [0; 32]
            || !safe_suggestion_text(&self.suggestion_text)
            || self.suggestion_text.len() > limits.max_text_bytes
            || self.evidence.is_empty()
            || self.evidence.len() > limits.max_evidence
            || self.expires_at.0 <= 0
        {
            return Err(ContextScoutErrorV1::InvalidCandidate);
        }
        let mut anchors = std::collections::BTreeSet::new();
        for evidence in &self.evidence {
            evidence.validate()?;
            if !anchors.insert(evidence.anchor_id) {
                return Err(ContextScoutErrorV1::InvalidCandidate);
            }
        }
        Ok(())
    }

    pub fn durable(&self) -> bool {
        self.evidence.iter().all(|evidence| evidence.durable())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutLimitsV1 {
    pub max_candidates: usize,
    pub max_evidence: usize,
    pub max_text_bytes: usize,
    pub max_model_input_tokens: usize,
    pub max_model_output_tokens: usize,
}

impl ContextScoutLimitsV1 {
    pub const fn bounded_defaults() -> Self {
        Self {
            max_candidates: MAX_SCOUT_CANDIDATES,
            max_evidence: MAX_SCOUT_EVIDENCE,
            max_text_bytes: MAX_SCOUT_TEXT_BYTES,
            max_model_input_tokens: MAX_SCOUT_MODEL_INPUT_TOKENS,
            max_model_output_tokens: MAX_SCOUT_MODEL_OUTPUT_TOKENS,
        }
    }

    pub fn validate(self) -> Result<(), ContextScoutErrorV1> {
        if self.max_candidates == 0
            || self.max_candidates > MAX_SCOUT_CANDIDATES
            || self.max_evidence == 0
            || self.max_evidence > MAX_SCOUT_EVIDENCE
            || self.max_text_bytes == 0
            || self.max_text_bytes > MAX_SCOUT_TEXT_BYTES
            || self.max_model_input_tokens == 0
            || self.max_model_input_tokens > MAX_SCOUT_MODEL_INPUT_TOKENS
            || self.max_model_output_tokens == 0
            || self.max_model_output_tokens > MAX_SCOUT_MODEL_OUTPUT_TOKENS
        {
            return Err(ContextScoutErrorV1::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutDeliveryWindowV1 {
    Immediate,
    NextBoundary,
    IdleWindow,
    OnRequest,
    Suppressed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutRouteV1 {
    Deterministic,
    ModelAssisted,
    DeterministicFallback,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutModelRunOutcomeV1 {
    #[default]
    NotRequested,
    Succeeded,
    Disabled,
    Unavailable,
    Cancelled,
    DeadlineExceeded,
    TokenBudgetExceeded,
    InvalidOutput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutModelBackendV1 {
    Disabled,
    CodexAppServer,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutModelReceiptV1 {
    pub requested_backend: ContextScoutModelBackendV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_microusd: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutSuggestionEnvelopeV1 {
    pub envelope_id: [u8; 16],
    pub address: ContextScoutAddressV1,
    pub input_watermark: [u8; 32],
    pub configuration_revision: [u8; 32],
    pub delivery_window: ContextScoutDeliveryWindowV1,
    pub candidate: ContextScoutCandidateV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutWorkV1 {
    pub address: ContextScoutAddressV1,
    pub generation: u64,
    pub input_watermark: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutOutcomeV1 {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutDeliveryReceiptV1 {
    pub receipt_id: [u8; 16],
    pub envelope_id: [u8; 16],
    pub delivered_at: UtcMicros,
    pub outcome: ContextScoutOutcomeV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutFeedbackKindV1 {
    ExplicitlyAccepted,
    ExplicitlyRejected,
    Dismissed,
    Corrected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutFeedbackV1 {
    pub receipt_id: [u8; 16],
    pub kind: ContextScoutFeedbackKindV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutDurableQueueEntryV1 {
    pub work: ContextScoutWorkV1,
    pub route: ContextScoutRouteV1,
    #[serde(default)]
    pub model_outcome: ContextScoutModelRunOutcomeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_receipt: Option<ContextScoutModelReceiptV1>,
    pub envelope: ContextScoutSuggestionEnvelopeV1,
}

impl ContextScoutDurableQueueEntryV1 {
    pub fn validate(&self) -> Result<(), ContextScoutErrorV1> {
        self.work.address.validate()?;
        if self.work.generation == 0 || self.work.input_watermark == [0; 32] {
            return Err(ContextScoutErrorV1::StaleWork);
        }
        match (self.route, self.model_outcome, self.model_receipt.as_ref()) {
            (
                ContextScoutRouteV1::ModelAssisted,
                ContextScoutModelRunOutcomeV1::Succeeded,
                Some(receipt),
            ) if receipt.requested_backend == ContextScoutModelBackendV1::CodexAppServer => {}
            (
                ContextScoutRouteV1::Deterministic | ContextScoutRouteV1::DeterministicFallback,
                ContextScoutModelRunOutcomeV1::NotRequested,
                None,
            ) => {}
            (
                ContextScoutRouteV1::DeterministicFallback,
                ContextScoutModelRunOutcomeV1::Disabled
                | ContextScoutModelRunOutcomeV1::Unavailable
                | ContextScoutModelRunOutcomeV1::DeadlineExceeded
                | ContextScoutModelRunOutcomeV1::TokenBudgetExceeded
                | ContextScoutModelRunOutcomeV1::InvalidOutput,
                None,
            ) => {}
            _ => return Err(ContextScoutErrorV1::InvalidCandidate),
        }
        validate_durable_envelope(&self.envelope)?;
        if self.work.address != self.envelope.address
            || self.work.input_watermark != self.envelope.input_watermark
        {
            return Err(ContextScoutErrorV1::StaleWork);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutLeaseV1 {
    pub lease_id: [u8; 16],
    pub expires_at: UtcMicros,
}

impl ContextScoutLeaseV1 {
    pub fn validate(self, now: UtcMicros) -> Result<(), ContextScoutErrorV1> {
        if self.lease_id == [0; 16] || now.0 <= 0 || self.expires_at.0 <= now.0 {
            return Err(ContextScoutErrorV1::StaleWork);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutDurableClaimV1 {
    pub entry: ContextScoutDurableQueueEntryV1,
    pub lease: ContextScoutLeaseV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutRecentDeliveryV1 {
    pub entry: ContextScoutDurableQueueEntryV1,
    pub receipt: ContextScoutDeliveryReceiptV1,
    pub feedback: Option<ContextScoutFeedbackV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutRecentStateV1 {
    pub configuration_revision: [u8; 32],
    pub observed_at: UtcMicros,
    pub pending: Vec<ContextScoutDurableQueueEntryV1>,
    pub deliveries: Vec<ContextScoutRecentDeliveryV1>,
    pub omitted: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutRuntimeModeV1 {
    Deterministic,
    ConfiguredModel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutServiceStateV1 {
    Active,
    Paused,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutControlV1 {
    pub configuration_revision: [u8; 32],
    pub state: ContextScoutServiceStateV1,
    pub mode: ContextScoutRuntimeModeV1,
    pub model_path: Option<ContextScoutModelBackendV1>,
    pub limits: ContextScoutLimitsV1,
}

impl ContextScoutControlV1 {
    pub fn validate(self) -> Result<(), ContextScoutErrorV1> {
        if self.configuration_revision == [0; 32]
            || matches!(
                (self.mode, self.model_path),
                (ContextScoutRuntimeModeV1::Deterministic, Some(_))
                    | (ContextScoutRuntimeModeV1::ConfiguredModel, None)
            )
            || self.model_path == Some(ContextScoutModelBackendV1::Disabled)
        {
            return Err(ContextScoutErrorV1::InvalidLimits);
        }
        self.limits.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextScoutStatusV1 {
    pub configuration_revision: [u8; 32],
    pub state: ContextScoutServiceStateV1,
    pub mode: ContextScoutRuntimeModeV1,
    pub model_path: Option<ContextScoutModelBackendV1>,
    pub limits: ContextScoutLimitsV1,
    pub active_suggestions: usize,
    pub last_route: Option<ContextScoutRouteV1>,
    pub last_suppression: Option<ContextScoutSuppressionV1>,
    pub last_model_outcome: Option<ContextScoutModelRunOutcomeV1>,
    pub last_model_receipt: Option<ContextScoutModelReceiptV1>,
    pub last_delivery_outcome: Option<ContextScoutOutcomeV1>,
    pub last_feedback: Option<ContextScoutFeedbackKindV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutExplanationV1 {
    pub status: ContextScoutStatusV1,
    pub recent: ContextScoutRecentStateV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutCapabilityStateV1 {
    pub state: ContextScoutServiceStateV1,
    pub mode: ContextScoutRuntimeModeV1,
    pub deterministic_available: bool,
    pub configured_model: Option<ContextScoutModelBackendV1>,
    pub configured_model_available: bool,
    pub last_model_outcome: Option<ContextScoutModelRunOutcomeV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutBudgetStateV1 {
    pub limits: ContextScoutLimitsV1,
    pub last_model_outcome: Option<ContextScoutModelRunOutcomeV1>,
    pub exhausted: bool,
    pub last_input_tokens: Option<u64>,
    pub last_output_tokens: Option<u64>,
    pub last_estimated_cost_microusd: Option<u64>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ContextScoutErrorV1 {
    #[error("Context Scout address is incomplete or ambiguous")]
    InvalidAddress,
    #[error("Context Scout evidence binding is incomplete")]
    InvalidEvidence,
    #[error("Context Scout candidate is malformed or exceeds a bound")]
    InvalidCandidate,
    #[error("Context Scout limits are invalid")]
    InvalidLimits,
    #[error("Context Scout typed configuration is unavailable")]
    ConfigurationUnavailable,
    #[error("Context Scout durable boundary received dirty-overlay evidence")]
    DirtyOverlayDurabilityViolation,
    #[error("Context Scout receipt or feedback does not match its envelope")]
    ReceiptBindingMismatch,
    #[error("Context Scout cancellation/work token is stale")]
    StaleWork,
    #[error("Context Scout bounded work or delivery channel is full")]
    CapacityExceeded,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutClaimWindowV1 {
    IdleWindow,
    OnRequest,
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
    #[schemars(range(min = 1, max = 32))]
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
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutCancelRequestV1 {
    pub address: ContextScoutAddressV1,
    pub work: ContextScoutWorkV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutClaimRequestV1 {
    pub address: ContextScoutAddressV1,
    pub window: ContextScoutClaimWindowV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutDeliveryRequestV1 {
    pub address: ContextScoutAddressV1,
    pub claim: ContextScoutDurableClaimV1,
    pub receipt: ContextScoutDeliveryReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutFeedbackRequestV1 {
    pub address: ContextScoutAddressV1,
    pub receipt: ContextScoutDeliveryReceiptV1,
    pub feedback: ContextScoutFeedbackV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutMutationOutcomeV1 {
    Stored,
    Duplicate,
    Superseded,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutMutationResultV1 {
    pub outcome: ContextScoutMutationOutcomeV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutEmptyOutcomeV1 {
    Empty,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutEmptyResultV1 {
    pub outcome: ContextScoutEmptyOutcomeV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(untagged)]
pub enum ContextScoutClaimResultV1 {
    Claimed(ContextScoutDurableClaimV1),
    Empty(ContextScoutEmptyResultV1),
}

fn validate_durable_envelope(
    envelope: &ContextScoutSuggestionEnvelopeV1,
) -> Result<(), ContextScoutErrorV1> {
    envelope.address.validate()?;
    envelope
        .candidate
        .validate(ContextScoutLimitsV1::bounded_defaults())?;
    if envelope.envelope_id == [0; 16]
        || envelope.input_watermark == [0; 32]
        || envelope.configuration_revision == [0; 32]
        || envelope.delivery_window == ContextScoutDeliveryWindowV1::Suppressed
        || !envelope.candidate.durable()
    {
        return Err(ContextScoutErrorV1::DirtyOverlayDurabilityViolation);
    }
    Ok(())
}

fn safe_suggestion_text(value: &str) -> bool {
    !value.trim().is_empty()
        && !value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
}

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
    control_spec("context_scout_pause", "Pause Context Scout"),
    control_spec("context_scout_resume", "Resume Context Scout"),
    control_spec("context_scout_cancel", "Cancel Context Scout work"),
    control_spec("context_scout_claim", "Claim a Context Scout delivery"),
    control_spec("context_scout_delivery", "Record a Context Scout delivery"),
    control_spec("context_scout_feedback", "Record Context Scout feedback"),
];

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
            cancellation: CancellationContract::cooperative(if is_effect {
                vec![
                    CancellationPoint::BeforeAdmission,
                    CancellationPoint::BeforeEffect,
                    CancellationPoint::EffectInFlight,
                    CancellationPoint::Reconciling,
                    CancellationPoint::AfterCommit,
                ]
            } else {
                vec![
                    CancellationPoint::BeforeAdmission,
                    CancellationPoint::BeforeRead,
                    CancellationPoint::DuringRead,
                ]
            })?,
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
                vec![
                    TerminalState::Completed,
                    TerminalState::Cancelled,
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
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.context-scout-surface")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
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
                assert!(
                    capability
                        .cancellation()
                        .points()
                        .contains(&CancellationPoint::EffectInFlight)
                );
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
}
