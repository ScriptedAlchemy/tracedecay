//! Transport-neutral observability record/query boundary and dashboard read models.

mod share;

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    AnalyticsModeV1, CoverageStateV1, ObservabilityEnvelopeV1, RejectedArgumentErrorClassV1,
    RejectedArgumentNameV1, RejectedArgumentSurfaceV1,
};

use crate::ApplicationContractError;

pub use share::*;

pub type ObservabilityFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ApplicationContractError>> + Send + 'a>>;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct ObservabilityHorizonV1 {
    pub since_micros: i64,
    pub until_micros: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservabilityQueryV1 {
    pub authorized_scope_ref: String,
    pub event_kinds: Vec<String>,
    pub horizon: ObservabilityHorizonV1,
    pub after_watermark: Option<String>,
    pub limit: u32,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ObservabilityPageV1 {
    pub events: Vec<ObservabilityEnvelopeV1>,
    /// Registered authority cursor corresponding to each event at the same
    /// index. Consumers must not derive storage identity from event payloads.
    pub event_cursors: Vec<String>,
    pub watermark: String,
    pub coverage: CoverageStateV1,
    pub next_watermark: Option<String>,
}

pub trait ObservabilityRecordPort: Send + Sync {
    fn record<'a>(&'a self, envelope: ObservabilityEnvelopeV1) -> ObservabilityFuture<'a, String>;
}

pub trait ObservabilityQueryPort: Send + Sync {
    fn query<'a>(
        &'a self,
        query: ObservabilityQueryV1,
    ) -> ObservabilityFuture<'a, ObservabilityPageV1>;
}

pub struct ObservabilityApplicationV1<R, Q> {
    recorder: R,
    query: Q,
}

impl<R, Q> ObservabilityApplicationV1<R, Q>
where
    R: ObservabilityRecordPort,
    Q: ObservabilityQueryPort,
{
    #[hotpath::skip]
    pub const fn new(recorder: R, query: Q) -> Self {
        Self { recorder, query }
    }

    pub async fn record(
        &self,
        envelope: ObservabilityEnvelopeV1,
    ) -> Result<String, ApplicationContractError> {
        self.recorder.record(envelope).await
    }

    pub async fn query(
        &self,
        query: ObservabilityQueryV1,
    ) -> Result<ObservabilityPageV1, ApplicationContractError> {
        self.query.query(query).await
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MetricCoverageV1 {
    /// Exact denominator cardinality. `None` means the denominator is unknown.
    pub eligible: Option<u64>,
    pub observed: u64,
    pub completed: u64,
    pub censored: u64,
    pub unknown: u64,
    pub excluded: u64,
    pub state: CoverageStateV1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricEvidenceClassV1 {
    Measurement,
    Association,
    CalibratedPrediction,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricSourceV1 {
    ObservabilityEnvelope,
    FeedbackObservations,
    ProviderUsageObservation,
    SavingsLedger,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MetricProvenanceV1 {
    pub source: MetricSourceV1,
    pub source_revision: String,
    pub projector_revision: String,
    pub watermark: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MetricCohortV1 {
    pub descriptor_revision: String,
    pub eligible_population: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct MetricTemporalV1 {
    pub horizon: ObservabilityHorizonV1,
    pub baseline_watermark: Option<String>,
    pub delta: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct MetricUncertaintyV1 {
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub struct MetricCalibrationV1 {
    pub estimator_revision: String,
    pub calibration_revision: String,
    pub cohort_revision: String,
    pub support: u64,
    pub drift_valid: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct MetricValueV1 {
    pub descriptor_revision: String,
    pub metric: String,
    /// Aggregate value. It is absent whenever its denominator or coverage is
    /// insufficient; observed lower bounds remain available in `coverage`.
    pub value: Option<f64>,
    pub unit: String,
    pub denominator: String,
    pub denominator_value: Option<u64>,
    pub coverage: MetricCoverageV1,
    pub evidence_class: MetricEvidenceClassV1,
    pub provenance: MetricProvenanceV1,
    pub cohort: MetricCohortV1,
    pub temporal: MetricTemporalV1,
    pub uncertainty: MetricUncertaintyV1,
    pub calibration: Option<MetricCalibrationV1>,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct AnalyticsModeReadModelV1 {
    pub current: Option<AnalyticsModeV1>,
    pub transition_watermark: Option<String>,
    pub coverage: MetricCoverageV1,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonDispositionV1 {
    Promote,
    Reject,
    InsufficientEvidence,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct PerformanceComparisonReadModelV1 {
    pub baseline_build: Option<String>,
    pub candidate_build: Option<String>,
    pub workload: Option<String>,
    pub corpus: Option<String>,
    pub environment: Option<String>,
    pub oracle: Option<String>,
    pub configuration: Option<String>,
    pub platform: Option<String>,
    pub rollback_profile: Option<String>,
    pub eligible_outcomes: Option<u64>,
    pub paired_outcomes: Option<u64>,
    pub regression_observed: Option<bool>,
    pub disposition: ComparisonDispositionV1,
    pub coverage: MetricCoverageV1,
    pub unavailable_reason: Option<String>,
}

/// One surface × operation × argument × error-class cell in the rejected-argument view.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct RejectedArgumentGroupV1 {
    pub surface: RejectedArgumentSurfaceV1,
    pub operation: String,
    pub argument: RejectedArgumentNameV1,
    pub error_class: RejectedArgumentErrorClassV1,
    pub count: u64,
    /// Eligible-attempt rate for this cell. Absent when the attempt
    /// denominator or coverage is insufficient.
    pub rate: Option<f64>,
}

/// Frequency and rate projection for dispatcher rejected-argument observations.
///
/// Counts may be known while `rejection_rate` stays absent: Plan 26 forbids
/// fabricating a rate when the eligible-attempt denominator is unknown.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct RejectedArgumentAnalyticsV1 {
    pub coverage: MetricCoverageV1,
    pub projector_revision: String,
    pub watermark: String,
    pub eligible_attempts: Option<u64>,
    pub rejected_total: Option<u64>,
    pub rejection_rate: Option<f64>,
    pub redacted_name_count: u64,
    pub groups: Vec<RejectedArgumentGroupV1>,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ObservatoryReadModelV1 {
    pub authorized_scope_ref: String,
    pub horizon: ObservabilityHorizonV1,
    pub watermark: String,
    pub observed_at_micros: i64,
    pub current: bool,
    pub metrics: Vec<MetricValueV1>,
    pub analytics_mode: AnalyticsModeReadModelV1,
    pub comparison: PerformanceComparisonReadModelV1,
    pub rejected_arguments: RejectedArgumentAnalyticsV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct CostsReadModelV1 {
    pub authorized_scope_ref: String,
    pub horizon: ObservabilityHorizonV1,
    pub watermark: String,
    pub observed_at_micros: i64,
    pub current: bool,
    pub usage: Vec<MetricValueV1>,
    pub estimated_cost: Vec<MetricValueV1>,
    /// Provider-backed operation latency, projected from the same retained
    /// Plan 26 operation-resource events as Observatory. Each entry keeps
    /// provider/model identity explicit; `None` is a real uncorrelated state,
    /// never a client-side guess.
    pub latency: Vec<ProviderLatencyReadModelV1>,
    pub pricing_revision: Option<String>,
}

/// One provider/model cohort in the Costs latency read model.
///
/// The percentile cells are ordinary canonical metrics so every value carries
/// its exact unit, horizon, denominator, coverage/censoring, and projector
/// provenance. Identity provenance is kept separately because latency is
/// measured by `OperationResourceObservedV1`, while provider/model identity
/// may be joined from an exact `ProviderUsageObservationV1` request/session.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct ProviderLatencyReadModelV1 {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub identity_provenance: MetricProvenanceV1,
    pub identity_unavailable_reason: Option<String>,
    pub queue: LatencyDistributionReadModelV1,
    pub start: LatencyDistributionReadModelV1,
    pub first_progress: LatencyDistributionReadModelV1,
    pub service: LatencyDistributionReadModelV1,
    pub terminal: LatencyDistributionReadModelV1,
}

/// p50/p95/p99 for one provider operation latency stage.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
pub struct LatencyDistributionReadModelV1 {
    pub p50: MetricValueV1,
    pub p95: MetricValueV1,
    pub p99: MetricValueV1,
}
