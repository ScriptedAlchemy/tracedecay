//! Interpretation of a daemon `status` payload's `project_open` snapshot.
//!
//! A stalled snapshot is a typed refusal, not a successful empty report. Status
//! and Doctor both reject it through here so a stalled open can never be
//! printed as a success or downgraded to a warning.

use tracedecay_contracts::project_open::{
    ProjectOpenStatusReasonV1, ProjectOpenStatusStateV1, ProjectOpenStatusV1,
};
use tracedecay_domain::errors::{ProjectOpenFailureKind, Result, TraceDecayError};
use tracedecay_runtime_core::git_discovery::REPOSITORY_DISCOVERY_DEFERRED_REASON_CODE;

/// Typed error for a stalled `project_open` snapshot. Status, Doctor, and the
/// CLI all use this so a stalled open is never a successful empty report.
/// `RetryBackoff` covers capacity, code-runtime-budget, and observability
/// stalls alike, so it maps to the honest typed backoff that keeps the recorded
/// detail, never to a fabricated capacity verdict. Callers that hold the exact
/// `ProjectOpenFailure` replay it instead of this fallback.
pub fn stalled_project_open_error(status: &ProjectOpenStatusV1) -> TraceDecayError {
    let detail = status
        .detail
        .clone()
        .unwrap_or_else(|| "project open stalled".to_owned());
    match status.reason {
        ProjectOpenStatusReasonV1::DeferredRepositoryDiscovery => {
            TraceDecayError::project_route(REPOSITORY_DISCOVERY_DEFERRED_REASON_CODE, true, detail)
        }
        ProjectOpenStatusReasonV1::UnrepairableVerdict => TraceDecayError::project_open(
            ProjectOpenFailureKind::AuthorityVerdict {
                migration_pending: false,
            },
            detail,
        ),
        ProjectOpenStatusReasonV1::RetryBackoff
        | ProjectOpenStatusReasonV1::Unavailable
        | ProjectOpenStatusReasonV1::Converging
        | ProjectOpenStatusReasonV1::Ready => TraceDecayError::project_open(
            ProjectOpenFailureKind::BackedOff {
                retry_after_ms: status.retry_after_ms.unwrap_or(0),
            },
            detail,
        ),
    }
}

/// Reject a successful status/Doctor payload that still carries a stalled
/// `project_open` snapshot. CLI status and Doctor both use this so a leaked
/// snapshot cannot stay a printed success or a warning.
pub fn reject_stalled_project_open_status(daemon_status: &serde_json::Value) -> Result<()> {
    let Some(project_open) = daemon_status
        .get("project_open")
        .cloned()
        .filter(|value| !value.is_null())
        .map(serde_json::from_value::<ProjectOpenStatusV1>)
        .transpose()?
    else {
        return Ok(());
    };
    if project_open.state != ProjectOpenStatusStateV1::Stalled {
        return Ok(());
    }
    Err(stalled_project_open_error(&project_open))
}
