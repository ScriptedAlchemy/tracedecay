use tracedecay_domain::ObservationScopeV1;

use crate::admission::{HostAdmission, is_admission_cancellation};
use crate::observation::ObservationCancellation;
use crate::runtime::snapshot_observation::host_admission_error;
use crate::runtime::source::{TranscriptIngestError, TranscriptIngestResult};

use super::{CursorTranscriptIngestStats, MAX_CURSOR_PROJECTIONS_PER_PASS};

pub(in crate::runtime) async fn drain_cursor_observation_projections(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    if cancellation.is_cancelled() {
        return Err(TranscriptIngestError::Cancelled { provider: "cursor" });
    }
    let outcome = admission
        .drain_projection_queue(
            "cursor",
            scope,
            cancellation,
            MAX_CURSOR_PROJECTIONS_PER_PASS,
        )
        .await
        .map_err(|outcome| {
            if is_admission_cancellation(&outcome, cancellation) {
                TranscriptIngestError::Cancelled { provider: "cursor" }
            } else {
                host_admission_error("cursor", outcome)
            }
        })?;
    Ok(CursorTranscriptIngestStats {
        sessions_upserted: u64::try_from(outcome.session_ids.len()).unwrap_or(u64::MAX),
        messages_upserted: outcome.projected_outputs,
        bytes_consumed: 0,
        source_deferred: outcome.deferred,
    })
}
