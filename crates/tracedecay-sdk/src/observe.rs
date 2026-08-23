//! Opt-in hotpath probes for the blocking SDK HTTP client.
//!
//! `hotpath::http!` wraps async reqwest 0.12 (`ClientWithMiddleware`) and
//! cannot sit on `reqwest::blocking::Client` without changing the public
//! blocking type. Header/connection time and body/decode time are therefore
//! split with `measure_block!` around `.send()` and `.json()` / decode.
//!
//! Error class is the typed [`ClientError`] / [`RemoteClientError`] variant,
//! or a closed [`ApplicationProblemKind`] name — never an unbounded message.

use crate::client::ClientError;
use crate::remote_client::RemoteClientError;

pub(crate) fn headers<T, E>(send: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    hotpath::measure_block!("sdk.http.headers", send())
}

pub(crate) fn body_decode<T, E>(decode: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    hotpath::measure_block!("sdk.http.body_decode", decode())
}

pub(crate) fn finish<T>(result: Result<T, ClientError>) -> Result<T, ClientError> {
    if let Err(error) = &result {
        record_client_error(error);
    }
    result
}

pub(crate) fn finish_remote<T>(
    result: Result<T, RemoteClientError>,
) -> Result<T, RemoteClientError> {
    if let Err(error) = &result {
        record_remote_error(error);
    }
    result
}

pub(crate) fn record_client_error(error: &ClientError) {
    let class = match error {
        ClientError::InvalidConfiguration(_) => "invalid_configuration",
        ClientError::InvalidRequest(_) => "invalid_request",
        ClientError::Transport(_) => "transport",
        ClientError::MissingMcpTransport { .. } => "missing_mcp_transport",
        ClientError::UnsupportedTransport { .. } => "unsupported_transport",
        ClientError::Authentication(_) => "authentication",
        ClientError::Protocol { .. } => "protocol",
        ClientError::Problem(problem) => problem_kind_name(&problem.kind),
    };
    hotpath::val!("sdk.http.error_class").set(&class);
}

pub(crate) fn record_remote_error(error: &RemoteClientError) {
    let class = match error {
        RemoteClientError::Configuration(_) => "configuration",
        RemoteClientError::Transport(_) => "transport",
        RemoteClientError::Protocol(_) => "protocol",
    };
    hotpath::val!("sdk.http.error_class").set(&class);
}

fn problem_kind_name(kind: &str) -> &'static str {
    match kind {
        "invalid_request" => "invalid_request",
        "not_found_or_not_authorized" => "not_found_or_not_authorized",
        "conflict" => "conflict",
        "partial_effect" => "partial_effect",
        "stale" => "stale",
        "unsupported" => "unsupported",
        "unavailable" => "unavailable",
        "execution_failed" => "execution_failed",
        "reset_required" => "reset_required",
        "saturated" => "saturated",
        "cancelled" => "cancelled",
        "timed_out" => "timed_out",
        _ => "problem",
    }
}

#[cfg(test)]
mod tests {
    use super::problem_kind_name;

    #[test]
    fn unknown_wire_kind_collapses_to_typed_problem_class() {
        assert_eq!(problem_kind_name("invalid_request"), "invalid_request");
        assert_eq!(problem_kind_name("not a taxonomy member"), "problem");
    }
}
