//! Authorized read path that composes retained full-day fragments with fresh
//! partial-day boundary pages.

use serde::{Deserialize, Serialize};
use tracedecay_domain::{CoverageStateV1, UtcMicros};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::clock::now_micros;
use crate::observability::{
    ObservabilityFuture, ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1,
};
use crate::work::work_authority;
use crate::{ApplicationProblem, RequestAdmission, RequestContext, RetryDirective};

use super::projection::TELEMETRY_DROP_EVENT_KIND_V1;
use super::rollup::{
    ExecutionTopologyRollupErrorV1, ExecutionTopologyRollupFragmentV1,
    MAX_EXECUTION_TOPOLOGY_ROLLUP_DAYS_V1, MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1,
    MAX_EXECUTION_TOPOLOGY_ROLLUP_READ_BYTES_V1, build_execution_topology_boundary_fragment,
    project_execution_topology_fragments_with_boundaries,
};
use super::support::{invalid_problem, unavailable_model, unavailable_model_with_state_at};
use super::{
    EXECUTION_TOPOLOGY_CAPABILITY_ID_V1, EXECUTION_TOPOLOGY_EVENT_KINDS_V1,
    EXECUTION_TOPOLOGY_USE_CASE_ID_V1, ExecutionMetricUnavailableV1,
    ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1,
    MAX_EXECUTION_TOPOLOGY_EVENTS_V1,
};

const UTC_DAY_MICROS_V1: i64 = 86_400_000_000;

/// Exact full-day range requested from the retained rollup authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionTopologyRollupFragmentQueryV1 {
    pub authorized_scope_ref: String,
    pub horizon: ObservabilityHorizonV1,
}

/// Transport-neutral retained response. Fragment documents must be canonical
/// serde JSON; the application re-deserializes them before projection so a
/// malformed or noncanonical interior can never become a raw-query fallback.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExecutionTopologyRollupFragmentPageV1 {
    pub horizon: ObservabilityHorizonV1,
    pub coverage: CoverageStateV1,
    pub fragment_documents: Vec<String>,
}

pub trait ExecutionTopologyRollupQueryPort: Send + Sync {
    fn query_rollup_fragments<'a>(
        &'a self,
        query: ExecutionTopologyRollupFragmentQueryV1,
    ) -> ObservabilityFuture<'a, ExecutionTopologyRollupFragmentPageV1>;
}

/// Reads execution-topology metrics through the daily retained-rollup path.
/// It reads raw observations only for up to two partial-day boundaries; every
/// complete interior day must come from the rollup port or the result remains
/// typed unavailable.
///
/// # Errors
///
/// Returns invalid-request and authority-admission problems with the same
/// semantics as the live topology metrics operation. Observation and retained
/// rollup availability are represented by a typed unavailable model.
pub async fn execution_topology_rollup_metrics<R, O>(
    rollups: &R,
    observations: &O,
    context: &RequestContext,
    request: &ExecutionTopologyMetricsRequestV1,
) -> Result<ExecutionTopologyMetricsV1, ApplicationProblem>
where
    R: ExecutionTopologyRollupQueryPort,
    O: ObservabilityQueryPort,
{
    validate_request(request)?;
    let observed_at = now_micros();
    admit(context, observed_at)?;
    authorize(context)?;
    let authorized_scope_ref = work_authority(context)?.project_id().as_str().to_owned();
    let observed_at_micros = observed_at.0;
    let HorizonSlicesV1 {
        boundaries: boundary_horizons,
        full_days,
    } = split_horizon(&request.horizon);

    if exceeds_rollup_fragment_limit(full_days.as_ref(), boundary_horizons.len()) {
        return Ok(unavailable(
            authorized_scope_ref,
            request.horizon.clone(),
            observed_at_micros,
            ExecutionMetricUnavailableV1::EventBudgetExceeded,
        ));
    }

    let mut boundaries = Vec::new();
    let mut remaining_boundary_events = request.max_events;
    for horizon in boundary_horizons {
        if remaining_boundary_events == 0 {
            return Ok(unavailable(
                authorized_scope_ref,
                request.horizon.clone(),
                observed_at_micros,
                ExecutionMetricUnavailableV1::EventBudgetExceeded,
            ));
        }
        admit(context, now_micros())?;
        let page = match observations
            .query(boundary_query(
                &authorized_scope_ref,
                horizon.clone(),
                remaining_boundary_events,
            ))
            .await
        {
            Ok(page) => page,
            Err(_) => {
                return Ok(unavailable(
                    authorized_scope_ref,
                    request.horizon.clone(),
                    observed_at_micros,
                    ExecutionMetricUnavailableV1::StoreUnavailable,
                ));
            }
        };
        let boundary_event_count = match u32::try_from(page.events.len()) {
            Ok(count) => count,
            Err(_) => {
                return Ok(unavailable(
                    authorized_scope_ref,
                    request.horizon.clone(),
                    observed_at_micros,
                    ExecutionMetricUnavailableV1::EventBudgetExceeded,
                ));
            }
        };
        if page.next_watermark.is_some() || boundary_event_count > remaining_boundary_events {
            return Ok(unavailable(
                authorized_scope_ref,
                request.horizon.clone(),
                observed_at_micros,
                ExecutionMetricUnavailableV1::EventBudgetExceeded,
            ));
        }
        if page.coverage != CoverageStateV1::Known {
            return Ok(unavailable_with_state(
                authorized_scope_ref,
                request.horizon.clone(),
                observed_at_micros,
                ExecutionMetricUnavailableV1::StoreUnavailable,
                page.coverage,
            ));
        }
        remaining_boundary_events = remaining_boundary_events.saturating_sub(boundary_event_count);
        let boundary =
            match build_execution_topology_boundary_fragment(&authorized_scope_ref, &horizon, page)
            {
                Ok(fragment) => fragment,
                Err(error) => {
                    let (reason, state) = match error {
                        ExecutionTopologyRollupErrorV1::FragmentBudgetExceeded => (
                            ExecutionMetricUnavailableV1::EventBudgetExceeded,
                            CoverageStateV1::Capped,
                        ),
                        _ => (
                            ExecutionMetricUnavailableV1::StoreUnavailable,
                            CoverageStateV1::Unknown,
                        ),
                    };
                    return Ok(unavailable_with_state(
                        authorized_scope_ref,
                        request.horizon.clone(),
                        observed_at_micros,
                        reason,
                        state,
                    ));
                }
            };
        boundaries.push(boundary);
    }

    let fragments = if let Some(horizon) = full_days {
        admit(context, now_micros())?;
        let page = match rollups
            .query_rollup_fragments(ExecutionTopologyRollupFragmentQueryV1 {
                authorized_scope_ref: authorized_scope_ref.clone(),
                horizon: horizon.clone(),
            })
            .await
        {
            Ok(page) => page,
            Err(_) => {
                return Ok(unavailable(
                    authorized_scope_ref,
                    request.horizon.clone(),
                    observed_at_micros,
                    ExecutionMetricUnavailableV1::StoreUnavailable,
                ));
            }
        };
        match deserialize_complete_interiors(&horizon, page) {
            Ok(fragments) => fragments,
            Err(failure) => {
                return Ok(unavailable_with_state(
                    authorized_scope_ref,
                    request.horizon.clone(),
                    observed_at_micros,
                    failure.reason,
                    failure.coverage,
                ));
            }
        }
    } else {
        Vec::new()
    };

    Ok(project_execution_topology_fragments_with_boundaries(
        &authorized_scope_ref,
        &request.horizon,
        observed_at_micros,
        &fragments,
        &boundaries,
    ))
}

fn validate_request(request: &ExecutionTopologyMetricsRequestV1) -> Result<(), ApplicationProblem> {
    if request.horizon.until_micros <= request.horizon.since_micros {
        return Err(invalid_problem(
            "application.execution-topology-rollup.invalid-horizon",
            "The execution topology metrics horizon must end after it starts.",
        ));
    }
    if request.max_events == 0 || request.max_events > MAX_EXECUTION_TOPOLOGY_EVENTS_V1 {
        return Err(invalid_problem(
            "application.execution-topology-rollup.invalid-event-budget",
            "The execution topology metrics event budget must be between 1 and 10000.",
        ));
    }
    Ok(())
}

fn admit(context: &RequestContext, observed_at: UtcMicros) -> Result<(), ApplicationProblem> {
    match context.admission_at(observed_at) {
        RequestAdmission::Admitted => Ok(()),
        RequestAdmission::Cancelled => Err(ApplicationProblem::cancelled_before_admission()),
        RequestAdmission::TimedOut => Err(ApplicationProblem::timed_out_before_admission()),
    }
}

fn authorize(context: &RequestContext) -> Result<(), ApplicationProblem> {
    let capability = CapabilityId::new(EXECUTION_TOPOLOGY_CAPABILITY_ID_V1).map_err(|_| {
        invalid_problem(
            "application.execution-topology-rollup.invalid-authority",
            "The execution topology metrics authority is unavailable.",
        )
    })?;
    let use_case = UseCaseId::new(EXECUTION_TOPOLOGY_USE_CASE_ID_V1).map_err(|_| {
        invalid_problem(
            "application.execution-topology-rollup.invalid-authority",
            "The execution topology metrics authority is unavailable.",
        )
    })?;
    if context.allows(&capability, &use_case) {
        Ok(())
    } else {
        Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ))
    }
}

fn boundary_query(
    authorized_scope_ref: &str,
    horizon: ObservabilityHorizonV1,
    max_events: u32,
) -> ObservabilityQueryV1 {
    ObservabilityQueryV1 {
        authorized_scope_ref: authorized_scope_ref.to_owned(),
        event_kinds: EXECUTION_TOPOLOGY_EVENT_KINDS_V1
            .iter()
            .map(|kind| (*kind).to_owned())
            .chain(std::iter::once(TELEMETRY_DROP_EVENT_KIND_V1.to_owned()))
            .collect(),
        horizon,
        after_watermark: None,
        limit: max_events,
    }
}

fn deserialize_complete_interiors(
    horizon: &ObservabilityHorizonV1,
    page: ExecutionTopologyRollupFragmentPageV1,
) -> Result<Vec<ExecutionTopologyRollupFragmentV1>, InteriorFailureV1> {
    if page.horizon != *horizon {
        return Err(InteriorFailureV1::partial());
    }
    if !matches!(
        page.coverage,
        CoverageStateV1::Known | CoverageStateV1::Capped
    ) {
        return Err(InteriorFailureV1::from_coverage(page.coverage));
    }
    let mut total_bytes = 0usize;
    let mut fragments = page
        .fragment_documents
        .into_iter()
        .map(|document| {
            if document.len() > MAX_EXECUTION_TOPOLOGY_ROLLUP_FRAGMENT_BYTES_V1 {
                return Err(InteriorFailureV1::capped());
            }
            total_bytes = total_bytes.saturating_add(document.len());
            if total_bytes > MAX_EXECUTION_TOPOLOGY_ROLLUP_READ_BYTES_V1 {
                return Err(InteriorFailureV1::capped());
            }
            let fragment = serde_json::from_str::<ExecutionTopologyRollupFragmentV1>(&document)
                .map_err(|_| InteriorFailureV1::unknown())?;
            let canonical =
                serde_json::to_string(&fragment).map_err(|_| InteriorFailureV1::unknown())?;
            if canonical != document {
                return Err(InteriorFailureV1::unknown());
            }
            Ok(fragment)
        })
        .collect::<Result<Vec<_>, _>>()?;
    fragments.sort_by_key(|fragment| fragment.horizon().since_micros);
    let mut expected_since = horizon.since_micros;
    for fragment in &fragments {
        if fragment.horizon().since_micros != expected_since
            || fragment.horizon().until_micros <= fragment.horizon().since_micros
            || fragment.horizon().until_micros > horizon.until_micros
        {
            return Err(InteriorFailureV1::partial());
        }
        expected_since = fragment.horizon().until_micros;
    }
    if expected_since != horizon.until_micros {
        return Err(InteriorFailureV1::partial());
    }
    Ok(fragments)
}

#[derive(Clone, Copy)]
struct InteriorFailureV1 {
    reason: ExecutionMetricUnavailableV1,
    coverage: CoverageStateV1,
}

impl InteriorFailureV1 {
    const fn partial() -> Self {
        Self {
            reason: ExecutionMetricUnavailableV1::StoreUnavailable,
            coverage: CoverageStateV1::Partial,
        }
    }

    const fn capped() -> Self {
        Self {
            reason: ExecutionMetricUnavailableV1::EventBudgetExceeded,
            coverage: CoverageStateV1::Capped,
        }
    }

    const fn unknown() -> Self {
        Self {
            reason: ExecutionMetricUnavailableV1::StoreUnavailable,
            coverage: CoverageStateV1::Unknown,
        }
    }

    const fn from_coverage(coverage: CoverageStateV1) -> Self {
        Self {
            reason: match coverage {
                CoverageStateV1::Capped => ExecutionMetricUnavailableV1::EventBudgetExceeded,
                _ => ExecutionMetricUnavailableV1::StoreUnavailable,
            },
            coverage,
        }
    }
}

struct HorizonSlicesV1 {
    boundaries: Vec<ObservabilityHorizonV1>,
    full_days: Option<ObservabilityHorizonV1>,
}

fn exceeds_rollup_fragment_limit(
    full_days: Option<&ObservabilityHorizonV1>,
    boundary_count: usize,
) -> bool {
    let Some(horizon) = full_days else {
        return boundary_count > MAX_EXECUTION_TOPOLOGY_ROLLUP_DAYS_V1;
    };
    let days = horizon.until_micros.saturating_sub(horizon.since_micros) / UTC_DAY_MICROS_V1;
    usize::try_from(days).map_or(true, |count| {
        count.saturating_add(boundary_count) > MAX_EXECUTION_TOPOLOGY_ROLLUP_DAYS_V1
    })
}

fn split_horizon(horizon: &ObservabilityHorizonV1) -> HorizonSlicesV1 {
    let first_full_start = if horizon.since_micros.rem_euclid(UTC_DAY_MICROS_V1) == 0 {
        horizon.since_micros
    } else {
        day_start(horizon.since_micros).saturating_add(UTC_DAY_MICROS_V1)
    };
    let full_end = day_start(horizon.until_micros);
    if first_full_start < full_end {
        let mut boundaries = Vec::new();
        if horizon.since_micros < first_full_start {
            boundaries.push(ObservabilityHorizonV1 {
                since_micros: horizon.since_micros,
                until_micros: first_full_start,
            });
        }
        if full_end < horizon.until_micros {
            boundaries.push(ObservabilityHorizonV1 {
                since_micros: full_end,
                until_micros: horizon.until_micros,
            });
        }
        return HorizonSlicesV1 {
            boundaries,
            full_days: Some(ObservabilityHorizonV1 {
                since_micros: first_full_start,
                until_micros: full_end,
            }),
        };
    }

    let until_last_day = horizon.until_micros.saturating_sub(1);
    if day_start(horizon.since_micros) == day_start(until_last_day) {
        return HorizonSlicesV1 {
            boundaries: vec![horizon.clone()],
            full_days: None,
        };
    }
    let boundary = day_start(horizon.until_micros);
    HorizonSlicesV1 {
        boundaries: vec![
            ObservabilityHorizonV1 {
                since_micros: horizon.since_micros,
                until_micros: boundary,
            },
            ObservabilityHorizonV1 {
                since_micros: boundary,
                until_micros: horizon.until_micros,
            },
        ],
        full_days: None,
    }
}

fn day_start(micros: i64) -> i64 {
    micros
        .div_euclid(UTC_DAY_MICROS_V1)
        .saturating_mul(UTC_DAY_MICROS_V1)
}

fn unavailable(
    authorized_scope_ref: String,
    horizon: ObservabilityHorizonV1,
    observed_at_micros: i64,
    reason: ExecutionMetricUnavailableV1,
) -> ExecutionTopologyMetricsV1 {
    unavailable_model(authorized_scope_ref, horizon, observed_at_micros, reason)
}

fn unavailable_with_state(
    authorized_scope_ref: String,
    horizon: ObservabilityHorizonV1,
    observed_at_micros: i64,
    reason: ExecutionMetricUnavailableV1,
    coverage: CoverageStateV1,
) -> ExecutionTopologyMetricsV1 {
    unavailable_model_with_state_at(
        authorized_scope_ref,
        horizon,
        observed_at_micros,
        "execution-topology:rollup-unavailable".to_owned(),
        reason,
        coverage,
    )
}
