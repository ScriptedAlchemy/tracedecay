use std::collections::{BTreeMap, BTreeSet};

use tracedecay_application::{
    AggregateCapabilityV1, AggregateShareCellV1, AggregateShareDimensionV1,
    AggregateShareExportRequestV1, AggregateShareMetricV1, AggregateSharePacketV1,
    AggregateShareUnitV1, ApplicationContractError, ObservabilityAggregateExportPort,
    ObservabilityFuture, now_micros,
};
use tracedecay_domain::{
    AdoptionEligibilityObservedV1, AdoptionOutcomeLinkedV1, CoverageStateV1,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityTerminalResultV1,
};
use tracedecay_global_db::{AnalyticsEventQuery, RegisteredGlobalDb};

use super::{EVENT_LIMIT, OBSERVABILITY_PROVIDER};

#[derive(Clone, Copy)]
pub struct RegisteredAggregateShareExporterV1<'a> {
    db: &'a RegisteredGlobalDb,
}

impl<'a> RegisteredAggregateShareExporterV1<'a> {
    pub const fn new(db: &'a RegisteredGlobalDb) -> Self {
        Self { db }
    }
}

impl ObservabilityAggregateExportPort for RegisteredAggregateShareExporterV1<'_> {
    fn export_aggregate(
        &self,
        request: AggregateShareExportRequestV1,
    ) -> ObservabilityFuture<'_, AggregateSharePacketV1> {
        Box::pin(async move {
            request.validate()?;
            let generated_at_micros = now_micros().0;
            if request.horizon.until_micros > generated_at_micros {
                return Err(ApplicationContractError::InvalidRange {
                    field: "aggregate_share.future_horizon",
                });
            }
            let mut rows = self
                .db
                .query_analytics_events(&AnalyticsEventQuery {
                    provider: Some(OBSERVABILITY_PROVIDER.to_owned()),
                    project_id: Some(request.authorized_scope_ref),
                    since: Some(request.horizon.since_micros.div_euclid(1_000_000)),
                    until: Some(
                        request
                            .horizon
                            .until_micros
                            .saturating_add(999_999)
                            .div_euclid(1_000_000),
                    ),
                    limit: EVENT_LIMIT.saturating_add(1),
                    ..AnalyticsEventQuery::default()
                })
                .await
                .map_err(ApplicationContractError::Domain)?;
            let source_capped = rows.len() > EVENT_LIMIT;
            if source_capped {
                rows.truncate(EVENT_LIMIT);
            }
            let mut accumulators = BTreeMap::new();
            let mut invalid_source = false;
            let mut events = Vec::with_capacity(rows.len());
            for row in rows {
                let Some(envelope) = row
                    .metadata_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<ObservabilityEnvelopeV1>(json).ok())
                    .filter(|event| event.validate().is_ok())
                else {
                    invalid_source = true;
                    continue;
                };
                events.push(envelope);
            }
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
                .collect::<BTreeMap<_, _>>();
            for envelope in &events {
                accumulate(&mut accumulators, envelope);
                if !matches!(envelope.payload, ObservabilityPayloadV1::TelemetryDrop(_)) {
                    let represented = explicit_drop_carriers
                        .get(&(envelope.process_boot_id.clone(), envelope.producer_sequence))
                        .copied()
                        .unwrap_or(0);
                    let fallback = envelope.dropped_count.saturating_sub(represented);
                    if fallback > 0 {
                        accumulate_telemetry_drop(&mut accumulators, envelope, fallback);
                    }
                }
            }
            let candidate_cell_count = accumulators.len();
            let mut cells = accumulators
                .into_values()
                .filter_map(|accumulator| accumulator.finish(source_capped, invalid_source))
                .collect::<Vec<_>>();
            let suppressed_cell_count = candidate_cell_count.saturating_sub(cells.len()) as u64;
            cells.sort_by_key(|cell| cell.metric as u8);
            let requested = usize::from(request.max_cells);
            let capped_cell_count = cells.len().saturating_sub(requested) as u64;
            cells.truncate(requested);
            let packet = AggregateSharePacketV1 {
                schema_revision: 1,
                descriptor_revision: "aggregate-share.v1".to_owned(),
                horizon: request.horizon,
                generated_at_micros,
                cells,
                suppressed_cell_count,
                capped_cell_count,
            };
            packet
                .validate()
                .map_err(|error| ApplicationContractError::Domain(error.to_owned()))?;
            Ok(packet)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CellKey {
    metric: AggregateShareMetricV1,
    unit: AggregateShareUnitV1,
    capability: AggregateCapabilityV1,
}

struct CellAccumulator {
    key: CellKey,
    eligible: u64,
    observed: u64,
    completed: u64,
    censored: u64,
    unknown: u64,
    value_sum: f64,
    coverage_known: bool,
    windows: BTreeSet<i64>,
}

impl CellAccumulator {
    fn new(key: CellKey) -> Self {
        Self {
            key,
            eligible: 0,
            observed: 0,
            completed: 0,
            censored: 0,
            unknown: 0,
            value_sum: 0.0,
            coverage_known: true,
            windows: BTreeSet::new(),
        }
    }

    fn observe(
        &mut self,
        event: &ObservabilityEnvelopeV1,
        contribution: (u64, u64, u64, u64, u64, f64),
    ) {
        let (eligible, observed, completed, censored, unknown, value) = contribution;
        let day = event.event_time_micros.div_euclid(86_400_000_000);
        self.windows.insert(day);
        self.eligible = self.eligible.saturating_add(eligible);
        self.observed = self.observed.saturating_add(observed);
        self.completed = self.completed.saturating_add(completed);
        self.censored = self.censored.saturating_add(censored);
        self.unknown = self.unknown.saturating_add(unknown);
        self.value_sum += value;
        self.coverage_known &= event.coverage == CoverageStateV1::Known;
    }

    fn finish(self, source_capped: bool, invalid_source: bool) -> Option<AggregateShareCellV1> {
        let contribution_windows = self.windows.len() as u64;
        if contribution_windows
            < tracedecay_application::AGGREGATE_SHARE_MIN_CONTRIBUTION_WINDOWS_V1
        {
            return None;
        }
        let coverage = if source_capped {
            CoverageStateV1::Capped
        } else if !invalid_source && self.coverage_known && self.censored == 0 && self.unknown == 0
        {
            CoverageStateV1::Known
        } else {
            CoverageStateV1::Partial
        };
        Some(AggregateShareCellV1 {
            metric: self.key.metric,
            unit: self.key.unit,
            dimensions: vec![AggregateShareDimensionV1::Capability(self.key.capability)],
            eligible: self.eligible,
            observed: self.observed,
            completed: self.completed,
            censored: self.censored,
            unknown: self.unknown,
            value: (!source_capped && !invalid_source && self.coverage_known)
                .then_some(self.value_sum),
            coverage,
            contribution_windows,
        })
    }
}

fn accumulator(
    cells: &mut BTreeMap<CellKey, CellAccumulator>,
    key: CellKey,
) -> &mut CellAccumulator {
    cells
        .entry(key)
        .or_insert_with(|| CellAccumulator::new(key))
}

fn accumulate(cells: &mut BTreeMap<CellKey, CellAccumulator>, event: &ObservabilityEnvelopeV1) {
    match &event.payload {
        ObservabilityPayloadV1::RetrievalQuery(query) => {
            let unknown = u64::from(matches!(
                event.terminal_result,
                None | Some(ObservabilityTerminalResultV1::Unknown)
            ));
            let completed = 1_u64.saturating_sub(unknown);
            accumulator(
                cells,
                CellKey {
                    metric: AggregateShareMetricV1::RetrievalQueries,
                    unit: AggregateShareUnitV1::Events,
                    capability: AggregateCapabilityV1::Retrieval,
                },
            )
            .observe(event, (1, 1, completed, 0, unknown, 1.0));
            let answered = u64::from(query.answered);
            accumulator(
                cells,
                CellKey {
                    metric: AggregateShareMetricV1::RetrievalAnswered,
                    unit: AggregateShareUnitV1::Events,
                    capability: AggregateCapabilityV1::Retrieval,
                },
            )
            .observe(event, (1, 1, answered, 0, 0, answered as f64));
        }
        ObservabilityPayloadV1::AdoptionEligibility(value) => {
            accumulate_adoption_eligibility(cells, event, value);
        }
        ObservabilityPayloadV1::AdoptionOutcome(value) => {
            accumulate_adoption_outcome(cells, event, value);
        }
        ObservabilityPayloadV1::Latency(value) => {
            accumulator(
                cells,
                CellKey {
                    metric: AggregateShareMetricV1::OperationLatency,
                    unit: AggregateShareUnitV1::Microseconds,
                    capability: AggregateCapabilityV1::Runtime,
                },
            )
            .observe(event, (1, 1, 1, 0, 0, value.service_micros as f64));
        }
        ObservabilityPayloadV1::OperationResource(value) => {
            accumulator(
                cells,
                CellKey {
                    metric: AggregateShareMetricV1::OperationLatency,
                    unit: AggregateShareUnitV1::Microseconds,
                    capability: AggregateCapabilityV1::Runtime,
                },
            )
            .observe(
                event,
                (
                    1,
                    1,
                    u64::from(event.terminal_result.is_some()),
                    0,
                    u64::from(event.terminal_result.is_none()),
                    value.service_latency_micros as f64,
                ),
            );
        }
        ObservabilityPayloadV1::TelemetryDrop(value) => {
            accumulate_telemetry_drop(cells, event, value.proved_drop_lower_bound);
        }
        ObservabilityPayloadV1::Storage(value) => {
            if let Some(duration_micros) = value.duration_micros {
                accumulator(
                    cells,
                    CellKey {
                        metric: AggregateShareMetricV1::StorageLatency,
                        unit: AggregateShareUnitV1::Microseconds,
                        capability: AggregateCapabilityV1::Storage,
                    },
                )
                .observe(event, (1, 1, 1, 0, 0, duration_micros as f64));
            }
        }
        ObservabilityPayloadV1::Index(value)
            if value.outcome == tracedecay_domain::IndexOutcomeV1::Published =>
        {
            accumulator(
                cells,
                CellKey {
                    metric: AggregateShareMetricV1::IndexPublication,
                    unit: AggregateShareUnitV1::Events,
                    capability: AggregateCapabilityV1::Index,
                },
            )
            .observe(event, (1, 1, 1, 0, 0, 1.0));
        }
        _ => {}
    }
}

fn accumulate_telemetry_drop(
    cells: &mut BTreeMap<CellKey, CellAccumulator>,
    event: &ObservabilityEnvelopeV1,
    dropped: u64,
) {
    accumulator(
        cells,
        CellKey {
            metric: AggregateShareMetricV1::TelemetryDropsLowerBound,
            unit: AggregateShareUnitV1::Events,
            capability: AggregateCapabilityV1::Runtime,
        },
    )
    .observe(event, (1, 1, 1, 0, 0, dropped as f64));
}

fn accumulate_adoption_eligibility(
    cells: &mut BTreeMap<CellKey, CellAccumulator>,
    event: &ObservabilityEnvelopeV1,
    value: &AdoptionEligibilityObservedV1,
) {
    accumulator(
        cells,
        CellKey {
            metric: AggregateShareMetricV1::AdoptionEligible,
            unit: AggregateShareUnitV1::Events,
            capability: AggregateCapabilityV1::Adoption,
        },
    )
    .observe(
        event,
        (
            value.eligible,
            value.eligible,
            value.available,
            0,
            0,
            value.eligible as f64,
        ),
    );
}

fn accumulate_adoption_outcome(
    cells: &mut BTreeMap<CellKey, CellAccumulator>,
    event: &ObservabilityEnvelopeV1,
    value: &AdoptionOutcomeLinkedV1,
) {
    accumulator(
        cells,
        CellKey {
            metric: AggregateShareMetricV1::AdoptionIndependentlyUseful,
            unit: AggregateShareUnitV1::Events,
            capability: AggregateCapabilityV1::Adoption,
        },
    )
    .observe(
        event,
        (
            value.invoked,
            value
                .terminal
                .saturating_add(value.censored)
                .saturating_add(value.unknown),
            value.independently_useful,
            value.censored,
            value.unknown,
            value.independently_useful as f64,
        ),
    );
}
