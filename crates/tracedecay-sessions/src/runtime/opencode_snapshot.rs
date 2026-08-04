use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::backup::StepResult;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tracedecay_domain::ObservationSourceGenerationV1;

use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::{TranscriptIngestError, TranscriptIngestResult};

const PROVIDER: &str = "opencode";
const MAX_SNAPSHOT_DATABASE_BYTES: u64 = 512 * 1024 * 1024;
pub(super) const MAX_SNAPSHOT_DATABASE_IO_BYTES: u64 = MAX_SNAPSHOT_DATABASE_BYTES * 2;
const SNAPSHOT_PAGES_PER_STEP: i32 = 128;
const SNAPSHOT_BUSY_PAUSE: Duration = Duration::from_millis(1);

pub(super) struct OpenCodeDatabaseSnapshot {
    _directory: tempfile::TempDir,
    pub(super) path: PathBuf,
    pub(super) generation: ObservationSourceGenerationV1,
    pub(super) source_file_identity: u64,
}

pub(super) async fn snapshot_database(
    database_path: PathBuf,
    scratch_root: PathBuf,
    budget: HostScanBudget,
) -> TranscriptIngestResult<(Option<OpenCodeDatabaseSnapshot>, HostScanBudget)> {
    tokio::task::spawn_blocking(move || {
        snapshot_database_blocking(&database_path, &scratch_root, budget)
    })
    .await
    .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })?
}

fn snapshot_database_blocking(
    database_path: &Path,
    scratch_root: &Path,
    mut budget: HostScanBudget,
) -> TranscriptIngestResult<(Option<OpenCodeDatabaseSnapshot>, HostScanBudget)> {
    if !budget.checkpoint() {
        return Ok((None, budget));
    }
    match std::fs::metadata(database_path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok((None, budget)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((None, budget));
        }
        Err(error) => return Err(scan_error("stat OpenCode database", database_path, error)),
    }
    let source_file_identity =
        tracedecay_runtime_core::db::sqlite_generation_identity(database_path).map_err(|_| {
            scan_error(
                "identify OpenCode database",
                database_path,
                std::io::Error::other("OpenCode database identity is unavailable"),
            )
        })?;
    create_snapshot_scratch_root(scratch_root).map_err(|error| {
        scan_error("create OpenCode snapshot scratch root", scratch_root, error)
    })?;
    let directory = tempfile::Builder::new()
        .prefix("read-")
        .tempdir_in(scratch_root)
        .map_err(|error| scan_error("create OpenCode snapshot directory", scratch_root, error))?;
    let snapshot_path = directory.path().join("opencode.db");
    let source_connection = Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| scan_error("open live OpenCode database", database_path, error))?;
    source_connection
        .busy_timeout(Duration::from_millis(50))
        .map_err(|error| scan_error("bound OpenCode snapshot lock wait", database_path, error))?;
    let page_count = pragma_u64(&source_connection, "page_count", database_path)?;
    let page_size = pragma_u64(&source_connection, "page_size", database_path)?;
    let logical_bytes = page_count.saturating_mul(page_size);
    if logical_bytes > MAX_SNAPSHOT_DATABASE_BYTES || !budget.try_charge_input(logical_bytes) {
        return Ok((None, budget));
    }
    let mut snapshot_connection = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| scan_error("create OpenCode database snapshot", database_path, error))?;
    {
        let backup = rusqlite::backup::Backup::new(&source_connection, &mut snapshot_connection)
            .map_err(|error| {
                scan_error("start OpenCode database snapshot", database_path, error)
            })?;
        loop {
            if !budget.checkpoint() {
                return Ok((None, budget));
            }
            match backup.step(SNAPSHOT_PAGES_PER_STEP).map_err(|error| {
                scan_error("copy OpenCode database snapshot", database_path, error)
            })? {
                StepResult::Done => break,
                StepResult::More => {}
                StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(SNAPSHOT_BUSY_PAUSE);
                }
                _ => {}
            }
        }
    }
    drop(snapshot_connection);
    drop(source_connection);
    let generation = database_generation(&snapshot_path, database_path, &mut budget)?;
    let Some(generation) = generation else {
        return Ok((None, budget));
    };
    Ok((
        Some(OpenCodeDatabaseSnapshot {
            _directory: directory,
            path: snapshot_path,
            generation,
            source_file_identity,
        }),
        budget,
    ))
}

fn pragma_u64(
    connection: &Connection,
    pragma: &'static str,
    source_path: &Path,
) -> TranscriptIngestResult<u64> {
    connection
        .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0))
        .map_err(|error| scan_error("measure OpenCode database", source_path, error))
        .and_then(|value| {
            u64::try_from(value)
                .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })
        })
}

fn database_generation(
    path: &Path,
    error_path: &Path,
    budget: &mut HostScanBudget,
) -> TranscriptIngestResult<Option<ObservationSourceGenerationV1>> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| scan_error("stat OpenCode snapshot generation", error_path, error))?;
    if metadata.len() > MAX_SNAPSHOT_DATABASE_BYTES || !budget.try_charge_input(metadata.len()) {
        return Ok(None);
    }
    let mut file = File::open(path)
        .map_err(|error| scan_error("open OpenCode snapshot generation", error_path, error))?;
    let mut digest = Sha256::new();
    digest.update(b"tracedecay.opencode.snapshot-generation.v1");
    digest.update(metadata.len().to_be_bytes());
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        if !budget.checkpoint() {
            return Ok(None);
        }
        let read = file
            .read(&mut buffer)
            .map_err(|error| scan_error("hash OpenCode snapshot generation", error_path, error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let digest = digest.finalize();
    let bytes: [u8; 8] = digest[..8]
        .try_into()
        .map_err(|_| TranscriptIngestError::InvalidFrameState { provider: PROVIDER })?;
    ObservationSourceGenerationV1::new(u64::from_be_bytes(bytes).max(1))
        .map(Some)
        .map_err(TranscriptIngestError::from)
}

fn create_snapshot_scratch_root(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

pub(super) fn snapshot_scratch_root() -> PathBuf {
    std::env::temp_dir().join("tracedecay-opencode-read")
}

fn scan_error(
    operation: &'static str,
    path: &Path,
    error: impl std::error::Error + Send + Sync + 'static,
) -> TranscriptIngestError {
    TranscriptIngestError::ScanIo {
        operation,
        path: path.to_path_buf(),
        source: std::io::Error::other(error),
    }
}
