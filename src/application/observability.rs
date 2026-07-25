//! Production Plan 26 read-model composition over the canonical accounting store.

use tracedecay_application::{
    ApplicationContractError, ObservabilityFuture, ObservabilityPageV1, ObservabilityQueryPort,
    ObservabilityQueryV1, ObservabilityRecordPort,
};
use tracedecay_application::{
    CostsReadModelV1, MetricCoverageV1, MetricValueV1, ObservabilityHorizonV1,
    ObservatoryReadModelV1,
};
use tracedecay_domain::CoverageStateV1;
use tracedecay_domain::ObservabilityEnvelopeV1;

use crate::global_db::{AnalyticsEventInsert, AnalyticsEventQuery, RegisteredGlobalDb};

const EVENT_LIMIT: usize = 10_000;
const ANALYTICS_DESCRIPTOR: &str = "analytics-events.v1";
const COST_DESCRIPTOR: &str = "provider-costs.v1";

/// Production adapter for the canonical application record/query boundary.
/// The complete versioned envelope is retained as JSON while indexed columns
/// provide bounded scope/kind/time queries.
pub(crate) struct RegisteredObservabilityPortV1<'a> {
    db: &'a RegisteredGlobalDb,
}

impl<'a> RegisteredObservabilityPortV1<'a> {
    pub(crate) const fn new(db: &'a RegisteredGlobalDb) -> Self {
        Self { db }
    }
}

impl ObservabilityRecordPort for RegisteredObservabilityPortV1<'_> {
    fn record<'a>(&'a self, envelope: ObservabilityEnvelopeV1) -> ObservabilityFuture<'a, String> {
        Box::pin(async move {
            let metadata_json = serde_json::to_string(&envelope)
                .map_err(|error| ApplicationContractError::Domain(error.to_string()))?;
            let insert = AnalyticsEventInsert {
                provider: "tracedecay".to_string(),
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
                .append_analytics_event(&insert)
                .await
                .map(|id| format!("analytics:{id}"))
                .map_err(ApplicationContractError::Domain)
        })
    }
}

impl ObservabilityQueryPort for RegisteredObservabilityPortV1<'_> {
    fn query<'a>(
        &'a self,
        query: ObservabilityQueryV1,
    ) -> ObservabilityFuture<'a, ObservabilityPageV1> {
        Box::pin(async move {
            let requested = usize::try_from(query.limit.max(1)).unwrap_or(EVENT_LIMIT);
            let scan_limit = requested.saturating_add(1).min(EVENT_LIMIT);
            let rows = self
                .db
                .query_analytics_events(&AnalyticsEventQuery {
                    project_id: Some(query.authorized_scope_ref),
                    event_kind: (query.event_kinds.len() == 1)
                        .then(|| query.event_kinds[0].clone()),
                    since: Some(query.horizon.since_micros.div_euclid(1_000_000)),
                    limit: scan_limit,
                    ..AnalyticsEventQuery::default()
                })
                .await
                .map_err(ApplicationContractError::Domain)?;
            let mut events = Vec::new();
            let after = query
                .after_watermark
                .as_deref()
                .and_then(|value| value.strip_prefix("analytics:"))
                .and_then(|value| value.parse::<i64>().ok());
            for row in &rows {
                if after.is_some_and(|watermark| row.id >= watermark) {
                    continue;
                }
                if !query.event_kinds.is_empty() && !query.event_kinds.contains(&row.event_kind) {
                    continue;
                }
                let Some(metadata) = row.metadata_json.as_deref() else {
                    continue;
                };
                if let Ok(envelope) = serde_json::from_str::<ObservabilityEnvelopeV1>(metadata) {
                    events.push(envelope);
                }
                if events.len() == requested {
                    break;
                }
            }
            let capped = rows.len() > events.len() && events.len() == requested;
            let watermark = rows.first().map_or_else(
                || "analytics:empty".to_string(),
                |row| format!("analytics:{}", row.id),
            );
            let next_watermark = capped.then(|| {
                rows.last()
                    .map_or_else(|| watermark.clone(), |row| format!("analytics:{}", row.id))
            });
            Ok(ObservabilityPageV1 {
                events,
                watermark,
                coverage: if capped {
                    CoverageStateV1::Capped
                } else {
                    CoverageStateV1::Known
                },
                next_watermark,
            })
        })
    }
}

fn coverage(eligible: u64, observed: u64, capped: bool) -> MetricCoverageV1 {
    let state = if capped {
        CoverageStateV1::Capped
    } else if eligible == observed {
        CoverageStateV1::Known
    } else {
        CoverageStateV1::Partial
    };
    MetricCoverageV1 {
        eligible,
        observed,
        completed: observed,
        censored: 0,
        unknown: eligible.saturating_sub(observed),
        excluded: 0,
        state,
    }
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn horizon(since_seconds: i64, observed_at_micros: i64) -> ObservabilityHorizonV1 {
    ObservabilityHorizonV1 {
        since_micros: since_seconds.saturating_mul(1_000_000),
        until_micros: observed_at_micros,
    }
}

/// Canonical Observatory projection shared by CLI, MCP, and dashboard HTTP.
pub(crate) async fn observatory_read_model(
    db: &RegisteredGlobalDb,
    scope_ref: Option<&str>,
    since_seconds: i64,
) -> Result<ObservatoryReadModelV1, String> {
    let eligible = db
        .count_analytics_events(scope_ref, since_seconds)
        .await?
        .max(0) as u64;
    let events = db
        .query_analytics_events(&AnalyticsEventQuery {
            project_id: scope_ref.map(str::to_owned),
            since: Some(since_seconds),
            limit: EVENT_LIMIT,
            ..AnalyticsEventQuery::default()
        })
        .await?;
    let observed = events.len() as u64;
    let capped = eligible > observed;
    let failed = events
        .iter()
        .filter(|event| event.outcome.as_deref() == Some("error"))
        .count() as u64;
    let dropped = events
        .iter()
        .filter(|event| event.event_kind == "telemetry.drop.observed.v1")
        .count() as u64;
    let watermark = events.first().map_or_else(
        || "analytics:empty".to_string(),
        |event| format!("analytics:{}", event.id),
    );
    let observed_at_micros = now_micros();
    let metric_coverage = coverage(eligible, observed, capped);
    let metric = |name: &str, value: u64, unit: &str| MetricValueV1 {
        descriptor_revision: ANALYTICS_DESCRIPTOR.to_string(),
        metric: name.to_string(),
        value: Some(value as f64),
        unit: unit.to_string(),
        denominator: "eligible_observability_events".to_string(),
        coverage: metric_coverage.clone(),
    };
    Ok(ObservatoryReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: horizon(since_seconds, observed_at_micros),
        watermark,
        observed_at_micros,
        current: !capped,
        metrics: vec![
            metric("observability_events", observed, "events"),
            metric("observability_failures", failed, "events"),
            metric("telemetry_drop_reports", dropped, "events"),
        ],
    })
}

/// Canonical Costs projection. Prices are recorded at ingest; transports never
/// join a pricing table or recompute dollar formulas.
pub(crate) async fn costs_read_model(
    db: &RegisteredGlobalDb,
    scope_ref: Option<&str>,
    since_seconds: i64,
) -> CostsReadModelV1 {
    let since = since_seconds.max(0) as u64;
    let total_cost = db.total_cost_since(since).await;
    let tokens = db.total_tokens_since(since).await;
    let savings = db.sum_savings_by_project_id(scope_ref, since_seconds).await;
    let observed_at_micros = now_micros();
    let known = total_cost.is_some() && tokens.is_some();
    let metric_coverage = MetricCoverageV1 {
        eligible: u64::from(known),
        observed: u64::from(known),
        completed: u64::from(known),
        censored: 0,
        unknown: u64::from(!known),
        excluded: 0,
        state: if known {
            CoverageStateV1::Known
        } else {
            CoverageStateV1::Unknown
        },
    };
    let metric = |name: &str, value: Option<f64>, unit: &str, denominator: &str| MetricValueV1 {
        descriptor_revision: COST_DESCRIPTOR.to_string(),
        metric: name.to_string(),
        value,
        unit: unit.to_string(),
        denominator: denominator.to_string(),
        coverage: metric_coverage.clone(),
    };
    CostsReadModelV1 {
        authorized_scope_ref: scope_ref.unwrap_or("all").to_string(),
        horizon: horizon(since_seconds, observed_at_micros),
        watermark: format!("costs:{observed_at_micros}"),
        observed_at_micros,
        current: known,
        usage: vec![
            metric(
                "provider_tokens",
                tokens.map(|value| value as f64),
                "tokens",
                "ingested_provider_turns",
            ),
            metric(
                "saved_tokens",
                Some(savings.saved_tokens as f64),
                "tokens",
                "eligible_savings_calls",
            ),
        ],
        estimated_cost: vec![metric(
            "provider_cost",
            total_cost,
            "usd",
            "priced_provider_turns",
        )],
        pricing_revision: Some("recorded-at-ingest".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_coverage_never_claims_current() {
        let value = coverage(10, 8, false);
        assert_eq!(value.state, CoverageStateV1::Partial);
        assert_eq!(value.unknown, 2);
    }
}
