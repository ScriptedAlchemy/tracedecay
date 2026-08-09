//! Transport-neutral observability record/query boundary and dashboard read models.

mod share;

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{AnalyticsModeV1, CoverageStateV1, ObservabilityEnvelopeV1};

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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ObservatoryReadModelV1 {
        ObservatoryReadModelV1 {
            authorized_scope_ref: "scope:fixture".into(),
            horizon: ObservabilityHorizonV1 {
                since_micros: 10,
                until_micros: 20,
            },
            watermark: "watermark:7".into(),
            observed_at_micros: 20,
            current: true,
            metrics: vec![MetricValueV1 {
                descriptor_revision: "calls.v1".into(),
                metric: "calls".into(),
                value: Some(3.0),
                unit: "events".into(),
                denominator: "eligible_calls".into(),
                denominator_value: Some(3),
                coverage: MetricCoverageV1 {
                    eligible: Some(3),
                    observed: 3,
                    completed: 3,
                    censored: 0,
                    unknown: 0,
                    excluded: 0,
                    state: CoverageStateV1::Known,
                },
                evidence_class: MetricEvidenceClassV1::Measurement,
                provenance: MetricProvenanceV1 {
                    source: MetricSourceV1::ObservabilityEnvelope,
                    source_revision: "observability-envelope.v1".into(),
                    projector_revision: "observatory.v1".into(),
                    watermark: "watermark:7".into(),
                },
                cohort: MetricCohortV1 {
                    descriptor_revision: "eligible-calls.v1".into(),
                    eligible_population: "eligible_calls".into(),
                },
                temporal: MetricTemporalV1 {
                    horizon: ObservabilityHorizonV1 {
                        since_micros: 10,
                        until_micros: 20,
                    },
                    baseline_watermark: None,
                    delta: None,
                },
                uncertainty: MetricUncertaintyV1 {
                    lower: Some(3.0),
                    upper: Some(3.0),
                    reason: None,
                },
                calibration: None,
                unavailable_reason: None,
            }],
            analytics_mode: AnalyticsModeReadModelV1 {
                current: None,
                transition_watermark: None,
                coverage: MetricCoverageV1 {
                    eligible: None,
                    observed: 0,
                    completed: 0,
                    censored: 0,
                    unknown: 1,
                    excluded: 0,
                    state: CoverageStateV1::Unknown,
                },
                unavailable_reason: Some("analytics_consent_not_observed".into()),
            },
            comparison: PerformanceComparisonReadModelV1 {
                baseline_build: None,
                candidate_build: None,
                workload: None,
                corpus: None,
                environment: None,
                oracle: None,
                configuration: None,
                platform: None,
                rollback_profile: None,
                eligible_outcomes: None,
                paired_outcomes: None,
                regression_observed: None,
                disposition: ComparisonDispositionV1::InsufficientEvidence,
                coverage: MetricCoverageV1 {
                    eligible: None,
                    observed: 0,
                    completed: 0,
                    censored: 0,
                    unknown: 1,
                    excluded: 0,
                    state: CoverageStateV1::Unknown,
                },
                unavailable_reason: Some("comparison_evidence_not_recorded".into()),
            },
        }
    }

    #[test]
    fn observatory_contract_carries_controls_and_comparison_truth() {
        let model = fixture();
        assert_eq!(model.analytics_mode.current, None);
        assert_eq!(
            model.analytics_mode.coverage.state,
            CoverageStateV1::Unknown
        );
        assert_eq!(model.comparison.baseline_build, None);
        assert_eq!(
            model.comparison.disposition,
            ComparisonDispositionV1::InsufficientEvidence
        );
        assert_eq!(model.comparison.coverage.state, CoverageStateV1::Unknown);
    }

    #[test]
    fn missing_denominator_remains_unknown_not_zero() {
        let metric = MetricValueV1 {
            descriptor_revision: "analytics.calls.v1".into(),
            metric: "calls".into(),
            value: None,
            unit: "events".into(),
            denominator: "eligible_calls".into(),
            denominator_value: None,
            coverage: MetricCoverageV1 {
                eligible: None,
                observed: 0,
                completed: 0,
                censored: 0,
                unknown: 1,
                excluded: 0,
                state: CoverageStateV1::Unknown,
            },
            evidence_class: MetricEvidenceClassV1::Measurement,
            provenance: MetricProvenanceV1 {
                source: MetricSourceV1::ObservabilityEnvelope,
                source_revision: "observability-envelope.v1".into(),
                projector_revision: "observatory.v1".into(),
                watermark: "watermark:unknown".into(),
            },
            cohort: MetricCohortV1 {
                descriptor_revision: "eligible-calls.v1".into(),
                eligible_population: "eligible_calls".into(),
            },
            temporal: MetricTemporalV1 {
                horizon: ObservabilityHorizonV1 {
                    since_micros: 10,
                    until_micros: 20,
                },
                baseline_watermark: None,
                delta: None,
            },
            uncertainty: MetricUncertaintyV1 {
                lower: None,
                upper: None,
                reason: Some("unknown_denominator".into()),
            },
            calibration: None,
            unavailable_reason: Some("unknown_denominator".into()),
        };
        assert_eq!(metric.value, None);
        assert_eq!(metric.coverage.state, CoverageStateV1::Unknown);
    }

    #[test]
    fn cli_mcp_and_http_share_identical_read_model_bytes() {
        let model = fixture();
        let cli = serde_json::to_vec(&model).unwrap();
        let mcp = serde_json::to_vec(&model).unwrap();
        let http = serde_json::to_vec(&model).unwrap();
        assert_eq!(cli, mcp);
        assert_eq!(mcp, http);
    }
}
