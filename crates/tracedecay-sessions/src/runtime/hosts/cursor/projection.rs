use std::collections::{BTreeSet, HashSet};
use std::future::Future;
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tracedecay_domain::{ObservationScopeV1, ProjectId};

use crate::admission::{HostAdmission, is_admission_cancellation};
use crate::observation::ObservationCancellation;
use crate::runtime::snapshot_observation::host_admission_error;
use crate::runtime::source::{TranscriptIngestError, TranscriptIngestResult};

use super::MAX_CURSOR_PROJECTIONS_PER_PASS;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CursorTranscriptIngestStats {
    pub sessions_upserted: u64,
    pub messages_upserted: u64,
    pub bytes_consumed: u64,
    pub source_deferred: bool,
}

#[derive(Debug, Default)]
pub(in crate::runtime) struct CursorSweepIngestOutcome {
    pub stats: CursorTranscriptIngestStats,
    pub session_ids: BTreeSet<String>,
}

pub async fn try_ingest_cursor_project_sweep_capped<S: BuildHasher>(
    project_root: &Path,
    admission: &dyn HostAdmission,
    project_id: ProjectId,
    max_new_bytes: Option<u64>,
    skip_session_ids: HashSet<String, S>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_project_sweep_capped_with_admission(
        project_root,
        project_id,
        admission,
        max_new_bytes,
        skip_session_ids,
        &ObservationCancellation::default(),
    )
    .await
}

pub fn try_ingest_cursor_project_sweep_capped_with_admission<'a, S: BuildHasher>(
    project_root: &'a Path,
    project_id: ProjectId,
    admission: &'a dyn HostAdmission,
    max_new_bytes: Option<u64>,
    skip_session_ids: HashSet<String, S>,
    cancellation: &'a ObservationCancellation,
) -> Pin<Box<dyn Future<Output = TranscriptIngestResult<CursorTranscriptIngestStats>> + Send + 'a>>
{
    let sweep = super::try_ingest_cursor_project_sweep_capped_with_session_ids(
        project_root,
        project_id,
        admission,
        max_new_bytes,
        skip_session_ids,
        cancellation,
    );
    Box::pin(async move { sweep.await.map(|outcome| outcome.stats) })
}

pub async fn try_ingest_cursor_user_sweep_capped<S: BuildHasher>(
    registered_roots: &[PathBuf],
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    skip_session_ids: HashSet<String, S>,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    try_ingest_cursor_user_sweep_capped_with_admission(
        registered_roots,
        admission,
        max_new_bytes,
        skip_session_ids,
        &ObservationCancellation::default(),
    )
    .await
}

pub async fn try_ingest_cursor_user_sweep_capped_with_admission<S: BuildHasher>(
    registered_roots: &[PathBuf],
    admission: &dyn HostAdmission,
    max_new_bytes: Option<u64>,
    skip_session_ids: HashSet<String, S>,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    super::try_ingest_cursor_user_sweep_capped_with_session_ids(
        registered_roots,
        admission,
        max_new_bytes,
        skip_session_ids,
        cancellation,
    )
    .await
    .map(|outcome| outcome.stats)
}

pub(in crate::runtime) struct CursorProjectionDrainStats {
    pub session_ids: Vec<String>,
    pub messages_upserted: u64,
    pub source_deferred: bool,
}

pub(in crate::runtime) async fn drain_cursor_observation_projections(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CursorTranscriptIngestStats> {
    drain_cursor_observation_projections_with_sessions(admission, scope, cancellation)
        .await
        .map(CursorProjectionDrainStats::into_transcript_stats)
}

pub(in crate::runtime) async fn drain_cursor_observation_projections_with_sessions(
    admission: &dyn HostAdmission,
    scope: &ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> TranscriptIngestResult<CursorProjectionDrainStats> {
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
    Ok(CursorProjectionDrainStats {
        session_ids: outcome.session_ids,
        messages_upserted: outcome.projected_outputs,
        source_deferred: outcome.deferred,
    })
}

impl CursorProjectionDrainStats {
    fn into_transcript_stats(self) -> CursorTranscriptIngestStats {
        CursorTranscriptIngestStats {
            sessions_upserted: u64::try_from(self.session_ids.len()).unwrap_or(u64::MAX),
            messages_upserted: self.messages_upserted,
            bytes_consumed: 0,
            source_deferred: self.source_deferred,
        }
    }

    pub(in crate::runtime) fn into_sweep_outcome(
        self,
        bytes_consumed: u64,
        deferred: bool,
    ) -> CursorSweepIngestOutcome {
        let session_ids = self.session_ids.iter().cloned().collect();
        let mut stats = self.into_transcript_stats();
        stats.bytes_consumed = bytes_consumed;
        stats.source_deferred |= deferred;
        CursorSweepIngestOutcome { stats, session_ids }
    }
}

pub(in crate::runtime) fn cursor_ingest_or_default(
    result: &TranscriptIngestResult<CursorTranscriptIngestStats>,
) -> CursorTranscriptIngestStats {
    result.as_ref().map_or_else(
        |_| {
            tracing::error!(
                reason_code = "cursor_observation_ingest_failed",
                "Cursor transcript ingest failed"
            );
            CursorTranscriptIngestStats::default()
        },
        |stats| *stats,
    )
}
