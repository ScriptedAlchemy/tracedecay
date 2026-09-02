//! Plan-26 canonical feedback observation wire vocabulary.
//!
//! The privacy-safe operation/outcome enums, source events, and the durable
//! observation envelope live here so wire-facing crates (daemon protocol,
//! dashboard, application surfaces) consume them without depending on the
//! feedback engine. Envelope mapping, read models, and emitter/queue ports
//! remain in `tracedecay-usecases`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::feedback::{
    CiFailureSourceDegradationV1, FeedbackCycleObservationV1, FeedbackObservationKindV1,
    ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    ManifestDigest, RejectedArgumentErrorClassV1, RejectedArgumentNameV1,
    RejectedArgumentSurfaceV1, UtcMicros, canonical_sha256,
};

const OBSERVATION_ENVELOPE_DOMAIN: &str = "tracedecay.feedback.observation.v1";
const SOURCE_EVENT_ENVELOPE_DOMAIN: &str = "tracedecay.feedback.source-event.v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackOperationV1 {
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
pub enum FeedbackOutcomeV1 {
    Accepted,
    Admitted,
    Rejected,
    AtCapacity,
    Completed,
    Unavailable,
    ResetRequired,
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
pub enum FeedbackDeliveryRouteV1 {
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
pub enum FeedbackRejectedArgumentV1 {
    RequestBody,
    Pagination,
    RequestHandle,
    Operation,
    Lifecycle,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackArgumentRejectionClassV1 {
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
pub enum FeedbackLspMethodClassV1 {
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
pub enum FeedbackLspStateV1 {
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
pub enum FeedbackGitHubLifecycleV1 {
    Current,
    Outdated,
    Resolved,
    Edited,
    Deleted,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackAdvisoryProviderV1 {
    #[serde(rename = "github_review")]
    GitHubReview,
    CiLocalization,
    Proximity,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCiProviderV1 {
    GitHubActions,
    RetainedObservation,
    ExactCodeEvidence,
}

#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCoverageV1 {
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
pub enum FeedbackRelevanceDispositionV1 {
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
pub enum FeedbackStackTransitionV1 {
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
    pub coverage: FeedbackCoverageV1,
}

impl FeedbackObservationDeliveryV1 {
    #[hotpath::skip]
    pub const fn pending() -> Self {
        Self {
            emitted: 0,
            delayed: 0,
            dropped: 0,
            coverage: FeedbackCoverageV1::Unknown,
        }
    }

    #[hotpath::skip]
    pub const fn delivered(dropped: u64) -> Self {
        Self {
            emitted: 1,
            delayed: 0,
            dropped,
            coverage: if dropped == 0 {
                FeedbackCoverageV1::Known
            } else {
                FeedbackCoverageV1::Partial
            },
        }
    }

    fn validate(&self, persisted: bool) -> Option<()> {
        match self.coverage {
            FeedbackCoverageV1::Known
                if (!persisted || self.emitted == 1) && self.delayed == 0 && self.dropped == 0 =>
            {
                Some(())
            }
            FeedbackCoverageV1::Partial
                if (!persisted || self.emitted == 1) && (self.delayed > 0 || self.dropped > 0) =>
            {
                Some(())
            }
            FeedbackCoverageV1::Unknown if !persisted && self.emitted == 0 => Some(()),
            FeedbackCoverageV1::Sampled
            | FeedbackCoverageV1::Capped
            | FeedbackCoverageV1::Stale
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
pub enum FeedbackProximityTransitionV1 {
    Emitted,
    Suppressed,
    Expired,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackProximityRiskV1 {
    None,
    BelowThreshold,
    AtOrAboveThreshold,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackAnchorOperationV1 {
    Anchor,
    HandleExpansion,
    EvidenceExpansion,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackHookScoutPhaseV1 {
    Admission,
    Delivery,
    FeedbackTerminal,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackSseLifecycleV1 {
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

/// Closed feedback source-event family. All values are bounded enums,
/// digests, counts, or durations; provider payloads never enter this type.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum FeedbackSourceEventV1 {
    ArgumentRejected {
        operation: FeedbackOperationV1,
        outcome: FeedbackOutcomeV1,
    },
    SurfaceArgumentRejected {
        operation: FeedbackOperationV1,
        route: Option<FeedbackDeliveryRouteV1>,
        argument: FeedbackRejectedArgumentV1,
        rejection: FeedbackArgumentRejectionClassV1,
        schema_revision: u16,
        outcome: FeedbackOutcomeV1,
    },
    LspState {
        state: FeedbackLspStateV1,
        method: Option<FeedbackLspMethodClassV1>,
        outcome: FeedbackOutcomeV1,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    Dispatch {
        operation: FeedbackOperationV1,
        outcome: FeedbackOutcomeV1,
        capacity: u32,
        admitted: u32,
    },
    Delivery {
        operation: FeedbackOperationV1,
        route: FeedbackDeliveryRouteV1,
        outcome: FeedbackOutcomeV1,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    Truncation {
        operation: FeedbackOperationV1,
        returned_count: u32,
        omitted_count: u32,
    },
    RelevanceFeedback {
        disposition: FeedbackRelevanceDispositionV1,
    },
    EvidenceDiversity {
        eligible_source_families: u32,
        represented_source_families: u32,
        selected_count: u32,
    },
    AnchorExpansion {
        operation: FeedbackAnchorOperationV1,
        outcome: FeedbackOutcomeV1,
        returned_count: u32,
        duration_micros: Option<u64>,
    },
    GitHubLifecycle {
        lifecycle: FeedbackGitHubLifecycleV1,
        item_count: u32,
    },
    GitHubIngress {
        outcome: FeedbackOutcomeV1,
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
        provider: FeedbackAdvisoryProviderV1,
        state: ProviderEvaluationStateV1,
    },
    CiLocalization {
        outcome: FeedbackOutcomeV1,
        provider: FeedbackCiProviderV1,
        exact_evidence: bool,
        coverage: FeedbackCoverageV1,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_degradation: Option<CiFailureSourceDegradationV1>,
        localized_count: u32,
        candidate_count: u32,
        duration_micros: Option<u64>,
    },
    Proximity {
        transition: FeedbackProximityTransitionV1,
        risk: FeedbackProximityRiskV1,
        configuration_revision: ManifestDigest,
        candidate_count: u32,
        affected_count: u32,
    },
    HostDelivery {
        route: FeedbackDeliveryRouteV1,
        outcome: FeedbackOutcomeV1,
        rollback: bool,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    HookScout {
        route: FeedbackDeliveryRouteV1,
        phase: FeedbackHookScoutPhaseV1,
        outcome: FeedbackOutcomeV1,
        item_count: u32,
        duration_micros: Option<u64>,
    },
    Cancellation {
        operation: FeedbackOperationV1,
        outcome: FeedbackOutcomeV1,
    },
    AuthorizationRevoked {
        operation: FeedbackOperationV1,
        outcome: FeedbackOutcomeV1,
        propagation_micros: u64,
    },
    StackTransition {
        transition: FeedbackStackTransitionV1,
        outcome: FeedbackOutcomeV1,
    },
    SseLifecycle {
        lifecycle: FeedbackSseLifecycleV1,
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

impl FeedbackSourceEventV1 {
    #[hotpath::skip]
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

pub fn rejected_argument_cell(
    event: &FeedbackSourceEventV1,
) -> Option<(
    RejectedArgumentSurfaceV1,
    String,
    RejectedArgumentNameV1,
    RejectedArgumentErrorClassV1,
)> {
    match event {
        FeedbackSourceEventV1::SurfaceArgumentRejected {
            operation,
            route,
            argument,
            rejection,
            ..
        } => Some((
            match route {
                Some(FeedbackDeliveryRouteV1::Cli) => RejectedArgumentSurfaceV1::Cli,
                Some(FeedbackDeliveryRouteV1::Mcp) => RejectedArgumentSurfaceV1::Mcp,
                Some(FeedbackDeliveryRouteV1::Http) => RejectedArgumentSurfaceV1::Http,
                Some(
                    FeedbackDeliveryRouteV1::Lsp
                    | FeedbackDeliveryRouteV1::HookV2
                    | FeedbackDeliveryRouteV1::HookLegacy
                    | FeedbackDeliveryRouteV1::Scout,
                )
                | None => RejectedArgumentSurfaceV1::Unknown,
            },
            serde_variant_name(operation),
            match argument {
                FeedbackRejectedArgumentV1::RequestBody => RejectedArgumentNameV1::RequestBody,
                FeedbackRejectedArgumentV1::Pagination => RejectedArgumentNameV1::Pagination,
                FeedbackRejectedArgumentV1::RequestHandle => RejectedArgumentNameV1::RequestHandle,
                FeedbackRejectedArgumentV1::Operation => RejectedArgumentNameV1::Operation,
                FeedbackRejectedArgumentV1::Lifecycle => RejectedArgumentNameV1::Lifecycle,
                FeedbackRejectedArgumentV1::Unknown => RejectedArgumentNameV1::Unknown,
            },
            match rejection {
                FeedbackArgumentRejectionClassV1::Missing => RejectedArgumentErrorClassV1::Missing,
                FeedbackArgumentRejectionClassV1::InvalidShape => {
                    RejectedArgumentErrorClassV1::InvalidShape
                }
                FeedbackArgumentRejectionClassV1::OutOfBounds => {
                    RejectedArgumentErrorClassV1::OutOfBounds
                }
                FeedbackArgumentRejectionClassV1::Unsupported => {
                    RejectedArgumentErrorClassV1::Unsupported
                }
                FeedbackArgumentRejectionClassV1::Unauthorized => {
                    RejectedArgumentErrorClassV1::Unauthorized
                }
                FeedbackArgumentRejectionClassV1::Stale => RejectedArgumentErrorClassV1::Stale,
                FeedbackArgumentRejectionClassV1::Unknown => RejectedArgumentErrorClassV1::Unknown,
            },
        )),
        FeedbackSourceEventV1::ArgumentRejected { operation, .. } => Some((
            RejectedArgumentSurfaceV1::Unknown,
            serde_variant_name(operation),
            RejectedArgumentNameV1::Unknown,
            RejectedArgumentErrorClassV1::Unknown,
        )),
        _ => None,
    }
}

fn serde_variant_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => "unknown".to_owned(),
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
    pub source_event: Option<FeedbackSourceEventV1>,
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
