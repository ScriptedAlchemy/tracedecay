//! `SQLite` incarnation/cursor coverage, observation admission, and projection
//! drains against the central host-admission facade.

use std::collections::BTreeSet;
use std::path::Path;

#[cfg(any(unix, windows))]
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
};
use tracedecay_store::ObservationPersistOutcome;
use tracedecay_store::observation::{ObservationCoverageReason, ObservationCursorAdvance};

use crate::application::host_admission::{HostAdmissionFacade, HostAdmissionOutcome};
use crate::application::observation::{CaptureObservationOutcome, ObservationCancellation};
use crate::sessions::shared::TranscriptIngestStats;
use crate::sessions::source::SqliteFileIdentityError;

use super::observation::{
    HermesAdmission, HermesAdmissionAction, HermesProjectionMetadata, observation_source,
    prepare_observation_row_with_cancellation,
};
use super::rows::HermesRow;
use super::{MAX_HERMES_PROJECTIONS_PER_DRAIN, PROVIDER};

pub(crate) fn sqlite_incarnation(
    path: &Path,
) -> Result<(ObservationSourceGenerationV1, u64, u64), String> {
    let file_identity =
        crate::sessions::source::sqlite_generation_identity(path).map_err(|error| {
            match error {
                SqliteFileIdentityError::Open => "could not open Hermes SQLite authority",
                SqliteFileIdentityError::Inspect => "could not inspect Hermes SQLite authority",
                SqliteFileIdentityError::Identify => "could not identify Hermes SQLite authority",
                SqliteFileIdentityError::Unavailable => {
                    "Hermes SQLite physical identity is unavailable"
                }
            }
            .to_string()
        })?;
    let resume_fingerprint = sqlite_resume_fingerprint(path, file_identity)?;
    let generation = ObservationSourceGenerationV1::new(file_identity)
        .map_err(|_| "invalid Hermes SQLite generation".to_string())?;
    Ok((generation, file_identity, resume_fingerprint))
}

#[allow(clippy::too_many_arguments)]
async fn advance_coverage(
    facade: &HostAdmissionFacade<'_>,
    source: ObservationSourceIdentityV1,
    range: ObservationSourceRangeV1,
    expected_cursor: Option<ObservationSourceCursorV1>,
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    reason: ObservationCoverageReason,
    receipt: Option<tracedecay_domain::SanitizationReceiptV1>,
    file_identity: u64,
    resume_fingerprint: u64,
    cancellation: &ObservationCancellation,
) -> Result<(), String> {
    let advance = match receipt {
        Some(receipt) => ObservationCursorAdvance::for_ordering_with_sanitization_receipt(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::SqliteRowId,
            expected_cursor,
            range,
            reason,
            receipt,
        ),
        None => ObservationCursorAdvance::for_ordering(
            source,
            scope,
            generation,
            ObservationOrderingDomainV1::SqliteRowId,
            expected_cursor,
            range,
            reason,
        ),
    }
    .map_err(|_| "invalid Hermes coverage transition".to_string())?
    .with_resume_checkpoint(file_identity, resume_fingerprint);
    facade
        .advance_non_durable_source_cursor(advance, cancellation.clone())
        .await
        .map(|_| ())
        .map_err(host_admission_error)
}

fn host_admission_error(outcome: HostAdmissionOutcome) -> String {
    crate::sessions::snapshot_observation::host_admission_status_message("Hermes", outcome.status)
}

pub(crate) async fn drain_hermes_projections_with_admission(
    facade: &HostAdmissionFacade<'_>,
    scope: &ObservationScopeV1,
) -> Result<(), String> {
    drain_hermes_projections_with_admission_and_cancellation(
        facade,
        scope,
        &ObservationCancellation::default(),
    )
    .await
}

pub(crate) async fn drain_hermes_projections_with_admission_and_cancellation(
    facade: &HostAdmissionFacade<'_>,
    scope: &ObservationScopeV1,
    cancellation: &ObservationCancellation,
) -> Result<(), String> {
    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let outcome = facade
            .drain_projection_queue(
                PROVIDER,
                scope,
                cancellation,
                MAX_HERMES_PROJECTIONS_PER_DRAIN,
            )
            .await
            .map_err(host_admission_error)?;
        let processed = outcome
            .projected
            .saturating_add(outcome.skipped)
            .saturating_add(outcome.exact_duplicates);
        if processed < u64::try_from(MAX_HERMES_PROJECTIONS_PER_DRAIN).unwrap_or(u64::MAX) {
            return Ok(());
        }
    }
}

pub(crate) async fn admit_rows_with_admission(
    facade: &HostAdmissionFacade<'_>,
    rows: &[HermesRow],
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    file_identity: u64,
    resume_fingerprint: u64,
    route: impl Fn(&HermesRow) -> Option<HermesProjectionMetadata>,
) -> Result<TranscriptIngestStats, String> {
    admit_rows_with_admission_and_cancellation(
        facade,
        rows,
        scope,
        generation,
        file_identity,
        resume_fingerprint,
        route,
        &ObservationCancellation::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn admit_rows_with_admission_and_cancellation(
    facade: &HostAdmissionFacade<'_>,
    rows: &[HermesRow],
    scope: ObservationScopeV1,
    generation: ObservationSourceGenerationV1,
    file_identity: u64,
    resume_fingerprint: u64,
    route: impl Fn(&HermesRow) -> Option<HermesProjectionMetadata>,
    cancellation: &ObservationCancellation,
) -> Result<TranscriptIngestStats, String> {
    let mut stats = TranscriptIngestStats::default();
    let mut sessions = BTreeSet::new();
    for row in rows {
        if cancellation.is_cancelled() {
            break;
        }
        let source = observation_source(row)?;
        let expected_cursor = facade
            .get_source_cursor(&source, &scope)
            .await
            .map_err(host_admission_error)?;
        if cancellation.is_cancelled() {
            break;
        }
        let end = u64::try_from(row.id).map_err(|_| "invalid Hermes SQLite row id".to_string())?;
        if expected_cursor
            .as_ref()
            .is_some_and(|cursor| cursor.generation() == generation && cursor.position() >= end)
        {
            continue;
        }
        let admission = prepare_observation_row_with_cancellation(
            row,
            route(row).as_ref(),
            &scope,
            generation,
            expected_cursor,
            file_identity,
            resume_fingerprint,
            cancellation,
        )?;
        let HermesAdmission {
            source,
            range,
            expected_cursor,
            action,
        } = admission;
        match action {
            HermesAdmissionAction::Cover(reason) => {
                advance_coverage(
                    facade,
                    source,
                    range,
                    expected_cursor,
                    scope.clone(),
                    generation,
                    reason,
                    None,
                    file_identity,
                    resume_fingerprint,
                    cancellation,
                )
                .await?;
            }
            HermesAdmissionAction::Capture(request) => {
                match facade
                    .capture_observation(*request)
                    .await
                    .map_err(host_admission_error)?
                {
                    CaptureObservationOutcome::Persisted { outcome, .. } => {
                        if matches!(*outcome, ObservationPersistOutcome::Committed(_)) {
                            stats.messages_upserted = stats.messages_upserted.saturating_add(1);
                        }
                        sessions.insert(row.session_id.clone());
                    }
                    CaptureObservationOutcome::Rejected { receipt, .. } => {
                        advance_coverage(
                            facade,
                            source,
                            range,
                            expected_cursor,
                            scope.clone(),
                            generation,
                            ObservationCoverageReason::SanitizerRejected,
                            Some(receipt),
                            file_identity,
                            resume_fingerprint,
                            cancellation,
                        )
                        .await?;
                    }
                    CaptureObservationOutcome::Quarantined { receipt, .. } => {
                        advance_coverage(
                            facade,
                            source,
                            range,
                            expected_cursor,
                            scope.clone(),
                            generation,
                            ObservationCoverageReason::SanitizerQuarantined,
                            Some(receipt),
                            file_identity,
                            resume_fingerprint,
                            cancellation,
                        )
                        .await?;
                    }
                }
            }
        }
    }
    stats.sessions_upserted = sessions.len() as u64;
    Ok(stats)
}

#[cfg(unix)]
fn file_mtime_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(unix)]
fn sqlite_resume_fingerprint(path: &Path, file_identity: u64) -> Result<u64, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|_| "could not inspect Hermes SQLite authority".to_string())?;
    let mut resume_hasher = Sha256::new();
    resume_hasher.update(file_identity.to_le_bytes());
    resume_hasher.update(metadata.len().to_le_bytes());
    resume_hasher.update(file_mtime_secs(path).to_le_bytes());
    let resume_digest = resume_hasher.finalize();
    let mut resume_bytes = [0_u8; 8];
    resume_bytes.copy_from_slice(&resume_digest[..8]);
    Ok(u64::from_le_bytes(resume_bytes))
}

#[cfg(windows)]
fn sqlite_resume_fingerprint(path: &Path, file_identity: u64) -> Result<u64, String> {
    use std::os::windows::fs::MetadataExt;

    let metadata = std::fs::metadata(path)
        .map_err(|_| "could not inspect Hermes SQLite authority".to_string())?;
    let mut resume_hasher = Sha256::new();
    resume_hasher.update(file_identity.to_le_bytes());
    resume_hasher.update(metadata.len().to_le_bytes());
    resume_hasher.update(metadata.last_write_time().to_le_bytes());
    let resume_digest = resume_hasher.finalize();
    let mut resume_bytes = [0_u8; 8];
    resume_bytes.copy_from_slice(&resume_digest[..8]);
    Ok(u64::from_le_bytes(resume_bytes))
}

#[cfg(not(any(unix, windows)))]
fn sqlite_resume_fingerprint(path: &Path, _file_identity: u64) -> Result<u64, String> {
    let _ = path;
    Err("Hermes SQLite physical identity is unavailable".to_string())
}
