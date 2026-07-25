//! Canonical, payload-safe Plan 26 observability contracts.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
}
