//! Opt-in hotpath probes for the HTTP adapter.
//!
//! Labels are compile-time static strings. Error class is the typed
//! [`ApplicationProblemKind`] name only — never an unbounded message.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tracedecay_application::ApplicationProblemKind;

#[cfg(any(feature = "hotpath", test))]
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
    #[cfg(feature = "hotpath")]
    hotpath::val!("api.http.error_class").set(&problem_kind_label(kind));
    #[cfg(not(feature = "hotpath"))]
    let _ = kind;
}

#[inline(always)]
pub(crate) fn record_contract_error_class() {
    #[cfg(feature = "hotpath")]
    hotpath::val!("api.http.error_class").set(&"application_contract");
}

#[inline(always)]
pub(crate) fn record_response_bytes(len: usize) {
    #[cfg(feature = "hotpath")]
    hotpath::gauge!("api.http.response_bytes").set(len as f64);
    #[cfg(not(feature = "hotpath"))]
    let _ = len;
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

#[cfg(test)]
mod tests {
    use super::problem_kind_label;
    use tracedecay_application::ApplicationProblemKind;

    #[test]
    fn problem_kind_labels_are_the_typed_snake_case_names() {
        assert_eq!(
            problem_kind_label(ApplicationProblemKind::InvalidRequest),
            "invalid_request"
        );
        assert_eq!(
            problem_kind_label(ApplicationProblemKind::NotFoundOrNotAuthorized),
            "not_found_or_not_authorized"
        );
        assert_eq!(
            problem_kind_label(ApplicationProblemKind::TimedOut),
            "timed_out"
        );
    }
}
