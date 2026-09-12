//! Hotpath probes for the HTTP adapter.
//!
//! Every macro here is a compile-time no-op until the binary selects the
//! `hotpath/hotpath` backend, so the call sites need no crate feature.
//! Labels are compile-time static strings. Error class is the typed
//! [`ApplicationProblemKind`] name only — never an unbounded message.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tracedecay_contracts::ApplicationProblemKind;

pub(crate) const fn problem_kind_label(kind: ApplicationProblemKind) -> &'static str {
    match kind {
        ApplicationProblemKind::InvalidRequest => "invalid_request",
        ApplicationProblemKind::NotFoundOrNotAuthorized => "not_found_or_not_authorized",
        ApplicationProblemKind::Conflict => "conflict",
        ApplicationProblemKind::PartialEffect => "partial_effect",
        ApplicationProblemKind::Stale => "stale",
        ApplicationProblemKind::Unsupported => "unsupported",
        ApplicationProblemKind::Unavailable => "unavailable",
        ApplicationProblemKind::ExecutionFailed => "execution_failed",
        ApplicationProblemKind::ResetRequired => "reset_required",
        ApplicationProblemKind::Saturated => "saturated",
        ApplicationProblemKind::Cancelled => "cancelled",
        ApplicationProblemKind::TimedOut => "timed_out",
    }
}

#[inline(always)]
pub(crate) fn record_error_class(kind: ApplicationProblemKind) {
    hotpath::val!("api.http.error_class").set(&problem_kind_label(kind));
}

#[inline(always)]
pub(crate) fn record_contract_error_class() {
    hotpath::val!("api.http.error_class").set(&"application_contract");
}

#[inline(always)]
pub(crate) fn record_response_bytes(len: usize) {
    hotpath::gauge!("api.http.response_bytes").set(len as f64);
}

pub(crate) fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match hotpath::measure_block!("api.http.serialize", serde_json::to_vec(value)) {
        Ok(body) => {
            record_response_bytes(body.len());
            (
                status,
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                )],
                body,
            )
                .into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
