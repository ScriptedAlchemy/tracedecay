//! Plan-26 source-observation mapping for durable feedback-cycle telemetry.
//!
//! This module stops at the canonical, privacy-safe event envelope. Daemon
//! composition must enqueue that envelope through the existing authoritative
//! observation/analytics path; it must not write the analytics table directly.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_application::feedback::FeedbackObservationPort;
use tracedecay_domain::feedback::{
    CiFailureSourceDegradationV1, FeedbackCycleObservationV1, FeedbackEvaluationInputV1,
    FeedbackObservationKindV1, FeedbackSavedEvaluationV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};

use crate::request_identity::{
    derive_feedback_observation_idempotency, derive_feedback_source_event_idempotency,
};
use tracedecay_runtime_core::timeutil::nearest_rank;

const OBSERVATION_ENVELOPE_DOMAIN: &str = "tracedecay.feedback.observation.plan26.v1";
const SOURCE_EVENT_ENVELOPE_DOMAIN: &str = "tracedecay.feedback.source-event.plan26.v1";
const SAVED_EVALUATION_DOMAIN: &str = "tracedecay.feedback.saved-evaluation.plan26.v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26FeedbackOperationV1 {
    FeedbackCycle,
    FeedbackDiagnostics,
    FeedbackGet,
    FeedbackExpand,
    FeedbackList,
    PrimitiveImpact,
    PrimitiveAffectedTests,
    PrimitiveTestResults,
    LspSession,
    GitHubReview,
    CiLocalization,
    Proximity,
    HostDelivery,
    HookFeedback,
    ScoutFeedback,
    SseStream,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26FeedbackOutcomeV1 {
    Accepted,
    Admitted,
    Rejected,
    AtCapacity,
    Completed,
    Unavailable,
    Denied,
    Cancelled,
    TimedOut,
    Failed,
    Duplicate,
    Stale,
    Partial,
    RateLimited,
    RolledBack,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26DeliveryRouteV1 {
    Cli,
    Mcp,
    Http,
    Lsp,
    HookV2,
    HookLegacy,
    Scout,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26RejectedArgumentV1 {
    RequestBody,
    Pagination,
    RequestHandle,
    Operation,
    Lifecycle,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26ArgumentRejectionClassV1 {
    Missing,
    InvalidShape,
    OutOfBounds,
    Unsupported,
    Unauthorized,
    Stale,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26LspMethodClassV1 {
    Lifecycle,
    DocumentSync,
    Diagnostics,
    Navigation,
    ContextProjection,
    ContextExpansion,
    Cancellation,
    Progress,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26LspStateV1 {
    SessionOpened,
    Initialized,
    Detached,
    Reconnected,
    Shutdown,
    Exited,
    Expired,
    MethodAdmitted,
    MethodCompleted,
    MethodRejected,
    QueueBackpressured,
    CancellationRequested,
    CancellationAccepted,
    CancellationTooLate,
    AnalyzerStarting,
    AnalyzerRestarting,
    AnalyzerIndexing,
    AnalyzerDegraded,
    CacheReused,
    OverlayFresh,
    OverlayStale,
    DiagnosticPublished,
    DiagnosticCleared,
    ProviderConflict,
    HostDelivered,
    Partial,
    Dropped,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26GitHubLifecycleV1 {
    Current,
    Outdated,
    Resolved,
    Edited,
    Deleted,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26AdvisoryProviderV1 {
    #[serde(rename = "github_review")]
    GitHubReview,
    CiLocalization,
    Proximity,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26CiProviderV1 {
    GitHubActions,
    RetainedObservation,
    ExactCodeEvidence,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum Plan26CoverageV1 {
    Known,
    Partial,
    Stale,
    Unknown,
    Sampled,
    Capped,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum Plan26RelevanceDispositionV1 {
    Helpful,
    Stale,
    Irrelevant,
    Contradictory,
    Unknown,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum Plan26StackTransitionV1 {
    DependencyReady,
    ConflictDetected,
    ConflictCleared,
    UpstreamTipChanged,
    BaseDrifted,
    HeadDrifted,
    MergeBaseDrifted,
    Revoked,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackObservationDeliveryV1 {
    pub emitted: u64,
    pub delayed: u64,
    pub dropped: u64,
    pub coverage: Plan26CoverageV1,
}

impl FeedbackObservationDeliveryV1 {
    pub const fn pending() -> Self {
        Self {
            emitted: 0,
            delayed: 0,
            dropped: 0,
            coverage: Plan26CoverageV1::Unknown,
        }
    }

    pub const fn delivered(dropped: u64) -> Self {
        Self {
            emitted: 1,
            delayed: 0,
            dropped,
            coverage: if dropped == 0 {
                Plan26CoverageV1::Known
            } else {
                Plan26CoverageV1::Partial
            },
        }
    }

    fn validate(&self, persisted: bool) -> Option<()> {
        match self.coverage {
            Plan26CoverageV1::Known
                if (!persisted || self.emitted == 1) && self.delayed == 0 && self.dropped == 0 =>
            {
                Some(())
            }
            Plan26CoverageV1::Partial
                if (!persisted || self.emitted == 1) && (self.delayed > 0 || self.dropped > 0) =>
            {
                Some(())
            }
            Plan26CoverageV1::Unknown if !persisted && self.emitted == 0 => Some(()),
            Plan26CoverageV1::Sampled | Plan26CoverageV1::Capped | Plan26CoverageV1::Stale
                if !persisted || self.emitted == 1 =>
            {
                Some(())
            }
            _ => None,
        }
    }
}

impl Default for FeedbackObservationDeliveryV1 {
    fn default() -> Self {
        Self::pending()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26ProximityTransitionV1 {
    Emitted,
    Suppressed,
    Expired,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26ProximityRiskV1 {
    None,
    BelowThreshold,
    AtOrAboveThreshold,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26AnchorOperationV1 {
    Anchor,
    HandleExpansion,
    EvidenceExpansion,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26HookScoutPhaseV1 {
    Admission,
    Delivery,
    FeedbackTerminal,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Plan26SseLifecycleV1 {
    Opened,
    EventDelivered,
    Gap,
    Expired,
    Completed,
    Cancelled,
    TimedOut,
    Failed,
    Unavailable,
    Partial,
    Disconnected,
}

/// Closed PR11-PR13 source-event family. All values are bounded enums,
/// digests, counts, or durations; provider payloads never enter this type.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Plan26FeedbackSourceEventV1 {
    ArgumentRejected {
        operation: Plan26FeedbackOperationV1,
        outcome: Plan26FeedbackOutcomeV1,
    },
    SurfaceArgumentRejected {
        operation: Plan26FeedbackOperationV1,
        route: Option<Plan26DeliveryRouteV1>,
        argument: Plan26RejectedArgumentV1,
        rejection: Plan26ArgumentRejectionClassV1,
        schema_revision: u16,
        outcome: Plan26FeedbackOutcomeV1,
    },
    LspState {
        state: Plan26LspStateV1,
        method: Option<Plan26LspMethodClassV1>,
        outcome: Plan26FeedbackOutcomeV1,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    Dispatch {
        operation: Plan26FeedbackOperationV1,
        outcome: Plan26FeedbackOutcomeV1,
        capacity: u32,
        admitted: u32,
    },
    Delivery {
        operation: Plan26FeedbackOperationV1,
        route: Plan26DeliveryRouteV1,
        outcome: Plan26FeedbackOutcomeV1,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    Truncation {
        operation: Plan26FeedbackOperationV1,
        returned_count: u32,
        omitted_count: u32,
    },
    RelevanceFeedback {
        disposition: Plan26RelevanceDispositionV1,
    },
    EvidenceDiversity {
        eligible_source_families: u32,
        represented_source_families: u32,
        selected_count: u32,
    },
    AnchorExpansion {
        operation: Plan26AnchorOperationV1,
        outcome: Plan26FeedbackOutcomeV1,
        returned_count: u32,
        duration_micros: Option<u64>,
    },
    GitHubLifecycle {
        lifecycle: Plan26GitHubLifecycleV1,
        item_count: u32,
    },
    GitHubIngress {
        outcome: Plan26FeedbackOutcomeV1,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    GitHubRateLimit {
        duration_micros: Option<u64>,
    },
    GitHubStale {
        item_count: u32,
    },
    ProviderState {
        provider: Plan26AdvisoryProviderV1,
        state: ProviderEvaluationStateV1,
    },
    CiLocalization {
        outcome: Plan26FeedbackOutcomeV1,
        provider: Plan26CiProviderV1,
        exact_evidence: bool,
        coverage: Plan26CoverageV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_degradation: Option<CiFailureSourceDegradationV1>,
        localized_count: u32,
        candidate_count: u32,
        duration_micros: Option<u64>,
    },
    Proximity {
        transition: Plan26ProximityTransitionV1,
        risk: Plan26ProximityRiskV1,
        configuration_revision: ManifestDigest,
        candidate_count: u32,
        affected_count: u32,
    },
    HostDelivery {
        route: Plan26DeliveryRouteV1,
        outcome: Plan26FeedbackOutcomeV1,
        rollback: bool,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    HookScout {
        route: Plan26DeliveryRouteV1,
        phase: Plan26HookScoutPhaseV1,
        outcome: Plan26FeedbackOutcomeV1,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    Cancellation {
        operation: Plan26FeedbackOperationV1,
        outcome: Plan26FeedbackOutcomeV1,
    },
    AuthorizationRevoked {
        operation: Plan26FeedbackOperationV1,
        outcome: Plan26FeedbackOutcomeV1,
        propagation_micros: u64,
    },
    StackTransition {
        transition: Plan26StackTransitionV1,
        outcome: Plan26FeedbackOutcomeV1,
    },
    SseLifecycle {
        lifecycle: Plan26SseLifecycleV1,
        sequence: Option<u64>,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    TelemetryDropObserved {
        dropped_count: u64,
        last_sequence: u64,
        terminal: bool,
    },
}

impl Plan26FeedbackSourceEventV1 {
    pub const fn event_kind(&self) -> &'static str {
        match self {
            Self::ArgumentRejected { .. } | Self::SurfaceArgumentRejected { .. } => {
                "feedback.argument.rejected.v1"
            }
            Self::LspState { .. } => "feedback.lsp.state.observed.v1",
            Self::Dispatch { .. } => "feedback.dispatch.observed.v1",
            Self::Delivery { .. } => "feedback.delivery.observed.v1",
            Self::Truncation { .. } => "feedback.truncation.observed.v1",
            Self::RelevanceFeedback { .. } => "feedback.relevance.observed.v1",
            Self::EvidenceDiversity { .. } => "feedback.diversity.observed.v1",
            Self::AnchorExpansion { .. } => "feedback.expansion.observed.v1",
            Self::GitHubLifecycle { .. } => "feedback.github.lifecycle.observed.v1",
            Self::GitHubIngress { .. } => "feedback.github.ingress.observed.v1",
            Self::GitHubRateLimit { .. } => "feedback.github.rate_limit.observed.v1",
            Self::GitHubStale { .. } => "feedback.github.stale.observed.v1",
            Self::ProviderState { .. } => "feedback.provider.state.observed.v1",
            Self::CiLocalization { .. } => "feedback.ci.localization.observed.v1",
            Self::Proximity { .. } => "feedback.proximity.observed.v1",
            Self::HostDelivery { .. } => "feedback.host_delivery.observed.v1",
            Self::HookScout { .. } => "feedback.hook_scout.observed.v1",
            Self::Cancellation { .. } => "feedback.cancellation.observed.v1",
            Self::AuthorizationRevoked { .. } => "feedback.authorization_revocation.observed.v1",
            Self::StackTransition { .. } => "feedback.stack_transition.observed.v1",
            Self::SseLifecycle { .. } => "feedback.sse.lifecycle.observed.v1",
            Self::TelemetryDropObserved { .. } => "telemetry.drop.observed.v1",
        }
    }

    pub fn validate(&self) -> Option<()> {
        match self {
            Self::SurfaceArgumentRejected {
                schema_revision, ..
            } => (*schema_revision > 0).then_some(()),
            Self::Proximity {
                configuration_revision,
                affected_count,
                candidate_count,
                ..
            } => {
                configuration_revision.validate().ok()?;
                (affected_count <= candidate_count).then_some(())
            }
            Self::Dispatch {
                admitted, capacity, ..
            } => (admitted <= capacity).then_some(()),
            Self::CiLocalization {
                localized_count,
                candidate_count,
                ..
            } => (localized_count <= candidate_count).then_some(()),
            Self::EvidenceDiversity {
                eligible_source_families,
                represented_source_families,
                selected_count,
            } => (represented_source_families <= eligible_source_families
                && (*eligible_source_families == 0
                    || (*represented_source_families > 0 && *selected_count > 0)))
                .then_some(()),
            Self::TelemetryDropObserved {
                dropped_count,
                terminal,
                ..
            } => (*terminal || *dropped_count > 0).then_some(()),
            _ => Some(()),
        }
    }
}

/// Privacy-safe Plan-26 source event. It contains no source, path, diagnostic
/// message, overlay content, or transport-local delivery identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackObservationEnvelopeV1 {
    pub schema_version: u16,
    pub producer: String,
    pub privacy_class: String,
    pub idempotency_key: ManifestDigest,
    pub saved_evaluation_digest: ManifestDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<FeedbackCycleObservationV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event: Option<Plan26FeedbackSourceEventV1>,
    pub observed_at: UtcMicros,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_boot_id: Option<ManifestDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_sequence: Option<u64>,
    #[serde(default)]
    pub delivery: FeedbackObservationDeliveryV1,
}

impl FeedbackObservationEnvelopeV1 {
    pub fn validate(&self) -> Option<()> {
        let persisted = match (&self.producer_boot_id, self.producer_sequence) {
            (None, None) => false,
            (Some(boot_id), Some(sequence)) if sequence > 0 => {
                boot_id.validate().ok()?;
                true
            }
            _ => return None,
        };
        self.delivery.validate(persisted)?;
        if self.schema_version != 1
            || self.privacy_class != "operational_no_content"
            || self.idempotency_key.validate().is_err()
            || self.saved_evaluation_digest.validate().is_err()
        {
            return None;
        }
        let expected_key = match (&self.observation, &self.source_event) {
            (Some(observation), None)
                if self.producer == "feedback_cycle"
                    && observation.observed_at == self.observed_at =>
            {
                observation.validate().ok()?;
                canonical_sha256(&(
                    OBSERVATION_ENVELOPE_DOMAIN,
                    &self.saved_evaluation_digest,
                    observation,
                ))
                .ok()?
            }
            (None, Some(source_event)) if self.producer == "feedback_source" => {
                source_event.validate()?;
                canonical_sha256(&(
                    SOURCE_EVENT_ENVELOPE_DOMAIN,
                    &self.saved_evaluation_digest,
                    self.observed_at,
                    source_event,
                ))
                .ok()?
            }
            _ => return None,
        };
        (expected_key == self.idempotency_key).then_some(())
    }

    pub fn assign_delivery(
        &mut self,
        boot_id: ManifestDigest,
        producer_sequence: u64,
        delivery: FeedbackObservationDeliveryV1,
    ) -> Option<()> {
        if self.producer_boot_id.is_some() || self.producer_sequence.is_some() {
            return None;
        }
        boot_id.validate().ok()?;
        if producer_sequence == 0 {
            return None;
        }
        self.producer_boot_id = Some(boot_id);
        self.producer_sequence = Some(producer_sequence);
        self.delivery = delivery;
        self.validate()
    }

    /// Stable identity used by bounded ingress queues to converge retries and
    /// replay before an envelope reaches the durable observability authority.
    pub fn replay_identity(&self) -> Option<&str> {
        self.validate().map(|()| self.idempotency_key.as_str())
    }

    pub fn event_kind(&self) -> Option<&'static str> {
        self.validate()?;
        match (&self.observation, &self.source_event) {
            (Some(observation), None) => Some(match observation.kind {
                FeedbackObservationKindV1::Trigger => "feedback.cycle.triggered.v1",
                FeedbackObservationKindV1::EvaluationStage => "feedback.cycle.stage.observed.v1",
                FeedbackObservationKindV1::Terminal => "feedback.cycle.terminal.v1",
                FeedbackObservationKindV1::DedupeSuppressed => {
                    "feedback.cycle.dedupe_suppressed.v1"
                }
                FeedbackObservationKindV1::Latency => "feedback.cycle.latency.observed.v1",
            }),
            (None, Some(source_event)) => Some(source_event.event_kind()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackObservationReadModelV1 {
    pub schema_version: u16,
    pub total_count: u64,
    pub first_observed_at: Option<UtcMicros>,
    pub last_observed_at: Option<UtcMicros>,
    pub event_counts: BTreeMap<String, u64>,
    pub coverage: Plan26CoverageV1,
    pub watermark: FeedbackObservationWatermarkV1,
    pub denominators: FeedbackObservationDenominatorsV1,
    pub system_quality: FeedbackSystemQualityReadModelV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackObservationWatermarkV1 {
    pub producer_boot_id: Option<ManifestDigest>,
    pub producer_sequence: Option<u64>,
    pub observed_through: Option<UtcMicros>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackObservationDenominatorsV1 {
    pub eligible: u64,
    pub persisted: u64,
    pub emitted: u64,
    pub delayed: u64,
    pub dropped: u64,
    pub retention_dropped: u64,
    pub incomplete_boots: u64,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSystemMetricKindV1 {
    Coverage,
    Relevance,
    Diversity,
    Latency,
    Omission,
    Denial,
    Staleness,
    RevocationPropagation,
    StackTransitions,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSystemMetricUnitV1 {
    Ratio,
    Microseconds,
    Transitions,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSystemMetricDenominatorV1 {
    EligibleObservations,
    RelevanceLabels,
    EligibleSourceFamilies,
    LatencySamples,
    ReturnedAndOmittedItems,
    OutcomeObservations,
    RevocationObservations,
    StackTransitionObservations,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSystemMetricUnavailableReasonV1 {
    NoEligibleObservations,
    NoRelevanceLabels,
    NoDiversityObservations,
    NoLatencySamples,
    NoTruncationObservations,
    NoOutcomeObservations,
    NoRevocationObservations,
    NoStackTransitionObservations,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackSystemMetricV1 {
    pub metric: FeedbackSystemMetricKindV1,
    pub value: Option<f64>,
    pub unit: FeedbackSystemMetricUnitV1,
    pub numerator: Option<u64>,
    pub denominator: Option<u64>,
    pub denominator_population: FeedbackSystemMetricDenominatorV1,
    pub coverage: Plan26CoverageV1,
    pub unavailable_reason: Option<FeedbackSystemMetricUnavailableReasonV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackSystemQualityReadModelV1 {
    pub schema_version: u16,
    pub metrics: Vec<FeedbackSystemMetricV1>,
}

impl FeedbackObservationReadModelV1 {
    pub fn empty() -> Self {
        Self {
            schema_version: 1,
            total_count: 0,
            first_observed_at: None,
            last_observed_at: None,
            event_counts: BTreeMap::new(),
            coverage: Plan26CoverageV1::Unknown,
            watermark: FeedbackObservationWatermarkV1 {
                producer_boot_id: None,
                producer_sequence: None,
                observed_through: None,
            },
            denominators: FeedbackObservationDenominatorsV1 {
                eligible: 0,
                persisted: 0,
                emitted: 0,
                delayed: 0,
                dropped: 0,
                retention_dropped: 0,
                incomplete_boots: 0,
            },
            system_quality: FeedbackSystemQualityReadModelV1::project(
                &[],
                0,
                0,
                Plan26CoverageV1::Unknown,
            ),
        }
    }

    pub fn project(observations: &[FeedbackObservationEnvelopeV1]) -> Option<Self> {
        Self::project_with_accounting(observations, 0, 0)
    }

    pub fn project_with_accounting(
        observations: &[FeedbackObservationEnvelopeV1],
        retention_dropped: u64,
        incomplete_boots: u64,
    ) -> Option<Self> {
        let mut event_counts = BTreeMap::<String, u64>::new();
        let mut first_observed_at = None;
        let mut last_observed_at = None;
        let mut emitted = 0u64;
        let mut delayed = 0u64;
        let mut dropped = 0u64;
        let mut watermark = FeedbackObservationWatermarkV1 {
            producer_boot_id: None,
            producer_sequence: None,
            observed_through: None,
        };
        for observation in observations {
            let kind = observation.event_kind()?;
            let count = event_counts.entry(kind.to_owned()).or_default();
            *count = count.saturating_add(1);
            first_observed_at = Some(
                first_observed_at.map_or(observation.observed_at, |first: UtcMicros| {
                    first.min(observation.observed_at)
                }),
            );
            last_observed_at = Some(
                last_observed_at.map_or(observation.observed_at, |last: UtcMicros| {
                    last.max(observation.observed_at)
                }),
            );
            emitted = emitted.saturating_add(observation.delivery.emitted);
            delayed = delayed.saturating_add(observation.delivery.delayed);
            dropped = dropped.saturating_add(observation.delivery.dropped);
            if let (Some(boot_id), Some(sequence)) = (
                observation.producer_boot_id.as_ref(),
                observation.producer_sequence,
            ) {
                watermark.producer_boot_id = Some(boot_id.clone());
                watermark.producer_sequence = Some(sequence);
                watermark.observed_through = Some(observation.observed_at);
            }
        }
        let persisted = observations.len().try_into().unwrap_or(u64::MAX);
        let eligible = emitted
            .max(persisted)
            .saturating_add(delayed)
            .saturating_add(dropped)
            .saturating_add(retention_dropped);
        let coverage = if incomplete_boots > 0 {
            Plan26CoverageV1::Unknown
        } else if retention_dropped > 0 {
            Plan26CoverageV1::Capped
        } else if delayed > 0 || dropped > 0 {
            Plan26CoverageV1::Partial
        } else if observations.is_empty() {
            Plan26CoverageV1::Unknown
        } else if observations
            .iter()
            .all(|observation| observation.producer_sequence.is_some())
        {
            Plan26CoverageV1::Known
        } else {
            Plan26CoverageV1::Unknown
        };
        let system_quality =
            FeedbackSystemQualityReadModelV1::project(observations, eligible, persisted, coverage);
        Some(Self {
            schema_version: 1,
            total_count: persisted,
            first_observed_at,
            last_observed_at,
            event_counts,
            coverage,
            watermark,
            denominators: FeedbackObservationDenominatorsV1 {
                eligible,
                persisted,
                emitted,
                delayed,
                dropped,
                retention_dropped,
                incomplete_boots,
            },
            system_quality,
        })
    }
}

impl FeedbackSystemQualityReadModelV1 {
    fn project(
        observations: &[FeedbackObservationEnvelopeV1],
        eligible_observations: u64,
        persisted_observations: u64,
        observation_coverage: Plan26CoverageV1,
    ) -> Self {
        let mut relevance_helpful = 0u64;
        let mut relevance_total = 0u64;
        let mut relevance_unknown = 0u64;
        let mut diversity_represented = 0u64;
        let mut diversity_eligible = 0u64;
        let mut latency_samples = Vec::new();
        let mut returned_items = 0u64;
        let mut omitted_items = 0u64;
        let mut outcome_total = 0u64;
        let mut denied_outcomes = 0u64;
        let mut stale_outcomes = 0u64;
        let mut revocation_samples = Vec::new();
        let mut stack_transitions = 0u64;

        for envelope in observations {
            if let Some(observation) = &envelope.observation
                && let Some(latency) = observation.latency_micros
            {
                latency_samples.push(latency);
            }
            let Some(event) = envelope.source_event.as_ref() else {
                continue;
            };
            if let Some(duration) = source_event_duration_micros(event) {
                latency_samples.push(duration);
            }
            if let Some(outcome) = source_event_outcome(event) {
                outcome_total = outcome_total.saturating_add(1);
                denied_outcomes = denied_outcomes
                    .saturating_add(u64::from(outcome == Plan26FeedbackOutcomeV1::Denied));
                stale_outcomes = stale_outcomes
                    .saturating_add(u64::from(outcome == Plan26FeedbackOutcomeV1::Stale));
            }
            match event {
                Plan26FeedbackSourceEventV1::RelevanceFeedback { disposition } => {
                    relevance_total = relevance_total.saturating_add(1);
                    match disposition {
                        Plan26RelevanceDispositionV1::Helpful => {
                            relevance_helpful = relevance_helpful.saturating_add(1);
                        }
                        Plan26RelevanceDispositionV1::Unknown => {
                            relevance_unknown = relevance_unknown.saturating_add(1);
                        }
                        Plan26RelevanceDispositionV1::Stale
                        | Plan26RelevanceDispositionV1::Irrelevant
                        | Plan26RelevanceDispositionV1::Contradictory => {}
                    }
                }
                Plan26FeedbackSourceEventV1::EvidenceDiversity {
                    eligible_source_families,
                    represented_source_families,
                    ..
                } => {
                    diversity_eligible =
                        diversity_eligible.saturating_add(u64::from(*eligible_source_families));
                    diversity_represented = diversity_represented
                        .saturating_add(u64::from(*represented_source_families));
                }
                Plan26FeedbackSourceEventV1::Truncation {
                    returned_count,
                    omitted_count,
                    ..
                } => {
                    returned_items = returned_items.saturating_add(u64::from(*returned_count));
                    omitted_items = omitted_items.saturating_add(u64::from(*omitted_count));
                }
                Plan26FeedbackSourceEventV1::GitHubStale { item_count } => {
                    outcome_total = outcome_total.saturating_add(u64::from(*item_count));
                    stale_outcomes = stale_outcomes.saturating_add(u64::from(*item_count));
                }
                Plan26FeedbackSourceEventV1::AuthorizationRevoked {
                    propagation_micros, ..
                } => revocation_samples.push(*propagation_micros),
                Plan26FeedbackSourceEventV1::StackTransition { .. } => {
                    stack_transitions = stack_transitions.saturating_add(1);
                }
                _ => {}
            }
        }

        let metric_coverage = |has_support: bool| {
            if !has_support {
                Plan26CoverageV1::Unknown
            } else {
                observation_coverage
            }
        };
        let ratio =
            |metric, numerator: u64, denominator: u64, population, unavailable_reason, coverage| {
                let supported = denominator > 0;
                FeedbackSystemMetricV1 {
                    metric,
                    value: supported.then(|| numerator as f64 / denominator as f64),
                    unit: FeedbackSystemMetricUnitV1::Ratio,
                    numerator: supported.then_some(numerator),
                    denominator: supported.then_some(denominator),
                    denominator_population: population,
                    coverage: metric_coverage(supported && coverage),
                    unavailable_reason: (!supported).then_some(unavailable_reason),
                }
            };
        let latency_p95 = percentile_95(&mut latency_samples);
        let revocation_p95 = percentile_95(&mut revocation_samples);
        let total_items = returned_items.saturating_add(omitted_items);
        let stack_supported = stack_transitions > 0;
        let metrics = vec![
            ratio(
                FeedbackSystemMetricKindV1::Coverage,
                persisted_observations,
                eligible_observations,
                FeedbackSystemMetricDenominatorV1::EligibleObservations,
                FeedbackSystemMetricUnavailableReasonV1::NoEligibleObservations,
                true,
            ),
            ratio(
                FeedbackSystemMetricKindV1::Relevance,
                relevance_helpful,
                relevance_total,
                FeedbackSystemMetricDenominatorV1::RelevanceLabels,
                FeedbackSystemMetricUnavailableReasonV1::NoRelevanceLabels,
                relevance_unknown == 0,
            ),
            ratio(
                FeedbackSystemMetricKindV1::Diversity,
                diversity_represented,
                diversity_eligible,
                FeedbackSystemMetricDenominatorV1::EligibleSourceFamilies,
                FeedbackSystemMetricUnavailableReasonV1::NoDiversityObservations,
                true,
            ),
            scalar_metric(
                FeedbackSystemMetricKindV1::Latency,
                latency_p95,
                FeedbackSystemMetricUnitV1::Microseconds,
                latency_samples.len().try_into().unwrap_or(u64::MAX),
                FeedbackSystemMetricDenominatorV1::LatencySamples,
                FeedbackSystemMetricUnavailableReasonV1::NoLatencySamples,
                observation_coverage,
            ),
            ratio(
                FeedbackSystemMetricKindV1::Omission,
                omitted_items,
                total_items,
                FeedbackSystemMetricDenominatorV1::ReturnedAndOmittedItems,
                FeedbackSystemMetricUnavailableReasonV1::NoTruncationObservations,
                true,
            ),
            ratio(
                FeedbackSystemMetricKindV1::Denial,
                denied_outcomes,
                outcome_total,
                FeedbackSystemMetricDenominatorV1::OutcomeObservations,
                FeedbackSystemMetricUnavailableReasonV1::NoOutcomeObservations,
                true,
            ),
            ratio(
                FeedbackSystemMetricKindV1::Staleness,
                stale_outcomes,
                outcome_total,
                FeedbackSystemMetricDenominatorV1::OutcomeObservations,
                FeedbackSystemMetricUnavailableReasonV1::NoOutcomeObservations,
                true,
            ),
            scalar_metric(
                FeedbackSystemMetricKindV1::RevocationPropagation,
                revocation_p95,
                FeedbackSystemMetricUnitV1::Microseconds,
                revocation_samples.len().try_into().unwrap_or(u64::MAX),
                FeedbackSystemMetricDenominatorV1::RevocationObservations,
                FeedbackSystemMetricUnavailableReasonV1::NoRevocationObservations,
                observation_coverage,
            ),
            FeedbackSystemMetricV1 {
                metric: FeedbackSystemMetricKindV1::StackTransitions,
                value: stack_supported.then_some(stack_transitions as f64),
                unit: FeedbackSystemMetricUnitV1::Transitions,
                numerator: stack_supported.then_some(stack_transitions),
                denominator: stack_supported.then_some(stack_transitions),
                denominator_population:
                    FeedbackSystemMetricDenominatorV1::StackTransitionObservations,
                coverage: metric_coverage(stack_supported),
                unavailable_reason: (!stack_supported).then_some(
                    FeedbackSystemMetricUnavailableReasonV1::NoStackTransitionObservations,
                ),
            },
        ];
        Self {
            schema_version: 1,
            metrics,
        }
    }
}

fn scalar_metric(
    metric: FeedbackSystemMetricKindV1,
    value: Option<u64>,
    unit: FeedbackSystemMetricUnitV1,
    support: u64,
    denominator_population: FeedbackSystemMetricDenominatorV1,
    unavailable_reason: FeedbackSystemMetricUnavailableReasonV1,
    coverage: Plan26CoverageV1,
) -> FeedbackSystemMetricV1 {
    FeedbackSystemMetricV1 {
        metric,
        value: value.map(|value| value as f64),
        unit,
        numerator: value,
        denominator: (support > 0).then_some(support),
        denominator_population,
        coverage: if support > 0 {
            coverage
        } else {
            Plan26CoverageV1::Unknown
        },
        unavailable_reason: (support == 0).then_some(unavailable_reason),
    }
}

fn percentile_95(samples: &mut [u64]) -> Option<u64> {
    samples.sort_unstable();
    nearest_rank(samples, 95)
}

fn source_event_duration_micros(event: &Plan26FeedbackSourceEventV1) -> Option<u64> {
    match event {
        Plan26FeedbackSourceEventV1::LspState {
            duration_micros, ..
        }
        | Plan26FeedbackSourceEventV1::Delivery {
            duration_micros, ..
        }
        | Plan26FeedbackSourceEventV1::AnchorExpansion {
            duration_micros, ..
        }
        | Plan26FeedbackSourceEventV1::GitHubIngress {
            duration_micros, ..
        }
        | Plan26FeedbackSourceEventV1::CiLocalization {
            duration_micros, ..
        }
        | Plan26FeedbackSourceEventV1::HostDelivery {
            duration_micros, ..
        }
        | Plan26FeedbackSourceEventV1::HookScout {
            duration_micros, ..
        }
        | Plan26FeedbackSourceEventV1::SseLifecycle {
            duration_micros, ..
        } => *duration_micros,
        Plan26FeedbackSourceEventV1::GitHubRateLimit { duration_micros } => *duration_micros,
        _ => None,
    }
}

fn source_event_outcome(event: &Plan26FeedbackSourceEventV1) -> Option<Plan26FeedbackOutcomeV1> {
    match event {
        Plan26FeedbackSourceEventV1::ArgumentRejected { outcome, .. }
        | Plan26FeedbackSourceEventV1::SurfaceArgumentRejected { outcome, .. }
        | Plan26FeedbackSourceEventV1::LspState { outcome, .. }
        | Plan26FeedbackSourceEventV1::Dispatch { outcome, .. }
        | Plan26FeedbackSourceEventV1::Delivery { outcome, .. }
        | Plan26FeedbackSourceEventV1::AnchorExpansion { outcome, .. }
        | Plan26FeedbackSourceEventV1::GitHubIngress { outcome, .. }
        | Plan26FeedbackSourceEventV1::CiLocalization { outcome, .. }
        | Plan26FeedbackSourceEventV1::HostDelivery { outcome, .. }
        | Plan26FeedbackSourceEventV1::HookScout { outcome, .. }
        | Plan26FeedbackSourceEventV1::Cancellation { outcome, .. }
        | Plan26FeedbackSourceEventV1::AuthorizationRevoked { outcome, .. }
        | Plan26FeedbackSourceEventV1::StackTransition { outcome, .. } => Some(*outcome),
        _ => None,
    }
}

/// Maps one validated durable saved-content observation into its canonical
/// Plan-26 source envelope. Overlay and mismatched observations fail closed.
pub fn feedback_observation_envelope(
    input: &FeedbackEvaluationInputV1,
    observation: FeedbackCycleObservationV1,
) -> Option<FeedbackObservationEnvelopeV1> {
    let saved = input.saved().ok()?;
    observation.validate().ok()?;
    if observation.cycle_id != input.request.cycle_id
        || observation.scope != input.request.scope
        || observation.policy_digest != input.request.policy_digest
        || observation.configuration_digest != input.request.configuration_digest
        || observation.observed_at != input.observed_at
    {
        return None;
    }
    observation_envelope(&saved, observation)
}

fn observation_envelope(
    saved: &FeedbackSavedEvaluationV1,
    observation: FeedbackCycleObservationV1,
) -> Option<FeedbackObservationEnvelopeV1> {
    let saved_evaluation_digest = canonical_sha256(&(SAVED_EVALUATION_DOMAIN, saved)).ok()?;
    let idempotency_key =
        derive_feedback_observation_idempotency(&saved_evaluation_digest, &observation).ok()?;
    let envelope = FeedbackObservationEnvelopeV1 {
        schema_version: 1,
        producer: "feedback_cycle".to_owned(),
        privacy_class: "operational_no_content".to_owned(),
        idempotency_key,
        saved_evaluation_digest,
        observed_at: observation.observed_at,
        observation: Some(observation),
        source_event: None,
        producer_boot_id: None,
        producer_sequence: None,
        delivery: FeedbackObservationDeliveryV1::pending(),
    };
    envelope.validate()?;
    Some(envelope)
}

/// Maps a content-free owner event onto the same saved-content envelope and
/// idempotency boundary used by generic feedback-cycle observations.
pub fn plan26_feedback_source_event_envelope(
    input: &FeedbackEvaluationInputV1,
    source_event: Plan26FeedbackSourceEventV1,
) -> Option<FeedbackObservationEnvelopeV1> {
    let saved = input.saved().ok()?;
    source_event.validate()?;
    let saved_evaluation_digest = canonical_sha256(&(SAVED_EVALUATION_DOMAIN, saved)).ok()?;
    plan26_feedback_source_event_envelope_for_subject(
        saved_evaluation_digest,
        input.observed_at,
        source_event,
    )
}

pub fn plan26_feedback_source_event_envelope_for_subject(
    subject_digest: ManifestDigest,
    observed_at: UtcMicros,
    source_event: Plan26FeedbackSourceEventV1,
) -> Option<FeedbackObservationEnvelopeV1> {
    subject_digest.validate().ok()?;
    source_event.validate()?;
    let idempotency_key =
        derive_feedback_source_event_idempotency(&subject_digest, observed_at, &source_event)
            .ok()?;
    let envelope = FeedbackObservationEnvelopeV1 {
        schema_version: 1,
        producer: "feedback_source".to_owned(),
        privacy_class: "operational_no_content".to_owned(),
        idempotency_key,
        saved_evaluation_digest: subject_digest,
        observation: None,
        source_event: Some(source_event),
        observed_at,
        producer_boot_id: None,
        producer_sequence: None,
        delivery: FeedbackObservationDeliveryV1::pending(),
    };
    envelope.validate()?;
    Some(envelope)
}

pub trait Plan26FeedbackObservationEmitterV1 {
    fn observe_source_event(
        &self,
        input: &FeedbackEvaluationInputV1,
        source_event: Plan26FeedbackSourceEventV1,
    );

    fn observe_source_event_for_subject(
        &self,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        source_event: Plan26FeedbackSourceEventV1,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackObservationSinkOutcome {
    Enqueued,
    Duplicate,
    Dropped,
}

/// Bounded non-blocking daemon queue boundary. Implementations atomically use
/// `idempotency_key` to converge retries and replay; a plain append-only insert
/// is not conforming. `Dropped` is the explicit bounded-overflow outcome, not
/// permission to block or retry on the feedback path. Durable cursor/projection
/// commit and loss accounting remain daemon-owned.
pub trait Plan26FeedbackObservationQueue {
    fn enqueue_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome;

    /// Replays one previously accepted envelope through the exact same
    /// idempotency boundary. A duplicate outcome is successful convergence.
    fn replay_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        self.enqueue_feedback_observation(envelope)
    }
}

/// Daemon/store-owned durable observation ingress. The sink is responsible for
/// atomically retaining the idempotency key with the queued observation and
/// for preserving replay protection across process restart. This adapter has
/// no database handle, filesystem path, or retry worker of its own.
pub trait DurablePlan26FeedbackObservationSinkV1 {
    fn enqueue_durable_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome;

    fn replay_durable_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        self.enqueue_durable_feedback_observation(envelope)
    }
}

/// Concrete adapter from the durable daemon ingress to the application's
/// non-blocking queue boundary. Corrupt or privacy-invalid values are dropped
/// before the sink receives them, so a replay never turns bad input into state.
pub struct DurablePlan26FeedbackObservationQueueAdapterV1<S> {
    sink: S,
}

impl<S> DurablePlan26FeedbackObservationQueueAdapterV1<S> {
    pub fn new(sink: S) -> Self {
        Self { sink }
    }
}

impl<S> Plan26FeedbackObservationQueue for DurablePlan26FeedbackObservationQueueAdapterV1<S>
where
    S: DurablePlan26FeedbackObservationSinkV1,
{
    fn enqueue_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        if envelope.validate().is_none() {
            FeedbackObservationSinkOutcome::Dropped
        } else {
            self.sink.enqueue_durable_feedback_observation(envelope)
        }
    }

    fn replay_feedback_observation(
        &self,
        envelope: FeedbackObservationEnvelopeV1,
    ) -> FeedbackObservationSinkOutcome {
        if envelope.validate().is_none() {
            FeedbackObservationSinkOutcome::Dropped
        } else {
            self.sink.replay_durable_feedback_observation(envelope)
        }
    }
}

/// Compatibility name for existing root-owned observation sinks.
pub use Plan26FeedbackObservationQueue as FeedbackObservationEventSink;

/// Adapts canonical Plan-26 envelopes to the application's one-way observation
/// port. Observation loss cannot alter feedback truth or trigger a retry cycle.
pub struct Plan26FeedbackObservationAdapter<S> {
    sink: S,
}

impl<S> Plan26FeedbackObservationAdapter<S> {
    pub fn new(sink: S) -> Self {
        Self { sink }
    }
}

impl<S> FeedbackObservationPort for Plan26FeedbackObservationAdapter<S>
where
    S: Plan26FeedbackObservationQueue,
{
    fn observe(&self, input: &FeedbackEvaluationInputV1, observation: FeedbackCycleObservationV1) {
        if let Some(envelope) = feedback_observation_envelope(input, observation) {
            let _ = self.sink.enqueue_feedback_observation(envelope);
        }
    }
}

impl<S> Plan26FeedbackObservationEmitterV1 for Plan26FeedbackObservationAdapter<S>
where
    S: Plan26FeedbackObservationQueue,
{
    fn observe_source_event(
        &self,
        input: &FeedbackEvaluationInputV1,
        source_event: Plan26FeedbackSourceEventV1,
    ) {
        if let Some(envelope) = plan26_feedback_source_event_envelope(input, source_event) {
            let _ = self.sink.enqueue_feedback_observation(envelope);
        }
    }

    fn observe_source_event_for_subject(
        &self,
        subject_digest: ManifestDigest,
        observed_at: UtcMicros,
        source_event: Plan26FeedbackSourceEventV1,
    ) {
        if let Some(envelope) = plan26_feedback_source_event_envelope_for_subject(
            subject_digest,
            observed_at,
            source_event,
        ) {
            let _ = self.sink.enqueue_feedback_observation(envelope);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_application::feedback::FeedbackObservationPort;
    use tracedecay_domain::feedback::{
        FeedbackActorContextV1, FeedbackBudgetV1, FeedbackContentIdentityV1, FeedbackCycleId,
        FeedbackCycleObservationV1, FeedbackCycleRequestV1, FeedbackEvaluationInputV1,
        FeedbackObservationKindV1, FeedbackScopeV1, FeedbackTargetV1, FeedbackTriggerV1,
    };
    use tracedecay_domain::{
        CodeGenerationId, CommitId, FileOccurrenceId, HostInstanceId, ManifestDigest, ProjectId,
        RepositoryId, SessionId, SourceSpan, SymbolOccurrenceId, UtcMicros, WorktreeId,
    };

    use super::*;

    const SHA256_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA256_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn digest(value: &str) -> ManifestDigest {
        ManifestDigest::new(value).unwrap()
    }

    fn scope() -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: id::<ProjectId>("project.feedback.fixture"),
            repository_id: id::<RepositoryId>("repository.feedback.fixture"),
            worktree_id: id::<WorktreeId>("worktree.feedback.fixture"),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: id::<CommitId>("commit.feedback.fixture"),
        }
    }

    fn saved_input() -> FeedbackEvaluationInputV1 {
        let request = FeedbackCycleRequestV1::new(
            id::<FeedbackCycleId>("cycle.feedback.observation"),
            scope(),
            FeedbackContentIdentityV1::SavedContent {
                generation_digest: digest(SHA256_A),
                file_digest: digest(SHA256_B),
            },
            FeedbackTriggerV1::PostEditHook,
            digest(SHA256_A),
            digest(SHA256_B),
            FeedbackBudgetV1::bounded(100, 100, 1_000, 1_000),
        )
        .unwrap();
        FeedbackEvaluationInputV1 {
            request,
            target: FeedbackTargetV1 {
                file: id::<FileOccurrenceId>("file.feedback.observation"),
                span: Some(SourceSpan {
                    start_byte: 1,
                    end_byte: 2,
                }),
                symbol: Some(id::<SymbolOccurrenceId>("symbol.feedback.observation")),
                generation_id: Some(id::<CodeGenerationId>("generation.feedback.observation")),
            },
            actor: FeedbackActorContextV1::default(),
            observed_at: UtcMicros(2_000_000),
        }
    }

    fn overlay_input() -> FeedbackEvaluationInputV1 {
        let session_id = id::<SessionId>("session.feedback.observation");
        let client_id = id::<HostInstanceId>("client.feedback.observation");
        let request = FeedbackCycleRequestV1::new(
            id::<FeedbackCycleId>("cycle.feedback.overlay-observation"),
            scope(),
            FeedbackContentIdentityV1::EphemeralOverlay {
                session_id: session_id.clone(),
                owner_client_id: client_id.clone(),
                agent_id: None,
                document_version: 1,
                overlay_digest: digest(SHA256_A),
            },
            FeedbackTriggerV1::DocumentSave,
            digest(SHA256_A),
            digest(SHA256_B),
            FeedbackBudgetV1::bounded(100, 100, 1_000, 1_000),
        )
        .unwrap();
        FeedbackEvaluationInputV1 {
            request,
            target: FeedbackTargetV1 {
                file: id::<FileOccurrenceId>("file.feedback.overlay-observation"),
                span: None,
                symbol: None,
                generation_id: None,
            },
            actor: FeedbackActorContextV1 {
                session_id: Some(session_id),
                client_id: Some(client_id),
                agent_id: None,
                turn_id: None,
            },
            observed_at: UtcMicros(2_000_000),
        }
    }

    fn overlay_trigger(input: &FeedbackEvaluationInputV1) -> FeedbackCycleObservationV1 {
        FeedbackCycleObservationV1 {
            cycle_id: input.request.cycle_id.clone(),
            scope: input.request.scope.clone(),
            policy_digest: input.request.policy_digest.clone(),
            configuration_digest: input.request.configuration_digest.clone(),
            kind: FeedbackObservationKindV1::Trigger,
            stage: None,
            termination: None,
            dedupe_key: None,
            observed_at: input.observed_at,
            latency_micros: None,
            advisory_only: true,
        }
    }

    #[derive(Clone, Default)]
    struct RecordingSink(Rc<RefCell<Vec<FeedbackObservationEnvelopeV1>>>);

    impl FeedbackObservationEventSink for RecordingSink {
        fn enqueue_feedback_observation(
            &self,
            envelope: FeedbackObservationEnvelopeV1,
        ) -> FeedbackObservationSinkOutcome {
            if self
                .0
                .borrow()
                .iter()
                .any(|record| record.idempotency_key == envelope.idempotency_key)
            {
                return FeedbackObservationSinkOutcome::Duplicate;
            }
            self.0.borrow_mut().push(envelope);
            FeedbackObservationSinkOutcome::Enqueued
        }
    }

    #[derive(Clone, Default)]
    struct DroppingSink(Rc<Cell<usize>>);

    impl FeedbackObservationEventSink for DroppingSink {
        fn enqueue_feedback_observation(
            &self,
            _envelope: FeedbackObservationEnvelopeV1,
        ) -> FeedbackObservationSinkOutcome {
            self.0.set(self.0.get() + 1);
            FeedbackObservationSinkOutcome::Dropped
        }
    }

    #[derive(Clone, Default)]
    struct RestartSafeSink {
        replay_identities: Arc<Mutex<BTreeSet<String>>>,
        calls: Arc<AtomicUsize>,
    }

    impl DurablePlan26FeedbackObservationSinkV1 for RestartSafeSink {
        fn enqueue_durable_feedback_observation(
            &self,
            envelope: FeedbackObservationEnvelopeV1,
        ) -> FeedbackObservationSinkOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let identity = envelope.replay_identity().unwrap().to_owned();
            if self.replay_identities.lock().unwrap().insert(identity) {
                FeedbackObservationSinkOutcome::Enqueued
            } else {
                FeedbackObservationSinkOutcome::Duplicate
            }
        }

        fn replay_durable_feedback_observation(
            &self,
            envelope: FeedbackObservationEnvelopeV1,
        ) -> FeedbackObservationSinkOutcome {
            self.enqueue_durable_feedback_observation(envelope)
        }
    }

    #[test]
    fn durable_observation_mapping_is_replay_stable_and_sink_suppresses_overlays() {
        let input = saved_input();
        let observation = FeedbackCycleObservationV1::trigger(&input).unwrap();
        let first = feedback_observation_envelope(&input, observation.clone()).unwrap();
        let replay = feedback_observation_envelope(&input, observation.clone()).unwrap();
        assert_eq!(first, replay);
        assert_eq!(first.schema_version, 1);
        assert_eq!(first.producer, "feedback_cycle");
        assert_eq!(first.privacy_class, "operational_no_content");

        let sink = RecordingSink::default();
        let recorded = sink.0.clone();
        let adapter = Plan26FeedbackObservationAdapter::new(sink);
        adapter.observe(&input, observation.clone());
        adapter.observe(&input, observation);
        let overlay = overlay_input();
        adapter.observe(&overlay, overlay_trigger(&overlay));
        assert_eq!(recorded.borrow().as_slice(), &[first]);
    }

    #[test]
    fn source_events_are_content_free_and_replay_stable() {
        let input = saved_input();
        let event = Plan26FeedbackSourceEventV1::CiLocalization {
            outcome: Plan26FeedbackOutcomeV1::Partial,
            provider: Plan26CiProviderV1::GitHubActions,
            exact_evidence: true,
            coverage: Plan26CoverageV1::Partial,
            source_degradation: Some(CiFailureSourceDegradationV1::Failed(
                tracedecay_domain::feedback::CiFailureSourceFailureV1::Schema,
            )),
            localized_count: 2,
            candidate_count: 3,
            duration_micros: Some(42),
        };
        let first = plan26_feedback_source_event_envelope(&input, event.clone()).unwrap();
        let replay = plan26_feedback_source_event_envelope(&input, event).unwrap();
        assert_eq!(first, replay);
        assert_eq!(first.producer, "feedback_source");
        assert!(first.observation.is_none());
        assert_eq!(
            first
                .source_event
                .as_ref()
                .map(Plan26FeedbackSourceEventV1::event_kind),
            Some("feedback.ci.localization.observed.v1")
        );
        let encoded = serde_json::to_string(&first).unwrap();
        assert!(encoded.contains("\"source_degradation\":{\"failed\":\"schema\"}"));
        assert!(!encoded.contains("file.feedback.observation"));
        assert!(!encoded.contains("symbol.feedback.observation"));
    }

    #[test]
    fn github_lifecycle_ingress_rate_limit_and_stale_are_distinct_events() {
        let input = saved_input();
        let events = [
            Plan26FeedbackSourceEventV1::GitHubLifecycle {
                lifecycle: Plan26GitHubLifecycleV1::Outdated,
                item_count: 1,
            },
            Plan26FeedbackSourceEventV1::GitHubIngress {
                outcome: Plan26FeedbackOutcomeV1::Partial,
                item_count: 1,
                duration_micros: None,
            },
            Plan26FeedbackSourceEventV1::GitHubRateLimit {
                duration_micros: Some(1_000),
            },
            Plan26FeedbackSourceEventV1::GitHubStale { item_count: 1 },
        ];
        let kinds = events
            .into_iter()
            .map(|event| {
                plan26_feedback_source_event_envelope(&input, event)
                    .unwrap()
                    .source_event
                    .unwrap()
                    .event_kind()
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(kinds.len(), 4);
    }

    #[test]
    fn advisory_provider_state_is_orthogonal_and_content_free() {
        let input = saved_input();
        let events = [
            Plan26FeedbackSourceEventV1::GitHubLifecycle {
                lifecycle: Plan26GitHubLifecycleV1::Current,
                item_count: 1,
            },
            Plan26FeedbackSourceEventV1::GitHubIngress {
                outcome: Plan26FeedbackOutcomeV1::Completed,
                item_count: 1,
                duration_micros: None,
            },
            Plan26FeedbackSourceEventV1::ProviderState {
                provider: Plan26AdvisoryProviderV1::GitHubReview,
                state: tracedecay_domain::feedback::ProviderEvaluationStateV1::Partial,
            },
        ];
        let envelopes = events
            .into_iter()
            .map(|event| plan26_feedback_source_event_envelope(&input, event).unwrap())
            .collect::<Vec<_>>();
        let kinds = envelopes
            .iter()
            .map(|envelope| envelope.event_kind().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(kinds.len(), 3);

        let provider = serde_json::to_string(&envelopes[2]).unwrap();
        assert!(provider.contains("\"provider\":\"github_review\""));
        assert!(provider.contains("\"state\":\"partial\""));
        assert!(!provider.contains("\"path\""));
        assert!(!provider.contains("\"source\""));
        assert!(!provider.contains("\"message\""));
        assert!(!provider.contains("\"payload\""));
    }

    #[test]
    fn lsp_state_observation_is_bounded_and_content_free() {
        let input = saved_input();
        let envelope = plan26_feedback_source_event_envelope(
            &input,
            Plan26FeedbackSourceEventV1::LspState {
                state: Plan26LspStateV1::DiagnosticPublished,
                method: Some(Plan26LspMethodClassV1::Diagnostics),
                outcome: Plan26FeedbackOutcomeV1::Partial,
                item_count: 3,
                duration_micros: Some(17),
            },
        )
        .unwrap();

        assert_eq!(
            envelope.event_kind(),
            Some("feedback.lsp.state.observed.v1")
        );
        let encoded = serde_json::to_string(&envelope).unwrap();
        assert!(!encoded.contains("file:///"));
        assert!(!encoded.contains("\"path\""));
        assert!(!encoded.contains("\"source\""));
        assert!(!encoded.contains("\"message\""));
        assert!(!encoded.contains("\"payload\""));
    }

    #[test]
    fn rejected_argument_observation_keeps_only_normalized_metadata() {
        let input = saved_input();
        let envelope = plan26_feedback_source_event_envelope(
            &input,
            Plan26FeedbackSourceEventV1::SurfaceArgumentRejected {
                operation: Plan26FeedbackOperationV1::FeedbackDiagnostics,
                route: Some(Plan26DeliveryRouteV1::Http),
                argument: Plan26RejectedArgumentV1::RequestBody,
                rejection: Plan26ArgumentRejectionClassV1::InvalidShape,
                schema_revision: 1,
                outcome: Plan26FeedbackOutcomeV1::Rejected,
            },
        )
        .unwrap();

        let encoded = serde_json::to_string(&envelope).unwrap();
        assert!(encoded.contains("\"argument\":\"request_body\""));
        assert!(encoded.contains("\"rejection\":\"invalid_shape\""));
        assert!(encoded.contains("\"schema_revision\":1"));
        assert!(!encoded.contains("\"value\""));
        assert!(!encoded.contains("\"raw\""));
    }

    #[test]
    fn read_model_preserves_denominators_and_event_families() {
        let input = saved_input();
        let observations = vec![
            feedback_observation_envelope(
                &input,
                FeedbackCycleObservationV1::trigger(&input).unwrap(),
            )
            .unwrap(),
            plan26_feedback_source_event_envelope(
                &input,
                Plan26FeedbackSourceEventV1::Delivery {
                    operation: Plan26FeedbackOperationV1::FeedbackList,
                    route: Plan26DeliveryRouteV1::Mcp,
                    outcome: Plan26FeedbackOutcomeV1::Completed,
                    item_count: 2,
                    duration_micros: Some(10),
                },
            )
            .unwrap(),
            plan26_feedback_source_event_envelope(
                &input,
                Plan26FeedbackSourceEventV1::Delivery {
                    operation: Plan26FeedbackOperationV1::FeedbackList,
                    route: Plan26DeliveryRouteV1::Http,
                    outcome: Plan26FeedbackOutcomeV1::Completed,
                    item_count: 2,
                    duration_micros: Some(12),
                },
            )
            .unwrap(),
            plan26_feedback_source_event_envelope(
                &input,
                Plan26FeedbackSourceEventV1::SseLifecycle {
                    lifecycle: Plan26SseLifecycleV1::Gap,
                    sequence: Some(7),
                    item_count: 0,
                    duration_micros: None,
                },
            )
            .unwrap(),
        ];
        let model = FeedbackObservationReadModelV1::project(&observations).unwrap();
        assert_eq!(model.total_count, 4);
        assert_eq!(
            model.event_counts.get("feedback.cycle.triggered.v1"),
            Some(&1)
        );
        assert_eq!(
            model.event_counts.get("feedback.delivery.observed.v1"),
            Some(&2)
        );
        assert_eq!(
            model.event_counts.get("feedback.sse.lifecycle.observed.v1"),
            Some(&1)
        );
        assert_eq!(model.first_observed_at, Some(input.observed_at));
        assert_eq!(model.last_observed_at, Some(input.observed_at));
    }

    #[test]
    fn read_model_exposes_delivery_denominators_watermark_and_unknown_boot_gap() {
        let input = saved_input();
        let mut envelope = feedback_observation_envelope(
            &input,
            FeedbackCycleObservationV1::trigger(&input).unwrap(),
        )
        .unwrap();
        let boot_id = canonical_sha256(&"feedback-observation-boot").unwrap();
        assert!(
            envelope
                .assign_delivery(
                    boot_id.clone(),
                    7,
                    FeedbackObservationDeliveryV1::delivered(2),
                )
                .is_some()
        );

        let model =
            FeedbackObservationReadModelV1::project_with_accounting(&[envelope], 3, 1).unwrap();
        assert_eq!(model.coverage, Plan26CoverageV1::Unknown);
        assert_eq!(model.watermark.producer_boot_id, Some(boot_id));
        assert_eq!(model.watermark.producer_sequence, Some(7));
        assert_eq!(model.denominators.persisted, 1);
        assert_eq!(model.denominators.emitted, 1);
        assert_eq!(model.denominators.dropped, 2);
        assert_eq!(model.denominators.retention_dropped, 3);
        assert_eq!(model.denominators.incomplete_boots, 1);
        assert_eq!(model.denominators.eligible, 6);
    }

    #[test]
    fn system_quality_projection_is_denominator_safe_and_complete() {
        let input = saved_input();
        let source = |event| plan26_feedback_source_event_envelope(&input, event).unwrap();
        let observations = vec![
            source(Plan26FeedbackSourceEventV1::RelevanceFeedback {
                disposition: Plan26RelevanceDispositionV1::Helpful,
            }),
            source(Plan26FeedbackSourceEventV1::EvidenceDiversity {
                eligible_source_families: 3,
                represented_source_families: 2,
                selected_count: 4,
            }),
            source(Plan26FeedbackSourceEventV1::Delivery {
                operation: Plan26FeedbackOperationV1::FeedbackList,
                route: Plan26DeliveryRouteV1::Http,
                outcome: Plan26FeedbackOutcomeV1::Denied,
                item_count: 0,
                duration_micros: Some(90),
            }),
            source(Plan26FeedbackSourceEventV1::Truncation {
                operation: Plan26FeedbackOperationV1::FeedbackList,
                returned_count: 8,
                omitted_count: 2,
            }),
            source(Plan26FeedbackSourceEventV1::AuthorizationRevoked {
                operation: Plan26FeedbackOperationV1::FeedbackGet,
                outcome: Plan26FeedbackOutcomeV1::Completed,
                propagation_micros: 40,
            }),
            source(Plan26FeedbackSourceEventV1::StackTransition {
                transition: Plan26StackTransitionV1::BaseDrifted,
                outcome: Plan26FeedbackOutcomeV1::Completed,
            }),
            source(Plan26FeedbackSourceEventV1::GitHubStale { item_count: 1 }),
        ];

        let projected = FeedbackObservationReadModelV1::project(&observations).unwrap();
        assert_eq!(projected.system_quality.metrics.len(), 9);
        let metric = |kind| {
            projected
                .system_quality
                .metrics
                .iter()
                .find(|metric| metric.metric == kind)
                .unwrap()
        };
        assert_eq!(
            metric(FeedbackSystemMetricKindV1::Relevance).value,
            Some(1.0)
        );
        assert_eq!(
            metric(FeedbackSystemMetricKindV1::Diversity).value,
            Some(2.0 / 3.0)
        );
        assert_eq!(
            metric(FeedbackSystemMetricKindV1::Latency).value,
            Some(90.0)
        );
        assert_eq!(
            metric(FeedbackSystemMetricKindV1::Omission).value,
            Some(0.2)
        );
        assert_eq!(metric(FeedbackSystemMetricKindV1::Denial).value, Some(0.25));
        assert_eq!(
            metric(FeedbackSystemMetricKindV1::Staleness).value,
            Some(0.25)
        );
        assert_eq!(
            metric(FeedbackSystemMetricKindV1::RevocationPropagation).value,
            Some(40.0)
        );
        assert_eq!(
            metric(FeedbackSystemMetricKindV1::StackTransitions).value,
            Some(1.0)
        );
    }

    #[test]
    fn unsupported_system_metrics_never_fabricate_zero() {
        let model = FeedbackObservationReadModelV1::project(&[]).unwrap();
        assert!(
            model
                .system_quality
                .metrics
                .iter()
                .all(|metric| metric.value.is_none()
                    && metric.denominator.is_none()
                    && metric.coverage == Plan26CoverageV1::Unknown)
        );
    }

    #[test]
    fn observation_envelope_replay_is_stable_and_drop_never_retries() {
        let input = saved_input();
        let observation = FeedbackCycleObservationV1::trigger(&input).unwrap();
        let envelope = feedback_observation_envelope(&input, observation.clone()).unwrap();
        assert!(envelope.validate().is_some());

        let encoded = serde_json::to_string(&envelope).unwrap();
        let replay: FeedbackObservationEnvelopeV1 = serde_json::from_str(&encoded).unwrap();
        assert_eq!(replay, envelope);
        assert_eq!(replay.replay_identity(), envelope.replay_identity());

        let dropping = DroppingSink::default();
        let dropped = dropping.0.clone();
        let adapter = Plan26FeedbackObservationAdapter::new(dropping);
        adapter.observe(&input, observation);
        assert_eq!(dropped.get(), 1);
    }

    #[test]
    fn durable_sink_adapter_replays_across_restart_and_rejects_corruption() {
        let input = saved_input();
        let envelope = feedback_observation_envelope(
            &input,
            FeedbackCycleObservationV1::trigger(&input).unwrap(),
        )
        .unwrap();
        let sink = RestartSafeSink::default();
        let first_adapter = DurablePlan26FeedbackObservationQueueAdapterV1::new(sink.clone());
        assert_eq!(
            first_adapter.enqueue_feedback_observation(envelope.clone()),
            FeedbackObservationSinkOutcome::Enqueued
        );

        let restarted_adapter = DurablePlan26FeedbackObservationQueueAdapterV1::new(sink.clone());
        assert_eq!(
            restarted_adapter.replay_feedback_observation(envelope.clone()),
            FeedbackObservationSinkOutcome::Duplicate
        );
        assert_eq!(sink.calls.load(Ordering::SeqCst), 2);

        let mut corrupted = envelope;
        corrupted.schema_version = 0;
        assert_eq!(
            restarted_adapter.enqueue_feedback_observation(corrupted),
            FeedbackObservationSinkOutcome::Dropped
        );
        assert_eq!(
            sink.calls.load(Ordering::SeqCst),
            2,
            "corrupt envelopes must not reach a durable sink"
        );
    }
}
