//! Dashboard feedback read descriptors and presentation mapping.
//!
//! The executable retains the selected project's daemon and feedback source.
//! This module owns the closed read-route vocabulary and the truthful
//! dashboard envelope assembled from an admitted feedback observation model.

use crate::http::HttpApplicationOperation;
use crate::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessStateV1,
    DashboardFreshnessV1, DashboardLegalActionKindV1, DashboardLegalActionRefV1, DashboardScopeV1,
    DashboardTimeV1, DashboardWatermarkV1, now_micros,
};

/// Operation advertised for re-reading feedback observation status.
pub const FEEDBACK_STATUS_REFRESH_OPERATION: &str = "feedback_status";

/// One selected-project feedback read bound through the canonical application
/// invocation router.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DashboardFeedbackReadRouteV1 {
    pub method: &'static str,
    /// Project-scoped dashboard tail, without `/api/projects/{id}/`.
    pub dashboard_tail: &'static str,
    /// Relative path accepted by [`crate::feedback_application_router`].
    pub application_path: &'static str,
    pub operation: HttpApplicationOperation,
}

const DASHBOARD_FEEDBACK_READ_ROUTES: [DashboardFeedbackReadRouteV1; 3] = [
    DashboardFeedbackReadRouteV1 {
        method: "POST",
        dashboard_tail: "feedback/get",
        application_path: "/get",
        operation: HttpApplicationOperation::FeedbackGet,
    },
    DashboardFeedbackReadRouteV1 {
        method: "POST",
        dashboard_tail: "feedback/expand",
        application_path: "/expand",
        operation: HttpApplicationOperation::FeedbackExpand,
    },
    DashboardFeedbackReadRouteV1 {
        method: "POST",
        dashboard_tail: "feedback/list",
        application_path: "/list",
        operation: HttpApplicationOperation::FeedbackList,
    },
];

/// Every selected-project feedback read route, in mount order.
const fn dashboard_feedback_read_routes() -> &'static [DashboardFeedbackReadRouteV1] {
    &DASHBOARD_FEEDBACK_READ_ROUTES
}

/// Resolve an exact selected-project feedback read route.
#[must_use]
pub fn dashboard_feedback_read_route(
    method: &str,
    dashboard_tail: &str,
) -> Option<&'static DashboardFeedbackReadRouteV1> {
    dashboard_feedback_read_routes()
        .iter()
        .find(|route| route.method == method && route.dashboard_tail == dashboard_tail)
}

/// Resolve the operation segment accepted by the canonical feedback router.
pub(crate) fn feedback_read_operation(operation: &str) -> Option<HttpApplicationOperation> {
    dashboard_feedback_read_routes()
        .iter()
        .find(|route| route.application_path.trim_start_matches('/') == operation)
        .map(|route| route.operation)
}

/// Coverage emitted by the feedback observation application projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackStatusCoverageV1 {
    Known,
    Partial,
    Sampled,
    Capped,
    Stale,
    Unknown,
}

/// Feedback source counts needed to preserve coverage truthfulness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedbackStatusDenominatorsV1 {
    pub eligible: u64,
    pub persisted: u64,
    pub delayed: u64,
    pub dropped: u64,
    pub retention_dropped: u64,
    pub incomplete_boots: u64,
}

/// API-owned presentation input over an executable-owned feedback payload.
pub struct FeedbackStatusPresentationV1<T> {
    pub payload: T,
    pub coverage: FeedbackStatusCoverageV1,
    pub total_count: u64,
    pub denominators: FeedbackStatusDenominatorsV1,
    pub last_observed_at_micros: Option<i64>,
    pub observed_through_micros: Option<i64>,
    pub producer_sequence: Option<u64>,
}

/// Project an admitted feedback status result into the dashboard wire envelope.
///
/// The fallback payload is supplied by the executable because it owns the
/// canonical empty observation model. An unavailable source always stays
/// `unknown`; it never becomes a healthy zero result.
#[must_use]
pub fn feedback_status_envelope<T, Error>(
    scope: DashboardScopeV1,
    projected: Result<FeedbackStatusPresentationV1<T>, Error>,
    fallback_payload: impl FnOnce() -> T,
) -> DashboardEnvelopeV1<T> {
    let Ok(presentation) = projected else {
        return DashboardEnvelopeV1::new(
            scope,
            DashboardDomainStateV1::Unknown,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            fallback_payload(),
        )
        .with_legal_actions(vec![DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::Refresh,
            FEEDBACK_STATUS_REFRESH_OPERATION,
        )]);
    };

    let coverage = match presentation.coverage {
        FeedbackStatusCoverageV1::Known => DashboardCoverageV1::complete(
            presentation.denominators.eligible,
            "feedback_observations",
        ),
        FeedbackStatusCoverageV1::Partial
        | FeedbackStatusCoverageV1::Sampled
        | FeedbackStatusCoverageV1::Capped => DashboardCoverageV1::partial(
            presentation.denominators.eligible,
            presentation.denominators.persisted,
            "feedback_observations",
            feedback_omission_reasons(presentation.denominators),
        ),
        FeedbackStatusCoverageV1::Stale | FeedbackStatusCoverageV1::Unknown => {
            DashboardCoverageV1::unknown()
        }
    };
    let domain_state = match presentation.coverage {
        FeedbackStatusCoverageV1::Known if presentation.total_count == 0 => {
            DashboardDomainStateV1::CompleteZeroFindings
        }
        FeedbackStatusCoverageV1::Known => DashboardDomainStateV1::Ready,
        FeedbackStatusCoverageV1::Stale => DashboardDomainStateV1::Stale,
        FeedbackStatusCoverageV1::Partial
        | FeedbackStatusCoverageV1::Sampled
        | FeedbackStatusCoverageV1::Capped => DashboardDomainStateV1::Partial,
        FeedbackStatusCoverageV1::Unknown => DashboardDomainStateV1::Unknown,
    };
    let freshness = match presentation.coverage {
        FeedbackStatusCoverageV1::Stale => DashboardFreshnessV1 {
            state: DashboardFreshnessStateV1::Stale,
            observed_at_micros: presentation.last_observed_at_micros,
            watermark: presentation
                .producer_sequence
                .map(|value| value.to_string()),
        },
        FeedbackStatusCoverageV1::Unknown => DashboardFreshnessV1::unknown(),
        FeedbackStatusCoverageV1::Known
        | FeedbackStatusCoverageV1::Partial
        | FeedbackStatusCoverageV1::Sampled
        | FeedbackStatusCoverageV1::Capped => DashboardFreshnessV1 {
            state: DashboardFreshnessStateV1::Fresh,
            observed_at_micros: presentation.last_observed_at_micros,
            watermark: presentation
                .producer_sequence
                .map(|value| value.to_string()),
        },
    };
    let source_watermark = presentation
        .producer_sequence
        .map(|sequence| DashboardWatermarkV1 {
            source: "feedback_observations".to_owned(),
            watermark: sequence.to_string(),
        });
    let time = DashboardTimeV1 {
        valid_time_micros: presentation.last_observed_at_micros,
        observation_time_micros: presentation
            .observed_through_micros
            .or(presentation.last_observed_at_micros)
            .unwrap_or_else(now_micros),
    };
    let mut envelope = DashboardEnvelopeV1::new(
        scope,
        domain_state,
        coverage,
        freshness,
        presentation.payload,
    )
    .with_legal_actions(vec![DashboardLegalActionRefV1::new(
        DashboardLegalActionKindV1::Refresh,
        FEEDBACK_STATUS_REFRESH_OPERATION,
    )]);
    envelope.source_watermark = source_watermark;
    envelope.time = time;
    envelope
}

fn feedback_omission_reasons(denominators: FeedbackStatusDenominatorsV1) -> Vec<String> {
    let mut reasons = Vec::new();
    if denominators.delayed > 0 {
        reasons.push("delayed_observations".to_owned());
    }
    if denominators.dropped > 0 {
        reasons.push("dropped_observations".to_owned());
    }
    if denominators.retention_dropped > 0 {
        reasons.push("retention_capped".to_owned());
    }
    if denominators.incomplete_boots > 0 {
        reasons.push("incomplete_producer_boot".to_owned());
    }
    reasons
}
