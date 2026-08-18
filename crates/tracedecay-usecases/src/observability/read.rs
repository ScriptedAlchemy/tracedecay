//! Canonical Observatory read composition over retained observation envelopes.

use tracedecay_application::{MetricSourceV1, ObservatoryReadModelV1, now_micros};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1,
};
use tracedecay_global_db::{AnalyticsEventQuery, RegisteredGlobalDb};

use super::read_model::{
    project_plan26_read_models, project_rejected_arguments, unavailable_plan26_read_models,
    unavailable_rejected_arguments,
};
use super::{
    ANALYTICS_DESCRIPTOR, EVENT_LIMIT, MeasurementDescriptor, MeasurementProvenance,
    MeasurementSpec, OBSERVABILITY_PROVIDER, coverage, horizon, measurement,
};

pub fn observatory_unavailable_read_model(
    scope_ref: Option<&str>,
    since_seconds: i64,
    reason: &str,
) -> ObservatoryReadModelV1 {
    let observed_at_micros = now_micros().0;
    let read_horizon = horizon(since_seconds, observed_at_micros);
    let watermark = "analytics:unavailable".to_string();
    let metric_coverage = coverage(None, 0, 1, CoverageStateV1::Unknown);
    let mut metrics = {
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
            metric("observability_eligible_events"),
            metric("observability_events"),
            metric("observability_late_arrivals"),
            metric("observability_failures"),
            metric("telemetry_drops_lower_bound"),
        ]
    };
    let (analytics_mode, comparison, mut plan_metrics) =
        unavailable_plan26_read_models(&read_horizon, &watermark, reason);
    metrics.append(&mut plan_metrics);
    ObservatoryReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        rejected_arguments: unavailable_rejected_arguments(&watermark, reason),
        watermark,
        observed_at_micros,
        current: false,
        metrics,
        analytics_mode,
        comparison,
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
    let observed = events.iter().fold(0_u64, |total, event| {
        total.saturating_add(event.emitted_count)
    });
    let delayed = events.iter().fold(0_u64, |total, event| {
        total.saturating_add(event.delayed_count)
    });
    let explicit_drop_carriers = events
        .iter()
        .filter_map(|event| match &event.payload {
            ObservabilityPayloadV1::TelemetryDrop(drop) => Some((
                (
                    event.process_boot_id.clone(),
                    drop.last_missing_sequence.saturating_add(1),
                ),
                drop.proved_drop_lower_bound,
            )),
            _ => None,
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let dropped = events.iter().fold(0u64, |total, event| {
        let payload_drops = match &event.payload {
            ObservabilityPayloadV1::TelemetryDrop(drop) => drop.proved_drop_lower_bound,
            _ => event.dropped_count.saturating_sub(
                explicit_drop_carriers
                    .get(&(event.process_boot_id.clone(), event.producer_sequence))
                    .copied()
                    .unwrap_or(0),
            ),
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
    let eligible = observed.saturating_add(dropped);
    let exact_eligible = complete.then_some(eligible);
    let metric_coverage = coverage(exact_eligible, observed, unknown, event_state);
    let reason = (!complete).then_some("incomplete_observability_coverage");
    let mut metrics = {
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
            metric("observability_eligible_events", eligible, "events"),
            metric("observability_events", observed, "events"),
            metric("observability_late_arrivals", delayed, "events"),
            metric("observability_failures", failed, "events"),
            metric("telemetry_drops_lower_bound", dropped, "events"),
        ]
    };
    let event_refs = events.iter().collect::<Vec<_>>();
    let (analytics_mode, comparison, mut plan_metrics) =
        project_plan26_read_models(&event_refs, &read_horizon, &watermark, event_state, unknown);
    metrics.append(&mut plan_metrics);
    let rejected_arguments =
        project_rejected_arguments(&event_refs, &watermark, complete, unknown);
    ObservatoryReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark,
        observed_at_micros,
        current: complete,
        metrics,
        analytics_mode,
        comparison,
        rejected_arguments,
    }
}
