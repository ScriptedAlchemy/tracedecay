//! Production Plan 26 read-model composition over the canonical accounting store.

use tracedecay_application::{
    ApplicationContractError, ObservabilityFuture, ObservabilityPageV1, ObservabilityQueryPort,
    ObservabilityQueryV1, ObservabilityRecordPort,
};
use tracedecay_application::{
    CostsReadModelV1, MetricCohortV1, MetricCoverageV1, MetricEvidenceClassV1, MetricProvenanceV1,
    MetricSourceV1, MetricTemporalV1, MetricUncertaintyV1, MetricValueV1, ObservabilityHorizonV1,
    ObservatoryReadModelV1, now_micros,
};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1,
};

use crate::feedback::observations::{
    FeedbackObservationReadModelV1, FeedbackSystemMetricDenominatorV1, FeedbackSystemMetricKindV1,
    FeedbackSystemMetricUnitV1, Plan26CoverageV1,
};
use tracedecay_global_db::{AnalyticsEventInsert, AnalyticsEventQuery, RegisteredGlobalDb};

const EVENT_LIMIT: usize = 10_000;
const OBSERVABILITY_SCAN_PAGE: usize = 64;
const OBSERVABILITY_PROVIDER: &str = "tracedecay-observability";
const ANALYTICS_DESCRIPTOR: &str = "analytics-events.v1";
const COST_DESCRIPTOR: &str = "provider-costs.v1";
const FEEDBACK_DESCRIPTOR: &str = "feedback-system-quality.v1";

/// Canonical wire projection used by every PR14 surface. Adapters may wrap the
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

/// Production adapter for the canonical application record/query boundary.
/// The complete versioned envelope is retained as JSON while indexed columns
/// provide bounded scope/kind/time queries.
#[derive(Clone, Copy)]
pub struct RegisteredObservabilityPortV1<'a> {
    db: &'a RegisteredGlobalDb,
}

impl<'a> RegisteredObservabilityPortV1<'a> {
    pub const fn new(db: &'a RegisteredGlobalDb) -> Self {
        Self { db }
    }
}

impl ObservabilityRecordPort for RegisteredObservabilityPortV1<'_> {
    fn record(&self, envelope: ObservabilityEnvelopeV1) -> ObservabilityFuture<'_, String> {
        Box::pin(async move {
            envelope
                .validate()
                .map_err(|error| ApplicationContractError::Domain(error.to_string()))?;
            let metadata_json = serde_json::to_string(&envelope)
                .map_err(|error| ApplicationContractError::Domain(error.to_string()))?;
            let insert = AnalyticsEventInsert {
                provider: OBSERVABILITY_PROVIDER.to_string(),
                project_id: envelope.scope_ref.clone(),
                session_id: None,
                timestamp: envelope.event_time_micros.div_euclid(1_000_000),
                event_kind: envelope.event_kind.clone(),
                hook_name: None,
                tool_name: None,
                tool_category: None,
                skill_name: None,
                hint_category: None,
                hint_id: Some(envelope.idempotency_key.clone()),
                outcome: envelope
                    .terminal_result
                    .map(|result| format!("{result:?}").to_ascii_lowercase()),
                metadata_json: Some(metadata_json),
            };
            self.db
                .append_observability_event(&insert)
                .await
                .map(|id| format!("analytics:{id}"))
                .map_err(ApplicationContractError::Domain)
        })
    }
}

impl ObservabilityQueryPort for RegisteredObservabilityPortV1<'_> {
    fn query(&self, query: ObservabilityQueryV1) -> ObservabilityFuture<'_, ObservabilityPageV1> {
        Box::pin(async move {
            if query.limit == 0 {
                return Err(ApplicationContractError::ZeroValue {
                    field: "observability_query.limit",
                });
            }
            if query.horizon.until_micros <= query.horizon.since_micros {
                return Err(ApplicationContractError::InvalidRange {
                    field: "observability_query.horizon",
                });
            }
            let requested = usize::try_from(query.limit)
                .unwrap_or(EVENT_LIMIT)
                .min(EVENT_LIMIT);
            let mut scan_before_id = match query.after_watermark.as_deref() {
                None => None,
                Some(value) => Some(
                    value
                        .strip_prefix("analytics:")
                        .and_then(|value| value.parse::<i64>().ok())
                        .filter(|value| *value > 0)
                        .ok_or(ApplicationContractError::InvalidRange {
                            field: "observability_query.after_watermark",
                        })?,
                ),
            };
            let scope_ref = query.authorized_scope_ref;
            let coarse_since = query.horizon.since_micros.div_euclid(1_000_000);
            let coarse_until = query
                .horizon
                .until_micros
                .saturating_add(999_999)
                .div_euclid(1_000_000);
            let scan_limit = requested.clamp(OBSERVABILITY_SCAN_PAGE, EVENT_LIMIT);
            let mut eligible = Vec::with_capacity(requested.saturating_add(1));
            let mut watermark_id = None;
            let mut invalid_in_page = false;
            let mut invalid_after_page = false;
            'scan: loop {
                let rows = self
                    .db
                    .query_analytics_events(&AnalyticsEventQuery {
                        provider: Some(OBSERVABILITY_PROVIDER.to_string()),
                        project_id: Some(scope_ref.clone()),
                        event_kind: (query.event_kinds.len() == 1)
                            .then(|| query.event_kinds[0].clone()),
                        since: Some(coarse_since),
                        until: Some(coarse_until),
                        before_id: scan_before_id,
                        limit: scan_limit,
                        ..AnalyticsEventQuery::default()
                    })
                    .await
                    .map_err(ApplicationContractError::Domain)?;
                if rows.is_empty() {
                    break;
                }
                let Some(newest_row) = rows.last() else {
                    break;
                };
                watermark_id.get_or_insert(newest_row.id);
                let Some(oldest_row) = rows.first() else {
                    break;
                };
                let next_scan_before_id = oldest_row.id;
                let exhausted = rows.len() < scan_limit;
                for row in rows.iter().rev() {
                    let row_requested =
                        query.event_kinds.is_empty() || query.event_kinds.contains(&row.event_kind);
                    let envelope = row
                        .metadata_json
                        .as_deref()
                        .and_then(|value| {
                            serde_json::from_str::<ObservabilityEnvelopeV1>(value).ok()
                        })
                        .filter(|envelope| envelope.validate().is_ok());
                    let Some(envelope) = envelope else {
                        if row_requested {
                            if eligible.len() < requested {
                                invalid_in_page = true;
                            } else {
                                invalid_after_page = true;
                            }
                        }
                        continue;
                    };
                    let envelope_requested = query.event_kinds.is_empty()
                        || query.event_kinds.contains(&envelope.event_kind);
                    if !row_requested && !envelope_requested {
                        continue;
                    }
                    if envelope.scope_ref != scope_ref
                        || envelope.event_kind != row.event_kind
                        || envelope.event_time_micros < query.horizon.since_micros
                        || envelope.event_time_micros >= query.horizon.until_micros
                    {
                        if envelope.event_time_micros >= query.horizon.since_micros
                            && envelope.event_time_micros < query.horizon.until_micros
                        {
                            if eligible.len() < requested {
                                invalid_in_page = true;
                            } else {
                                invalid_after_page = true;
                            }
                        }
                        continue;
                    }
                    if !envelope_requested {
                        continue;
                    }
                    eligible.push((row.id, envelope));
                    if eligible.len() > requested {
                        break 'scan;
                    }
                }
                if exhausted {
                    break;
                }
                scan_before_id = Some(next_scan_before_id);
            }
            let capped = eligible.len() > requested;
            if capped {
                eligible.pop();
            } else if invalid_after_page {
                invalid_in_page = true;
            }
            let next_watermark = if capped {
                Some(format!(
                    "analytics:{}",
                    eligible
                        .last()
                        .ok_or(ApplicationContractError::Inconsistent {
                            field: "observability_page.cursor",
                        })?
                        .0
                ))
            } else {
                None
            };
            let mut coverage = if capped {
                CoverageStateV1::Capped
            } else {
                CoverageStateV1::Known
            };
            for (_, event) in &eligible {
                coverage = merge_coverage_state(coverage, event.coverage);
            }
            if invalid_in_page {
                coverage = merge_coverage_state(coverage, CoverageStateV1::Partial);
            }
            eligible.sort_by(|(_, left), (_, right)| {
                (
                    left.event_time_micros,
                    left.observation_time_micros,
                    left.producer_sequence,
                    left.event_id.as_str(),
                )
                    .cmp(&(
                        right.event_time_micros,
                        right.observation_time_micros,
                        right.producer_sequence,
                        right.event_id.as_str(),
                    ))
            });
            let event_cursors = eligible
                .iter()
                .map(|(id, _)| format!("analytics:{id}"))
                .collect();
            let events = eligible
                .into_iter()
                .map(|(_, envelope)| envelope)
                .collect::<Vec<_>>();
            Ok(ObservabilityPageV1 {
                events,
                event_cursors,
                watermark: watermark_id.map_or_else(
                    || "analytics:empty".to_string(),
                    |id| format!("analytics:{id}"),
                ),
                coverage,
                next_watermark,
            })
        })
    }
}

fn merge_coverage_state(left: CoverageStateV1, right: CoverageStateV1) -> CoverageStateV1 {
    const fn rank(state: CoverageStateV1) -> u8 {
        match state {
            CoverageStateV1::Known => 0,
            CoverageStateV1::Capped => 1,
            CoverageStateV1::Sampled => 2,
            CoverageStateV1::Partial => 3,
            CoverageStateV1::Stale => 4,
            CoverageStateV1::Unknown => 5,
        }
    }
    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
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

pub fn observatory_unavailable_read_model(
    scope_ref: Option<&str>,
    since_seconds: i64,
    reason: &str,
) -> ObservatoryReadModelV1 {
    let observed_at_micros = now_micros().0;
    let read_horizon = horizon(since_seconds, observed_at_micros);
    let watermark = "analytics:unavailable".to_string();
    let metric_coverage = coverage(None, 0, 1, CoverageStateV1::Unknown);
    let metrics = {
        let metric = |name: &str| {
            measurement(MeasurementSpec {
                descriptor: MeasurementDescriptor::new(
                    ANALYTICS_DESCRIPTOR,
                    name,
                    "events",
                    "eligible_observability_events",
                ),
                provenance: MeasurementProvenance::new(
                    MetricSourceV1::ObservabilityEnvelope,
                    "observability-envelope.v1",
                    "observatory-projector.v1",
                    &watermark,
                ),
                horizon: &read_horizon,
                coverage: metric_coverage.clone(),
                value: None,
                unavailable_reason: Some(reason),
            })
        };
        vec![
            metric("observability_events"),
            metric("observability_failures"),
            metric("telemetry_drops_lower_bound"),
        ]
    };
    ObservatoryReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark,
        observed_at_micros,
        current: false,
        metrics,
    }
}

/// Canonical Observatory projection shared by CLI, MCP, and dashboard HTTP.
pub async fn observatory_read_model(
    db: &RegisteredGlobalDb,
    scope_ref: Option<&str>,
    since_seconds: i64,
) -> ObservatoryReadModelV1 {
    let observed_at_micros = now_micros().0;
    let rows = db
        .query_analytics_events(&AnalyticsEventQuery {
            provider: Some(OBSERVABILITY_PROVIDER.to_string()),
            project_id: scope_ref.map(str::to_owned),
            since: Some(since_seconds),
            until: Some(
                observed_at_micros
                    .saturating_add(999_999)
                    .div_euclid(1_000_000),
            ),
            limit: EVENT_LIMIT.saturating_add(1),
            ..AnalyticsEventQuery::default()
        })
        .await;
    let Ok(mut rows) = rows else {
        return observatory_unavailable_read_model(
            scope_ref,
            since_seconds,
            "observability_store_unavailable",
        );
    };
    let capped = rows.len() > EVENT_LIMIT;
    if capped {
        rows.remove(0);
    }
    let mut invalid = 0u64;
    let events = rows
        .iter()
        .filter_map(|row| {
            let envelope = row
                .metadata_json
                .as_deref()
                .and_then(|value| serde_json::from_str::<ObservabilityEnvelopeV1>(value).ok())
                .filter(|envelope| envelope.validate().is_ok());
            if envelope.is_none() {
                invalid = invalid.saturating_add(1);
            }
            envelope
        })
        .collect::<Vec<_>>();
    let observed = events.len() as u64;
    let dropped = events.iter().fold(0u64, |total, event| {
        let payload_drops = match &event.payload {
            ObservabilityPayloadV1::TelemetryDrop(drop) => drop.proved_drop_lower_bound,
            _ => event.dropped_count,
        };
        total.saturating_add(payload_drops)
    });
    let unknown = invalid.saturating_add(dropped);
    let event_state = if capped {
        CoverageStateV1::Capped
    } else if events
        .iter()
        .any(|event| event.coverage == CoverageStateV1::Unknown)
    {
        CoverageStateV1::Unknown
    } else if events
        .iter()
        .any(|event| event.coverage == CoverageStateV1::Stale)
    {
        CoverageStateV1::Stale
    } else if invalid > 0
        || dropped > 0
        || events
            .iter()
            .any(|event| event.coverage == CoverageStateV1::Partial)
    {
        CoverageStateV1::Partial
    } else if events
        .iter()
        .any(|event| event.coverage == CoverageStateV1::Sampled)
    {
        CoverageStateV1::Sampled
    } else if events
        .iter()
        .any(|event| event.coverage == CoverageStateV1::Capped)
    {
        CoverageStateV1::Capped
    } else {
        CoverageStateV1::Known
    };
    let complete = event_state == CoverageStateV1::Known;
    let failed = events
        .iter()
        .filter(|event| {
            matches!(
                event.terminal_result,
                Some(
                    ObservabilityTerminalResultV1::Failed | ObservabilityTerminalResultV1::TimedOut
                )
            )
        })
        .count() as u64;
    let watermark = rows.last().map_or_else(
        || "analytics:empty".to_string(),
        |event| format!("analytics:{}", event.id),
    );
    let read_horizon = horizon(since_seconds, observed_at_micros);
    let exact_eligible = complete.then_some(observed);
    let metric_coverage = coverage(exact_eligible, observed, unknown, event_state);
    let reason = (!complete).then_some("incomplete_observability_coverage");
    let metrics = {
        let metric = |name: &str, value: u64, unit: &str| {
            measurement(MeasurementSpec {
                descriptor: MeasurementDescriptor::new(
                    ANALYTICS_DESCRIPTOR,
                    name,
                    unit,
                    "eligible_observability_events",
                ),
                provenance: MeasurementProvenance::new(
                    MetricSourceV1::ObservabilityEnvelope,
                    "observability-envelope.v1",
                    "observatory-projector.v1",
                    &watermark,
                ),
                horizon: &read_horizon,
                coverage: metric_coverage.clone(),
                value: complete.then_some(value as f64),
                unavailable_reason: reason,
            })
        };
        vec![
            metric("observability_events", observed, "events"),
            metric("observability_failures", failed, "events"),
            metric("telemetry_drops_lower_bound", dropped, "events"),
        ]
    };
    ObservatoryReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark,
        observed_at_micros,
        current: complete,
        metrics,
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
    read_model.current &= feedback.coverage == Plan26CoverageV1::Known;
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

const fn feedback_coverage_state(coverage: Plan26CoverageV1) -> CoverageStateV1 {
    match coverage {
        Plan26CoverageV1::Known => CoverageStateV1::Known,
        Plan26CoverageV1::Partial => CoverageStateV1::Partial,
        Plan26CoverageV1::Stale => CoverageStateV1::Stale,
        Plan26CoverageV1::Unknown => CoverageStateV1::Unknown,
        Plan26CoverageV1::Sampled => CoverageStateV1::Sampled,
        Plan26CoverageV1::Capped => CoverageStateV1::Capped,
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
                    "ingested_provider_turns",
                    MetricSourceV1::AccountingTurn,
                    "accounting-turn.v1",
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
                "priced_provider_turns",
                MetricSourceV1::AccountingTurn,
                "accounting-turn.v1",
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

/// Canonical Costs projection. Prices are recorded at ingest; transports never
/// join a pricing table or recompute dollar formulas.
pub async fn costs_read_model(
    db: &RegisteredGlobalDb,
    scope_ref: Option<&str>,
    since_seconds: i64,
) -> CostsReadModelV1 {
    let since = since_seconds.max(0) as u64;
    // `turns.project_hash` is a provider-import label, not an authoritative
    // ProjectId. Never return global turn totals under a project-scoped label.
    let accounting = if scope_ref.is_none() {
        db.accounting_totals_since(since).await
    } else {
        None
    };
    let savings = db
        .savings_totals_with_watermark(scope_ref, since_seconds)
        .await
        .ok();
    let observed_at_micros = now_micros().0;
    let read_horizon = horizon(since_seconds, observed_at_micros);
    let accounting_watermark = accounting.map_or_else(
        || "turns:unknown".to_string(),
        |(turns, _, _, latest)| format!("turns:{turns}:{latest}"),
    );
    let savings_watermark = savings.as_ref().map_or_else(
        || "savings:unknown".to_string(),
        |(_, latest)| format!("savings:{latest}"),
    );
    let accounting_coverage = accounting.map_or_else(
        || coverage(None, 0, 1, CoverageStateV1::Unknown),
        |(turns, _, _, _)| coverage(Some(turns), turns, 0, CoverageStateV1::Known),
    );
    let savings_coverage = savings.as_ref().map_or_else(
        || coverage(None, 0, 1, CoverageStateV1::Unknown),
        |(totals, _)| coverage(Some(totals.calls), totals.calls, 0, CoverageStateV1::Known),
    );
    let accounting_reason = accounting.is_none().then_some(if scope_ref.is_some() {
        "project_turn_scope_unavailable"
    } else {
        "accounting_store_unavailable"
    });
    let savings_reason = savings.is_none().then_some("savings_store_unavailable");
    let tokens = accounting.map(|(_, tokens, _, _)| tokens as f64);
    let saved_tokens = savings
        .as_ref()
        .map(|(totals, _)| totals.saved_tokens as f64);
    let pricing_reason = accounting
        .is_some()
        .then_some("pricing_revision_unavailable")
        .or(accounting_reason);
    let usage = vec![
        measurement(MeasurementSpec {
            descriptor: MeasurementDescriptor::new(
                COST_DESCRIPTOR,
                "provider_tokens",
                "tokens",
                "ingested_provider_turns",
            ),
            provenance: MeasurementProvenance::new(
                MetricSourceV1::AccountingTurn,
                "accounting-turn.v1",
                "costs-projector.v1",
                &accounting_watermark,
            ),
            horizon: &read_horizon,
            coverage: accounting_coverage.clone(),
            value: tokens,
            unavailable_reason: accounting_reason,
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
            "priced_provider_turns",
        ),
        provenance: MeasurementProvenance::new(
            MetricSourceV1::AccountingTurn,
            "accounting-turn.v1",
            "costs-projector.v1",
            &accounting_watermark,
        ),
        horizon: &read_horizon,
        coverage: if accounting.is_some() {
            coverage(
                None,
                accounting.map_or(0, |value| value.0),
                1,
                CoverageStateV1::Unknown,
            )
        } else {
            accounting_coverage
        },
        value: None,
        unavailable_reason: pricing_reason,
    })];
    let known = usage
        .iter()
        .chain(&estimated_cost)
        .all(|metric| metric.coverage.state == CoverageStateV1::Known);
    let watermark = format!("{accounting_watermark};{savings_watermark}");
    CostsReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark,
        observed_at_micros,
        current: known,
        usage,
        estimated_cost,
        pricing_revision: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        ObservabilityPayloadV1, ObservabilityRetentionClassV1, RetrievalQueryObservedV1,
    };

    fn envelope(event_id: &str, event_time_micros: i64) -> ObservabilityEnvelopeV1 {
        ObservabilityEnvelopeV1 {
            event_id: event_id.to_string(),
            event_kind: "retrieval.query.observed.v1".to_string(),
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
    fn pr14_surface_serializers_preserve_values_denominators_and_coverage() {
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
                event_kinds: vec!["retrieval.query.observed.v1".to_string()],
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
                event_kinds: vec!["retrieval.query.observed.v1".to_string()],
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
