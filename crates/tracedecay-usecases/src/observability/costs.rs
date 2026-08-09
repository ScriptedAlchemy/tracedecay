//! Canonical Costs read-model composition and wire serialization.

use tracedecay_application::{CostsReadModelV1, MetricCoverageV1, MetricSourceV1, now_micros};
use tracedecay_domain::{CoverageStateV1, ObservationScopeV1};
use tracedecay_global_db::RegisteredGlobalDb;

use super::cost_latency::{provider_latency_read_model, unavailable_provider_latency};
use super::{
    COST_DESCRIPTOR, MeasurementDescriptor, MeasurementProvenance, MeasurementSpec, coverage,
    horizon, measurement,
};
use crate::provider_pricing::load_table;
use crate::provider_usage::{
    AggregatedProviderUsageCountersV1, ProviderUsageAggregateV1, ProviderUsageCoverageV1,
    price_provider_usage, provider_usage_aggregate,
};

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
    let latency = vec![unavailable_provider_latency(&read_horizon, reason)];
    CostsReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark: "costs:unavailable".to_string(),
        observed_at_micros,
        current: false,
        usage,
        estimated_cost,
        latency,
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
    costs_read_model_with_provider_usage_and_observability(
        savings_db,
        Some(savings_db),
        scope_ref,
        scope_ref,
        since_seconds,
        &provider_usage,
    )
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
    costs_read_model_with_provider_usage_and_observability(
        db,
        Some(db),
        scope_ref,
        scope_ref,
        since_seconds,
        provider_usage,
    )
    .await
}

/// Builds the Costs projection from pinned provider usage and the same
/// registered observability authority. The two reads share the exact horizon
/// but retain separate watermarks and provenance.
pub async fn costs_read_model_with_provider_usage_and_observability(
    db: &RegisteredGlobalDb,
    observability_db: Option<&RegisteredGlobalDb>,
    scope_ref: Option<&str>,
    observability_scope_ref: Option<&str>,
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
    let read_horizon = horizon(since_seconds, observed_at_micros);
    let latency = provider_latency_read_model(
        observability_db,
        observability_scope_ref,
        &read_horizon,
        provider_usage,
    )
    .await;
    let provider_cost = cost_summary.total_cost_usd;
    let priced_usage = cost_summary
        .usage_events
        .saturating_sub(cost_summary.unpriced_events);
    let unpriced_usage = cost_summary.unpriced_events;
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
    let latency_metrics_known = latency.iter().all(|group| {
        [
            &group.queue,
            &group.start,
            &group.first_progress,
            &group.service,
            &group.terminal,
        ]
        .into_iter()
        .flat_map(|distribution| [&distribution.p50, &distribution.p95, &distribution.p99])
        .all(|metric| metric.coverage.state == CoverageStateV1::Known)
    });
    let known = usage
        .iter()
        .chain(&estimated_cost)
        .all(|metric| metric.coverage.state == CoverageStateV1::Known)
        && latency_metrics_known;
    let watermark = format!("{provider_watermark};{savings_watermark}");
    CostsReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: read_horizon,
        watermark,
        observed_at_micros,
        current: known,
        usage,
        estimated_cost,
        latency,
        pricing_revision: Some(cost_summary.pricing_revision),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
