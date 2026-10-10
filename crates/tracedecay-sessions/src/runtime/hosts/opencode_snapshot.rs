use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, Row};
use tracedecay_domain::ObservationSourceGenerationV1;
use tracedecay_domain::canonical_text::canonical_framed_sha256_bytes;

use super::opencode::{install_progress_handler, scan_error};
use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::{HostCoverageReason, TranscriptIngestError, TranscriptIngestResult};

const PROVIDER: &str = "opencode";
/// A verified snapshot establishes the physical source identity before
/// admission. `generation` is a database-wide content digest: an in-place row
/// edit leaves the database file identity intact yet still produces a new
/// batch generation, which arms the rewrite sweep that re-admits changed
/// records.
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
    let Some(content_generation) =
        fingerprint_database(reader.connection(), identity, path, &mut budget)?
    else {
        return Ok((OpenedOpenCodeDatabase::Stopped, budget));
    };
    let generation = ObservationSourceGenerationV1::new(content_generation)
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

/// One ordered pass over the message and part rows folds the database into
/// a database-wide content digest seeded by the file identity. The pass is
/// bounded by the open budget's deadline and cancellation rather than its
/// unit count: the batch generation must cover every row before any record
/// can admit, so a partial fingerprint cannot be charged incrementally.
fn fingerprint_database(
    connection: &Connection,
    file_identity: u64,
    path: &Path,
    budget: &mut HostScanBudget,
) -> TranscriptIngestResult<Option<u64>> {
    install_progress_handler(connection, path, budget)?;
    let file_identity = file_identity.to_le_bytes();
    let mut digest = canonical_framed_sha256_bytes(
        b"tracedecay.opencode.database-generation.seed.v1",
        &[&file_identity[..]],
    );
    for (query, columns) in [
        (
            "SELECT m.session_id, m.rowid, m.id, m.data, s.parent_id, s.directory
             FROM message m JOIN session s ON s.id = m.session_id
             ORDER BY m.session_id, m.rowid",
            &[0_usize, 1, 2, 3, 4, 5][..],
        ),
        (
            "SELECT m.session_id, p.rowid, p.id, p.message_id, p.data
             FROM part p JOIN message m ON m.id = p.message_id
             JOIN session s ON s.id = m.session_id
             ORDER BY m.session_id, p.rowid",
            &[0_usize, 1, 2, 3, 4][..],
        ),
    ] {
        let mut statement = connection
            .prepare(query)
            .map_err(|error| scan_error("prepare database generation query", path, error))?;
        let mut rows = statement
            .query([])
            .map_err(|error| scan_error("query database generation rows", path, error))?;
        loop {
            let row = match rows.next() {
                Ok(Some(row)) => row,
                Ok(None) => break,
                Err(error) => {
                    if !budget.checkpoint() {
                        return Ok(None);
                    }
                    return Err(scan_error("read database generation row", path, error));
                }
            };
            let frame = row_frame(row, columns, path)?;
            digest = canonical_framed_sha256_bytes(
                b"tracedecay.opencode.database-generation.fold.v1",
                &[&digest[..], &frame[..]],
            );
        }
    }
    let mut generation = [0_u8; 8];
    generation.copy_from_slice(&digest[..8]);
    Ok(Some(u64::from_le_bytes(generation).max(1)))
}

/// One framed digest of a row's payload-shaping columns. Each column
/// contributes a type tag plus its bytes so `NULL`, empty, and numeric values
/// never collide.
fn row_frame(row: &Row<'_>, columns: &[usize], path: &Path) -> TranscriptIngestResult<[u8; 32]> {
    let mut parts = Vec::with_capacity(columns.len().saturating_mul(2));
    for &column in columns {
        let value = row
            .get_ref(column)
            .map_err(|error| scan_error("decode database generation value", path, error))?;
        match value {
            ValueRef::Null => parts.extend([vec![0_u8], Vec::new()]),
            ValueRef::Integer(value) => parts.extend([vec![1_u8], value.to_le_bytes().to_vec()]),
            ValueRef::Real(value) => parts.extend([vec![2_u8], value.to_le_bytes().to_vec()]),
            ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                parts.extend([vec![3_u8], bytes.to_vec()]);
            }
        }
    }
    let parts: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    Ok(canonical_framed_sha256_bytes(
        b"tracedecay.opencode.database-generation.row.v1",
        &parts,
    ))
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}
