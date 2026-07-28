//! Canonical, payload-safe Plan 26 observability contracts.

pub mod accounting {
    /// A single parsed turn from a Claude Code session transcript, ready for
    /// insertion into the `turns` accounting projection.
    pub struct CostTurn {
        pub message_id: String,
        pub project_hash: String,
        pub session_id: String,
        pub model: String,
        pub timestamp: u64,
        pub input_tokens: u64,
        pub output_tokens: u64,
        pub cache_write_tokens: u64,
        pub cache_read_tokens: u64,
        pub cost_usd: f64,
        pub category: String,
        pub tool_names: String,
    }
}

pub use accounting::CostTurn;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStateV1 {
    Known,
    Partial,
    Stale,
    Unknown,
    Sampled,
    Capped,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservabilityRetentionClassV1 {
    OptionalLocalDetail30d,
    LocalRollup395d,
    ProductReceipt,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservabilityTerminalResultV1 {
    Succeeded,
    Abstained,
    Denied,
    Cancelled,
    TimedOut,
    Failed,
    Partial,
    Unknown,
}

/// Common envelope persisted by the single observability authority.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ObservabilityEnvelopeV1 {
    pub event_id: String,
    pub event_kind: String,
    pub schema_revision: u32,
    pub idempotency_key: String,
    pub trace_id: String,
    pub scope_ref: String,
    pub capability: String,
    pub operation: String,
    pub event_time_micros: i64,
    pub observation_time_micros: i64,
    pub valid_from_micros: Option<i64>,
    pub valid_until_micros: Option<i64>,
    pub quantity: Option<f64>,
    pub unit: Option<String>,
    pub terminal_result: Option<ObservabilityTerminalResultV1>,
    pub producer_revision: String,
    pub configuration_revision: String,
    pub policy_revision: String,
    pub watermark: String,
    pub coverage: CoverageStateV1,
    pub sampling_probability: Option<f64>,
    pub retention_class: ObservabilityRetentionClassV1,
    pub emitted_count: u64,
    pub delayed_count: u64,
    pub dropped_count: u64,
    pub process_boot_id: String,
    pub producer_sequence: u64,
    pub payload: ObservabilityPayloadV1,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ObservabilityPayloadV1 {
    RetrievalQuery(RetrievalQueryObservedV1),
    RetrievalPlanner(RetrievalPlannerObservedV1),
    Retriever(RetrieverObservedV1),
    RetrievalSynthesis(RetrievalSynthesisObservedV1),
    RetrievalSource(RetrievalSourceObservedV1),
    ContextOutcome(ContextOutcomeObservedV1),
    RetrievalAblation(RetrievalAblationObservedV1),
    AdoptionEligibility(AdoptionEligibilityObservedV1),
    AdoptionOutcome(AdoptionOutcomeLinkedV1),
    AnalyticsConsent(AnalyticsConsentChangedV1),
    OperationResource(OperationResourceObservedV1),
    TelemetryDrop(TelemetryDropObservedV1),
    HealthSnapshot(HealthSnapshotObservedV1),
    Activity(ActivityObservedV1),
}

impl ObservabilityPayloadV1 {
    pub const fn event_kind(&self) -> &'static str {
        match self {
            Self::RetrievalQuery(_) => "retrieval.query.observed.v1",
            Self::RetrievalPlanner(_) => "retrieval.planner.observed.v1",
            Self::Retriever(_) => "retriever.observed.v1",
            Self::RetrievalSynthesis(_) => "retrieval.synthesis.observed.v1",
            Self::RetrievalSource(_) => "retrieval.source.observed.v1",
            Self::ContextOutcome(_) => "context.outcome.observed.v1",
            Self::RetrievalAblation(_) => "retrieval.ablation.observed.v1",
            Self::AdoptionEligibility(_) => "adoption.eligibility.observed.v1",
            Self::AdoptionOutcome(_) => "adoption.outcome.linked.v1",
            Self::AnalyticsConsent(_) => "analytics.consent.changed.v1",
            Self::OperationResource(_) => "operation.resource.observed.v1",
            Self::TelemetryDrop(_) => "telemetry.drop.observed.v1",
            Self::HealthSnapshot(_) => "health.snapshot.observed.v1",
            Self::Activity(_) => "activity.observed.v1",
        }
    }
}

impl ObservabilityEnvelopeV1 {
    /// Rejects envelopes that cannot be projected without inventing source,
    /// time, sampling, or event-type semantics.
    pub fn validate(&self) -> Result<(), &'static str> {
        for value in [
            self.event_id.as_str(),
            self.idempotency_key.as_str(),
            self.trace_id.as_str(),
            self.scope_ref.as_str(),
            self.capability.as_str(),
            self.operation.as_str(),
            self.producer_revision.as_str(),
            self.configuration_revision.as_str(),
            self.policy_revision.as_str(),
            self.watermark.as_str(),
            self.process_boot_id.as_str(),
        ] {
            if value.is_empty()
                || value.len() > 512
                || value.trim() != value
                || value.chars().any(char::is_control)
            {
                return Err("identifier");
            }
        }
        if self.schema_revision != 1 || self.event_kind != self.payload.event_kind() {
            return Err("event_kind");
        }
        if self.observation_time_micros < self.event_time_micros
            || self
                .valid_until_micros
                .zip(self.valid_from_micros)
                .is_some_and(|(until, from)| until < from)
        {
            return Err("temporal_range");
        }
        match (self.coverage, self.sampling_probability) {
            (CoverageStateV1::Sampled, Some(probability))
                if probability.is_finite() && probability > 0.0 && probability <= 1.0 => {}
            (CoverageStateV1::Sampled, _) => return Err("sampling_probability"),
            (_, Some(_)) => return Err("sampling_probability"),
            _ => {}
        }
        if self.quantity.is_some_and(|value| !value.is_finite()) {
            return Err("quantity");
        }
        match &self.payload {
            ObservabilityPayloadV1::Activity(activity) => {
                if activity.units == 0
                    || !matches!(
                        activity.family.as_str(),
                        "hook" | "session_ingest" | "code_index" | "tool_call"
                    )
                    || activity.detail.as_deref().is_some_and(|detail| {
                        detail.is_empty()
                            || detail.len() > 128
                            || detail.trim() != detail
                            || detail.chars().any(char::is_control)
                    })
                {
                    return Err("activity");
                }
            }
            ObservabilityPayloadV1::HealthSnapshot(snapshot)
                if snapshot.scope_digest != self.scope_ref
                    || snapshot.dimensions.is_empty()
                    || snapshot.dimensions.len() > 16
                    || snapshot.dimensions.iter().any(|(name, dimension)| {
                        !matches!(
                            name.as_str(),
                            "acyclicity"
                                | "depth"
                                | "equality"
                                | "redundancy"
                                | "modularity"
                                | "coverage_discipline"
                        ) || dimension.score_ppm > 1_000_000
                    }) =>
            {
                return Err("health_snapshot");
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalQueryObservedV1 {
    pub query_family: String,
    pub enabled_lanes: Vec<String>,
    pub candidate_budget: u64,
    pub context_budget: u64,
    pub token_budget: u64,
    pub answered: bool,
    pub source_coverage: CoverageStateV1,
    pub lane_coverage: CoverageStateV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalPlannerObservedV1 {
    pub planner_revision: String,
    pub requested_lanes: Vec<String>,
    pub admitted_lanes: Vec<String>,
    pub abstained: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrieverObservedV1 {
    pub retriever_kind: String,
    pub profile_revision: String,
    pub requested_candidates: u64,
    pub consumed_candidates: u64,
    pub eligible_candidates: u64,
    pub returned_candidates: u64,
    pub unique_contributions: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalSynthesisObservedV1 {
    pub candidate_count: u64,
    pub context_count: u64,
    pub context_tokens: u64,
    pub abstained: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetrievalSourceObservedV1 {
    pub source_kind: String,
    pub eligible: u64,
    pub observed: u64,
    pub denied: u64,
    pub unknown: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextOutcomeObservedV1 {
    pub outcome: String,
    pub independently_observed: bool,
    pub censored: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RetrievalAblationObservedV1 {
    pub descriptor_revision: String,
    pub baseline_value: f64,
    pub candidate_value: f64,
    pub unit: String,
    pub coverage: CoverageStateV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdoptionEligibilityObservedV1 {
    pub capability: String,
    pub eligible: u64,
    pub enabled: u64,
    pub available: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AdoptionOutcomeLinkedV1 {
    pub invoked: u64,
    pub terminal: u64,
    pub independently_useful: u64,
    pub repeat_useful: u64,
    pub censored: u64,
    pub unknown: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsModeV1 {
    Off,
    LocalOnly,
    AggregateShare,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnalyticsConsentChangedV1 {
    pub previous: AnalyticsModeV1,
    pub current: AnalyticsModeV1,
    pub share_staging_age_seconds: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OperationResourceObservedV1 {
    pub scheduled_latency_micros: u64,
    pub service_latency_micros: u64,
    pub process_rss_bytes: Option<u64>,
    pub process_pss_bytes: Option<u64>,
    pub cpu_user_micros: Option<u64>,
    pub cpu_system_micros: Option<u64>,
    pub read_bytes: Option<u64>,
    pub write_bytes: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_amount: Option<f64>,
    pub cost_currency: Option<String>,
    pub pricing_revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TelemetryDropObservedV1 {
    pub first_missing_sequence: u64,
    pub last_missing_sequence: u64,
    pub proved_drop_lower_bound: u64,
    pub clean_shutdown_observed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthDimensionObservedV1 {
    pub score_ppm: u64,
    pub denominator: Option<u64>,
}

/// Payload-safe health observation retained by the registered observability
/// authority. Project paths and source content never enter this record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HealthSnapshotObservedV1 {
    pub scope_digest: String,
    pub quality_signal: u32,
    pub files_analyzed: u64,
    pub function_denominator: u64,
    pub dimensions: BTreeMap<String, HealthDimensionObservedV1>,
}

/// One bounded project activity observation. `family` and `detail` are
/// producer-controlled finite labels; paths, source, and messages are excluded.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActivityObservedV1 {
    pub family: String,
    pub units: u64,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerformanceMeasurementDescriptorV1 {
    pub descriptor_revision: String,
    pub metric: String,
    pub unit: String,
    pub eligible_population: String,
    pub horizon: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkRunAggregateV1 {
    pub descriptor: PerformanceMeasurementDescriptorV1,
    pub eligible: u64,
    pub observed: u64,
    pub censored: u64,
    pub unknown: u64,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
    pub coverage: CoverageStateV1,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PairedEffectEstimateV1 {
    pub baseline_revision: String,
    pub candidate_revision: String,
    pub paired_samples: u64,
    pub effect: Option<f64>,
    pub unit: String,
    pub coverage: CoverageStateV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PerformanceDispositionV1 {
    Promote,
    Reject,
    InsufficientEvidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_wire_values_are_closed_and_stable() {
        let values = [
            (CoverageStateV1::Known, "\"known\""),
            (CoverageStateV1::Partial, "\"partial\""),
            (CoverageStateV1::Stale, "\"stale\""),
            (CoverageStateV1::Unknown, "\"unknown\""),
            (CoverageStateV1::Sampled, "\"sampled\""),
            (CoverageStateV1::Capped, "\"capped\""),
        ];
        for (value, expected) in values {
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
    }

    #[test]
    fn performance_disposition_does_not_invent_success() {
        assert_eq!(
            serde_json::to_string(&PerformanceDispositionV1::InsufficientEvidence).unwrap(),
            "\"insufficient_evidence\""
        );
    }

    #[test]
    fn envelope_rejects_payload_kind_mismatch() {
        let envelope = ObservabilityEnvelopeV1 {
            event_id: "event:1".into(),
            event_kind: "telemetry.drop.observed.v1".into(),
            schema_revision: 1,
            idempotency_key: "idempotency:1".into(),
            trace_id: "trace:1".into(),
            scope_ref: "scope:1".into(),
            capability: "retrieval".into(),
            operation: "query".into(),
            event_time_micros: 1,
            observation_time_micros: 1,
            valid_from_micros: None,
            valid_until_micros: None,
            quantity: None,
            unit: None,
            terminal_result: None,
            producer_revision: "producer.v1".into(),
            configuration_revision: "config.v1".into(),
            policy_revision: "policy.v1".into(),
            watermark: "watermark:1".into(),
            coverage: CoverageStateV1::Known,
            sampling_probability: None,
            retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
            emitted_count: 1,
            delayed_count: 0,
            dropped_count: 0,
            process_boot_id: "boot:1".into(),
            producer_sequence: 1,
            payload: ObservabilityPayloadV1::RetrievalQuery(RetrievalQueryObservedV1 {
                query_family: "exact_technical".into(),
                enabled_lanes: vec!["exact_literal".into()],
                candidate_budget: 1,
                context_budget: 1,
                token_budget: 1,
                answered: true,
                source_coverage: CoverageStateV1::Known,
                lane_coverage: CoverageStateV1::Known,
            }),
        };
        assert_eq!(envelope.validate(), Err("event_kind"));
    }
}
