use tracedecay_application::{
    CostsReadModelV1, MetricSourceV1, ObservatoryReadModelV1, now_micros,
};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1,
};
use tracedecay_global_db::{AnalyticsEventQuery, RegisteredGlobalDb};

use super::{
    ANALYTICS_DESCRIPTOR, COST_DESCRIPTOR, EVENT_LIMIT, MeasurementDescriptor,
    MeasurementProvenance, MeasurementSpec, OBSERVABILITY_PROVIDER, coverage, horizon, measurement,
};

/// Checked Observatory projection for authorities that expose typed source
/// coverage instead of collapsing store failure into unknown measurements.
pub async fn observatory_read_model_checked(
    db: &RegisteredGlobalDb,
    scope_ref: Option<&str>,
    since_seconds: i64,
) -> Result<ObservatoryReadModelV1, String> {
    let observed_at_micros = now_micros().0;
    let mut rows = db
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
        .await?;
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
    Ok(ObservatoryReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark,
        observed_at_micros,
        current: complete,
        metrics,
    })
}

/// Checked costs projection for typed authorities that cannot turn an
/// unavailable ledger into a successful unknown-valued read.
pub async fn costs_read_model_checked(
    db: &RegisteredGlobalDb,
    scope_ref: Option<&str>,
    since_seconds: i64,
) -> Result<CostsReadModelV1, String> {
    let since = since_seconds.max(0) as u64;
    // `turns.project_hash` is a provider-import label, not an authoritative
    // ProjectId. Never return global turn totals under a project-scoped label.
    let accounting = if scope_ref.is_none() {
        Some(db.try_accounting_totals_since(since).await?)
    } else {
        None
    };
    let savings = Some(
        db.savings_totals_with_watermark(scope_ref, since_seconds)
            .await?,
    );
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
    Ok(CostsReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark,
        observed_at_micros,
        current: known,
        usage,
        estimated_cost,
        pricing_revision: None,
    })
}
