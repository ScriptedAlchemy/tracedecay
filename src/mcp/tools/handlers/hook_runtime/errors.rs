use crate::application::host_admission::{
    HostAdmissionAuthorities, HostAdmissionFacade, HostAdmissionOutcome, HostAdmissionScope,
    HostAdmissionStatus, SharedHostAdmissionBroker,
};
use crate::application::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::automation::config_error;
use crate::automation::run_ledger::AutomationRunStatus;
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::mcp::tools::ToolResult;
use crate::privacy::{ObservationRecordParseErrorV1, parse_normalized_observation_record_v1};
use crate::sessions::claude_observation::ClaudeObservationIngestError;
use crate::sessions::source::TranscriptSource;
use crate::tracedecay::TraceDecay;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracedecay_domain::{
    CanonicalBoundaryKindV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    ProjectId, ProviderId, RetentionClass, SessionId, UtcMicros,
};
use tracedecay_store::StoreShardScopeV1;

use super::super::SessionAuthorities;
use super::super::support::tool_json;

pub(super) fn map_transcript_ingest_error(
    error: &crate::sessions::source::TranscriptIngestError,
) -> TraceDecayError {
    let failure = crate::sessions::classify_transcript_ingest_failure("requested", "hook", error);
    TraceDecayError::hook_runtime(
        failure.reason_code,
        failure.retryable,
        format!("transcript ingest failed: {}", failure.reason_code),
    )
}

pub(super) fn map_claude_observation_ingest_error(
    error: &ClaudeObservationIngestError,
) -> TraceDecayError {
    let failure = crate::sessions::classify_claude_observation_failure(error);
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
        "authority_unavailable" | "authority_write_failed" | "observation_storage_failed" => {
            HostAdmissionStatus::Unavailable
        }
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
