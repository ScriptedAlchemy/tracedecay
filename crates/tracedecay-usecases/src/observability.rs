//! Production Plan 26 read-model composition over the canonical accounting store.

mod delivery_recorder;
mod delivery_settlement;
mod delivery_spool;
mod emit;
mod execution_emit;
mod export;
mod github_stack_emit;
mod producer;
mod read;
mod read_model;
#[cfg(test)]
mod read_model_tests;
mod retrieval_emit;
mod store;
mod work_blocked_interval_emit;
mod work_duplicate_emit;
mod work_owner_observation_recovery;
mod work_retry_leak_emit;

pub use delivery_recorder::{
    BoundedDeliverySettlementRecorderV1, DeliverySettlementRecordOutcomeV1,
    DeliverySettlementRecorderSummaryV1,
};
pub use delivery_settlement::{DeliverySettlementAuthorityV1, DeliverySettlementEmissionV1};
pub use emit::{
    record_adoption_eligibility, record_adoption_outcome, record_index, record_latency,
    record_operation_resource, record_retrieval_query, record_storage,
};
pub use execution_emit::{
    ExecutionTopologyObservationUnavailableV1, NativeIntegrationObservationResultV1,
    execution_owner_fact_envelope, record_native_integration_transition,
};
pub use export::RegisteredAggregateShareExporterV1;
pub use github_stack_emit::{
    GitHubStackCapabilityObservationResultV1, GitHubStackCapabilityObservationUnavailableV1,
    GitHubStackProbeOutcomeV1, GitHubStackProbeOwnerMountErrorV1, GitHubStackProbeOwnerV1,
    record_github_stack_capability,
};
pub use producer::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1,
    ObservabilityOwnerEmissionOutcomeV1, ObservabilityProducerDeadlinesV1,
    ObservabilityProducerIdentityV1, ObservabilityProducerSummaryV1,
};
pub use read::{observatory_read_model, observatory_unavailable_read_model};
pub use retrieval_emit::{
    AblationDimensionV1, RetrievalEmissionSummaryV1, emit_retrieval_pipeline,
    observe_stage_ablation, record_analytics_consent, record_context_outcome,
    record_retrieval_ablation, record_retrieval_planner, record_retrieval_source,
    record_retrieval_synthesis, record_retriever,
};
pub use store::RegisteredObservabilityPortV1;
pub use tracedecay_global_db::{
    DeliverySourceReceiptReadV1, MAX_PENDING_RECEIPTED_DELIVERIES_V1,
    PendingDeliverySourceReceiptV1,
};
pub use work_blocked_interval_emit::{
    record_work_blocked_interval_observation, work_blocked_interval_observation_envelope,
};
pub use work_duplicate_emit::record_work_duplicate_observation;
pub use work_owner_observation_recovery::{
    WorkOwnerObservationRecoverySummaryV1, WorkOwnerObservationRecoveryV1,
};
pub use work_retry_leak_emit::{
    WorkOwnerObservationResultV1, record_work_leak_observation, record_work_retry_observation,
};

use tracedecay_application::{
    CostsReadModelV1, MetricCohortV1, MetricCoverageV1, MetricEvidenceClassV1, MetricProvenanceV1,
    MetricSourceV1, MetricTemporalV1, MetricUncertaintyV1, MetricValueV1, ObservabilityHorizonV1,
    ObservatoryReadModelV1, now_micros,
};
use tracedecay_domain::{CoverageStateV1, ObservationScopeV1};

use crate::feedback::observations::{
    FeedbackCoverageV1, FeedbackObservationReadModelV1, FeedbackSystemMetricDenominatorV1,
    FeedbackSystemMetricKindV1, FeedbackSystemMetricUnitV1,
};
use tracedecay_global_db::RegisteredGlobalDb;

use crate::provider_pricing::load_table;
use crate::provider_usage::{
    AggregatedProviderUsageCountersV1, ProviderUsageAggregateV1, ProviderUsageCoverageV1,
    price_provider_usage, provider_usage_aggregate,
};

const EVENT_LIMIT: usize = 10_000;
const OBSERVABILITY_SCAN_PAGE: usize = 64;
const OBSERVABILITY_PROVIDER: &str = "tracedecay-observability";
const ANALYTICS_DESCRIPTOR: &str = "analytics-events.v1";
const COST_DESCRIPTOR: &str = "provider-costs.v1";
const FEEDBACK_DESCRIPTOR: &str = "feedback-system-quality.v1";

/// Canonical wire projection used by every dashboard wire surface. Adapters may wrap the
/// value in their transport framing but may not recompute metrics or coverage.
fn canonical_observatory_value(
    model: &ObservatoryReadModelV1,
) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(model)
}

pub fn observatory_cli_value(
    model: &ObservatoryReadModelV1,
) -> Result<serde_json::Value, serde_json::Error> {
    canonical_observatory_value(model)
}

pub fn observatory_mcp_value(
    model: &ObservatoryReadModelV1,
) -> Result<serde_json::Value, serde_json::Error> {
    canonical_observatory_value(model)
}

pub fn observatory_http_value(
    model: &ObservatoryReadModelV1,
) -> Result<serde_json::Value, serde_json::Error> {
    canonical_observatory_value(model)
}

/// Bounded public JSON export. It is the same canonical model as interactive
/// surfaces, including absent values, exact denominator, and coverage state.
pub fn observatory_export_bytes(
    model: &ObservatoryReadModelV1,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(model)
}

fn canonical_costs_value(model: &CostsReadModelV1) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(model)
}

pub fn costs_cli_value(model: &CostsReadModelV1) -> Result<serde_json::Value, serde_json::Error> {
    canonical_costs_value(model)
}

pub fn costs_mcp_value(model: &CostsReadModelV1) -> Result<serde_json::Value, serde_json::Error> {
    canonical_costs_value(model)
}

pub fn costs_http_value(model: &CostsReadModelV1) -> Result<serde_json::Value, serde_json::Error> {
    canonical_costs_value(model)
}

pub fn costs_export_bytes(model: &CostsReadModelV1) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(model)
}

fn coverage(
    eligible: Option<u64>,
    observed: u64,
    unknown: u64,
    state: CoverageStateV1,
) -> MetricCoverageV1 {
    MetricCoverageV1 {
        eligible,
        observed,
        completed: observed,
        censored: 0,
        unknown,
        excluded: 0,
        state,
    }
}

fn horizon(since_seconds: i64, observed_at_micros: i64) -> ObservabilityHorizonV1 {
    ObservabilityHorizonV1 {
        since_micros: since_seconds.saturating_mul(1_000_000),
        until_micros: observed_at_micros,
    }
}

struct MeasurementDescriptor<'a> {
    revision: &'a str,
    metric: &'a str,
    unit: &'a str,
    denominator: &'a str,
}

impl<'a> MeasurementDescriptor<'a> {
    const fn new(revision: &'a str, metric: &'a str, unit: &'a str, denominator: &'a str) -> Self {
        Self {
            revision,
            metric,
            unit,
            denominator,
        }
    }
}

struct MeasurementProvenance<'a> {
    source: MetricSourceV1,
    source_revision: &'a str,
    projector_revision: &'a str,
    watermark: &'a str,
}

impl<'a> MeasurementProvenance<'a> {
    const fn new(
        source: MetricSourceV1,
        source_revision: &'a str,
        projector_revision: &'a str,
        watermark: &'a str,
    ) -> Self {
        Self {
            source,
            source_revision,
            projector_revision,
            watermark,
        }
    }
}

struct MeasurementSpec<'a> {
    descriptor: MeasurementDescriptor<'a>,
    provenance: MeasurementProvenance<'a>,
    horizon: &'a ObservabilityHorizonV1,
    coverage: MetricCoverageV1,
    value: Option<f64>,
    unavailable_reason: Option<&'a str>,
}

fn measurement(spec: MeasurementSpec<'_>) -> MetricValueV1 {
    let MeasurementSpec {
        descriptor,
        provenance,
        horizon,
        coverage,
        value,
        unavailable_reason,
    } = spec;
    let uncertainty = match value {
        Some(value) => MetricUncertaintyV1 {
            lower: Some(value),
            upper: Some(value),
            reason: None,
        },
        None => MetricUncertaintyV1 {
            lower: None,
            upper: None,
            reason: unavailable_reason.map(str::to_owned),
        },
    };
    MetricValueV1 {
        descriptor_revision: descriptor.revision.to_string(),
        metric: descriptor.metric.to_string(),
        value,
        unit: descriptor.unit.to_string(),
        denominator: descriptor.denominator.to_string(),
        denominator_value: coverage.eligible,
        coverage,
        evidence_class: MetricEvidenceClassV1::Measurement,
        provenance: MetricProvenanceV1 {
            source: provenance.source,
            source_revision: provenance.source_revision.to_string(),
            projector_revision: provenance.projector_revision.to_string(),
            watermark: provenance.watermark.to_string(),
        },
        cohort: MetricCohortV1 {
            descriptor_revision: format!("{}.v1", descriptor.denominator),
            eligible_population: descriptor.denominator.to_string(),
        },
        temporal: MetricTemporalV1 {
            horizon: horizon.clone(),
            baseline_watermark: None,
            delta: None,
        },
        uncertainty,
        calibration: None,
        unavailable_reason: unavailable_reason.map(str::to_owned),
    }
}

/// Adds Plan 37 feedback-system quality measurements to the canonical
/// Observatory model. Adapters call this composer instead of re-deriving
/// values, denominators, coverage, or unavailable states.
pub fn attach_feedback_system_quality(
    read_model: &mut ObservatoryReadModelV1,
    feedback: Option<&FeedbackObservationReadModelV1>,
    unavailable_reason: Option<&str>,
) {
    let Some(feedback) = feedback else {
        let coverage = coverage(None, 0, 1, CoverageStateV1::Unknown);
        for (kind, unit, denominator) in feedback_metric_descriptors() {
            read_model.metrics.push(measurement(MeasurementSpec {
                descriptor: MeasurementDescriptor::new(
                    FEEDBACK_DESCRIPTOR,
                    kind,
                    unit,
                    denominator,
                ),
                provenance: MeasurementProvenance::new(
                    MetricSourceV1::FeedbackObservations,
                    "feedback-observations.v1",
                    "feedback-system-quality-projector.v1",
                    "feedback:unavailable",
                ),
                horizon: &read_model.horizon,
                coverage: coverage.clone(),
                value: None,
                unavailable_reason: unavailable_reason
                    .or(Some("feedback_observations_unavailable")),
            }));
        }
        read_model.current = false;
        return;
    };

    let watermark = feedback.watermark.producer_sequence.map_or_else(
        || "feedback:empty".to_string(),
        |value| format!("feedback:{value}"),
    );
    let unknown = feedback
        .denominators
        .delayed
        .saturating_add(feedback.denominators.dropped)
        .saturating_add(feedback.denominators.retention_dropped)
        .saturating_add(feedback.denominators.incomplete_boots);
    for metric in &feedback.system_quality.metrics {
        let state = feedback_coverage_state(metric.coverage);
        let complete = state == CoverageStateV1::Known;
        let observed = metric.denominator.unwrap_or(0);
        let coverage = coverage(
            complete.then_some(observed),
            observed,
            if complete { 0 } else { unknown.max(1) },
            state,
        );
        let unavailable = metric
            .unavailable_reason
            .map(feedback_unavailable_reason)
            .or((!complete).then_some("incomplete_feedback_coverage"));
        read_model.metrics.push(measurement(MeasurementSpec {
            descriptor: MeasurementDescriptor::new(
                FEEDBACK_DESCRIPTOR,
                feedback_metric_name(metric.metric),
                feedback_metric_unit(metric.unit),
                feedback_denominator_name(metric.denominator_population),
            ),
            provenance: MeasurementProvenance::new(
                MetricSourceV1::FeedbackObservations,
                "feedback-observations.v1",
                "feedback-system-quality-projector.v1",
                &watermark,
            ),
            horizon: &read_model.horizon,
            coverage,
            value: complete.then_some(metric.value).flatten(),
            unavailable_reason: unavailable,
        }));
    }
    read_model.current &= feedback.coverage == FeedbackCoverageV1::Known;
    read_model.watermark = format!("{};{watermark}", read_model.watermark);
}

fn feedback_metric_descriptors() -> [(&'static str, &'static str, &'static str); 9] {
    [
        ("feedback_coverage", "ratio", "eligible_observations"),
        ("feedback_relevance", "ratio", "relevance_labels"),
        ("feedback_diversity", "ratio", "eligible_source_families"),
        ("feedback_latency_p95", "microseconds", "latency_samples"),
        (
            "feedback_omission_rate",
            "ratio",
            "returned_and_omitted_items",
        ),
        ("feedback_denial_rate", "ratio", "outcome_observations"),
        ("feedback_staleness_rate", "ratio", "outcome_observations"),
        (
            "feedback_revocation_propagation_p95",
            "microseconds",
            "revocation_observations",
        ),
        (
            "feedback_stack_transitions",
            "transitions",
            "stack_transition_observations",
        ),
    ]
}

const fn feedback_metric_name(kind: FeedbackSystemMetricKindV1) -> &'static str {
    match kind {
        FeedbackSystemMetricKindV1::Coverage => "feedback_coverage",
        FeedbackSystemMetricKindV1::Relevance => "feedback_relevance",
        FeedbackSystemMetricKindV1::Diversity => "feedback_diversity",
        FeedbackSystemMetricKindV1::Latency => "feedback_latency_p95",
        FeedbackSystemMetricKindV1::Omission => "feedback_omission_rate",
        FeedbackSystemMetricKindV1::Denial => "feedback_denial_rate",
        FeedbackSystemMetricKindV1::Staleness => "feedback_staleness_rate",
        FeedbackSystemMetricKindV1::RevocationPropagation => "feedback_revocation_propagation_p95",
        FeedbackSystemMetricKindV1::StackTransitions => "feedback_stack_transitions",
    }
}

const fn feedback_metric_unit(unit: FeedbackSystemMetricUnitV1) -> &'static str {
    match unit {
        FeedbackSystemMetricUnitV1::Ratio => "ratio",
        FeedbackSystemMetricUnitV1::Microseconds => "microseconds",
        FeedbackSystemMetricUnitV1::Transitions => "transitions",
    }
}

const fn feedback_denominator_name(denominator: FeedbackSystemMetricDenominatorV1) -> &'static str {
    match denominator {
        FeedbackSystemMetricDenominatorV1::EligibleObservations => "eligible_observations",
        FeedbackSystemMetricDenominatorV1::RelevanceLabels => "relevance_labels",
        FeedbackSystemMetricDenominatorV1::EligibleSourceFamilies => "eligible_source_families",
        FeedbackSystemMetricDenominatorV1::LatencySamples => "latency_samples",
        FeedbackSystemMetricDenominatorV1::ReturnedAndOmittedItems => "returned_and_omitted_items",
        FeedbackSystemMetricDenominatorV1::OutcomeObservations => "outcome_observations",
        FeedbackSystemMetricDenominatorV1::RevocationObservations => "revocation_observations",
        FeedbackSystemMetricDenominatorV1::StackTransitionObservations => {
            "stack_transition_observations"
        }
    }
}

const fn feedback_coverage_state(coverage: FeedbackCoverageV1) -> CoverageStateV1 {
    match coverage {
        FeedbackCoverageV1::Known => CoverageStateV1::Known,
        FeedbackCoverageV1::Partial => CoverageStateV1::Partial,
        FeedbackCoverageV1::Stale => CoverageStateV1::Stale,
        FeedbackCoverageV1::Unknown => CoverageStateV1::Unknown,
        FeedbackCoverageV1::Sampled => CoverageStateV1::Sampled,
        FeedbackCoverageV1::Capped => CoverageStateV1::Capped,
    }
}

const fn feedback_unavailable_reason(
    reason: crate::feedback::observations::FeedbackSystemMetricUnavailableReasonV1,
) -> &'static str {
    use crate::feedback::observations::FeedbackSystemMetricUnavailableReasonV1;
    match reason {
        FeedbackSystemMetricUnavailableReasonV1::NoEligibleObservations => {
            "no_eligible_observations"
        }
        FeedbackSystemMetricUnavailableReasonV1::NoRelevanceLabels => "no_relevance_labels",
        FeedbackSystemMetricUnavailableReasonV1::NoDiversityObservations => {
            "no_diversity_observations"
        }
        FeedbackSystemMetricUnavailableReasonV1::NoLatencySamples => "no_latency_samples",
        FeedbackSystemMetricUnavailableReasonV1::NoTruncationObservations => {
            "no_truncation_observations"
        }
        FeedbackSystemMetricUnavailableReasonV1::NoOutcomeObservations => "no_outcome_observations",
        FeedbackSystemMetricUnavailableReasonV1::NoRevocationObservations => {
            "no_revocation_observations"
        }
        FeedbackSystemMetricUnavailableReasonV1::NoStackTransitionObservations => {
            "no_stack_transition_observations"
        }
    }
}

pub fn costs_unavailable_read_model(
    scope_ref: Option<&str>,
    since_seconds: i64,
    reason: &str,
) -> CostsReadModelV1 {
    let observed_at_micros = now_micros().0;
    let read_horizon = horizon(since_seconds, observed_at_micros);
    let coverage = coverage(None, 0, 1, CoverageStateV1::Unknown);
    let (usage, estimated_cost) = {
        let metric = |name: &str,
                      unit: &str,
                      denominator: &str,
                      source: MetricSourceV1,
                      source_revision: &str| {
            measurement(MeasurementSpec {
                descriptor: MeasurementDescriptor::new(COST_DESCRIPTOR, name, unit, denominator),
                provenance: MeasurementProvenance::new(
                    source,
                    source_revision,
                    "costs-projector.v1",
                    "costs:unavailable",
                ),
                horizon: &read_horizon,
                coverage: coverage.clone(),
                value: None,
                unavailable_reason: Some(reason),
            })
        };
        (
            vec![
                metric(
                    "provider_tokens",
                    "tokens",
                    "provider_usage_observations",
                    MetricSourceV1::ProviderUsageObservation,
                    "provider-usage-observation.v1",
                ),
                metric(
                    "saved_tokens",
                    "tokens",
                    "eligible_savings_calls",
                    MetricSourceV1::SavingsLedger,
                    "savings-ledger.v1",
                ),
            ],
            vec![metric(
                "provider_cost",
                "usd",
                "priced_provider_usage_observations",
                MetricSourceV1::ProviderUsageObservation,
                "provider-usage-observation.v1",
            )],
        )
    };
    CostsReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark: "costs:unavailable".to_string(),
        observed_at_micros,
        current: false,
        usage,
        estimated_cost,
        pricing_revision: None,
    }
}

fn provider_token_total(aggregate: &ProviderUsageAggregateV1) -> Option<u64> {
    if aggregate.coverage != ProviderUsageCoverageV1::Complete {
        return None;
    }
    aggregate
        .totals
        .input_tokens?
        .checked_add(aggregate.totals.output_tokens?)
}

fn provider_token_total_since(
    aggregate: &ProviderUsageAggregateV1,
    since_seconds: i64,
) -> Option<u64> {
    if since_seconds <= 0 {
        return provider_token_total(aggregate);
    }
    if aggregate.coverage != ProviderUsageCoverageV1::Complete {
        return None;
    }
    aggregate.deltas.iter().try_fold(0_u64, |total, delta| {
        let timestamp = delta.native_timestamp?;
        if timestamp < since_seconds {
            return Some(total);
        }
        let tokens = delta
            .counters
            .input_tokens?
            .checked_add(delta.counters.output_tokens?)?;
        total.checked_add(tokens)
    })
}

fn provider_coverage(aggregate: &ProviderUsageAggregateV1) -> MetricCoverageV1 {
    let observed = aggregate.deltas.len() as u64;
    let unknown = aggregate.issues.len() as u64;
    let state = match aggregate.coverage {
        ProviderUsageCoverageV1::Complete => CoverageStateV1::Known,
        ProviderUsageCoverageV1::Partial => CoverageStateV1::Partial,
        ProviderUsageCoverageV1::Unavailable => CoverageStateV1::Unknown,
    };
    coverage(
        (aggregate.coverage == ProviderUsageCoverageV1::Complete)
            .then_some(aggregate.observations_seen),
        observed,
        unknown,
        state,
    )
}

/// Canonical costs projection over separate savings and provider authorities.
///
/// Provider usage is read only when the caller supplies both the retained
/// project session store and its exact typed scope. Projectless/profile-wide
/// callers receive typed unavailable provider metrics.
pub async fn costs_read_model(
    savings_db: &RegisteredGlobalDb,
    provider_usage_db: Option<&RegisteredGlobalDb>,
    provider_scope: Option<&ObservationScopeV1>,
    scope_ref: Option<&str>,
    since_seconds: i64,
) -> CostsReadModelV1 {
    let provider_usage = match (provider_usage_db, provider_scope) {
        (Some(db), Some(scope)) => provider_usage_aggregate(db, scope, None, None).await,
        _ => ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Unavailable,
            observations_seen: 0,
            totals: AggregatedProviderUsageCountersV1::unknown(),
            deltas: Vec::new(),
            issues: Vec::new(),
            upper_observation_sequence: None,
        },
    };
    costs_read_model_with_provider_usage(savings_db, scope_ref, since_seconds, &provider_usage)
        .await
}

/// Builds the costs projection from an already-pinned provider-usage read.
/// Dashboard composite reads use this to avoid rescanning the same immutable
/// observation frontier for sibling panels.
pub async fn costs_read_model_with_provider_usage(
    db: &RegisteredGlobalDb,
    scope_ref: Option<&str>,
    since_seconds: i64,
    provider_usage: &ProviderUsageAggregateV1,
) -> CostsReadModelV1 {
    let savings = db
        .savings_totals_with_watermark(scope_ref, since_seconds)
        .await
        .ok();
    let observed_at_micros = now_micros().0;
    let pricing = load_table();
    let cost_summary = price_provider_usage(provider_usage, pricing, since_seconds);
    let provider_cost = cost_summary.total_cost_usd;
    let priced_usage = cost_summary
        .usage_events
        .saturating_sub(cost_summary.unpriced_events);
    let unpriced_usage = cost_summary.unpriced_events;
    let read_horizon = horizon(since_seconds, observed_at_micros);
    let provider_watermark = provider_usage.upper_observation_sequence.map_or_else(
        || "provider-usage:unknown".to_string(),
        |upper| format!("provider-usage:{upper}"),
    );
    let savings_watermark = savings.as_ref().map_or_else(
        || "savings:unknown".to_string(),
        |(_, latest)| format!("savings:{latest}"),
    );
    let usage_coverage = provider_coverage(provider_usage);
    let savings_coverage = savings.as_ref().map_or_else(
        || coverage(None, 0, 1, CoverageStateV1::Unknown),
        |(totals, _)| coverage(Some(totals.calls), totals.calls, 0, CoverageStateV1::Known),
    );
    let provider_reason = (provider_usage.coverage != ProviderUsageCoverageV1::Complete).then_some(
        if provider_usage.coverage == ProviderUsageCoverageV1::Partial {
            "provider_usage_partial"
        } else {
            "provider_usage_unavailable"
        },
    );
    let savings_reason = savings.is_none().then_some("savings_store_unavailable");
    let tokens =
        provider_token_total_since(provider_usage, since_seconds).map(|value| value as f64);
    let saved_tokens = savings
        .as_ref()
        .map(|(totals, _)| totals.saved_tokens as f64);
    let pricing_reason = if provider_usage.coverage == ProviderUsageCoverageV1::Unavailable {
        provider_reason
    } else if provider_cost.is_none() {
        Some("provider_model_pricing_unavailable")
    } else {
        None
    };
    let usage = vec![
        measurement(MeasurementSpec {
            descriptor: MeasurementDescriptor::new(
                COST_DESCRIPTOR,
                "provider_tokens",
                "tokens",
                "provider_usage_observations",
            ),
            provenance: MeasurementProvenance::new(
                MetricSourceV1::ProviderUsageObservation,
                "provider-usage-observation.v1",
                "costs-projector.v1",
                &provider_watermark,
            ),
            horizon: &read_horizon,
            coverage: usage_coverage.clone(),
            value: tokens,
            unavailable_reason: provider_reason,
        }),
        measurement(MeasurementSpec {
            descriptor: MeasurementDescriptor::new(
                COST_DESCRIPTOR,
                "saved_tokens",
                "tokens",
                "eligible_savings_calls",
            ),
            provenance: MeasurementProvenance::new(
                MetricSourceV1::SavingsLedger,
                "savings-ledger.v1",
                "costs-projector.v1",
                &savings_watermark,
            ),
            horizon: &read_horizon,
            coverage: savings_coverage,
            value: saved_tokens,
            unavailable_reason: savings_reason,
        }),
    ];
    let estimated_cost = vec![measurement(MeasurementSpec {
        descriptor: MeasurementDescriptor::new(
            COST_DESCRIPTOR,
            "provider_cost",
            "usd",
            "priced_provider_usage_observations",
        ),
        provenance: MeasurementProvenance::new(
            MetricSourceV1::ProviderUsageObservation,
            "provider-usage-observation.v1",
            "costs-projector.v1",
            &provider_watermark,
        ),
        horizon: &read_horizon,
        coverage: if provider_cost.is_some() {
            coverage(Some(priced_usage), priced_usage, 0, CoverageStateV1::Known)
        } else {
            coverage(None, priced_usage, unpriced_usage, CoverageStateV1::Partial)
        },
        value: provider_cost,
        unavailable_reason: pricing_reason,
    })];
    let known = usage
        .iter()
        .chain(&estimated_cost)
        .all(|metric| metric.coverage.state == CoverageStateV1::Known);
    let watermark = format!("{provider_watermark};{savings_watermark}");
    CostsReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark,
        observed_at_micros,
        current: known,
        usage,
        estimated_cost,
        pricing_revision: Some(cost_summary.pricing_revision),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::{
        AggregatedProviderUsageCountersV1, ProviderUsageAggregateV1, ProviderUsageCoverageV1,
    };
    use tracedecay_application::{
        ObservabilityQueryPort, ObservabilityQueryV1, ObservabilityRecordPort,
    };
    use tracedecay_domain::{
        ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityRetentionClassV1,
        ObservabilityTerminalResultV1, RetrievalQueryObservedV1,
    };

    fn envelope(event_id: &str, event_time_micros: i64) -> ObservabilityEnvelopeV1 {
        ObservabilityEnvelopeV1 {
            event_id: event_id.to_string(),
            event_kind: "retrieval.query.completed.v1".to_string(),
            schema_revision: 1,
            idempotency_key: format!("idempotency:{event_id}"),
            trace_id: format!("trace:{event_id}"),
            scope_ref: "scope:boundary".to_string(),
            capability: "retrieval".to_string(),
            operation: "query".to_string(),
            event_time_micros,
            observation_time_micros: event_time_micros,
            valid_from_micros: None,
            valid_until_micros: None,
            quantity: None,
            unit: None,
            terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
            producer_revision: "producer.v1".to_string(),
            configuration_revision: "configuration.v1".to_string(),
            policy_revision: "policy.v1".to_string(),
            watermark: format!("watermark:{event_id}"),
            coverage: CoverageStateV1::Known,
            sampling_probability: None,
            retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
            emitted_count: 1,
            delayed_count: 0,
            dropped_count: 0,
            process_boot_id: "boot:boundary".to_string(),
            producer_sequence: 1,
            payload: ObservabilityPayloadV1::RetrievalQuery(RetrievalQueryObservedV1 {
                query_family: "exact_technical".to_string(),
                enabled_lanes: vec!["exact_literal".to_string()],
                candidate_budget: 1,
                context_budget: 1,
                token_budget: 1,
                answered: true,
                source_coverage: CoverageStateV1::Known,
                lane_coverage: CoverageStateV1::Known,
            }),
        }
    }

    #[test]
    fn partial_coverage_never_claims_current() {
        let value = coverage(None, 8, 2, CoverageStateV1::Partial);
        assert_eq!(value.state, CoverageStateV1::Partial);
        assert_eq!(value.unknown, 2);
        assert_eq!(value.eligible, None);
    }

    #[test]
    fn provider_token_metric_is_absent_for_partial_or_incomplete_evidence() {
        let aggregate = ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Partial,
            observations_seen: 2,
            totals: AggregatedProviderUsageCountersV1 {
                input_tokens: Some(10),
                output_tokens: Some(4),
                ..AggregatedProviderUsageCountersV1::unknown()
            },
            deltas: Vec::new(),
            issues: Vec::new(),
            upper_observation_sequence: Some(2),
        };
        assert_eq!(provider_token_total(&aggregate), None);

        let aggregate = ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Complete,
            totals: AggregatedProviderUsageCountersV1 {
                input_tokens: Some(10),
                output_tokens: None,
                ..AggregatedProviderUsageCountersV1::unknown()
            },
            ..aggregate
        };
        assert_eq!(provider_token_total(&aggregate), None);
    }

    #[test]
    fn complete_provider_usage_sums_input_and_output_without_double_counting_total() {
        let aggregate = ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Complete,
            observations_seen: 2,
            totals: AggregatedProviderUsageCountersV1 {
                input_tokens: Some(10),
                output_tokens: Some(4),
                total_tokens: Some(14),
                ..AggregatedProviderUsageCountersV1::unknown()
            },
            deltas: Vec::new(),
            issues: Vec::new(),
            upper_observation_sequence: Some(2),
        };
        assert_eq!(provider_token_total(&aggregate), Some(14));
    }

    #[test]
    fn feedback_quality_metrics_are_typed_and_never_fabricate_empty_zeroes() {
        let mut observatory =
            observatory_unavailable_read_model(Some("scope:test"), 1, "store_unavailable");
        let feedback = FeedbackObservationReadModelV1::project(&[]).unwrap();
        attach_feedback_system_quality(&mut observatory, Some(&feedback), None);

        let feedback_metrics = observatory
            .metrics
            .iter()
            .filter(|metric| metric.descriptor_revision == FEEDBACK_DESCRIPTOR)
            .collect::<Vec<_>>();
        assert_eq!(feedback_metrics.len(), 9);
        assert!(
            feedback_metrics.iter().all(|metric| {
                metric.value.is_none()
                    && metric.denominator_value.is_none()
                    && metric.coverage.state == CoverageStateV1::Unknown
                    && metric.unavailable_reason.is_some()
            }),
            "unsupported feedback measurements remain explicitly unknown"
        );
    }

    #[test]
    fn surface_serializers_preserve_values_denominators_and_coverage() {
        let observatory = observatory_unavailable_read_model(
            Some("scope:parity"),
            10,
            "fixture_source_unavailable",
        );
        let cli = observatory_cli_value(&observatory).expect("CLI JSON");
        let mcp = observatory_mcp_value(&observatory).expect("MCP JSON");
        let http = observatory_http_value(&observatory).expect("HTTP JSON");
        let dashboard = serde_json::to_value(&observatory).expect("dashboard payload");
        let export: serde_json::Value =
            serde_json::from_slice(&observatory_export_bytes(&observatory).expect("export JSON"))
                .expect("decode export JSON");
        assert_eq!(cli, mcp);
        assert_eq!(cli, http);
        assert_eq!(cli, dashboard);
        assert_eq!(cli, export);
        assert_eq!(cli["metrics"][0]["value"], serde_json::Value::Null);
        assert_eq!(
            cli["metrics"][0]["denominator_value"],
            serde_json::Value::Null
        );
        assert_eq!(cli["metrics"][0]["coverage"]["state"], "unknown");
        assert_eq!(
            cli["metrics"][0]["unavailable_reason"],
            "fixture_source_unavailable"
        );

        let costs =
            costs_unavailable_read_model(Some("scope:parity"), 10, "fixture_cost_unavailable");
        let cli = costs_cli_value(&costs).expect("CLI costs JSON");
        let mcp = costs_mcp_value(&costs).expect("MCP costs JSON");
        let http = costs_http_value(&costs).expect("HTTP costs JSON");
        let dashboard = serde_json::to_value(&costs).expect("dashboard costs payload");
        let export: serde_json::Value =
            serde_json::from_slice(&costs_export_bytes(&costs).expect("costs export JSON"))
                .expect("decode costs export JSON");
        assert_eq!(cli, mcp);
        assert_eq!(cli, http);
        assert_eq!(cli, dashboard);
        assert_eq!(cli, export);
        assert_eq!(cli["usage"][0]["coverage"]["state"], "unknown");
        assert_eq!(
            cli["usage"][0]["denominator_value"],
            serde_json::Value::Null
        );
        assert_eq!(
            cli["usage"][0]["unavailable_reason"],
            "fixture_cost_unavailable"
        );
    }

    #[tokio::test]
    async fn exact_horizon_scans_past_dense_coarse_boundary_rows() {
        let harness = tracedecay_global_db::tests::harness::RegisteredGlobalDbHarness::open(
            "observability-exact-horizon",
        )
        .await;
        let port = RegisteredObservabilityPortV1::new(&harness.registered);
        for (index, event_time_micros) in [1_510_000, 1_520_000, 1_530_000].into_iter().enumerate()
        {
            port.record(envelope(&format!("eligible:{index}"), event_time_micros))
                .await
                .expect("record eligible event");
        }
        for index in 0..70 {
            let event_time_micros = if index % 2 == 0 {
                1_100_000 + index
            } else {
                1_900_000 + index
            };
            port.record(envelope(&format!("boundary:{index}"), event_time_micros))
                .await
                .expect("record coarse boundary event");
        }

        let first = port
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: "scope:boundary".to_string(),
                event_kinds: vec!["retrieval.query.completed.v1".to_string()],
                horizon: ObservabilityHorizonV1 {
                    since_micros: 1_500_000,
                    until_micros: 1_600_000,
                },
                after_watermark: None,
                limit: 2,
            })
            .await
            .expect("first exact page");
        assert_eq!(
            first
                .events
                .iter()
                .map(|event| event.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["eligible:1", "eligible:2"]
        );
        assert_eq!(first.coverage, CoverageStateV1::Capped);
        let second = port
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: "scope:boundary".to_string(),
                event_kinds: vec!["retrieval.query.completed.v1".to_string()],
                horizon: ObservabilityHorizonV1 {
                    since_micros: 1_500_000,
                    until_micros: 1_600_000,
                },
                after_watermark: first.next_watermark,
                limit: 2,
            })
            .await
            .expect("second exact page");
        assert_eq!(
            second
                .events
                .iter()
                .map(|event| event.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["eligible:0"]
        );
        assert_eq!(second.coverage, CoverageStateV1::Known);
        assert_eq!(second.next_watermark, None);
    }
}
