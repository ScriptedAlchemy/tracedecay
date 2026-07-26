//! Production Plan 26 read-model composition over the canonical accounting store.

use tracedecay_application::{
    ApplicationContractError, ObservabilityFuture, ObservabilityPageV1, ObservabilityQueryPort,
    ObservabilityQueryV1, ObservabilityRecordPort,
};
use tracedecay_application::{
    CostsReadModelV1, MetricCohortV1, MetricCoverageV1, MetricEvidenceClassV1, MetricProvenanceV1,
    MetricSourceV1, MetricTemporalV1, MetricUncertaintyV1, MetricValueV1, ObservabilityHorizonV1,
    ObservatoryReadModelV1,
};
use tracedecay_domain::{
    CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1,
};

use crate::global_db::{AnalyticsEventInsert, AnalyticsEventQuery, RegisteredGlobalDb};

const EVENT_LIMIT: usize = 10_000;
const OBSERVABILITY_SCAN_PAGE: usize = 64;
const OBSERVABILITY_PROVIDER: &str = "tracedecay-observability";
const ANALYTICS_DESCRIPTOR: &str = "analytics-events.v1";
const COST_DESCRIPTOR: &str = "provider-costs.v1";

/// Production adapter for the canonical application record/query boundary.
/// The complete versioned envelope is retained as JSON while indexed columns
/// provide bounded scope/kind/time queries.
#[derive(Clone, Copy)]
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
    fn query<'a>(
        &'a self,
        query: ObservabilityQueryV1,
    ) -> ObservabilityFuture<'a, ObservabilityPageV1> {
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
            let scan_limit = requested.max(OBSERVABILITY_SCAN_PAGE).min(EVENT_LIMIT);
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

fn measurement(
    descriptor_revision: &str,
    metric: &str,
    value: Option<f64>,
    unit: &str,
    denominator: &str,
    coverage: MetricCoverageV1,
    source: MetricSourceV1,
    source_revision: &str,
    projector_revision: &str,
    watermark: &str,
    horizon: &ObservabilityHorizonV1,
    unavailable_reason: Option<&str>,
) -> MetricValueV1 {
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
        descriptor_revision: descriptor_revision.to_string(),
        metric: metric.to_string(),
        value,
        unit: unit.to_string(),
        denominator: denominator.to_string(),
        denominator_value: coverage.eligible,
        coverage,
        evidence_class: MetricEvidenceClassV1::Measurement,
        provenance: MetricProvenanceV1 {
            source,
            source_revision: source_revision.to_string(),
            projector_revision: projector_revision.to_string(),
            watermark: watermark.to_string(),
        },
        cohort: MetricCohortV1 {
            descriptor_revision: format!("{denominator}.v1"),
            eligible_population: denominator.to_string(),
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

pub(crate) fn observatory_unavailable_read_model(
    scope_ref: Option<&str>,
    since_seconds: i64,
    reason: &str,
) -> ObservatoryReadModelV1 {
    let observed_at_micros = now_micros();
    let read_horizon = horizon(since_seconds, observed_at_micros);
    let watermark = "analytics:unavailable".to_string();
    let metric_coverage = coverage(None, 0, 1, CoverageStateV1::Unknown);
    let metrics = {
        let metric = |name: &str| {
            measurement(
                ANALYTICS_DESCRIPTOR,
                name,
                None,
                "events",
                "eligible_observability_events",
                metric_coverage.clone(),
                MetricSourceV1::ObservabilityEnvelope,
                "observability-envelope.v1",
                "observatory-projector.v1",
                &watermark,
                &read_horizon,
                Some(reason),
            )
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
pub(crate) async fn observatory_read_model(
    db: &RegisteredGlobalDb,
    scope_ref: Option<&str>,
    since_seconds: i64,
) -> ObservatoryReadModelV1 {
    let observed_at_micros = now_micros();
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
            measurement(
                ANALYTICS_DESCRIPTOR,
                name,
                complete.then_some(value as f64),
                unit,
                "eligible_observability_events",
                metric_coverage.clone(),
                MetricSourceV1::ObservabilityEnvelope,
                "observability-envelope.v1",
                "observatory-projector.v1",
                &watermark,
                &read_horizon,
                reason,
            )
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

pub(crate) fn costs_unavailable_read_model(
    scope_ref: Option<&str>,
    since_seconds: i64,
    reason: &str,
) -> CostsReadModelV1 {
    let observed_at_micros = now_micros();
    let read_horizon = horizon(since_seconds, observed_at_micros);
    let coverage = coverage(None, 0, 1, CoverageStateV1::Unknown);
    let (usage, estimated_cost) = {
        let metric = |name: &str,
                      unit: &str,
                      denominator: &str,
                      source: MetricSourceV1,
                      source_revision: &str| {
            measurement(
                COST_DESCRIPTOR,
                name,
                None,
                unit,
                denominator,
                coverage.clone(),
                source,
                source_revision,
                "costs-projector.v1",
                "costs:unavailable",
                &read_horizon,
                Some(reason),
            )
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
pub(crate) async fn costs_read_model(
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
    let observed_at_micros = now_micros();
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
        measurement(
            COST_DESCRIPTOR,
            "provider_tokens",
            tokens,
            "tokens",
            "ingested_provider_turns",
            accounting_coverage.clone(),
            MetricSourceV1::AccountingTurn,
            "accounting-turn.v1",
            "costs-projector.v1",
            &accounting_watermark,
            &read_horizon,
            accounting_reason,
        ),
        measurement(
            COST_DESCRIPTOR,
            "saved_tokens",
            saved_tokens,
            "tokens",
            "eligible_savings_calls",
            savings_coverage,
            MetricSourceV1::SavingsLedger,
            "savings-ledger.v1",
            "costs-projector.v1",
            &savings_watermark,
            &read_horizon,
            savings_reason,
        ),
    ];
    let estimated_cost = vec![measurement(
        COST_DESCRIPTOR,
        "provider_cost",
        None,
        "usd",
        "priced_provider_turns",
        if accounting.is_some() {
            coverage(
                None,
                accounting.map_or(0, |value| value.0),
                1,
                CoverageStateV1::Unknown,
            )
        } else {
            accounting_coverage
        },
        MetricSourceV1::AccountingTurn,
        "accounting-turn.v1",
        "costs-projector.v1",
        &accounting_watermark,
        &read_horizon,
        pricing_reason,
    )];
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

    #[tokio::test]
    async fn exact_horizon_scans_past_dense_coarse_boundary_rows() {
        let harness = crate::global_db::tests::harness::RegisteredGlobalDbHarness::open(
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
