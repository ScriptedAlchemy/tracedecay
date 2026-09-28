use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracedecay_domain::ObservationSourceGenerationV1;

use super::opencode::scan_error;
use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::{HostCoverageReason, TranscriptIngestError, TranscriptIngestResult};

const PROVIDER: &str = "opencode";
const SQLITE_HEADER_BYTES: usize = 100;
const WAL_HEADER_BYTES: usize = 32;
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
/// Bytes actually read while identifying a live database: the SQLite header
/// and, when present, the WAL header. The database body is never charged.
pub(super) const SOURCE_OPEN_READ_BUDGET: u64 =
    (SQLITE_HEADER_BYTES + WAL_HEADER_BYTES) as u64 + 64;

/// A live OpenCode database opened in place.
///
/// Observation identity uses a bounded header fingerprint, not a digest of the
/// database body. Rowid paging still follows `source_file_identity`, so an
/// ordinary append does not rewind the scan frontier.
pub(super) struct OpenCodeDatabase {
    pub(super) path: PathBuf,
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
    let mut wal_length = 0_u64;
    let mut wal_header = [0_u8; WAL_HEADER_BYTES];
    let mut wal_header_len = 0_usize;
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
            Ok(metadata) if metadata.is_file() && index == 1 => {
                wal_length = metadata.len();
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
    let mut header = [0_u8; SQLITE_HEADER_BYTES];
    let header_read = match read_prefix(path, &mut header) {
        Ok(read) => read,
        Err(error) => return Err(scan_error("read OpenCode database header", path, error)),
    };
    if !budget.try_charge_input(read_len(header_read)?) {
        return Ok((OpenedOpenCodeDatabase::Stopped, budget));
    }
    if header_read != SQLITE_HEADER_BYTES || header[..SQLITE_MAGIC.len()] != *SQLITE_MAGIC {
        return Ok((
            OpenedOpenCodeDatabase::Refused(HostCoverageReason::DatabaseUnreadable),
            budget,
        ));
    }

    if wal_length > 0 {
        let wal_path = sqlite_sidecar(path, "-wal");
        if !budget.checkpoint() {
            return Ok((OpenedOpenCodeDatabase::Stopped, budget));
        }
        match read_prefix(&wal_path, &mut wal_header) {
            Ok(read) => {
                wal_header_len = read;
                if !budget.try_charge_input(read_len(read)?) {
                    return Ok((OpenedOpenCodeDatabase::Stopped, budget));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(scan_error("read OpenCode WAL header", path, error));
            }
        }
    }

    let identity = match tracedecay_runtime_core::db::sqlite_generation_identity(path) {
        Ok(identity) => identity,
        Err(_) => {
            return Ok((
                OpenedOpenCodeDatabase::Refused(HostCoverageReason::SourceIdentityUnavailable),
                budget,
            ));
        }
    };
    let generation = header_generation(&header, wal_length, &wal_header[..wal_header_len])?;
    Ok((
        OpenedOpenCodeDatabase::Ready(OpenCodeDatabase {
            path: path.to_path_buf(),
            generation,
            source_file_identity: identity,
        }),
        budget,
    ))
}

/// Fingerprint the SQLite header and WAL header only.
///
/// The change counter and WAL length move when a commit lands, so observation
/// identity still changes when rows change. Hashing the body would read every
/// page of a multi-gigabyte host database before any message could be admitted.
fn header_generation(
    header: &[u8],
    wal_length: u64,
    wal_header: &[u8],
) -> TranscriptIngestResult<ObservationSourceGenerationV1> {
    let mut digest = Sha256::new();
    digest.update(b"tracedecay.opencode.source-generation.v2");
    digest.update(header);
    digest.update(wal_length.to_be_bytes());
    digest.update(wal_header);
    let digest = digest.finalize();
    let bytes: [u8; 8] = digest[..8]
        .try_into()
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    ObservationSourceGenerationV1::new(u64::from_be_bytes(bytes).max(1))
        .map_err(TranscriptIngestError::from)
}

fn read_len(read: usize) -> TranscriptIngestResult<u64> {
    u64::try_from(read).map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })
}

fn read_prefix(path: &Path, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut file = std::fs::File::open(path)?;
    file.read(buffer)
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}
