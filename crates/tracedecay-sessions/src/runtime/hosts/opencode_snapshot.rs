use std::path::{Path, PathBuf};

use tracedecay_domain::ObservationSourceGenerationV1;

use super::opencode::scan_error;
use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::{HostCoverageReason, TranscriptIngestError, TranscriptIngestResult};

const PROVIDER: &str = "opencode";
/// A verified snapshot establishes the physical source identity before admission.
/// Content changes are identified by each record's existing payload digest.
pub(super) struct OpenCodeDatabase {
    pub(super) reader: tracedecay_rusqlite_runtime::VerifiedReader,
    pub(super) generation: ObservationSourceGenerationV1,
    pub(super) source_file_identity: u64,
}

pub(super) enum OpenedOpenCodeDatabase {
    Ready(OpenCodeDatabase),
    Refused(HostCoverageReason),
    /// The open budget ended before a source decision (cancellation or deadline).
    Stopped,
}

pub(super) async fn open_database(
    database_path: PathBuf,
    budget: HostScanBudget,
) -> TranscriptIngestResult<(OpenedOpenCodeDatabase, HostScanBudget)> {
    let inspected = tokio::task::spawn_blocking(move || inspect_database(&database_path, budget))
        .await
        .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })??;
    Ok(inspected)
}

fn inspect_database(
    path: &Path,
    mut budget: HostScanBudget,
) -> TranscriptIngestResult<(OpenedOpenCodeDatabase, HostScanBudget)> {
    for (index, member) in [
        path.to_path_buf(),
        sqlite_sidecar(path, "-wal"),
        sqlite_sidecar(path, "-shm"),
    ]
    .into_iter()
    .enumerate()
    {
        if !budget.try_charge_unit() {
            return Ok((OpenedOpenCodeDatabase::Stopped, budget));
        }
        match std::fs::symlink_metadata(&member) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(scan_error(
                    "stat OpenCode database",
                    &member,
                    std::io::Error::other(
                        "OpenCode database family members must not be symbolic links",
                    ),
                ));
            }
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) if index == 0 => {
                return Ok((
                    OpenedOpenCodeDatabase::Refused(HostCoverageReason::DatabaseNotAFile),
                    budget,
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && index == 0 => {
                return Ok((
                    OpenedOpenCodeDatabase::Refused(HostCoverageReason::DatabaseMissing),
                    budget,
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(scan_error("stat OpenCode database", path, error)),
        }
    }

    if !budget.checkpoint() {
        return Ok((OpenedOpenCodeDatabase::Stopped, budget));
    }
    let reader = match tracedecay_rusqlite_runtime::open_verified_read_snapshot(path) {
        Ok(reader) => reader,
        Err(error) => {
            let reason = match error {
                tracedecay_rusqlite_runtime::VerifiedReaderError::Identity(_) => {
                    HostCoverageReason::SourceIdentityUnavailable
                }
                _ => HostCoverageReason::DatabaseUnreadable,
            };
            return Ok((OpenedOpenCodeDatabase::Refused(reason), budget));
        }
    };
    let identity = reader.file_identity();
    let generation =
        ObservationSourceGenerationV1::new(identity).map_err(TranscriptIngestError::from)?;
    Ok((
        OpenedOpenCodeDatabase::Ready(OpenCodeDatabase {
            reader,
            generation,
            source_file_identity: identity,
        }),
        budget,
    ))
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}
