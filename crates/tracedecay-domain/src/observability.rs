//! Canonical, payload-safe Plan 26 observability contracts.

mod activity;
#[cfg(test)]
mod activity_tests;
mod mcp_dispatch;

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
pub use activity::ActivityObservedV1;
pub use mcp_dispatch::{
    McpDispatchCancellationV1, McpDispatchDeadlineV1, McpDispatchObservedV1, McpDispatchTerminalV1,
};

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
    OperationResource(Box<OperationResourceObservedV1>),
    TelemetryDrop(TelemetryDropObservedV1),
    HealthSnapshot(HealthSnapshotObservedV1),
    Activity(ActivityObservedV1),
    McpDispatch(McpDispatchObservedV1),
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
            Self::McpDispatch(_) => "mcp.dispatch.observed.v1",
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
            if !crate::canonical_text::is_canonical_text_within(
                value,
                crate::canonical_text::CANONICAL_TEXT_MAX_BYTES,
            ) {
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
            ObservabilityPayloadV1::OperationResource(resource) => {
                resource.validate(self.terminal_result)?;
                if resource
                    .absolute_deadline_micros
                    .is_some_and(|deadline| deadline < self.event_time_micros)
                {
                    return Err("absolute_deadline");
                }
            }
            ObservabilityPayloadV1::Activity(activity) => {
                if !activity.is_valid() {
                    return Err("activity");
                }
            }
            ObservabilityPayloadV1::McpDispatch(dispatch) => {
                dispatch.validate(self.terminal_result)?;
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage_timings: Vec<OperationStageTimingV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_timings: Vec<OperationPhaseTimingV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absolute_deadline_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "OperationAvailabilityV1::is_unknown")]
    pub availability: OperationAvailabilityV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_outcome: Option<OperationActivationOutcomeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStageV1 {
    Scheduled,
    Admitted,
    Started,
    FirstProgress,
    FirstUsefulResult,
    Terminal,
}

impl OperationStageV1 {
    const fn order(self) -> u8 {
        match self {
            Self::Scheduled => 0,
            Self::Admitted => 1,
            Self::Started => 2,
            Self::FirstProgress => 3,
            Self::FirstUsefulResult => 4,
            Self::Terminal => 5,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationStageTimingV1 {
    pub stage: OperationStageV1,
    pub elapsed_micros: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationPhaseV1 {
    ProcessSpawn,
    ProcessReady,
    InputRead,
    InputValidation,
    Dispatch,
    OutputSerialization,
    OutputWrite,
}

impl OperationPhaseV1 {
    const fn order(self) -> u8 {
        match self {
            Self::ProcessSpawn => 0,
            Self::ProcessReady => 1,
            Self::InputRead => 2,
            Self::InputValidation => 3,
            Self::Dispatch => 4,
            Self::OutputSerialization => 5,
            Self::OutputWrite => 6,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationPhaseTimingV1 {
    pub phase: OperationPhaseV1,
    pub duration_micros: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationAvailabilityV1 {
    #[default]
    Unknown,
    Available,
    InvalidHttpResponse,
    EmptyResponse,
}

impl OperationAvailabilityV1 {
    fn is_unknown(&self) -> bool {
        *self == Self::Unknown
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationActivationOutcomeV1 {
    Admitted,
    Committed,
    Deferred,
    Unavailable,
    RestartRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationReadinessV1 {
    pub foreground_ready_micros: Option<u64>,
    pub background_complete_micros: Option<u64>,
}

impl OperationResourceObservedV1 {
    pub fn validate(
        &self,
        terminal_result: Option<ObservabilityTerminalResultV1>,
    ) -> Result<(), &'static str> {
        if !self.stage_timings.is_empty() {
            let mut previous_order = None;
            let mut previous_elapsed = None;
            for timing in &self.stage_timings {
                let order = timing.stage.order();
                if previous_order.is_some_and(|previous| order <= previous)
                    || previous_elapsed.is_some_and(|previous| timing.elapsed_micros < previous)
                {
                    return Err("stage_timings");
                }
                previous_order = Some(order);
                previous_elapsed = Some(timing.elapsed_micros);
            }
            if self.stage_timings.first().map(|timing| timing.stage)
                != Some(OperationStageV1::Scheduled)
                || self.stage_timings.get(1).map(|timing| timing.stage)
                    != Some(OperationStageV1::Admitted)
                || self.stage_timings.get(2).map(|timing| timing.stage)
                    != Some(OperationStageV1::Started)
            {
                return Err("stage_timings");
            }
            let has_terminal = self
                .stage_timings
                .last()
                .is_some_and(|timing| timing.stage == OperationStageV1::Terminal);
            if has_terminal != terminal_result.is_some() {
                return Err("terminal_result");
            }
        }

        let mut previous_phase = None;
        for timing in &self.phase_timings {
            let order = timing.phase.order();
            if previous_phase.is_some_and(|previous| order <= previous) {
                return Err("phase_timings");
            }
            previous_phase = Some(order);
        }

        match self.activation_outcome {
            Some(OperationActivationOutcomeV1::Committed)
                if self.availability != OperationAvailabilityV1::Available
                    || terminal_result.is_some_and(|result| {
                        result != ObservabilityTerminalResultV1::Succeeded
                    }) =>
            {
                return Err("availability");
            }
            Some(OperationActivationOutcomeV1::Deferred)
                if terminal_result
                    .is_some_and(|result| result != ObservabilityTerminalResultV1::Partial) =>
            {
                return Err("activation_outcome");
            }
            _ => {}
        }
        Ok(())
    }

    pub fn is_current(&self) -> bool {
        self.availability == OperationAvailabilityV1::Available
            && self.activation_outcome == Some(OperationActivationOutcomeV1::Committed)
    }

    pub fn readiness(&self) -> OperationReadinessV1 {
        let foreground_allowed = self.activation_outcome.is_none() || self.is_current();
        OperationReadinessV1 {
            foreground_ready_micros: foreground_allowed
                .then(|| {
                    self.stage_timings
                        .iter()
                        .find(|timing| timing.stage == OperationStageV1::FirstUsefulResult)
                        .map(|timing| timing.elapsed_micros)
                })
                .flatten(),
            background_complete_micros: self
                .stage_timings
                .iter()
                .find(|timing| timing.stage == OperationStageV1::Terminal)
                .map(|timing| timing.elapsed_micros),
        }
    }
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
    use schemars::schema_for;

    #[derive(JsonSchema, Serialize)]
    #[schemars(rename = "ObservabilityPayloadV1")]
    #[serde(rename_all = "snake_case", tag = "kind", content = "value")]
    enum DirectOperationResourceSchemaV1 {
        OperationResource(CoverageStateV1),
    }

    #[derive(JsonSchema, Serialize)]
    #[schemars(rename = "ObservabilityPayloadV1")]
    #[serde(rename_all = "snake_case", tag = "kind", content = "value")]
    enum BoxedOperationResourceSchemaV1 {
        OperationResource(Box<CoverageStateV1>),
    }

    #[derive(Serialize)]
    #[serde(rename_all = "snake_case", tag = "kind", content = "value")]
    enum DirectOperationResourceWireV1<'a> {
        OperationResource(&'a OperationResourceObservedV1),
    }

    fn stage(stage: OperationStageV1, elapsed_micros: u64) -> OperationStageTimingV1 {
        OperationStageTimingV1 {
            stage,
            elapsed_micros,
        }
    }

    fn phase(phase: OperationPhaseV1, duration_micros: u64) -> OperationPhaseTimingV1 {
        OperationPhaseTimingV1 {
            phase,
            duration_micros,
        }
    }

    fn operation_resource(
        stage_timings: Vec<OperationStageTimingV1>,
    ) -> OperationResourceObservedV1 {
        OperationResourceObservedV1 {
            scheduled_latency_micros: 5,
            service_latency_micros: 34,
            process_rss_bytes: None,
            process_pss_bytes: None,
            cpu_user_micros: None,
            cpu_system_micros: None,
            read_bytes: None,
            write_bytes: None,
            input_tokens: None,
            output_tokens: None,
            cost_amount: None,
            cost_currency: None,
            pricing_revision: None,
            stage_timings,
            phase_timings: Vec::new(),
            absolute_deadline_micros: None,
            availability: OperationAvailabilityV1::Unknown,
            activation_outcome: None,
            process_count: None,
            input_bytes: None,
            output_bytes: None,
        }
    }

    fn operation_envelope(
        stage_timings: Vec<OperationStageTimingV1>,
        terminal_result: Option<ObservabilityTerminalResultV1>,
    ) -> ObservabilityEnvelopeV1 {
        ObservabilityEnvelopeV1 {
            event_id: "event:operation:1".into(),
            event_kind: "operation.resource.observed.v1".into(),
            schema_revision: 1,
            idempotency_key: "idempotency:operation:1".into(),
            trace_id: "trace:operation:1".into(),
            scope_ref: "scope:1".into(),
            capability: "runtime".into(),
            operation: "background_refresh".into(),
            event_time_micros: 1,
            observation_time_micros: 1,
            valid_from_micros: None,
            valid_until_micros: None,
            quantity: None,
            unit: None,
            terminal_result,
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
            payload: ObservabilityPayloadV1::OperationResource(Box::new(operation_resource(
                stage_timings,
            ))),
        }
    }

    #[test]
    fn boxed_operation_resource_preserves_wire_shape_and_round_trips() {
        let resource = operation_resource(Vec::new());
        let direct =
            serde_json::to_value(DirectOperationResourceWireV1::OperationResource(&resource))
                .unwrap();
        let payload = ObservabilityPayloadV1::OperationResource(Box::new(resource));
        let boxed = serde_json::to_value(&payload).unwrap();

        assert_eq!(boxed, direct);
        assert_eq!(
            serde_json::from_value::<ObservabilityPayloadV1>(boxed).unwrap(),
            payload
        );
    }

    #[test]
    fn boxing_is_transparent_to_schemars_tagged_enum_shape() {
        let direct_wire = serde_json::to_value(DirectOperationResourceSchemaV1::OperationResource(
            CoverageStateV1::Known,
        ))
        .unwrap();
        let boxed_wire = serde_json::to_value(BoxedOperationResourceSchemaV1::OperationResource(
            Box::new(CoverageStateV1::Known),
        ))
        .unwrap();
        let direct_schema = serde_json::to_value(schema_for!(DirectOperationResourceSchemaV1))
            .expect("direct schema must serialize");
        let boxed_schema = serde_json::to_value(schema_for!(BoxedOperationResourceSchemaV1))
            .expect("boxed schema must serialize");

        assert_eq!(boxed_wire, direct_wire);
        assert_eq!(boxed_schema, direct_schema);
    }

    #[test]
    fn operation_stage_wire_values_are_closed_and_stable() {
        let values = [
            (OperationStageV1::Scheduled, "\"scheduled\""),
            (OperationStageV1::Admitted, "\"admitted\""),
            (OperationStageV1::Started, "\"started\""),
            (OperationStageV1::FirstProgress, "\"first_progress\""),
            (
                OperationStageV1::FirstUsefulResult,
                "\"first_useful_result\"",
            ),
            (OperationStageV1::Terminal, "\"terminal\""),
        ];

        for (value, expected) in values {
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
        assert!(serde_json::from_str::<OperationStageV1>("\"unknown\"").is_err());
    }

    #[test]
    fn foreground_readiness_can_precede_background_completion() {
        let envelope = operation_envelope(
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 5),
                stage(OperationStageV1::Started, 8),
                stage(OperationStageV1::FirstUsefulResult, 21),
            ],
            None,
        );
        let ObservabilityPayloadV1::OperationResource(resource) = &envelope.payload else {
            unreachable!();
        };

        assert_eq!(
            resource.readiness(),
            OperationReadinessV1 {
                foreground_ready_micros: Some(21),
                background_complete_micros: None,
            }
        );
        assert_eq!(envelope.validate(), Ok(()));
    }

    #[test]
    fn successful_terminal_records_both_readiness_milestones() {
        let envelope = operation_envelope(
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 5),
                stage(OperationStageV1::Started, 8),
                stage(OperationStageV1::FirstProgress, 13),
                stage(OperationStageV1::FirstUsefulResult, 21),
                stage(OperationStageV1::Terminal, 34),
            ],
            Some(ObservabilityTerminalResultV1::Succeeded),
        );
        let ObservabilityPayloadV1::OperationResource(resource) = &envelope.payload else {
            unreachable!();
        };

        assert_eq!(
            resource.readiness(),
            OperationReadinessV1 {
                foreground_ready_micros: Some(21),
                background_complete_micros: Some(34),
            }
        );
        assert_eq!(envelope.validate(), Ok(()));
    }

    #[test]
    fn failed_terminal_does_not_fabricate_foreground_readiness() {
        let envelope = operation_envelope(
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 5),
                stage(OperationStageV1::Started, 8),
                stage(OperationStageV1::Terminal, 34),
            ],
            Some(ObservabilityTerminalResultV1::Failed),
        );
        let ObservabilityPayloadV1::OperationResource(resource) = &envelope.payload else {
            unreachable!();
        };

        assert_eq!(
            resource.readiness(),
            OperationReadinessV1 {
                foreground_ready_micros: None,
                background_complete_micros: Some(34),
            }
        );
        assert_eq!(envelope.validate(), Ok(()));
    }

    #[test]
    fn stage_validation_rejects_order_duplicates_and_missing_prefix() {
        let invalid = [
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Started, 8),
            ],
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 5),
                stage(OperationStageV1::Admitted, 8),
            ],
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 8),
                stage(OperationStageV1::Started, 5),
            ],
        ];

        for stage_timings in invalid {
            assert_eq!(
                operation_resource(stage_timings).validate(None),
                Err("stage_timings")
            );
        }
    }

    #[test]
    fn envelope_rejects_terminal_stage_and_outcome_mismatch() {
        let terminal_without_outcome = operation_envelope(
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 5),
                stage(OperationStageV1::Started, 8),
                stage(OperationStageV1::Terminal, 34),
            ],
            None,
        );
        let outcome_without_terminal = operation_envelope(
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 5),
                stage(OperationStageV1::Started, 8),
                stage(OperationStageV1::FirstUsefulResult, 21),
            ],
            Some(ObservabilityTerminalResultV1::Succeeded),
        );

        assert_eq!(terminal_without_outcome.validate(), Err("terminal_result"));
        assert_eq!(outcome_without_terminal.validate(), Err("terminal_result"));
    }

    #[test]
    fn host_runtime_vocabulary_is_closed_and_content_free() {
        let phases = [
            (OperationPhaseV1::ProcessSpawn, "\"process_spawn\""),
            (OperationPhaseV1::ProcessReady, "\"process_ready\""),
            (OperationPhaseV1::InputRead, "\"input_read\""),
            (OperationPhaseV1::InputValidation, "\"input_validation\""),
            (OperationPhaseV1::Dispatch, "\"dispatch\""),
            (
                OperationPhaseV1::OutputSerialization,
                "\"output_serialization\"",
            ),
            (OperationPhaseV1::OutputWrite, "\"output_write\""),
        ];
        for (value, expected) in phases {
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
        let outcomes = [
            (OperationActivationOutcomeV1::Admitted, "\"admitted\""),
            (OperationActivationOutcomeV1::Committed, "\"committed\""),
            (OperationActivationOutcomeV1::Deferred, "\"deferred\""),
            (OperationActivationOutcomeV1::Unavailable, "\"unavailable\""),
            (
                OperationActivationOutcomeV1::RestartRequired,
                "\"restart_required\"",
            ),
        ];
        for (value, expected) in outcomes {
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
        assert_eq!(
            serde_json::to_string(&OperationAvailabilityV1::Unknown).unwrap(),
            "\"unknown\""
        );
        assert_eq!(
            serde_json::to_string(&OperationAvailabilityV1::Available).unwrap(),
            "\"available\""
        );
        assert_eq!(
            serde_json::to_string(&OperationAvailabilityV1::InvalidHttpResponse).unwrap(),
            "\"invalid_http_response\""
        );
        assert_eq!(
            serde_json::to_string(&OperationAvailabilityV1::EmptyResponse).unwrap(),
            "\"empty_response\""
        );
    }

    #[test]
    fn valid_host_activation_reuses_operation_observability() {
        let mut envelope = operation_envelope(
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 5),
                stage(OperationStageV1::Started, 8),
                stage(OperationStageV1::FirstProgress, 13),
                stage(OperationStageV1::FirstUsefulResult, 21),
                stage(OperationStageV1::Terminal, 34),
            ],
            Some(ObservabilityTerminalResultV1::Succeeded),
        );
        {
            let ObservabilityPayloadV1::OperationResource(resource) = &mut envelope.payload else {
                unreachable!();
            };
            resource.absolute_deadline_micros = Some(50);
            resource.availability = OperationAvailabilityV1::Available;
            resource.activation_outcome = Some(OperationActivationOutcomeV1::Committed);
            resource.process_count = Some(2);
            resource.input_bytes = Some(128);
            resource.output_bytes = Some(64);
            resource.phase_timings = vec![
                phase(OperationPhaseV1::ProcessSpawn, 3),
                phase(OperationPhaseV1::ProcessReady, 4),
                phase(OperationPhaseV1::InputRead, 2),
                phase(OperationPhaseV1::InputValidation, 1),
                phase(OperationPhaseV1::Dispatch, 8),
                phase(OperationPhaseV1::OutputSerialization, 2),
                phase(OperationPhaseV1::OutputWrite, 1),
            ];
        }

        assert_eq!(envelope.validate(), Ok(()));
        let ObservabilityPayloadV1::OperationResource(resource) = &envelope.payload else {
            unreachable!();
        };
        assert!(resource.is_current());
        assert_eq!(
            resource.readiness(),
            OperationReadinessV1 {
                foreground_ready_micros: Some(21),
                background_complete_micros: Some(34),
            }
        );
    }

    #[test]
    fn invalid_empty_and_default_responses_cannot_become_ready() {
        for availability in [
            OperationAvailabilityV1::InvalidHttpResponse,
            OperationAvailabilityV1::EmptyResponse,
            OperationAvailabilityV1::Unknown,
        ] {
            let mut resource = operation_resource(vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 5),
                stage(OperationStageV1::Started, 8),
                stage(OperationStageV1::FirstUsefulResult, 21),
                stage(OperationStageV1::Terminal, 34),
            ]);
            resource.availability = availability;
            resource.activation_outcome = Some(OperationActivationOutcomeV1::Committed);

            assert_eq!(
                resource.validate(Some(ObservabilityTerminalResultV1::Succeeded)),
                Err("availability")
            );
            assert!(!resource.is_current());
            assert_eq!(resource.readiness().foreground_ready_micros, None);
        }
    }

    #[test]
    fn deferred_activation_never_becomes_current() {
        let mut resource = operation_resource(vec![
            stage(OperationStageV1::Scheduled, 0),
            stage(OperationStageV1::Admitted, 5),
            stage(OperationStageV1::Started, 8),
            stage(OperationStageV1::Terminal, 34),
        ]);
        resource.availability = OperationAvailabilityV1::Available;
        resource.activation_outcome = Some(OperationActivationOutcomeV1::Deferred);

        assert_eq!(
            resource.validate(Some(ObservabilityTerminalResultV1::Partial)),
            Ok(())
        );
        assert!(!resource.is_current());
        assert_eq!(
            resource.readiness(),
            OperationReadinessV1 {
                foreground_ready_micros: None,
                background_complete_micros: Some(34),
            }
        );
    }

    #[test]
    fn host_phase_order_and_absolute_deadline_are_validated() {
        let mut envelope = operation_envelope(
            vec![
                stage(OperationStageV1::Scheduled, 0),
                stage(OperationStageV1::Admitted, 5),
                stage(OperationStageV1::Started, 8),
            ],
            None,
        );
        if let ObservabilityPayloadV1::OperationResource(resource) = &mut envelope.payload {
            resource.phase_timings = vec![
                phase(OperationPhaseV1::OutputWrite, 1),
                phase(OperationPhaseV1::ProcessSpawn, 3),
            ];
        }
        assert_eq!(envelope.validate(), Err("phase_timings"));

        if let ObservabilityPayloadV1::OperationResource(resource) = &mut envelope.payload {
            resource.phase_timings.clear();
            resource.absolute_deadline_micros = Some(envelope.event_time_micros - 1);
        }
        assert_eq!(envelope.validate(), Err("absolute_deadline"));
    }

    #[test]
    fn legacy_resource_json_without_stage_timings_round_trips_and_validates() {
        let legacy_json = r#"{
            "scheduled_latency_micros": 5,
            "service_latency_micros": 34,
            "process_rss_bytes": null,
            "process_pss_bytes": null,
            "cpu_user_micros": null,
            "cpu_system_micros": null,
            "read_bytes": null,
            "write_bytes": null,
            "input_tokens": null,
            "output_tokens": null,
            "cost_amount": null,
            "cost_currency": null,
            "pricing_revision": null
        }"#;
        let expected: serde_json::Value = serde_json::from_str(legacy_json).unwrap();
        let resource: OperationResourceObservedV1 = serde_json::from_str(legacy_json).unwrap();

        assert!(resource.stage_timings.is_empty());
        assert_eq!(
            resource.validate(Some(ObservabilityTerminalResultV1::Succeeded)),
            Ok(())
        );
        assert_eq!(serde_json::to_value(resource).unwrap(), expected);
    }

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

    #[test]
    fn mcp_dispatch_telemetry_keeps_terminal_and_control_states_typed() {
        let payload = McpDispatchObservedV1 {
            route_admission_micros: 4,
            handler_micros: 16,
            result_materialization_micros: 3,
            total_micros: 23,
            deadline: McpDispatchDeadlineV1::Enforced,
            cancellation: McpDispatchCancellationV1::NotRequested,
            terminal: McpDispatchTerminalV1::Completed,
        };
        let mut envelope = ObservabilityEnvelopeV1 {
            event_id: "event:mcp-dispatch:1".into(),
            event_kind: "mcp.dispatch.observed.v1".into(),
            schema_revision: 1,
            idempotency_key: "idempotency:mcp-dispatch:1".into(),
            trace_id: "trace:mcp-dispatch:1".into(),
            scope_ref: "scope:mcp-dispatch".into(),
            capability: "mcp".into(),
            operation: "dispatch".into(),
            event_time_micros: 1,
            observation_time_micros: 1,
            valid_from_micros: None,
            valid_until_micros: None,
            quantity: None,
            unit: None,
            terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
            producer_revision: "mcp-dispatch-observer.v1".into(),
            configuration_revision: "registered-project-session.v1".into(),
            policy_revision: "mcp-dispatch-deadline.v1".into(),
            watermark: "mcp-dispatch:1".into(),
            coverage: CoverageStateV1::Known,
            sampling_probability: None,
            retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
            emitted_count: 1,
            delayed_count: 0,
            dropped_count: 0,
            process_boot_id: "boot:mcp-dispatch".into(),
            producer_sequence: 1,
            payload: ObservabilityPayloadV1::McpDispatch(payload.clone()),
        };

        assert_eq!(envelope.validate(), Ok(()));

        envelope.terminal_result = Some(ObservabilityTerminalResultV1::TimedOut);
        assert_eq!(envelope.validate(), Err("mcp_dispatch_terminal"));

        envelope.terminal_result = Some(ObservabilityTerminalResultV1::Succeeded);
        if let ObservabilityPayloadV1::McpDispatch(payload) = &mut envelope.payload {
            payload.total_micros = 22;
        }
        assert_eq!(envelope.validate(), Err("mcp_dispatch_timings"));

        if let ObservabilityPayloadV1::McpDispatch(payload) = &mut envelope.payload {
            payload.total_micros = 23;
            payload.deadline = McpDispatchDeadlineV1::Expired;
            payload.cancellation = McpDispatchCancellationV1::DeadlineTriggered;
            payload.terminal = McpDispatchTerminalV1::TimedOut;
        }
        envelope.terminal_result = Some(ObservabilityTerminalResultV1::TimedOut);
        assert_eq!(envelope.validate(), Ok(()));

        let failed = McpDispatchObservedV1 {
            deadline: McpDispatchDeadlineV1::Enforced,
            cancellation: McpDispatchCancellationV1::NotRequested,
            terminal: McpDispatchTerminalV1::Failed,
            ..payload
        };
        assert_eq!(
            failed.validate(Some(ObservabilityTerminalResultV1::Failed)),
            Ok(())
        );

        let shutdown = McpDispatchObservedV1 {
            cancellation: McpDispatchCancellationV1::ShutdownTriggered,
            terminal: McpDispatchTerminalV1::Shutdown,
            ..failed
        };
        assert_eq!(
            shutdown.validate(Some(ObservabilityTerminalResultV1::Cancelled)),
            Ok(())
        );
    }
}
