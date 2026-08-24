use crate::errors::TraceDecayError;
use serde_json::{Value, json};
use tracedecay_sessions::runtime::claude_observation::ClaudeObservationIngestError;
use tracedecay_usecases::host_admission::{HostAdmissionOutcome, HostAdmissionStatus};

/// Builds a hook-runtime error that carries the admission status its authority
/// actually reported.
///
/// Every hook-runtime failure raised from this module goes through here, so
/// [`structured_hook_error_data`] can serialize the reported status instead of
/// inferring one from the reason code.
pub(super) fn hook_admission_error(
    status: HostAdmissionStatus,
    reason_code: impl Into<String>,
    retryable: bool,
    detail: impl Into<String>,
) -> TraceDecayError {
    TraceDecayError::hook_runtime_with_status(reason_code, retryable, detail, status.as_wire())
}

pub(super) fn map_transcript_ingest_error(
    error: &tracedecay_sessions::runtime::source::TranscriptIngestError,
) -> TraceDecayError {
    let disposition = tracedecay_sessions::runtime::classify_transcript_ingest_disposition(error);
    hook_admission_error(
        disposition.status,
        disposition.reason_code,
        disposition.retryable,
        format!("transcript ingest failed: {}", disposition.reason_code),
    )
}

pub(super) fn map_claude_observation_ingest_error(
    error: &ClaudeObservationIngestError,
) -> TraceDecayError {
    let failure = tracedecay_sessions::runtime::classify_claude_observation_failure(error);
    hook_admission_error(
        failure.status,
        failure.reason_code,
        failure.retryable,
        error.to_string(),
    )
}

pub(crate) fn structured_hook_error_data(error: &TraceDecayError) -> Option<Value> {
    let (reason_code, retryable, detail) = error.hook_runtime_context()?;
    // The status is whatever the admission authority reported, carried through
    // the error rather than re-derived here. Failures raised without an
    // authority behind them (spool I/O, refresh ownership) report the
    // application-level default.
    let status = error
        .hook_runtime_status()
        .and_then(HostAdmissionStatus::from_wire)
        .unwrap_or(HostAdmissionStatus::Degraded);
    Some(json!({
        "tool": "tracedecay_hook_runtime",
        "status": status,
        "reason_code": reason_code,
        "retryable": retryable,
        "detail": detail,
    }))
}

pub(super) fn map_host_admission_outcome(outcome: HostAdmissionOutcome) -> TraceDecayError {
    hook_admission_error(
        outcome.status,
        outcome.reason_code.unwrap_or("canonical_admission_failed"),
        outcome.retryable,
        "projectless Hermes receipt host admission failed",
    )
}

#[cfg(test)]
mod tests;
