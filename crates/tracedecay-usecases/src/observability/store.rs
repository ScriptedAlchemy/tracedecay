//! Registered-store adapter for canonical observability envelopes.

use tracedecay_application::{
    ApplicationContractError, ExecutionTopologyRollupFragmentPageV1,
    ExecutionTopologyRollupFragmentQueryV1, ExecutionTopologyRollupQueryPort, ObservabilityFuture,
    ObservabilityPageV1, ObservabilityQueryPort, ObservabilityQueryV1, ObservabilityRecordPort,
};
use tracedecay_domain::{CoverageStateV1, ObservabilityEnvelopeV1};
use tracedecay_global_db::{
    AnalyticsEventInsert, AnalyticsEventQuery, ObservabilityRollupFragmentQueryV1,
    RegisteredGlobalDb,
};

use super::{EVENT_LIMIT, OBSERVABILITY_PROVIDER, OBSERVABILITY_SCAN_PAGE};

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
            let mut watermark_id: Option<i64> = None;
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
                let Some(oldest_row) = rows.first() else {
                    break;
                };
                let next_scan_before_id = oldest_row.id;
                let exhausted = rows.len() < scan_limit;
                for row in rows.iter().rev() {
                    let row_requested =
                        query.event_kinds.is_empty() || query.event_kinds.contains(&row.event_kind);
                    if row_requested {
                        watermark_id = Some(watermark_id.map_or(row.id, |id| id.max(row.id)));
                    }
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

impl ExecutionTopologyRollupQueryPort for RegisteredObservabilityPortV1<'_> {
    fn query_rollup_fragments<'a>(
        &'a self,
        query: ExecutionTopologyRollupFragmentQueryV1,
    ) -> ObservabilityFuture<'a, ExecutionTopologyRollupFragmentPageV1> {
        Box::pin(async move {
            const MICROS_PER_SECOND: i64 = 1_000_000;
            if query.horizon.since_micros.rem_euclid(MICROS_PER_SECOND) != 0
                || query.horizon.until_micros.rem_euclid(MICROS_PER_SECOND) != 0
            {
                return Err(ApplicationContractError::InvalidRange {
                    field: "execution_topology_rollup.horizon",
                });
            }
            let page = self
                .db
                .query_observability_rollup_fragments(&ObservabilityRollupFragmentQueryV1 {
                    authorized_scope_ref: query.authorized_scope_ref,
                    since_day_start_seconds: query.horizon.since_micros / MICROS_PER_SECOND,
                    until_day_start_seconds: query.horizon.until_micros / MICROS_PER_SECOND,
                })
                .await
                .map_err(ApplicationContractError::Domain)?;
            Ok(ExecutionTopologyRollupFragmentPageV1 {
                horizon: query.horizon,
                coverage: page.coverage,
                fragment_documents: page
                    .fragments
                    .into_iter()
                    .map(|fragment| fragment.fragment_json)
                    .collect(),
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
