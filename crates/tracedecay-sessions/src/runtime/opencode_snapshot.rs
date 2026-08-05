use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tracedecay_domain::ObservationSourceGenerationV1;
use tracedecay_runtime_core::sqlite_read_snapshot::SnapshotDatabase;

use crate::runtime::host_scan::HostScanBudget;
use crate::runtime::source::{TranscriptIngestError, TranscriptIngestResult};

const PROVIDER: &str = "opencode";
const MAX_SNAPSHOT_DATABASE_BYTES: u64 = 512 * 1024 * 1024;
pub(super) const MAX_SNAPSHOT_DATABASE_IO_BYTES: u64 = MAX_SNAPSHOT_DATABASE_BYTES * 2;

pub(super) struct OpenCodeDatabaseSnapshot {
    _snapshot: SnapshotDatabase,
    pub(super) path: PathBuf,
    pub(super) generation: ObservationSourceGenerationV1,
    /// The persisted scan-frontier incarnation. Observation identity uses the
    /// exact content generation above; rowid paging separately follows the
    /// physical source incarnation so ordinary WAL appends do not reset it.
    pub(super) source_file_identity: u64,
}

pub(super) async fn snapshot_database(
    database_path: PathBuf,
    scratch_root: PathBuf,
    budget: HostScanBudget,
) -> TranscriptIngestResult<(Option<OpenCodeDatabaseSnapshot>, HostScanBudget)> {
    let measured_path = database_path.clone();
    let measured =
        tokio::task::spawn_blocking(move || measure_database_family(&measured_path, budget))
            .await
            .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })??;
    let (source_identity, mut budget) = measured;
    let Some(source_identity) = source_identity else {
        return Ok((None, budget));
    };

    let snapshot_control = tracedecay_runtime_core::sqlite_read_snapshot::SnapshotReadControl::new(
        budget.deadline(),
        {
            let cancellation = budget.cancellation();
            move || cancellation.is_cancelled()
        },
    );
    let snapshot = tracedecay_runtime_core::sqlite_read_snapshot::open_foreign_in(
        &database_path,
        &scratch_root,
        snapshot_control,
    )
    .await
    .map_err(|error| scan_error("freeze OpenCode database snapshot", &database_path, error))?;
    if !budget.checkpoint() {
        return Ok((None, budget));
    }
    let snapshot_path = snapshot
        .attach_token()
        .and_then(|token| token.verified_identity_path().map(Path::to_path_buf))
        .map_err(|error| scan_error("verify OpenCode database snapshot", &database_path, error))?;
    let generation_path = snapshot_path.clone();
    let error_path = database_path.clone();
    let generated = tokio::task::spawn_blocking(move || {
        database_generation(&generation_path, &error_path, budget)
    })
    .await
    .map_err(|_| TranscriptIngestError::BlockingScanTaskFailed { provider: PROVIDER })??;
    let (generation, returned_budget) = generated;
    budget = returned_budget;
    let Some(generation) = generation else {
        return Ok((None, budget));
    };
    snapshot
        .validate_source()
        .map_err(|_| TranscriptIngestError::ScanGenerationChanged {
            path: database_path,
        })?;
    Ok((
        Some(OpenCodeDatabaseSnapshot {
            _snapshot: snapshot,
            path: snapshot_path,
            generation,
            source_file_identity: source_identity,
        }),
        budget,
    ))
}

fn measure_database_family(
    path: &Path,
    mut budget: HostScanBudget,
) -> TranscriptIngestResult<(Option<u64>, HostScanBudget)> {
    let mut family_bytes = 0_u64;
    for (index, member) in [
        path.to_path_buf(),
        sqlite_sidecar(path, "-wal"),
        sqlite_sidecar(path, "-shm"),
    ]
    .into_iter()
    .enumerate()
    {
        if !budget.try_charge_unit() {
            return Ok((None, budget));
        }
        match std::fs::metadata(&member) {
            Ok(metadata) if metadata.is_file() => {
                family_bytes = family_bytes.saturating_add(metadata.len());
            }
            Ok(_) if index == 0 => {
                budget.mark_unavailable();
                return Ok((None, budget));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && index == 0 => {
                return Ok((None, budget));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(scan_error("stat OpenCode database", path, error)),
        }
    }
    if family_bytes > MAX_SNAPSHOT_DATABASE_BYTES || !budget.try_charge_input(family_bytes) {
        return Ok((None, budget));
    }
    let identity = tracedecay_runtime_core::db::sqlite_generation_identity(path).map_err(|_| {
        scan_error(
            "identify OpenCode database",
            path,
            std::io::Error::other("OpenCode database identity is unavailable"),
        )
    })?;
    Ok((Some(identity), budget))
}

fn database_generation(
    path: &Path,
    error_path: &Path,
    mut budget: HostScanBudget,
) -> TranscriptIngestResult<(Option<ObservationSourceGenerationV1>, HostScanBudget)> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| scan_error("stat OpenCode snapshot generation", error_path, error))?;
    if metadata.len() > MAX_SNAPSHOT_DATABASE_BYTES || !budget.try_charge_input(metadata.len()) {
        return Ok((None, budget));
    }
    let mut file = std::fs::File::open(path)
        .map_err(|error| scan_error("open OpenCode snapshot generation", error_path, error))?;
    let mut digest = Sha256::new();
    digest.update(b"tracedecay.opencode.snapshot-generation.v1");
    digest.update(metadata.len().to_be_bytes());
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        if !budget.checkpoint() {
            return Ok((None, budget));
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
    let generation = ObservationSourceGenerationV1::new(u64::from_be_bytes(bytes).max(1))
        .map_err(TranscriptIngestError::from)?;
    Ok((Some(generation), budget))
}

fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

pub(super) fn snapshot_scratch_root() -> Option<PathBuf> {
    tracedecay_runtime_core::storage::default_profile_root()
        .ok()
        .map(|root| root.join("scratch/sqlite-read/opencode"))
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
