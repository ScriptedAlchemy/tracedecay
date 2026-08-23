//! Opt-in hotpath probes for dashboard HTTP, events, and projections.
//!
//! Labels are compile-time static. Poll/delivery sites share one bucket each —
//! never a per-tick, per-project, or per-receipt name. Error class is a closed
//! typed set, never an unbounded message.

use axum::http::StatusCode;
#[cfg(feature = "hotpath")]
use axum::http::header;
use axum::response::Response;
use tracedecay_api::read_model::DashboardFreshnessStateV1;

#[inline(always)]
pub(crate) fn record_error_class(class: &'static str) {
    #[cfg(feature = "hotpath")]
    hotpath::val!("dashboard.http.error_class").set(&class);
    #[cfg(not(feature = "hotpath"))]
    let _ = class;
}

#[inline(always)]
pub(crate) fn record_response_bytes(len: usize) {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("dashboard.http.response_bytes").set(len as f64);
    #[cfg(not(feature = "hotpath"))]
    let _ = len;
}

#[inline(always)]
#[cfg(any(feature = "hotpath", test))]
pub(crate) fn record_status_class(status: StatusCode) {
    #[cfg(feature = "hotpath")]
    {
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
    #[cfg(not(feature = "hotpath"))]
    let _ = status;
}

#[inline(always)]
pub(crate) fn observe_response(response: &Response) {
    #[cfg(feature = "hotpath")]
    {
        record_status_class(response.status());
        if let Some(len) = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
        {
            record_response_bytes(len);
        }
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = response;
}

#[inline(always)]
pub(crate) fn record_freshness_state(state: DashboardFreshnessStateV1) {
    #[cfg(feature = "hotpath")]
    {
        let class = match state {
            DashboardFreshnessStateV1::Fresh => "fresh",
            DashboardFreshnessStateV1::Stale => "stale",
            DashboardFreshnessStateV1::Unknown => "unknown",
            DashboardFreshnessStateV1::Absent => "absent",
            DashboardFreshnessStateV1::Unsupported => "unsupported",
        };
        hotpath::val!("dashboard.freshness.state").set(&class);
    }
    #[cfg(not(feature = "hotpath"))]
    let _ = state;
}

#[cfg(test)]
mod tests {
    use super::record_status_class;
    use axum::http::StatusCode;

    #[test]
    fn status_class_recording_accepts_only_closed_codes() {
        record_status_class(StatusCode::OK);
        record_status_class(StatusCode::FORBIDDEN);
        record_status_class(StatusCode::INTERNAL_SERVER_ERROR);
    }
}
