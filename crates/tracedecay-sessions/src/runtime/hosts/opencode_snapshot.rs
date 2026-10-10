use std::path::{Path, PathBuf};

use tracedecay_domain::ObservationSourceGenerationV1;
use tracedecay_domain::canonical_text::canonical_framed_sha256_bytes;

use super::opencode::scan_error;
use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::{HostCoverageReason, TranscriptIngestError, TranscriptIngestResult};

const PROVIDER: &str = "opencode";
/// A verified snapshot establishes the physical source identity before
/// admission. `generation` folds each family member's write state, so an
/// in-place row edit still produces a new batch generation — arming the
/// rewrite sweep — without reading a single row at open.
pub(super) struct OpenCodeDatabase {
    pub(super) reader: tracedecay_rusqlite_runtime::VerifiedReader,
    pub(super) generation: ObservationSourceGenerationV1,
    pub(super) source_file_identity: u64,
}

pub(super) enum OpenedOpenCodeDatabase {
    /// The opened snapshot. Boxed so a refusal or a stopped scan does not
    /// carry the verified reader's SQLite connection.
    Ready(Box<OpenCodeDatabase>),
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
    let mut member_state = Vec::with_capacity(3);
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
            Ok(metadata) if metadata.is_file() => {
                let modified = metadata
                    .modified()
                    .map_err(|error| scan_error("stat OpenCode database", &member, error))?
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|error| {
                        scan_error(
                            "stat OpenCode database",
                            &member,
                            std::io::Error::other(error),
                        )
                    })?;
                member_state.push((modified.as_nanos() as u64, metadata.len()));
            }
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
    let identity_bytes = identity.to_le_bytes();
    let mut digest = canonical_framed_sha256_bytes(
        b"tracedecay.opencode.database-generation.seed.v1",
        &[&identity_bytes[..]],
    );
    for (modified_nanos, length) in &member_state {
        digest = canonical_framed_sha256_bytes(
            b"tracedecay.opencode.database-generation.fold.v1",
            &[
                &digest[..],
                &modified_nanos.to_le_bytes()[..],
                &length.to_le_bytes()[..],
            ],
        );
    }
    let mut content_generation = [0_u8; 8];
    content_generation.copy_from_slice(&digest[..8]);
    let generation =
        ObservationSourceGenerationV1::new(u64::from_le_bytes(content_generation).max(1))
            .map_err(TranscriptIngestError::from)?;
    Ok((
        OpenedOpenCodeDatabase::Ready(Box::new(OpenCodeDatabase {
            reader,
            generation,
            source_file_identity: identity,
        })),
        budget,
    ))
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}
