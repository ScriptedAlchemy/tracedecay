use crate::errors::TraceDecayError;
use serde_json::{Value, json};
use tracedecay_sessions::runtime::claude_observation::ClaudeObservationIngestError;
use tracedecay_usecases::host_admission::{HostAdmissionOutcome, HostAdmissionStatus};

pub(super) fn map_transcript_ingest_error(
    error: &tracedecay_sessions::runtime::source::TranscriptIngestError,
) -> TraceDecayError {
    let failure = tracedecay_sessions::runtime::classify_transcript_ingest_failure(
        "requested",
        "hook",
        error,
    );
    TraceDecayError::hook_runtime(
        failure.reason_code,
        failure.retryable,
        format!("transcript ingest failed: {}", failure.reason_code),
    )
}

pub(super) fn map_claude_observation_ingest_error(
    error: &ClaudeObservationIngestError,
) -> TraceDecayError {
    let failure = tracedecay_sessions::runtime::classify_claude_observation_failure(error);
    TraceDecayError::hook_runtime(failure.reason_code, failure.retryable, error.to_string())
}

pub(crate) fn structured_hook_error_data(error: &TraceDecayError) -> Option<Value> {
    let (reason_code, retryable, detail) = error.hook_runtime_context()?;
    Some(json!({
        "tool": "tracedecay_hook_runtime",
        "status": hook_admission_error_status(reason_code),
        "reason_code": reason_code,
        "retryable": retryable,
        "detail": detail,
    }))
}

fn hook_admission_error_status(reason_code: &str) -> HostAdmissionStatus {
    match reason_code {
        "unknown_provider" => HostAdmissionStatus::Unknown,
        "authority_unavailable"
        | "authority_write_failed"
        | "observation_storage_failed"
        | "temporal_refresh_unavailable" => HostAdmissionStatus::Unavailable,
        "cursor_conflict" | "observation_cursor_conflict" | "observation_cancelled" => {
            HostAdmissionStatus::Backpressured
        }
        _ => HostAdmissionStatus::Degraded,
    }
}

pub(super) fn map_host_admission_outcome(outcome: HostAdmissionOutcome) -> TraceDecayError {
    TraceDecayError::hook_runtime(
        outcome.reason_code.unwrap_or("canonical_admission_failed"),
        outcome.retryable,
        "projectless Hermes receipt host admission failed",
    )
}

#[cfg(test)]
mod tests;
