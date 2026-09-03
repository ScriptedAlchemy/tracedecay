use tracedecay_domain::errors::TraceDecayError;
use tracedecay_sessions::admission::{HostAdmissionOutcome, HostAdmissionStatus};
use tracedecay_sessions::runtime::claude_observation::ClaudeObservationIngestError;

/// Builds a hook-runtime error that carries the admission status its authority
/// actually reported.
///
/// Every hook-runtime failure raised from this module goes through here, so
/// [`tracedecay_mcp::structured_hook_error_data`] can serialize the reported
/// status instead of inferring one from the reason code.
pub fn hook_admission_error(
    status: HostAdmissionStatus,
    reason_code: impl Into<String>,
    retryable: bool,
    detail: impl Into<String>,
) -> TraceDecayError {
    TraceDecayError::hook_runtime_with_status(reason_code, retryable, detail, status.as_wire())
}

pub fn map_transcript_ingest_error(
    error: &tracedecay_sessions::runtime::source::TranscriptIngestError,
) -> TraceDecayError {
    let disposition = tracedecay_sessions::runtime::classify_transcript_ingest_disposition(error);
    let detail = match error {
        tracedecay_sessions::runtime::source::TranscriptIngestError::HostAdmission {
            reason: "authority_write_failed",
            detail: Some(cause),
            ..
        } => format!("transcript ingest failed: authority_write_failed: {cause}"),
        _ => format!("transcript ingest failed: {}", disposition.reason_code),
    };
    hook_admission_error(
        disposition.status,
        disposition.reason_code,
        disposition.retryable,
        detail,
    )
}

pub fn map_claude_observation_ingest_error(
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

pub fn map_host_admission_outcome(outcome: HostAdmissionOutcome) -> TraceDecayError {
    hook_admission_error(
        outcome.status,
        outcome.reason_code.unwrap_or("canonical_admission_failed"),
        outcome.retryable,
        "projectless Hermes receipt host admission failed",
    )
}

#[cfg(test)]
mod tests;
