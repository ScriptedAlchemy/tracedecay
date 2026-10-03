//! Tracing probes for dashboard HTTP, events, and projections.
//!
//! Labels are compile-time static. Poll/delivery sites share one bucket each,
//! never a per-tick, per-project, or per-receipt name. Error class is a closed
//! typed set, never an unbounded message.

use axum::http::StatusCode;

use axum::http::header;
use axum::response::Response;
use tracedecay_api::read_model::DashboardFreshnessStateV1;

#[inline(always)]
pub(crate) fn record_error_class(class: &'static str) {
    tracing::trace!(name: "dashboard_api.http.error_class", value = ?class);
}

#[inline(always)]
pub(crate) fn record_status_class(status: StatusCode) {
    if status.is_success() {
        return;
    }
    let class = match status.as_u16() {
        400 => "invalid_request",
        403 => "forbidden",
        404 => "not_found_or_not_authorized",
        408 => "cancelled",
        409 => "conflict",
        422 => "unsupported",
        429 => "saturated",
        500 => "execution_failed",
        503 => "unavailable",
        504 => "timed_out",
        code if (400..500).contains(&code) => "client_error",
        _ => "server_error",
    };
    record_error_class(class);
}

#[inline(always)]
pub(crate) fn observe_response(response: &Response) {
    {
        record_status_class(response.status());
    }
}

#[inline(always)]
pub(crate) fn record_freshness_state(state: DashboardFreshnessStateV1) {
    {
        let class = match state {
            DashboardFreshnessStateV1::Fresh => "fresh",
            DashboardFreshnessStateV1::Stale => "stale",
            DashboardFreshnessStateV1::Unknown => "unknown",
            DashboardFreshnessStateV1::Absent => "absent",
            DashboardFreshnessStateV1::Unsupported => "unsupported",
        };
        tracing::trace!(name: "dashboard_api.freshness.state", value = ?class);
    }
}
