//! Bounded Context Scout host-facing contracts (Plan 22 / PR13).
//!
//! The daemon owns retrieval, policy persistence, model execution, and every
//! side effect. This module is deliberately limited to deterministic candidate
//! selection, model-adapter validation, exact-address coalescing, and durable
//! value contracts for delivery receipts and feedback.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(feature = "token-counting")]
use tiktoken_rs::o200k_base_singleton;
use tracedecay_domain::UtcMicros;

use crate::application::context::{CancellationToken, MonotonicDeadline};

const MAX_SCOUT_TEXT_BYTES: usize = 4 * 1024;
const MAX_SCOUT_CANDIDATES: usize = 32;
const MAX_SCOUT_EVIDENCE: usize = 16;
const MAX_SCOUT_RECENT_DELIVERIES: usize = 32;
const MAX_SCOUT_ACTIVE_ADDRESSES: usize = 32;
const MAX_SCOUT_MODEL_INPUT_TOKENS: usize = 2_048;
const MAX_SCOUT_MODEL_OUTPUT_TOKENS: usize = 256;

mod store;
#[cfg(test)]
mod store_tests;

pub use store::ProjectContextScoutDurableStoreV1;

/// Exact destination for one advisory suggestion. Every field is opaque and
/// fixed-size so a host integration cannot persist prompt/source/path data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    fn validate(self) -> Result<(), ContextScoutErrorV1> {
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

/// Only saved-content and clean-generation evidence is eligible for durable
/// envelopes. Dirty overlay data is represented only so callers can receive a
/// typed suppression; it must never cross a durable boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutEvidenceGenerationV1 {
    SavedContent,
    CleanGeneration,
    DirtyOverlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContextScoutEvidenceBindingV1 {
    pub anchor_id: [u8; 16],
    pub content_identity: [u8; 32],
    pub generation: ContextScoutEvidenceGenerationV1,
}

impl ContextScoutEvidenceBindingV1 {
    fn validate(self) -> Result<(), ContextScoutErrorV1> {
        if self.anchor_id == [0; 16] || self.content_identity == [0; 32] {
            return Err(ContextScoutErrorV1::InvalidEvidence);
        }
        Ok(())
    }

    const fn durable(self) -> bool {
        matches!(
            self.generation,
            ContextScoutEvidenceGenerationV1::SavedContent
                | ContextScoutEvidenceGenerationV1::CleanGeneration
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutCategoryV1 {
    Retrieval,
    Diagnostic,
    Coordination,
    Verification,
}

/// A daemon-produced candidate. The text is compact prompt-eligible advice;
/// its evidence remains separately pinned to durable opaque identities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextScoutCandidateV1 {
    pub dedupe_key: [u8; 32],
    pub category: ContextScoutCategoryV1,
    pub relevance_score: u16,
    pub suggestion_text: String,
    pub evidence: Vec<ContextScoutEvidenceBindingV1>,
    pub expires_at: UtcMicros,
}

impl ContextScoutCandidateV1 {
    fn validate(&self, limits: ContextScoutLimitsV1) -> Result<(), ContextScoutErrorV1> {
        if self.dedupe_key == [0; 32]
            || !safe_suggestion_text(&self.suggestion_text)
            || self.suggestion_text.len() > limits.max_text_bytes
            || self.evidence.is_empty()
            || self.evidence.len() > limits.max_evidence
            || self.expires_at.0 <= 0
        {
            return Err(ContextScoutErrorV1::InvalidCandidate);
        }
        let mut anchors = BTreeSet::new();
        for evidence in &self.evidence {
            evidence.validate()?;
            if !anchors.insert(evidence.anchor_id) {
                return Err(ContextScoutErrorV1::InvalidCandidate);
            }
        }
        Ok(())
    }

    fn durable(&self) -> bool {
        self.evidence.iter().all(|evidence| evidence.durable())
    }
}

/// Bounded limits supplied from typed configuration. There is no source-code
/// default model/provider or delivery timing policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

    fn validate(self) -> Result<(), ContextScoutErrorV1> {
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

/// The daemon/policy-owned receptivity result. A model adapter cannot choose
/// this value and no fixed timing threshold is embedded here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutDeliveryWindowV1 {
    Immediate,
    NextBoundary,
    IdleWindow,
    OnRequest,
    Suppressed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutTriggerV1 {
    SavedEdit,
    StopBoundary,
    ExplicitRequest,
}

/// Content-free daemon receptivity state. The selector owns delivery timing;
/// candidates and model output cannot override it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutDeliverySelectionInputV1 {
    pub trigger: ContextScoutTriggerV1,
    pub quiet_mode: bool,
    pub has_recent_delivery: bool,
    pub has_unresolved_interaction: bool,
    pub critical_safety_evidence: bool,
    pub delivered_dedupe_keys: BTreeSet<[u8; 32]>,
}

pub const fn select_context_scout_delivery_window(
    input: &ContextScoutDeliverySelectionInputV1,
) -> ContextScoutDeliveryWindowV1 {
    if let ContextScoutTriggerV1::ExplicitRequest = input.trigger {
        return ContextScoutDeliveryWindowV1::OnRequest;
    }
    if input.critical_safety_evidence {
        return ContextScoutDeliveryWindowV1::Immediate;
    }
    if input.quiet_mode {
        return ContextScoutDeliveryWindowV1::Suppressed;
    }
    if input.has_unresolved_interaction {
        return ContextScoutDeliveryWindowV1::NextBoundary;
    }
    if input.has_recent_delivery {
        return ContextScoutDeliveryWindowV1::IdleWindow;
    }
    match input.trigger {
        ContextScoutTriggerV1::SavedEdit => ContextScoutDeliveryWindowV1::NextBoundary,
        ContextScoutTriggerV1::StopBoundary => ContextScoutDeliveryWindowV1::Immediate,
        ContextScoutTriggerV1::ExplicitRequest => ContextScoutDeliveryWindowV1::OnRequest,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextScoutSelectionInputV1 {
    pub address: ContextScoutAddressV1,
    pub input_watermark: [u8; 32],
    pub configuration_revision: [u8; 32],
    pub envelope_id: [u8; 16],
    pub now: UtcMicros,
    pub delivery_window: ContextScoutDeliveryWindowV1,
    pub delivered_dedupe_keys: BTreeSet<[u8; 32]>,
    pub candidates: Vec<ContextScoutCandidateV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextScoutSuggestionEnvelopeV1 {
    pub envelope_id: [u8; 16],
    pub address: ContextScoutAddressV1,
    pub input_watermark: [u8; 32],
    pub configuration_revision: [u8; 32],
    pub delivery_window: ContextScoutDeliveryWindowV1,
    pub candidate: ContextScoutCandidateV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ContextScoutDecisionV1 {
    Ready {
        envelope: ContextScoutSuggestionEnvelopeV1,
    },
    Delayed {
        envelope: ContextScoutSuggestionEnvelopeV1,
    },
    Suppressed {
        reason: ContextScoutSuppressionV1,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutRouteV1 {
    Deterministic,
    ModelAssisted,
    DeterministicFallback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextScoutSelectionV1 {
    pub route: ContextScoutRouteV1,
    pub decision: ContextScoutDecisionV1,
    #[serde(default)]
    pub model_outcome: ContextScoutModelRunOutcomeV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_receipt: Option<ContextScoutModelReceiptV1>,
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

/// Deterministically choose at most one candidate without invoking a model,
/// query, database, or network. Equal scores are ordered by stable dedupe key,
/// category, and text, so replay is repeatable.
pub fn select_deterministic_context_scout(
    input: &ContextScoutSelectionInputV1,
    limits: ContextScoutLimitsV1,
) -> Result<ContextScoutDecisionV1, ContextScoutErrorV1> {
    limits.validate()?;
    input.address.validate()?;
    if input.input_watermark == [0; 32]
        || input.configuration_revision == [0; 32]
        || input.envelope_id == [0; 16]
        || input.now.0 <= 0
        || input.delivered_dedupe_keys.len() > MAX_SCOUT_RECENT_DELIVERIES
        || input.candidates.len() > limits.max_candidates
    {
        return Err(ContextScoutErrorV1::InvalidCandidate);
    }
    if input.delivery_window == ContextScoutDeliveryWindowV1::Suppressed {
        return Ok(ContextScoutDecisionV1::Suppressed {
            reason: ContextScoutSuppressionV1::QuietOrUnreceptive,
        });
    }
    validate_candidate_set(&input.candidates, limits)?;
    let mut candidates = input.candidates.iter().collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .relevance_score
            .cmp(&left.relevance_score)
            .then_with(|| left.dedupe_key.cmp(&right.dedupe_key))
            .then_with(|| left.category.cmp(&right.category))
            .then_with(|| left.suggestion_text.cmp(&right.suggestion_text))
    });
    let mut saw_expired = false;
    let mut saw_duplicate = false;
    let mut saw_overlay = false;
    for candidate in candidates {
        if !candidate.durable() {
            saw_overlay = true;
            continue;
        }
        if candidate.expires_at.0 <= input.now.0 {
            saw_expired = true;
            continue;
        }
        if input.delivered_dedupe_keys.contains(&candidate.dedupe_key) {
            saw_duplicate = true;
            continue;
        }
        let envelope = ContextScoutSuggestionEnvelopeV1 {
            envelope_id: input.envelope_id,
            address: input.address,
            input_watermark: input.input_watermark,
            configuration_revision: input.configuration_revision,
            delivery_window: input.delivery_window,
            candidate: candidate.clone(),
        };
        return Ok(match input.delivery_window {
            ContextScoutDeliveryWindowV1::Immediate => ContextScoutDecisionV1::Ready { envelope },
            ContextScoutDeliveryWindowV1::NextBoundary
            | ContextScoutDeliveryWindowV1::IdleWindow
            | ContextScoutDeliveryWindowV1::OnRequest => {
                ContextScoutDecisionV1::Delayed { envelope }
            }
            ContextScoutDeliveryWindowV1::Suppressed => unreachable!("checked above"),
        });
    }
    let reason = if saw_overlay {
        ContextScoutSuppressionV1::DirtyOverlay
    } else if saw_expired {
        ContextScoutSuppressionV1::Expired
    } else if saw_duplicate {
        ContextScoutSuppressionV1::Duplicate
    } else {
        ContextScoutSuppressionV1::NoEligibleCandidate
    };
    Ok(ContextScoutDecisionV1::Suppressed { reason })
}

/// Model input is already bounded and evidence-only. There is no tool,
/// command, credential, source, path, or delivery-policy capability here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutModelRequestV1 {
    pub candidates: Vec<ContextScoutModelCandidateInputV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutModelCandidateInputV1 {
    pub dedupe_key: [u8; 32],
    pub category: ContextScoutCategoryV1,
    pub suggestion_text: String,
    pub citation_anchor_ids: Vec<[u8; 16]>,
}

/// Structured model output may refine text only. It must select one supplied
/// deterministic candidate and cite exactly that candidate's durable anchors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutModelCandidateV1 {
    pub selected_dedupe_key: [u8; 32],
    pub suggestion_text: String,
    pub cited_anchor_ids: Vec<[u8; 16]>,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ContextScoutModelErrorV1 {
    #[error("configured Context Scout model route is disabled")]
    Disabled,
    #[error("configured Context Scout model route is unavailable")]
    Unavailable,
    #[error("configured Context Scout model route was cancelled")]
    Cancelled,
    #[error("configured Context Scout model route exceeded its deadline")]
    DeadlineExceeded,
    #[error("configured Context Scout model route exceeded its token budget")]
    TokenBudgetExceeded,
    #[error("configured Context Scout model route returned invalid structured output")]
    InvalidOutput,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

impl From<ContextScoutModelErrorV1> for ContextScoutModelRunOutcomeV1 {
    fn from(error: ContextScoutModelErrorV1) -> Self {
        match error {
            ContextScoutModelErrorV1::Disabled => Self::Disabled,
            ContextScoutModelErrorV1::Unavailable => Self::Unavailable,
            ContextScoutModelErrorV1::Cancelled => Self::Cancelled,
            ContextScoutModelErrorV1::DeadlineExceeded => Self::DeadlineExceeded,
            ContextScoutModelErrorV1::TokenBudgetExceeded => Self::TokenBudgetExceeded,
            ContextScoutModelErrorV1::InvalidOutput => Self::InvalidOutput,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutModelBackendV1 {
    Disabled,
    CodexAppServer,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextScoutModelProposalV1 {
    pub candidate: ContextScoutModelCandidateV1,
    pub receipt: ContextScoutModelReceiptV1,
}

#[derive(Clone, Debug)]
pub struct ContextScoutModelExecutionV1 {
    pub deadline: MonotonicDeadline,
    pub cancellation: CancellationToken,
    pub max_input_tokens: usize,
    pub max_output_tokens: usize,
}

impl ContextScoutModelExecutionV1 {
    pub fn new(
        deadline: MonotonicDeadline,
        cancellation: CancellationToken,
        limits: ContextScoutLimitsV1,
    ) -> Result<Self, ContextScoutErrorV1> {
        limits.validate()?;
        Ok(Self {
            deadline,
            cancellation,
            max_input_tokens: limits.max_model_input_tokens,
            max_output_tokens: limits.max_model_output_tokens,
        })
    }

    fn matches_limits(&self, limits: ContextScoutLimitsV1) -> bool {
        self.max_input_tokens == limits.max_model_input_tokens
            && self.max_output_tokens == limits.max_model_output_tokens
    }

    pub fn checkpoint(&self) -> Result<(), ContextScoutModelErrorV1> {
        if self.cancellation.is_cancelled() {
            return Err(ContextScoutModelErrorV1::Cancelled);
        }
        if self.deadline.is_elapsed_at(Instant::now()) {
            return Err(ContextScoutModelErrorV1::DeadlineExceeded);
        }
        Ok(())
    }

    pub fn validate_input(
        &self,
        request: &ContextScoutModelRequestV1,
    ) -> Result<(), ContextScoutModelErrorV1> {
        let tokens =
            serialized_token_count(request).ok_or(ContextScoutModelErrorV1::TokenBudgetExceeded)?;
        if tokens > self.max_input_tokens {
            return Err(ContextScoutModelErrorV1::TokenBudgetExceeded);
        }
        Ok(())
    }

    pub fn validate_output(
        &self,
        candidate: &ContextScoutModelCandidateV1,
    ) -> Result<(), ContextScoutModelErrorV1> {
        let tokens = serialized_token_count(candidate)
            .ok_or(ContextScoutModelErrorV1::TokenBudgetExceeded)?;
        if tokens > self.max_output_tokens {
            return Err(ContextScoutModelErrorV1::TokenBudgetExceeded);
        }
        Ok(())
    }
}

pub fn decode_context_scout_model_candidate(
    bytes: &[u8],
) -> Result<ContextScoutModelCandidateV1, ContextScoutModelErrorV1> {
    if bytes.is_empty() || bytes.len() > MAX_SCOUT_TEXT_BYTES {
        return Err(ContextScoutModelErrorV1::InvalidOutput);
    }
    serde_json::from_slice(bytes).map_err(|_| ContextScoutModelErrorV1::InvalidOutput)
}

/// The daemon's configured model gateway implements this narrow adapter. The
/// host bundle never selects a model name or grants capabilities itself.
pub type ContextScoutModelFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<ContextScoutModelProposalV1, ContextScoutModelErrorV1>>
            + Send
            + 'a,
    >,
>;

pub trait ContextScoutModelAssistantV1: Send + Sync {
    fn backend(&self) -> ContextScoutModelBackendV1;

    fn propose(
        &self,
        request: ContextScoutModelRequestV1,
        execution: ContextScoutModelExecutionV1,
    ) -> ContextScoutModelFuture<'_>;
}

impl<T> ContextScoutModelAssistantV1 for std::sync::Arc<T>
where
    T: ContextScoutModelAssistantV1 + ?Sized,
{
    fn backend(&self) -> ContextScoutModelBackendV1 {
        (**self).backend()
    }

    fn propose(
        &self,
        request: ContextScoutModelRequestV1,
        execution: ContextScoutModelExecutionV1,
    ) -> ContextScoutModelFuture<'_> {
        (**self).propose(request, execution)
    }
}

/// Run the model adapter only after constructing the deterministic candidate
/// lane. Invalid/unavailable output falls back without compromising capture.
pub async fn select_model_assisted_context_scout(
    input: &ContextScoutSelectionInputV1,
    limits: ContextScoutLimitsV1,
    model: &impl ContextScoutModelAssistantV1,
    execution: ContextScoutModelExecutionV1,
) -> Result<ContextScoutSelectionV1, ContextScoutErrorV1> {
    let deterministic = select_deterministic_context_scout(input, limits)?;
    let selected_candidate = match &deterministic {
        ContextScoutDecisionV1::Ready { envelope }
        | ContextScoutDecisionV1::Delayed { envelope } => &envelope.candidate,
        ContextScoutDecisionV1::Suppressed { .. } => {
            return Ok(ContextScoutSelectionV1 {
                route: ContextScoutRouteV1::Deterministic,
                decision: deterministic,
                model_outcome: ContextScoutModelRunOutcomeV1::NotRequested,
                model_receipt: None,
            });
        }
    };
    if !execution.matches_limits(limits) {
        return Err(ContextScoutErrorV1::InvalidLimits);
    }
    if let Err(error) = execution.checkpoint() {
        return Ok(ContextScoutSelectionV1 {
            route: ContextScoutRouteV1::DeterministicFallback,
            decision: deterministic,
            model_outcome: error.into(),
            model_receipt: None,
        });
    }
    let Some(request) = model_request(input, limits)? else {
        return Ok(ContextScoutSelectionV1 {
            route: ContextScoutRouteV1::DeterministicFallback,
            decision: deterministic,
            model_outcome: ContextScoutModelRunOutcomeV1::TokenBudgetExceeded,
            model_receipt: None,
        });
    };
    if let Err(error) = execution.validate_input(&request) {
        return Ok(ContextScoutSelectionV1 {
            route: ContextScoutRouteV1::DeterministicFallback,
            decision: deterministic,
            model_outcome: error.into(),
            model_receipt: None,
        });
    }
    let requested_backend = model.backend();
    let max_input_tokens = execution.max_input_tokens;
    let max_output_tokens = execution.max_output_tokens;
    let post_execution = execution.clone();
    let proposal = match model.propose(request, execution).await {
        Ok(proposal) => proposal,
        Err(error) => {
            return Ok(ContextScoutSelectionV1 {
                route: ContextScoutRouteV1::DeterministicFallback,
                decision: deterministic,
                model_outcome: error.into(),
                model_receipt: None,
            });
        }
    };
    if let Err(error) = post_execution.checkpoint() {
        return Ok(ContextScoutSelectionV1 {
            route: ContextScoutRouteV1::DeterministicFallback,
            decision: deterministic,
            model_outcome: error.into(),
            model_receipt: None,
        });
    }
    if !valid_model_receipt(
        &proposal.receipt,
        requested_backend,
        max_input_tokens,
        max_output_tokens,
    ) {
        return Ok(ContextScoutSelectionV1 {
            route: ContextScoutRouteV1::DeterministicFallback,
            decision: deterministic,
            model_outcome: ContextScoutModelRunOutcomeV1::InvalidOutput,
            model_receipt: None,
        });
    }
    let Some(candidate) = validated_model_candidate(
        selected_candidate,
        limits,
        selected_candidate.dedupe_key,
        proposal.candidate,
    ) else {
        return Ok(ContextScoutSelectionV1 {
            route: ContextScoutRouteV1::DeterministicFallback,
            decision: deterministic,
            model_outcome: ContextScoutModelRunOutcomeV1::InvalidOutput,
            model_receipt: None,
        });
    };
    let mut model_input = input.clone();
    model_input.candidates = vec![candidate];
    Ok(ContextScoutSelectionV1 {
        route: ContextScoutRouteV1::ModelAssisted,
        decision: select_deterministic_context_scout(&model_input, limits)?,
        model_outcome: ContextScoutModelRunOutcomeV1::Succeeded,
        model_receipt: Some(proposal.receipt),
    })
}

fn valid_model_receipt(
    receipt: &ContextScoutModelReceiptV1,
    requested_backend: ContextScoutModelBackendV1,
    max_input_tokens: usize,
    max_output_tokens: usize,
) -> bool {
    requested_backend == ContextScoutModelBackendV1::CodexAppServer
        && receipt.requested_backend == requested_backend
        && receipt
            .actual_model
            .as_ref()
            .is_some_and(|model| !model.trim().is_empty() && model.len() <= MAX_SCOUT_TEXT_BYTES)
        && receipt
            .input_tokens
            .is_some_and(|tokens| tokens <= max_input_tokens as u64)
        && receipt
            .output_tokens
            .is_some_and(|tokens| tokens <= max_output_tokens as u64)
        && receipt.estimated_cost_microusd.is_some()
}

fn model_request(
    input: &ContextScoutSelectionInputV1,
    limits: ContextScoutLimitsV1,
) -> Result<Option<ContextScoutModelRequestV1>, ContextScoutErrorV1> {
    limits.validate()?;
    input.address.validate()?;
    if input.candidates.len() > limits.max_candidates {
        return Err(ContextScoutErrorV1::InvalidCandidate);
    }
    validate_candidate_set(&input.candidates, limits)?;
    let mut eligible = input
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.durable()
                && candidate.expires_at.0 > input.now.0
                && !input.delivered_dedupe_keys.contains(&candidate.dedupe_key)
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| {
        right
            .relevance_score
            .cmp(&left.relevance_score)
            .then_with(|| left.dedupe_key.cmp(&right.dedupe_key))
            .then_with(|| left.category.cmp(&right.category))
            .then_with(|| left.suggestion_text.cmp(&right.suggestion_text))
    });
    let candidates = eligible
        .into_iter()
        .map(|candidate| ContextScoutModelCandidateInputV1 {
            dedupe_key: candidate.dedupe_key,
            category: candidate.category,
            suggestion_text: candidate.suggestion_text.clone(),
            citation_anchor_ids: candidate
                .evidence
                .iter()
                .map(|evidence| evidence.anchor_id)
                .collect(),
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(None);
    }
    let request = ContextScoutModelRequestV1 { candidates };
    let Some(tokens) = serialized_token_count(&request) else {
        return Ok(None);
    };
    if tokens > limits.max_model_input_tokens {
        return Ok(None);
    }
    Ok(Some(request))
}

fn validated_model_candidate(
    base: &ContextScoutCandidateV1,
    limits: ContextScoutLimitsV1,
    selected_dedupe_key: [u8; 32],
    output: ContextScoutModelCandidateV1,
) -> Option<ContextScoutCandidateV1> {
    if output.selected_dedupe_key != selected_dedupe_key {
        return None;
    }
    let output_tokens = serialized_token_count(&output)?;
    if !base.durable()
        || !safe_suggestion_text(&output.suggestion_text)
        || output.suggestion_text.len() > limits.max_text_bytes
        || output_tokens > limits.max_model_output_tokens
        || output.cited_anchor_ids.len() > limits.max_evidence
    {
        return None;
    }
    let expected = base
        .evidence
        .iter()
        .map(|evidence| evidence.anchor_id)
        .collect::<BTreeSet<_>>();
    let cited_len = output.cited_anchor_ids.len();
    let cited = output.cited_anchor_ids.into_iter().collect::<BTreeSet<_>>();
    if cited.len() != cited_len || expected != cited {
        return None;
    }
    Some(ContextScoutCandidateV1 {
        suggestion_text: output.suggestion_text,
        ..base.clone()
    })
}

fn validate_candidate_set(
    candidates: &[ContextScoutCandidateV1],
    limits: ContextScoutLimitsV1,
) -> Result<(), ContextScoutErrorV1> {
    let mut dedupe_keys = BTreeSet::new();
    for candidate in candidates {
        candidate.validate(limits)?;
        if !dedupe_keys.insert(candidate.dedupe_key) {
            return Err(ContextScoutErrorV1::InvalidCandidate);
        }
    }
    Ok(())
}

fn safe_suggestion_text(value: &str) -> bool {
    !value.trim().is_empty()
        && !value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
}

#[cfg(feature = "token-counting")]
pub(super) fn serialized_token_count(value: &impl Serialize) -> Option<usize> {
    let json = serde_json::to_string(value).ok()?;
    Some(o200k_base_singleton().encode_ordinary(&json).len())
}

#[cfg(not(feature = "token-counting"))]
pub(super) fn serialized_token_count(_value: &impl Serialize) -> Option<usize> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextScoutWorkV1 {
    pub address: ContextScoutAddressV1,
    pub generation: u64,
    pub input_watermark: [u8; 32],
}

/// Exact-address burst coalescer. Superseded generations are visible through a
/// new token; callers must not persist a result after `is_current` becomes
/// false.
#[derive(Default)]
pub struct ContextScoutCoalescerV1 {
    active: BTreeMap<ContextScoutAddressV1, ContextScoutWorkV1>,
}

impl ContextScoutCoalescerV1 {
    fn restore(&mut self, work: ContextScoutWorkV1) -> Result<(), ContextScoutErrorV1> {
        work.address.validate()?;
        if work.generation == 0 || work.input_watermark == [0; 32] {
            return Err(ContextScoutErrorV1::StaleWork);
        }
        if !self.active.contains_key(&work.address)
            && self.active.len() >= MAX_SCOUT_ACTIVE_ADDRESSES
        {
            return Err(ContextScoutErrorV1::CapacityExceeded);
        }
        match self.active.get(&work.address) {
            Some(current) if current.generation > work.generation => Ok(()),
            Some(current) if current.generation == work.generation => {
                if current == &work {
                    Ok(())
                } else {
                    Err(ContextScoutErrorV1::StaleWork)
                }
            }
            _ => {
                self.active.insert(work.address, work);
                Ok(())
            }
        }
    }

    pub fn enqueue(
        &mut self,
        address: ContextScoutAddressV1,
        input_watermark: [u8; 32],
    ) -> Result<ContextScoutWorkV1, ContextScoutErrorV1> {
        address.validate()?;
        if input_watermark == [0; 32] {
            return Err(ContextScoutErrorV1::InvalidCandidate);
        }
        if !self.active.contains_key(&address) && self.active.len() >= MAX_SCOUT_ACTIVE_ADDRESSES {
            return Err(ContextScoutErrorV1::CapacityExceeded);
        }
        let generation = self
            .active
            .get(&address)
            .map_or(1, |work| work.generation.saturating_add(1));
        let work = ContextScoutWorkV1 {
            address,
            generation,
            input_watermark,
        };
        self.active.insert(address, work);
        Ok(work)
    }

    pub fn is_current(&self, work: ContextScoutWorkV1) -> bool {
        self.active.get(&work.address) == Some(&work)
    }

    pub fn cancel(&mut self, work: ContextScoutWorkV1) -> Result<(), ContextScoutErrorV1> {
        if !self.is_current(work) {
            return Err(ContextScoutErrorV1::StaleWork);
        }
        self.active.remove(&work.address);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChannelSlotV1 {
    work: ContextScoutWorkV1,
    envelope: ContextScoutSuggestionEnvelopeV1,
    claimed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextScoutChannelOutcomeV1 {
    Offered,
    Coalesced,
    Superseded,
    Suppressed,
}

/// The single bounded suggestion channel. It stores no more than one pending
/// envelope per exact address/eligibility window.
#[derive(Default)]
pub struct ContextScoutSuggestionChannelV1 {
    slots: BTreeMap<ContextScoutAddressV1, ChannelSlotV1>,
}

pub struct ContextScoutClaimV1<'a> {
    channel: &'a mut ContextScoutSuggestionChannelV1,
    work: ContextScoutWorkV1,
    envelope: ContextScoutSuggestionEnvelopeV1,
    completed: bool,
}

impl ContextScoutClaimV1<'_> {
    pub fn envelope(&self) -> &ContextScoutSuggestionEnvelopeV1 {
        &self.envelope
    }

    pub fn complete(
        mut self,
        receipt: &ContextScoutDeliveryReceiptV1,
    ) -> Result<(), ContextScoutErrorV1> {
        validate_context_scout_delivery_receipt(&self.envelope, receipt)?;
        let slot = self
            .channel
            .slots
            .get(&self.work.address)
            .ok_or(ContextScoutErrorV1::ReceiptBindingMismatch)?;
        if slot.work != self.work || !slot.claimed {
            return Err(ContextScoutErrorV1::ReceiptBindingMismatch);
        }
        self.channel.slots.remove(&self.work.address);
        self.completed = true;
        Ok(())
    }
}

impl Drop for ContextScoutClaimV1<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Some(slot) = self.channel.slots.get_mut(&self.work.address)
            && slot.work == self.work
        {
            slot.claimed = false;
        }
    }
}

impl ContextScoutSuggestionChannelV1 {
    pub fn offer(
        &mut self,
        work: ContextScoutWorkV1,
        decision: ContextScoutDecisionV1,
    ) -> Result<ContextScoutChannelOutcomeV1, ContextScoutErrorV1> {
        let envelope = match decision {
            ContextScoutDecisionV1::Ready { envelope }
            | ContextScoutDecisionV1::Delayed { envelope } => envelope,
            ContextScoutDecisionV1::Suppressed { .. } => {
                return Ok(ContextScoutChannelOutcomeV1::Suppressed);
            }
        };
        validate_durable_envelope(&envelope)?;
        if work.address != envelope.address
            || work.input_watermark != envelope.input_watermark
            || work.generation == 0
        {
            return Err(ContextScoutErrorV1::StaleWork);
        }
        if !self.slots.contains_key(&envelope.address)
            && self.slots.len() >= MAX_SCOUT_ACTIVE_ADDRESSES
        {
            return Err(ContextScoutErrorV1::CapacityExceeded);
        }
        match self.slots.get(&envelope.address) {
            Some(existing) if existing.work.generation > work.generation => {
                Err(ContextScoutErrorV1::StaleWork)
            }
            Some(existing) if existing.work == work && existing.envelope == envelope => {
                Ok(ContextScoutChannelOutcomeV1::Coalesced)
            }
            Some(existing) if existing.work.generation == work.generation => {
                Err(ContextScoutErrorV1::StaleWork)
            }
            Some(_) => {
                self.slots.insert(
                    envelope.address,
                    ChannelSlotV1 {
                        work,
                        envelope,
                        claimed: false,
                    },
                );
                Ok(ContextScoutChannelOutcomeV1::Superseded)
            }
            None => {
                self.slots.insert(
                    envelope.address,
                    ChannelSlotV1 {
                        work,
                        envelope,
                        claimed: false,
                    },
                );
                Ok(ContextScoutChannelOutcomeV1::Offered)
            }
        }
    }

    pub fn claim(&mut self, address: ContextScoutAddressV1) -> Option<ContextScoutClaimV1<'_>> {
        let (work, envelope) = {
            let slot = self.slots.get_mut(&address)?;
            if slot.claimed {
                return None;
            }
            slot.claimed = true;
            (slot.work, slot.envelope.clone())
        };
        Some(ContextScoutClaimV1 {
            channel: self,
            work,
            envelope,
            completed: false,
        })
    }

    pub fn cancel(&mut self, work: ContextScoutWorkV1) -> Result<(), ContextScoutErrorV1> {
        let Some(slot) = self.slots.get(&work.address) else {
            return Err(ContextScoutErrorV1::StaleWork);
        };
        if slot.work != work {
            return Err(ContextScoutErrorV1::StaleWork);
        }
        self.slots.remove(&work.address);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutDeliveryReceiptV1 {
    pub receipt_id: [u8; 16],
    pub envelope_id: [u8; 16],
    pub delivered_at: UtcMicros,
    pub outcome: ContextScoutOutcomeV1,
}

pub fn validate_context_scout_delivery_receipt(
    envelope: &ContextScoutSuggestionEnvelopeV1,
    receipt: &ContextScoutDeliveryReceiptV1,
) -> Result<(), ContextScoutErrorV1> {
    validate_durable_envelope(envelope)?;
    if receipt.receipt_id == [0; 16]
        || receipt.envelope_id != envelope.envelope_id
        || receipt.delivered_at.0 <= 0
        || (receipt.outcome != ContextScoutOutcomeV1::ExpiredUnseen
            && receipt.delivered_at.0 >= envelope.candidate.expires_at.0)
    {
        return Err(ContextScoutErrorV1::ReceiptBindingMismatch);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutFeedbackKindV1 {
    ExplicitlyAccepted,
    ExplicitlyRejected,
    Dismissed,
    Corrected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutFeedbackV1 {
    pub receipt_id: [u8; 16],
    pub kind: ContextScoutFeedbackKindV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutRecentDeliveryV1 {
    pub entry: ContextScoutDurableQueueEntryV1,
    pub receipt: ContextScoutDeliveryReceiptV1,
    pub feedback: Option<ContextScoutFeedbackV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutRecentStateV1 {
    pub configuration_revision: [u8; 32],
    pub observed_at: UtcMicros,
    pub pending: Vec<ContextScoutDurableQueueEntryV1>,
    pub deliveries: Vec<ContextScoutRecentDeliveryV1>,
    pub omitted: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextScoutRecentReadOutcomeV1 {
    Ready(ContextScoutRecentStateV1),
    Unavailable,
}

pub fn validate_context_scout_feedback(
    receipt: &ContextScoutDeliveryReceiptV1,
    feedback: ContextScoutFeedbackV1,
) -> Result<(), ContextScoutErrorV1> {
    validate_receipt_shape(receipt)?;
    if feedback.receipt_id != receipt.receipt_id {
        return Err(ContextScoutErrorV1::ReceiptBindingMismatch);
    }
    Ok(())
}

fn validate_receipt_shape(
    receipt: &ContextScoutDeliveryReceiptV1,
) -> Result<(), ContextScoutErrorV1> {
    if receipt.receipt_id == [0; 16]
        || receipt.envelope_id == [0; 16]
        || receipt.delivered_at.0 <= 0
    {
        return Err(ContextScoutErrorV1::ReceiptBindingMismatch);
    }
    Ok(())
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

/// One exact durable queue entry. The queue records the work-generation token
/// next to the envelope, so a replay cannot turn a superseded model result
/// into a current delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// Result of one daemon/store serialized Scout mutation. `Duplicate` and
/// `Superseded` are convergent outcomes, while `Unavailable` commits nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextScoutDurableStoreOutcomeV1 {
    Stored,
    Duplicate,
    Superseded,
    Unavailable,
}

/// Caller-supplied lease identity and deadline. The store owns no timing
/// policy; it only applies the exact lease and compares its absolute expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextScoutLeaseV1 {
    pub lease_id: [u8; 16],
    pub expires_at: UtcMicros,
}

impl ContextScoutLeaseV1 {
    fn validate(self, now: UtcMicros) -> Result<(), ContextScoutErrorV1> {
        if self.lease_id == [0; 16] || now.0 <= 0 || self.expires_at.0 <= now.0 {
            return Err(ContextScoutErrorV1::StaleWork);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextScoutDurableClaimV1 {
    pub entry: ContextScoutDurableQueueEntryV1,
    pub lease: ContextScoutLeaseV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
// Boxing the large variant would ripple through in-flight construction/match
// sites; the size gap is accepted here.
#[allow(clippy::large_enum_variant)]
pub enum ContextScoutDurableClaimOutcomeV1 {
    Claimed(ContextScoutDurableClaimV1),
    Empty,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextScoutDurableStartupOutcomeV1 {
    Ready {
        entries: Vec<ContextScoutDurableQueueEntryV1>,
        truncated: bool,
    },
    Unavailable,
}

pub type ContextScoutStoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The daemon owns the physical queue, receipt, feedback, lease,
/// retry, and transaction boundaries behind this contract. The methods are
/// intentionally exact-addressed and contain no query, model, host payload,
/// filesystem path, or generic storage operation.
pub trait ContextScoutDurableStoreV1: Send + Sync {
    /// Requeues expired claims and returns at most `limit` unclaimed entries.
    fn startup(
        &self,
        now: UtcMicros,
        limit: usize,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStartupOutcomeV1>;

    /// Atomically commits one queue entry. The envelope already carries its
    /// durable evidence anchors; no duplicate checkpoint record is written.
    fn enqueue(
        &self,
        entry: ContextScoutDurableQueueEntryV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1>;

    /// Claims one exact address with a caller-owned lease.
    fn claim(
        &self,
        address: ContextScoutAddressV1,
        now: UtcMicros,
        lease: ContextScoutLeaseV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableClaimOutcomeV1>;

    /// Clears only the exact claim represented by `claimed`.
    fn requeue(
        &self,
        claimed: ContextScoutDurableClaimV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1>;

    /// Cancels exactly one currently queued work generation.
    fn cancel_work(
        &self,
        work: ContextScoutWorkV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1>;

    /// Atomically records the delivery receipt for this exact claimed queue
    /// entry. The lease is part of the write authority; an entry alone can
    /// never complete delivery after requeue or takeover.
    fn record_delivery<'a>(
        &'a self,
        claim: &'a ContextScoutDurableClaimV1,
        receipt: &'a ContextScoutDeliveryReceiptV1,
    ) -> ContextScoutStoreFuture<'a, ContextScoutDurableStoreOutcomeV1>;

    /// Records explicit feedback only after the receipt binding has survived
    /// the caller-side validation below.
    fn record_feedback<'a>(
        &'a self,
        receipt: &'a ContextScoutDeliveryReceiptV1,
        feedback: ContextScoutFeedbackV1,
    ) -> ContextScoutStoreFuture<'a, ContextScoutDurableStoreOutcomeV1>;
}

impl<T> ContextScoutDurableStoreV1 for Arc<T>
where
    T: ContextScoutDurableStoreV1 + ?Sized,
{
    fn startup(
        &self,
        now: UtcMicros,
        limit: usize,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStartupOutcomeV1> {
        (**self).startup(now, limit)
    }

    fn enqueue(
        &self,
        entry: ContextScoutDurableQueueEntryV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1> {
        (**self).enqueue(entry)
    }

    fn claim(
        &self,
        address: ContextScoutAddressV1,
        now: UtcMicros,
        lease: ContextScoutLeaseV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableClaimOutcomeV1> {
        (**self).claim(address, now, lease)
    }

    fn requeue(
        &self,
        claimed: ContextScoutDurableClaimV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1> {
        (**self).requeue(claimed)
    }

    fn cancel_work(
        &self,
        work: ContextScoutWorkV1,
    ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1> {
        (**self).cancel_work(work)
    }

    fn record_delivery<'a>(
        &'a self,
        claim: &'a ContextScoutDurableClaimV1,
        receipt: &'a ContextScoutDeliveryReceiptV1,
    ) -> ContextScoutStoreFuture<'a, ContextScoutDurableStoreOutcomeV1> {
        (**self).record_delivery(claim, receipt)
    }

    fn record_feedback<'a>(
        &'a self,
        receipt: &'a ContextScoutDeliveryReceiptV1,
        feedback: ContextScoutFeedbackV1,
    ) -> ContextScoutStoreFuture<'a, ContextScoutDurableStoreOutcomeV1> {
        (**self).record_feedback(receipt, feedback)
    }
}

/// Selected only by the daemon's typed configuration. There is no built-in
/// model name, route, retry, or timing default in this host-facing runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutRuntimeModeV1 {
    Deterministic,
    ConfiguredModel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutServiceStateV1 {
    Active,
    Paused,
    Disabled,
}

/// Typed configuration selected by the daemon configuration authority. The
/// Scout can report and obey this state, but cannot persist or change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextScoutControlV1 {
    pub configuration_revision: [u8; 32],
    pub state: ContextScoutServiceStateV1,
    pub mode: ContextScoutRuntimeModeV1,
    pub model_path: Option<ContextScoutModelBackendV1>,
    pub limits: ContextScoutLimitsV1,
}

impl ContextScoutControlV1 {
    fn validate(self) -> Result<(), ContextScoutErrorV1> {
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

/// Read-only status for host/dashboard projection. Internal Scout work is
/// suggestion coalescing only and never creates a task or work-graph node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutExplanationV1 {
    pub status: ContextScoutStatusV1,
    pub recent: ContextScoutRecentStateV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutCapabilityStateV1 {
    pub state: ContextScoutServiceStateV1,
    pub mode: ContextScoutRuntimeModeV1,
    pub deterministic_available: bool,
    pub configured_model: Option<ContextScoutModelBackendV1>,
    pub configured_model_available: bool,
    pub last_model_outcome: Option<ContextScoutModelRunOutcomeV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutBudgetStateV1 {
    pub limits: ContextScoutLimitsV1,
    pub last_model_outcome: Option<ContextScoutModelRunOutcomeV1>,
    pub exhausted: bool,
    pub last_input_tokens: Option<u64>,
    pub last_output_tokens: Option<u64>,
    pub last_estimated_cost_microusd: Option<u64>,
}

/// Result of preparing one suggestion. Silence is a successful, normal output
/// and does not create a durable queue entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextScoutRuntimeOutcomeV1 {
    Enqueued {
        entry: Box<ContextScoutDurableQueueEntryV1>,
        store_outcome: ContextScoutDurableStoreOutcomeV1,
    },
    Suppressed {
        reason: ContextScoutSuppressionV1,
    },
    Unavailable,
}

/// Concrete deterministic/model-assisted runtime over injected daemon queue
/// authority. It owns only process-local supersession tokens; every durable
/// mutation is delegated to `store` and can therefore survive a restart.
pub struct ContextScoutDurableRuntimeV1<S, M> {
    store: S,
    model: M,
    coalescer: ContextScoutCoalescerV1,
    last_route: Option<ContextScoutRouteV1>,
    last_suppression: Option<ContextScoutSuppressionV1>,
    last_model_outcome: Option<ContextScoutModelRunOutcomeV1>,
    last_model_receipt: Option<ContextScoutModelReceiptV1>,
    last_delivery_outcome: Option<ContextScoutOutcomeV1>,
    last_feedback: Option<ContextScoutFeedbackKindV1>,
}

impl<S, M> ContextScoutDurableRuntimeV1<S, M> {
    pub fn new(store: S, model: M) -> Self {
        Self {
            store,
            model,
            coalescer: ContextScoutCoalescerV1::default(),
            last_route: None,
            last_suppression: None,
            last_model_outcome: None,
            last_model_receipt: None,
            last_delivery_outcome: None,
            last_feedback: None,
        }
    }

    pub fn is_current(&self, work: ContextScoutWorkV1) -> bool {
        self.coalescer.is_current(work)
    }

    pub(crate) fn restore_startup(
        &mut self,
        startup: &ContextScoutDurableStartupOutcomeV1,
    ) -> Result<(), ContextScoutErrorV1> {
        let ContextScoutDurableStartupOutcomeV1::Ready { entries, truncated } = startup else {
            return Err(ContextScoutErrorV1::ConfigurationUnavailable);
        };
        if *truncated {
            return Err(ContextScoutErrorV1::CapacityExceeded);
        }
        for entry in entries {
            entry.validate()?;
            self.coalescer.restore(entry.work)?;
        }
        Ok(())
    }

    pub(crate) fn replace_model(&mut self, model: M) {
        self.model = model;
    }

    pub fn status(
        &self,
        control: ContextScoutControlV1,
    ) -> Result<ContextScoutStatusV1, ContextScoutErrorV1> {
        control.validate()?;
        Ok(ContextScoutStatusV1 {
            configuration_revision: control.configuration_revision,
            state: control.state,
            mode: control.mode,
            model_path: control.model_path,
            limits: control.limits,
            active_suggestions: self.coalescer.active.len(),
            last_route: self.last_route,
            last_suppression: self.last_suppression,
            last_model_outcome: self.last_model_outcome,
            last_model_receipt: self.last_model_receipt.clone(),
            last_delivery_outcome: self.last_delivery_outcome,
            last_feedback: self.last_feedback,
        })
    }
}

impl<S, M> ContextScoutDurableRuntimeV1<S, M>
where
    S: ContextScoutDurableStoreV1,
    M: ContextScoutModelAssistantV1,
{
    /// Obey daemon-owned enable/pause/model configuration before creating
    /// process-local work or invoking the configured model adapter.
    pub(crate) async fn prepare_controlled(
        &mut self,
        input: &ContextScoutSelectionInputV1,
        control: ContextScoutControlV1,
        model_execution: ContextScoutModelExecutionV1,
    ) -> Result<ContextScoutRuntimeOutcomeV1, ContextScoutErrorV1> {
        control.validate()?;
        if input.configuration_revision != control.configuration_revision {
            return Err(ContextScoutErrorV1::InvalidCandidate);
        }
        match control.state {
            ContextScoutServiceStateV1::Paused => {
                self.last_suppression = Some(ContextScoutSuppressionV1::Paused);
                Ok(ContextScoutRuntimeOutcomeV1::Suppressed {
                    reason: ContextScoutSuppressionV1::Paused,
                })
            }
            ContextScoutServiceStateV1::Disabled => {
                self.last_suppression = Some(ContextScoutSuppressionV1::Disabled);
                Ok(ContextScoutRuntimeOutcomeV1::Suppressed {
                    reason: ContextScoutSuppressionV1::Disabled,
                })
            }
            ContextScoutServiceStateV1::Active => {
                if control.mode == ContextScoutRuntimeModeV1::ConfiguredModel
                    && !model_execution.matches_limits(control.limits)
                {
                    return Err(ContextScoutErrorV1::InvalidLimits);
                }
                self.prepare(input, control.limits, control.mode, model_execution)
                    .await
            }
        }
    }

    /// Select, bind, and durably enqueue at most one suggestion. Privacy and
    /// dirty-overlay suppression run before model invocation or persistence.
    async fn prepare(
        &mut self,
        input: &ContextScoutSelectionInputV1,
        limits: ContextScoutLimitsV1,
        mode: ContextScoutRuntimeModeV1,
        model_execution: ContextScoutModelExecutionV1,
    ) -> Result<ContextScoutRuntimeOutcomeV1, ContextScoutErrorV1> {
        let selection = match mode {
            ContextScoutRuntimeModeV1::Deterministic => ContextScoutSelectionV1 {
                route: ContextScoutRouteV1::Deterministic,
                decision: select_deterministic_context_scout(input, limits)?,
                model_outcome: ContextScoutModelRunOutcomeV1::NotRequested,
                model_receipt: None,
            },
            ContextScoutRuntimeModeV1::ConfiguredModel => {
                select_model_assisted_context_scout(input, limits, &self.model, model_execution)
                    .await?
            }
        };
        self.last_route = Some(selection.route);
        self.last_model_outcome = Some(selection.model_outcome);
        self.last_model_receipt.clone_from(&selection.model_receipt);
        if selection.model_outcome == ContextScoutModelRunOutcomeV1::Cancelled {
            self.last_suppression = Some(ContextScoutSuppressionV1::Cancelled);
            return Ok(ContextScoutRuntimeOutcomeV1::Suppressed {
                reason: ContextScoutSuppressionV1::Cancelled,
            });
        }
        let model_receipt = selection.model_receipt.clone();
        let envelope = match selection.decision {
            ContextScoutDecisionV1::Ready { envelope }
            | ContextScoutDecisionV1::Delayed { envelope } => envelope,
            ContextScoutDecisionV1::Suppressed { reason } => {
                self.last_suppression = Some(reason);
                return Ok(ContextScoutRuntimeOutcomeV1::Suppressed { reason });
            }
        };
        let work = self
            .coalescer
            .enqueue(input.address, input.input_watermark)?;
        if !self.coalescer.is_current(work) {
            return Err(ContextScoutErrorV1::StaleWork);
        }
        let entry = ContextScoutDurableQueueEntryV1 {
            work,
            route: selection.route,
            model_outcome: selection.model_outcome,
            model_receipt,
            envelope,
        };
        entry.validate()?;
        let store_outcome = self.store.enqueue(entry.clone()).await;
        if store_outcome == ContextScoutDurableStoreOutcomeV1::Unavailable {
            let _ = self.coalescer.cancel(work);
            return Ok(ContextScoutRuntimeOutcomeV1::Unavailable);
        }
        self.last_suppression = None;
        Ok(ContextScoutRuntimeOutcomeV1::Enqueued {
            entry: Box::new(entry),
            store_outcome,
        })
    }

    /// Cancels a work generation locally first, preventing a model result from
    /// racing a durable write after the daemon's cancellation signal.
    pub async fn cancel(
        &mut self,
        work: ContextScoutWorkV1,
    ) -> Result<ContextScoutDurableStoreOutcomeV1, ContextScoutErrorV1> {
        self.coalescer.cancel(work)?;
        self.last_suppression = Some(ContextScoutSuppressionV1::Cancelled);
        Ok(self.store.cancel_work(work).await)
    }

    /// Validate receipt identity before delegating an atomic durable completion.
    pub async fn complete_delivery(
        &mut self,
        claim: &ContextScoutDurableClaimV1,
        receipt: &ContextScoutDeliveryReceiptV1,
    ) -> Result<ContextScoutDurableStoreOutcomeV1, ContextScoutErrorV1> {
        claim.entry.validate()?;
        claim.lease.validate(receipt.delivered_at)?;
        validate_context_scout_delivery_receipt(&claim.entry.envelope, receipt)?;
        let outcome = self.store.record_delivery(claim, receipt).await;
        if matches!(
            outcome,
            ContextScoutDurableStoreOutcomeV1::Stored
                | ContextScoutDurableStoreOutcomeV1::Duplicate
        ) && self.coalescer.is_current(claim.entry.work)
        {
            self.coalescer.cancel(claim.entry.work)?;
            self.last_delivery_outcome = Some(receipt.outcome);
        }
        Ok(outcome)
    }

    /// Feedback remains explicit and bound to the receipt; adjacency never
    /// becomes acceptance or correction.
    pub async fn record_feedback(
        &mut self,
        receipt: &ContextScoutDeliveryReceiptV1,
        feedback: ContextScoutFeedbackV1,
    ) -> Result<ContextScoutDurableStoreOutcomeV1, ContextScoutErrorV1> {
        validate_context_scout_feedback(receipt, feedback)?;
        let outcome = self.store.record_feedback(receipt, feedback).await;
        if matches!(
            outcome,
            ContextScoutDurableStoreOutcomeV1::Stored
                | ContextScoutDurableStoreOutcomeV1::Duplicate
        ) {
            self.last_feedback = Some(feedback.kind);
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn address() -> ContextScoutAddressV1 {
        ContextScoutAddressV1 {
            profile_id: [1; 16],
            provider_id: [2; 16],
            protected_session_id: [3; 32],
            thread_id: [4; 16],
            turn_id: [5; 16],
            agent_id: [6; 16],
            logical_message_id: [7; 16],
            project_id: [8; 16],
        }
    }

    #[test]
    fn delivery_selector_uses_trigger_quiet_recent_and_unresolved_state() {
        let base = ContextScoutDeliverySelectionInputV1 {
            trigger: ContextScoutTriggerV1::StopBoundary,
            quiet_mode: false,
            has_recent_delivery: false,
            has_unresolved_interaction: false,
            critical_safety_evidence: false,
            delivered_dedupe_keys: BTreeSet::new(),
        };
        assert_eq!(
            select_context_scout_delivery_window(&base),
            ContextScoutDeliveryWindowV1::Immediate
        );
        assert_eq!(
            select_context_scout_delivery_window(&ContextScoutDeliverySelectionInputV1 {
                quiet_mode: true,
                ..base.clone()
            }),
            ContextScoutDeliveryWindowV1::Suppressed
        );
        assert_eq!(
            select_context_scout_delivery_window(&ContextScoutDeliverySelectionInputV1 {
                has_recent_delivery: true,
                ..base.clone()
            }),
            ContextScoutDeliveryWindowV1::IdleWindow
        );
        assert_eq!(
            select_context_scout_delivery_window(&ContextScoutDeliverySelectionInputV1 {
                has_unresolved_interaction: true,
                ..base.clone()
            }),
            ContextScoutDeliveryWindowV1::NextBoundary
        );
        assert_eq!(
            select_context_scout_delivery_window(&ContextScoutDeliverySelectionInputV1 {
                trigger: ContextScoutTriggerV1::ExplicitRequest,
                quiet_mode: true,
                has_recent_delivery: true,
                has_unresolved_interaction: true,
                ..base.clone()
            }),
            ContextScoutDeliveryWindowV1::OnRequest
        );
        assert_eq!(
            select_context_scout_delivery_window(&ContextScoutDeliverySelectionInputV1 {
                quiet_mode: true,
                critical_safety_evidence: true,
                ..base
            }),
            ContextScoutDeliveryWindowV1::Immediate
        );
    }

    fn evidence(generation: ContextScoutEvidenceGenerationV1) -> ContextScoutEvidenceBindingV1 {
        ContextScoutEvidenceBindingV1 {
            anchor_id: [10; 16],
            content_identity: [11; 32],
            generation,
        }
    }

    fn candidate(key: u8, score: u16) -> ContextScoutCandidateV1 {
        ContextScoutCandidateV1 {
            dedupe_key: [key; 32],
            category: ContextScoutCategoryV1::Retrieval,
            relevance_score: score,
            suggestion_text: format!("Use evidence {key}."),
            evidence: vec![evidence(ContextScoutEvidenceGenerationV1::SavedContent)],
            expires_at: UtcMicros(100),
        }
    }

    fn input(candidates: Vec<ContextScoutCandidateV1>) -> ContextScoutSelectionInputV1 {
        ContextScoutSelectionInputV1 {
            address: address(),
            input_watermark: [14; 32],
            configuration_revision: [16; 32],
            envelope_id: [17; 16],
            now: UtcMicros(10),
            delivery_window: ContextScoutDeliveryWindowV1::Immediate,
            delivered_dedupe_keys: BTreeSet::new(),
            candidates,
        }
    }

    fn work(input_watermark: [u8; 32], generation: u64) -> ContextScoutWorkV1 {
        ContextScoutWorkV1 {
            address: address(),
            generation,
            input_watermark,
        }
    }

    fn model_execution() -> ContextScoutModelExecutionV1 {
        ContextScoutModelExecutionV1::new(
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            CancellationToken::new(),
            ContextScoutLimitsV1::bounded_defaults(),
        )
        .unwrap()
    }

    fn model_receipt() -> ContextScoutModelReceiptV1 {
        ContextScoutModelReceiptV1 {
            requested_backend: ContextScoutModelBackendV1::CodexAppServer,
            actual_model: Some("configured-test-model".to_owned()),
            input_tokens: Some(10),
            output_tokens: Some(5),
            estimated_cost_microusd: Some(1),
        }
    }

    #[test]
    fn successful_model_receipt_requires_actual_model_identity() {
        let mut receipt = model_receipt();
        receipt.actual_model = None;
        assert!(!valid_model_receipt(
            &receipt,
            ContextScoutModelBackendV1::CodexAppServer,
            MAX_SCOUT_MODEL_INPUT_TOKENS,
            MAX_SCOUT_MODEL_OUTPUT_TOKENS,
        ));
    }

    #[test]
    fn successful_model_receipt_requires_usage_and_cost_evidence() {
        for receipt in [
            ContextScoutModelReceiptV1 {
                input_tokens: None,
                ..model_receipt()
            },
            ContextScoutModelReceiptV1 {
                output_tokens: None,
                ..model_receipt()
            },
            ContextScoutModelReceiptV1 {
                estimated_cost_microusd: None,
                ..model_receipt()
            },
        ] {
            assert!(!valid_model_receipt(
                &receipt,
                ContextScoutModelBackendV1::CodexAppServer,
                MAX_SCOUT_MODEL_INPUT_TOKENS,
                MAX_SCOUT_MODEL_OUTPUT_TOKENS,
            ));
        }
    }

    #[test]
    fn deterministic_selection_is_stable_and_single_channel_coalesces() {
        let decision = select_deterministic_context_scout(
            &input(vec![candidate(2, 50), candidate(1, 50)]),
            ContextScoutLimitsV1::bounded_defaults(),
        )
        .unwrap();
        let ContextScoutDecisionV1::Ready { envelope } = decision.clone() else {
            panic!("candidate should be ready");
        };
        assert_eq!(envelope.candidate.dedupe_key, [1; 32]);
        let mut channel = ContextScoutSuggestionChannelV1::default();
        let work = work(envelope.input_watermark, 1);
        assert_eq!(
            channel.offer(work, decision.clone()).unwrap(),
            ContextScoutChannelOutcomeV1::Offered
        );
        assert_eq!(
            channel.offer(work, decision).unwrap(),
            ContextScoutChannelOutcomeV1::Coalesced
        );
        {
            let claim = channel.claim(address()).unwrap();
            assert_eq!(claim.envelope().envelope_id, [17; 16]);
        }
        assert!(channel.claim(address()).is_some());
    }

    #[test]
    fn deterministic_selection_reuses_durable_dedupe_state() {
        let mut selection = input(vec![candidate(1, 50), candidate(2, 40)]);
        selection.delivered_dedupe_keys.insert([1; 32]);
        let ContextScoutDecisionV1::Ready { envelope } = select_deterministic_context_scout(
            &selection,
            ContextScoutLimitsV1::bounded_defaults(),
        )
        .unwrap() else {
            panic!("the next unseen candidate should be ready");
        };
        assert_eq!(envelope.candidate.dedupe_key, [2; 32]);

        selection.delivered_dedupe_keys.insert([2; 32]);
        assert_eq!(
            select_deterministic_context_scout(
                &selection,
                ContextScoutLimitsV1::bounded_defaults()
            )
            .unwrap(),
            ContextScoutDecisionV1::Suppressed {
                reason: ContextScoutSuppressionV1::Duplicate
            }
        );
    }

    #[test]
    fn selection_and_delivery_fail_closed_on_stale_time() {
        let mut stale = input(vec![candidate(1, 10)]);
        stale.now = UtcMicros(0);
        assert_eq!(
            select_deterministic_context_scout(&stale, ContextScoutLimitsV1::bounded_defaults()),
            Err(ContextScoutErrorV1::InvalidCandidate)
        );

        let ContextScoutDecisionV1::Ready { envelope } = select_deterministic_context_scout(
            &input(vec![candidate(1, 10)]),
            ContextScoutLimitsV1::bounded_defaults(),
        )
        .unwrap() else {
            panic!("candidate should be ready");
        };
        let receipt = ContextScoutDeliveryReceiptV1 {
            receipt_id: [18; 16],
            envelope_id: envelope.envelope_id,
            delivered_at: envelope.candidate.expires_at,
            outcome: ContextScoutOutcomeV1::Displayed,
        };
        assert_eq!(
            validate_context_scout_delivery_receipt(&envelope, &receipt),
            Err(ContextScoutErrorV1::ReceiptBindingMismatch)
        );
        validate_context_scout_delivery_receipt(
            &envelope,
            &ContextScoutDeliveryReceiptV1 {
                outcome: ContextScoutOutcomeV1::ExpiredUnseen,
                ..receipt
            },
        )
        .unwrap();
    }

    #[test]
    fn dirty_overlay_never_crosses_durable_checkpoint_or_receipt_boundary() {
        let mut overlay = candidate(1, 10);
        overlay.evidence[0].generation = ContextScoutEvidenceGenerationV1::DirtyOverlay;
        let decision = select_deterministic_context_scout(
            &input(vec![overlay]),
            ContextScoutLimitsV1::bounded_defaults(),
        )
        .unwrap();
        assert_eq!(
            decision,
            ContextScoutDecisionV1::Suppressed {
                reason: ContextScoutSuppressionV1::DirtyOverlay
            }
        );
    }

    #[test]
    fn control_text_fails_closed() {
        let mut unsafe_candidate = candidate(1, 10);
        unsafe_candidate.suggestion_text = "render\u{1b}[31m".to_owned();
        assert_eq!(
            select_deterministic_context_scout(
                &input(vec![unsafe_candidate]),
                ContextScoutLimitsV1::bounded_defaults()
            ),
            Err(ContextScoutErrorV1::InvalidCandidate)
        );
    }

    struct BadModel;

    impl ContextScoutModelAssistantV1 for BadModel {
        fn backend(&self) -> ContextScoutModelBackendV1 {
            ContextScoutModelBackendV1::CodexAppServer
        }

        fn propose(
            &self,
            _request: ContextScoutModelRequestV1,
            _execution: ContextScoutModelExecutionV1,
        ) -> ContextScoutModelFuture<'_> {
            Box::pin(async {
                Ok(ContextScoutModelProposalV1 {
                    candidate: ContextScoutModelCandidateV1 {
                        selected_dedupe_key: [1; 32],
                        suggestion_text: "unbound model claim".to_string(),
                        cited_anchor_ids: vec![[99; 16]],
                    },
                    receipt: model_receipt(),
                })
            })
        }
    }

    struct MismatchedReceiptModel;

    impl ContextScoutModelAssistantV1 for MismatchedReceiptModel {
        fn backend(&self) -> ContextScoutModelBackendV1 {
            ContextScoutModelBackendV1::CodexAppServer
        }

        fn propose(
            &self,
            request: ContextScoutModelRequestV1,
            _execution: ContextScoutModelExecutionV1,
        ) -> ContextScoutModelFuture<'_> {
            Box::pin(async move {
                let candidate = request
                    .candidates
                    .first()
                    .ok_or(ContextScoutModelErrorV1::InvalidOutput)?;
                Ok(ContextScoutModelProposalV1 {
                    candidate: ContextScoutModelCandidateV1 {
                        selected_dedupe_key: candidate.dedupe_key,
                        suggestion_text: candidate.suggestion_text.clone(),
                        cited_anchor_ids: candidate.citation_anchor_ids.clone(),
                    },
                    receipt: ContextScoutModelReceiptV1 {
                        requested_backend: ContextScoutModelBackendV1::Unsupported,
                        ..model_receipt()
                    },
                })
            })
        }
    }

    struct CancellationIgnoringModel;

    impl ContextScoutModelAssistantV1 for CancellationIgnoringModel {
        fn backend(&self) -> ContextScoutModelBackendV1 {
            ContextScoutModelBackendV1::CodexAppServer
        }

        fn propose(
            &self,
            request: ContextScoutModelRequestV1,
            execution: ContextScoutModelExecutionV1,
        ) -> ContextScoutModelFuture<'_> {
            Box::pin(async move {
                execution.cancellation.cancel();
                let candidate = request
                    .candidates
                    .first()
                    .ok_or(ContextScoutModelErrorV1::InvalidOutput)?;
                Ok(ContextScoutModelProposalV1 {
                    candidate: ContextScoutModelCandidateV1 {
                        selected_dedupe_key: candidate.dedupe_key,
                        suggestion_text: candidate.suggestion_text.clone(),
                        cited_anchor_ids: candidate.citation_anchor_ids.clone(),
                    },
                    receipt: model_receipt(),
                })
            })
        }
    }

    struct DuplicateCitationModel;

    impl ContextScoutModelAssistantV1 for DuplicateCitationModel {
        fn backend(&self) -> ContextScoutModelBackendV1 {
            ContextScoutModelBackendV1::CodexAppServer
        }

        fn propose(
            &self,
            _request: ContextScoutModelRequestV1,
            _execution: ContextScoutModelExecutionV1,
        ) -> ContextScoutModelFuture<'_> {
            Box::pin(async {
                Ok(ContextScoutModelProposalV1 {
                    candidate: ContextScoutModelCandidateV1 {
                        selected_dedupe_key: [1; 32],
                        suggestion_text: "bounded refinement".to_owned(),
                        cited_anchor_ids: vec![[10; 16], [10; 16]],
                    },
                    receipt: model_receipt(),
                })
            })
        }
    }

    struct FailingModel(ContextScoutModelErrorV1);

    impl ContextScoutModelAssistantV1 for FailingModel {
        fn backend(&self) -> ContextScoutModelBackendV1 {
            ContextScoutModelBackendV1::CodexAppServer
        }

        fn propose(
            &self,
            _request: ContextScoutModelRequestV1,
            _execution: ContextScoutModelExecutionV1,
        ) -> ContextScoutModelFuture<'_> {
            let error = self.0;
            Box::pin(async move { Err(error) })
        }
    }

    #[tokio::test]
    async fn malformed_model_output_falls_back_to_evidence_bound_determinism() {
        let selection = select_model_assisted_context_scout(
            &input(vec![candidate(1, 10)]),
            ContextScoutLimitsV1::bounded_defaults(),
            &BadModel,
            model_execution(),
        )
        .await
        .unwrap();
        assert_eq!(selection.route, ContextScoutRouteV1::DeterministicFallback);
        assert_eq!(
            selection.model_outcome,
            ContextScoutModelRunOutcomeV1::InvalidOutput
        );
        assert!(matches!(
            selection.decision,
            ContextScoutDecisionV1::Ready { .. }
        ));
    }

    #[tokio::test]
    async fn model_failures_are_typed_while_deterministic_fallback_survives() {
        for (error, expected) in [
            (
                ContextScoutModelErrorV1::Disabled,
                ContextScoutModelRunOutcomeV1::Disabled,
            ),
            (
                ContextScoutModelErrorV1::Unavailable,
                ContextScoutModelRunOutcomeV1::Unavailable,
            ),
            (
                ContextScoutModelErrorV1::DeadlineExceeded,
                ContextScoutModelRunOutcomeV1::DeadlineExceeded,
            ),
            (
                ContextScoutModelErrorV1::TokenBudgetExceeded,
                ContextScoutModelRunOutcomeV1::TokenBudgetExceeded,
            ),
        ] {
            let selection = select_model_assisted_context_scout(
                &input(vec![candidate(1, 10)]),
                ContextScoutLimitsV1::bounded_defaults(),
                &FailingModel(error),
                model_execution(),
            )
            .await
            .unwrap();

            assert_eq!(selection.route, ContextScoutRouteV1::DeterministicFallback);
            assert_eq!(selection.model_outcome, expected);
            assert!(matches!(
                selection.decision,
                ContextScoutDecisionV1::Ready { .. }
            ));
        }
    }

    #[tokio::test]
    async fn mismatched_model_receipt_is_typed_invalid_output() {
        let selection = select_model_assisted_context_scout(
            &input(vec![candidate(1, 10)]),
            ContextScoutLimitsV1::bounded_defaults(),
            &MismatchedReceiptModel,
            model_execution(),
        )
        .await
        .unwrap();

        assert_eq!(selection.route, ContextScoutRouteV1::DeterministicFallback);
        assert_eq!(
            selection.model_outcome,
            ContextScoutModelRunOutcomeV1::InvalidOutput
        );
        assert!(selection.model_receipt.is_none());
    }

    #[tokio::test]
    async fn model_adapter_cannot_ignore_cancellation_before_durable_selection() {
        let selection = select_model_assisted_context_scout(
            &input(vec![candidate(1, 10)]),
            ContextScoutLimitsV1::bounded_defaults(),
            &CancellationIgnoringModel,
            model_execution(),
        )
        .await
        .unwrap();

        assert_eq!(selection.route, ContextScoutRouteV1::DeterministicFallback);
        assert_eq!(
            selection.model_outcome,
            ContextScoutModelRunOutcomeV1::Cancelled
        );
        assert!(selection.model_receipt.is_none());
    }

    #[tokio::test]
    async fn pre_cancelled_execution_never_calls_the_model_assistant() {
        let calls = Arc::new(AtomicUsize::new(0));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let execution = ContextScoutModelExecutionV1::new(
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            cancellation,
            ContextScoutLimitsV1::bounded_defaults(),
        )
        .unwrap();
        let selection = select_model_assisted_context_scout(
            &input(vec![candidate(1, 10)]),
            ContextScoutLimitsV1::bounded_defaults(),
            &RecordingModel {
                calls: Arc::clone(&calls),
            },
            execution,
        )
        .await
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            selection.model_outcome,
            ContextScoutModelRunOutcomeV1::Cancelled
        );
        assert!(selection.model_receipt.is_none());
    }

    #[tokio::test]
    async fn duplicate_candidate_keys_and_duplicate_model_citations_fail_closed() {
        let duplicate = input(vec![candidate(1, 10), candidate(1, 9)]);
        assert_eq!(
            select_deterministic_context_scout(
                &duplicate,
                ContextScoutLimitsV1::bounded_defaults()
            ),
            Err(ContextScoutErrorV1::InvalidCandidate)
        );

        let selection = select_model_assisted_context_scout(
            &input(vec![candidate(1, 10)]),
            ContextScoutLimitsV1::bounded_defaults(),
            &DuplicateCitationModel,
            model_execution(),
        )
        .await
        .unwrap();
        assert_eq!(selection.route, ContextScoutRouteV1::DeterministicFallback);
    }

    #[tokio::test]
    async fn model_schema_rejects_unknown_fields_and_input_budget_falls_back() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "selected_dedupe_key": vec![1_u8; 32],
            "suggestion_text": "bounded",
            "cited_anchor_ids": [vec![10_u8; 16]],
            "tool": "forbidden"
        }))
        .unwrap();
        assert_eq!(
            decode_context_scout_model_candidate(&bytes),
            Err(ContextScoutModelErrorV1::InvalidOutput)
        );

        let mut limits = ContextScoutLimitsV1::bounded_defaults();
        limits.max_model_input_tokens = 1;
        let execution = ContextScoutModelExecutionV1::new(
            MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            CancellationToken::new(),
            limits,
        )
        .unwrap();
        let selection = select_model_assisted_context_scout(
            &input(vec![candidate(1, 10)]),
            limits,
            &BadModel,
            execution,
        )
        .await
        .unwrap();
        assert_eq!(selection.route, ContextScoutRouteV1::DeterministicFallback);
    }

    #[test]
    fn saved_content_delivery_and_feedback_remain_exact() {
        let ContextScoutDecisionV1::Ready { envelope } = select_deterministic_context_scout(
            &input(vec![candidate(1, 10)]),
            ContextScoutLimitsV1::bounded_defaults(),
        )
        .unwrap() else {
            panic!("candidate should be ready");
        };
        let receipt = ContextScoutDeliveryReceiptV1 {
            receipt_id: [18; 16],
            envelope_id: envelope.envelope_id,
            delivered_at: UtcMicros(11),
            outcome: ContextScoutOutcomeV1::Displayed,
        };
        validate_context_scout_delivery_receipt(&envelope, &receipt).unwrap();
        validate_context_scout_feedback(
            &receipt,
            ContextScoutFeedbackV1 {
                receipt_id: receipt.receipt_id,
                kind: ContextScoutFeedbackKindV1::ExplicitlyAccepted,
            },
        )
        .unwrap();

        let mut channel = ContextScoutSuggestionChannelV1::default();
        channel
            .offer(
                work(envelope.input_watermark, 1),
                ContextScoutDecisionV1::Ready {
                    envelope: envelope.clone(),
                },
            )
            .unwrap();
        channel
            .claim(envelope.address)
            .unwrap()
            .complete(&receipt)
            .unwrap();
        assert!(channel.claim(envelope.address).is_none());
    }

    #[test]
    fn coalescing_and_cancellation_require_the_exact_work_generation() {
        let mut coalescer = ContextScoutCoalescerV1::default();
        let first = coalescer.enqueue(address(), [20; 32]).unwrap();
        let second = coalescer.enqueue(address(), [22; 32]).unwrap();
        assert!(!coalescer.is_current(first));
        assert_eq!(coalescer.cancel(first), Err(ContextScoutErrorV1::StaleWork));
        coalescer.cancel(second).unwrap();
    }

    #[test]
    fn channel_cancellation_rejects_a_superseded_generation_with_same_watermark() {
        let ContextScoutDecisionV1::Ready { envelope } = select_deterministic_context_scout(
            &input(vec![candidate(1, 10)]),
            ContextScoutLimitsV1::bounded_defaults(),
        )
        .unwrap() else {
            panic!("candidate should be ready");
        };
        let first = work(envelope.input_watermark, 1);
        let second = work(envelope.input_watermark, 2);
        let mut channel = ContextScoutSuggestionChannelV1::default();
        channel
            .offer(
                second,
                ContextScoutDecisionV1::Ready {
                    envelope: envelope.clone(),
                },
            )
            .unwrap();
        assert_eq!(
            channel.offer(first, ContextScoutDecisionV1::Ready { envelope }),
            Err(ContextScoutErrorV1::StaleWork)
        );
        assert_eq!(channel.cancel(first), Err(ContextScoutErrorV1::StaleWork));
        channel.cancel(second).unwrap();
    }

    #[derive(Default)]
    struct DurableStoreState {
        entries: BTreeMap<[u8; 16], ContextScoutDurableQueueEntryV1>,
        cancellations: Vec<ContextScoutWorkV1>,
        receipts: Vec<ContextScoutDeliveryReceiptV1>,
        feedback: Vec<ContextScoutFeedbackV1>,
    }

    #[derive(Clone, Default)]
    struct DurableStore(Arc<Mutex<DurableStoreState>>);

    impl ContextScoutDurableStoreV1 for DurableStore {
        fn startup(
            &self,
            _now: UtcMicros,
            _limit: usize,
        ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStartupOutcomeV1> {
            let entries = self.0.lock().unwrap().entries.values().cloned().collect();
            Box::pin(async move {
                ContextScoutDurableStartupOutcomeV1::Ready {
                    entries,
                    truncated: false,
                }
            })
        }

        fn enqueue(
            &self,
            entry: ContextScoutDurableQueueEntryV1,
        ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1> {
            if entry.validate().is_err() {
                return Box::pin(async { ContextScoutDurableStoreOutcomeV1::Unavailable });
            }
            let mut state = self.0.lock().unwrap();
            let outcome = match state.entries.get(&entry.envelope.envelope_id) {
                Some(existing) if existing == &entry => {
                    ContextScoutDurableStoreOutcomeV1::Duplicate
                }
                Some(_) => ContextScoutDurableStoreOutcomeV1::Superseded,
                None => {
                    state.entries.insert(entry.envelope.envelope_id, entry);
                    ContextScoutDurableStoreOutcomeV1::Stored
                }
            };
            Box::pin(async move { outcome })
        }

        fn claim(
            &self,
            _address: ContextScoutAddressV1,
            _now: UtcMicros,
            _lease: ContextScoutLeaseV1,
        ) -> ContextScoutStoreFuture<'_, ContextScoutDurableClaimOutcomeV1> {
            Box::pin(async { ContextScoutDurableClaimOutcomeV1::Empty })
        }

        fn requeue(
            &self,
            _claimed: ContextScoutDurableClaimV1,
        ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1> {
            Box::pin(async { ContextScoutDurableStoreOutcomeV1::Duplicate })
        }

        fn cancel_work(
            &self,
            work: ContextScoutWorkV1,
        ) -> ContextScoutStoreFuture<'_, ContextScoutDurableStoreOutcomeV1> {
            let mut state = self.0.lock().unwrap();
            state.entries.retain(|_, entry| entry.work != work);
            state.cancellations.push(work);
            Box::pin(async { ContextScoutDurableStoreOutcomeV1::Stored })
        }

        fn record_delivery<'a>(
            &'a self,
            claim: &'a ContextScoutDurableClaimV1,
            receipt: &'a ContextScoutDeliveryReceiptV1,
        ) -> ContextScoutStoreFuture<'a, ContextScoutDurableStoreOutcomeV1> {
            let mut state = self.0.lock().unwrap();
            if state.receipts.iter().any(|existing| existing == receipt) {
                return Box::pin(async { ContextScoutDurableStoreOutcomeV1::Duplicate });
            }
            if state.entries.get(&claim.entry.envelope.envelope_id) != Some(&claim.entry) {
                return Box::pin(async { ContextScoutDurableStoreOutcomeV1::Unavailable });
            }
            state.receipts.push(receipt.clone());
            Box::pin(async { ContextScoutDurableStoreOutcomeV1::Stored })
        }

        fn record_feedback<'a>(
            &'a self,
            _receipt: &'a ContextScoutDeliveryReceiptV1,
            feedback: ContextScoutFeedbackV1,
        ) -> ContextScoutStoreFuture<'a, ContextScoutDurableStoreOutcomeV1> {
            let mut state = self.0.lock().unwrap();
            let outcome = if state.feedback.contains(&feedback) {
                ContextScoutDurableStoreOutcomeV1::Duplicate
            } else {
                state.feedback.push(feedback);
                ContextScoutDurableStoreOutcomeV1::Stored
            };
            Box::pin(async move { outcome })
        }
    }

    #[derive(Clone)]
    struct RecordingModel {
        calls: Arc<AtomicUsize>,
    }

    impl ContextScoutModelAssistantV1 for RecordingModel {
        fn backend(&self) -> ContextScoutModelBackendV1 {
            ContextScoutModelBackendV1::CodexAppServer
        }

        fn propose(
            &self,
            request: ContextScoutModelRequestV1,
            _execution: ContextScoutModelExecutionV1,
        ) -> ContextScoutModelFuture<'_> {
            let calls = Arc::clone(&self.calls);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let candidate = request
                    .candidates
                    .first()
                    .ok_or(ContextScoutModelErrorV1::InvalidOutput)?;
                Ok(ContextScoutModelProposalV1 {
                    candidate: ContextScoutModelCandidateV1 {
                        selected_dedupe_key: candidate.dedupe_key,
                        suggestion_text: "Evidence-bound model refinement.".to_owned(),
                        cited_anchor_ids: candidate.citation_anchor_ids.clone(),
                    },
                    receipt: model_receipt(),
                })
            })
        }
    }

    #[tokio::test]
    async fn durable_runtime_replays_exact_entry_and_cancellation_is_generation_bound() {
        let store = DurableStore::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let model = RecordingModel {
            calls: Arc::clone(&calls),
        };
        let mut runtime = ContextScoutDurableRuntimeV1::new(store.clone(), model.clone());
        let outcome = runtime
            .prepare(
                &input(vec![candidate(1, 10)]),
                ContextScoutLimitsV1::bounded_defaults(),
                ContextScoutRuntimeModeV1::ConfiguredModel,
                model_execution(),
            )
            .await
            .unwrap();
        let ContextScoutRuntimeOutcomeV1::Enqueued {
            entry,
            store_outcome,
        } = outcome
        else {
            panic!("saved candidate should enqueue");
        };
        assert_eq!(store_outcome, ContextScoutDurableStoreOutcomeV1::Stored);
        assert_eq!(entry.route, ContextScoutRouteV1::ModelAssisted);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let status = runtime
            .status(ContextScoutControlV1 {
                configuration_revision: [16; 32],
                state: ContextScoutServiceStateV1::Active,
                mode: ContextScoutRuntimeModeV1::ConfiguredModel,
                model_path: Some(ContextScoutModelBackendV1::CodexAppServer),
                limits: ContextScoutLimitsV1::bounded_defaults(),
            })
            .unwrap();
        assert_eq!(
            status.last_model_outcome,
            Some(ContextScoutModelRunOutcomeV1::Succeeded)
        );
        assert_eq!(status.last_model_receipt, Some(model_receipt()));
        let mut forged_cancelled_entry = (*entry).clone();
        forged_cancelled_entry.route = ContextScoutRouteV1::DeterministicFallback;
        forged_cancelled_entry.model_outcome = ContextScoutModelRunOutcomeV1::Cancelled;
        forged_cancelled_entry.model_receipt = None;
        assert_eq!(
            forged_cancelled_entry.validate(),
            Err(ContextScoutErrorV1::InvalidCandidate)
        );
        let mut restarted = ContextScoutDurableRuntimeV1::new(store.clone(), model);
        let replay = restarted
            .prepare(
                &input(vec![candidate(1, 10)]),
                ContextScoutLimitsV1::bounded_defaults(),
                ContextScoutRuntimeModeV1::ConfiguredModel,
                model_execution(),
            )
            .await
            .unwrap();
        assert!(matches!(
            replay,
            ContextScoutRuntimeOutcomeV1::Enqueued {
                store_outcome: ContextScoutDurableStoreOutcomeV1::Duplicate,
                ..
            }
        ));
        assert_eq!(store.0.lock().unwrap().entries.len(), 1);

        assert_eq!(
            runtime.cancel(entry.work).await.unwrap(),
            ContextScoutDurableStoreOutcomeV1::Stored
        );
        assert_eq!(
            runtime.cancel(entry.work).await,
            Err(ContextScoutErrorV1::StaleWork)
        );
        assert_eq!(store.0.lock().unwrap().cancellations, vec![entry.work]);
    }

    #[tokio::test]
    async fn restart_restores_the_latest_durable_work_generation() {
        let store = DurableStore::default();
        let mut runtime = ContextScoutDurableRuntimeV1::new(store.clone(), BadModel);
        let first = input(vec![candidate(1, 10)]);
        runtime
            .prepare(
                &first,
                ContextScoutLimitsV1::bounded_defaults(),
                ContextScoutRuntimeModeV1::Deterministic,
                model_execution(),
            )
            .await
            .unwrap();
        let mut second = first.clone();
        second.input_watermark = [21; 32];
        second.envelope_id = [22; 16];
        let ContextScoutRuntimeOutcomeV1::Enqueued { entry: second, .. } = runtime
            .prepare(
                &second,
                ContextScoutLimitsV1::bounded_defaults(),
                ContextScoutRuntimeModeV1::Deterministic,
                model_execution(),
            )
            .await
            .unwrap()
        else {
            panic!("second generation should enqueue");
        };
        assert_eq!(second.work.generation, 2);

        let startup = store.startup(UtcMicros(10), 32).await;
        let mut restarted = ContextScoutDurableRuntimeV1::new(store, BadModel);
        restarted.restore_startup(&startup).unwrap();
        let mut third = first;
        third.input_watermark = [23; 32];
        third.envelope_id = [24; 16];
        let ContextScoutRuntimeOutcomeV1::Enqueued { entry: third, .. } = restarted
            .prepare(
                &third,
                ContextScoutLimitsV1::bounded_defaults(),
                ContextScoutRuntimeModeV1::Deterministic,
                model_execution(),
            )
            .await
            .unwrap()
        else {
            panic!("post-restart generation should enqueue");
        };
        assert_eq!(third.work.generation, 3);
    }

    #[tokio::test]
    async fn controlled_model_execution_cannot_widen_daemon_limits() {
        let store = DurableStore::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut runtime = ContextScoutDurableRuntimeV1::new(
            store.clone(),
            RecordingModel {
                calls: Arc::clone(&calls),
            },
        );
        let limits = ContextScoutLimitsV1::bounded_defaults();
        let control = ContextScoutControlV1 {
            configuration_revision: [16; 32],
            state: ContextScoutServiceStateV1::Active,
            mode: ContextScoutRuntimeModeV1::ConfiguredModel,
            model_path: Some(ContextScoutModelBackendV1::CodexAppServer),
            limits,
        };
        let widened_execution = ContextScoutModelExecutionV1 {
            deadline: MonotonicDeadline::at(Instant::now() + Duration::from_secs(5)),
            cancellation: CancellationToken::new(),
            max_input_tokens: limits.max_model_input_tokens.saturating_add(1),
            max_output_tokens: limits.max_model_output_tokens,
        };

        assert_eq!(
            runtime
                .prepare_controlled(&input(vec![candidate(1, 10)]), control, widened_execution,)
                .await,
            Err(ContextScoutErrorV1::InvalidLimits)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(store.0.lock().unwrap().entries.is_empty());
    }

    #[tokio::test]
    async fn cancelled_model_run_never_reaches_the_durable_queue() {
        let store = DurableStore::default();
        let mut runtime = ContextScoutDurableRuntimeV1::new(
            store.clone(),
            FailingModel(ContextScoutModelErrorV1::Cancelled),
        );
        let outcome = runtime
            .prepare(
                &input(vec![candidate(1, 10)]),
                ContextScoutLimitsV1::bounded_defaults(),
                ContextScoutRuntimeModeV1::ConfiguredModel,
                model_execution(),
            )
            .await
            .unwrap();

        assert_eq!(
            outcome,
            ContextScoutRuntimeOutcomeV1::Suppressed {
                reason: ContextScoutSuppressionV1::Cancelled,
            }
        );
        assert!(store.0.lock().unwrap().entries.is_empty());
        let status = runtime
            .status(ContextScoutControlV1 {
                configuration_revision: [16; 32],
                state: ContextScoutServiceStateV1::Active,
                mode: ContextScoutRuntimeModeV1::ConfiguredModel,
                model_path: Some(ContextScoutModelBackendV1::CodexAppServer),
                limits: ContextScoutLimitsV1::bounded_defaults(),
            })
            .unwrap();
        assert_eq!(
            status.last_model_outcome,
            Some(ContextScoutModelRunOutcomeV1::Cancelled)
        );
        assert_eq!(
            status.last_suppression,
            Some(ContextScoutSuppressionV1::Cancelled)
        );
        assert_eq!(
            status.last_route,
            Some(ContextScoutRouteV1::DeterministicFallback)
        );
        assert!(status.last_model_receipt.is_none());
    }

    #[tokio::test]
    async fn delivery_and_explicit_feedback_are_visible_in_status() {
        let store = DurableStore::default();
        let mut runtime = ContextScoutDurableRuntimeV1::new(store, BadModel);
        let ContextScoutRuntimeOutcomeV1::Enqueued { entry, .. } = runtime
            .prepare(
                &input(vec![candidate(1, 10)]),
                ContextScoutLimitsV1::bounded_defaults(),
                ContextScoutRuntimeModeV1::Deterministic,
                model_execution(),
            )
            .await
            .unwrap()
        else {
            panic!("deterministic candidate should enqueue");
        };
        let receipt = ContextScoutDeliveryReceiptV1 {
            receipt_id: [31; 16],
            envelope_id: entry.envelope.envelope_id,
            delivered_at: UtcMicros(20),
            outcome: ContextScoutOutcomeV1::Displayed,
        };
        let claim = ContextScoutDurableClaimV1 {
            entry: (*entry).clone(),
            lease: ContextScoutLeaseV1 {
                lease_id: [30; 16],
                expires_at: UtcMicros(40),
            },
        };
        runtime.complete_delivery(&claim, &receipt).await.unwrap();
        runtime
            .record_feedback(
                &receipt,
                ContextScoutFeedbackV1 {
                    receipt_id: receipt.receipt_id,
                    kind: ContextScoutFeedbackKindV1::ExplicitlyAccepted,
                },
            )
            .await
            .unwrap();
        let status = runtime
            .status(ContextScoutControlV1 {
                configuration_revision: [16; 32],
                state: ContextScoutServiceStateV1::Active,
                mode: ContextScoutRuntimeModeV1::Deterministic,
                model_path: None,
                limits: ContextScoutLimitsV1::bounded_defaults(),
            })
            .unwrap();
        assert_eq!(
            status.last_delivery_outcome,
            Some(ContextScoutOutcomeV1::Displayed)
        );
        assert_eq!(
            status.last_feedback,
            Some(ContextScoutFeedbackKindV1::ExplicitlyAccepted)
        );
        assert_eq!(status.limits, ContextScoutLimitsV1::bounded_defaults());
        assert_eq!(status.last_route, Some(ContextScoutRouteV1::Deterministic));
        assert!(status.last_suppression.is_none());
    }

    #[tokio::test]
    async fn dirty_overlay_never_invokes_model_or_durable_queue() {
        let store = DurableStore::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut runtime = ContextScoutDurableRuntimeV1::new(
            store.clone(),
            RecordingModel {
                calls: Arc::clone(&calls),
            },
        );
        let mut dirty = candidate(2, 10);
        dirty.evidence[0].generation = ContextScoutEvidenceGenerationV1::DirtyOverlay;
        assert_eq!(
            runtime
                .prepare(
                    &input(vec![dirty]),
                    ContextScoutLimitsV1::bounded_defaults(),
                    ContextScoutRuntimeModeV1::ConfiguredModel,
                    model_execution(),
                )
                .await
                .unwrap(),
            ContextScoutRuntimeOutcomeV1::Suppressed {
                reason: ContextScoutSuppressionV1::DirtyOverlay
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let state = store.0.lock().unwrap();
        assert!(state.entries.is_empty());
    }
}
