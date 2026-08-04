//! Daemon-owned orchestration for the process-isolated rusqlite parity helper.
//!
//! The helper is deliberately not linked into the daemon. This module accepts
//! only an explicit executable and an explicit authority-store path, freezes a
//! coherent single-file copy with the canonical `SQLite` snapshot machinery, and
//! exchanges one closed, versioned request from the shared parity protocol.

#![cfg(test)]

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Component, Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tracedecay_sqlite_parity_protocol::{
    CommandV1, CopiedDatabaseV1, CopiedSnapshotProvenanceV1, DatabaseKindV1, ErrorCodeV1,
    ErrorPayloadV1, MAX_REQUEST_BYTES, OutputV1, PROTOCOL_VERSION, ROW_DIGEST_ALGORITHM, RequestV1,
    ResponseOutcomeV1, ResponseV1, SessionStoreCursorV1, SessionStoreFamilyV1, SessionStorePageV1,
    SessionStoreRowV1, SessionStoreTableV1, SnapshotFileIdentityV1, VerifiedCopiedSnapshotV1,
    is_canonical_sha256_digest, validate_request,
};
use tracedecay_store::StoreRuntimeBindingV1;

use crate::cancellation::{CancellationToken, MonotonicDeadline};

const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 64 * 1024;
const PROCESS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);
const SNAPSHOT_SQL: &str = "VACUUM INTO ?1";
const QUERY_ONLY_OFF_SQL: &str = "PRAGMA query_only = OFF";
const QUERY_ONLY_ON_SQL: &str = "PRAGMA query_only = ON";

static NEXT_INVOCATION: AtomicU64 = AtomicU64::new(0);

/// A daemon-level parity operation bound to one active runtime publication.
///
/// The wire command itself is owned by `tracedecay-sqlite-parity-protocol`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RusqliteParityRequestV1 {
    store_identity: StoreRuntimeBindingV1,
    command: CommandV1,
}

impl RusqliteParityRequestV1 {
    pub fn new(store_identity: StoreRuntimeBindingV1, command: CommandV1) -> Self {
        Self {
            store_identity,
            command,
        }
    }

    pub fn store_identity(&self) -> &StoreRuntimeBindingV1 {
        &self.store_identity
    }

    pub fn command(&self) -> &CommandV1 {
        &self.command
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RusqliteParityResultV1 {
    store_identity: StoreRuntimeBindingV1,
    output: OutputV1,
}

impl RusqliteParityResultV1 {
    pub fn store_identity(&self) -> &StoreRuntimeBindingV1 {
        &self.store_identity
    }

    pub fn output(&self) -> &OutputV1 {
        &self.output
    }
}

/// Infrastructure-only failures from snapshotting, transport, or protocol validation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RusqliteParityInfrastructureErrorV1 {
    #[error("invalid rusqlite parity {field}: {message}")]
    InvalidPath {
        field: &'static str,
        message: String,
    },
    #[error(
        "rusqlite parity staging root '{}' overlaps known live store/profile root '{}'",
        staging_root.display(),
        live_root.display()
    )]
    StagingOverlapsKnownLiveRoot {
        staging_root: PathBuf,
        live_root: PathBuf,
    },
    #[error("rusqlite parity request store identity does not match the expected identity")]
    StoreIdentityMismatch {
        expected: Box<StoreRuntimeBindingV1>,
        request: Box<StoreRuntimeBindingV1>,
    },
    #[cfg(not(unix))]
    #[error("rusqlite parity is unavailable until safe process-group ownership is implemented")]
    UnsupportedPlatform,
    #[error("rusqlite parity operation was cancelled")]
    Cancelled,
    #[error("rusqlite parity monotonic deadline was exceeded")]
    DeadlineExceeded,
    #[error("could not create a coherent rusqlite parity snapshot: {message}")]
    Snapshot { message: String },
    #[error("could not serialize the rusqlite parity request: {message}")]
    RequestEncoding { message: String },
    #[error("rusqlite parity request violates the shared protocol: {error:?}")]
    RequestRejected { error: ErrorPayloadV1 },
    #[error("could not spawn the explicit rusqlite parity helper: {message}")]
    Spawn { message: String },
    #[error("rusqlite parity helper transport failed during {stage}: {message}")]
    Transport {
        stage: &'static str,
        message: String,
    },
    #[error("rusqlite parity helper {stream} exceeded the {limit}-byte limit")]
    OutputTooLarge { stream: &'static str, limit: usize },
    #[error("rusqlite parity helper exited unsuccessfully ({status}): {stderr}")]
    HelperExit { status: String, stderr: String },
    #[error("rusqlite parity helper returned malformed JSON: {message}")]
    MalformedResponse { message: String },
    #[error(
        "rusqlite parity helper protocol version mismatch: expected {expected}, received {actual}"
    )]
    ProtocolVersionMismatch { expected: u16, actual: u16 },
    #[error("rusqlite parity helper response request identity did not match its request")]
    ResponseIdentityMismatch,
    #[error("rusqlite parity helper verified snapshot did not match sealed request provenance")]
    ResponseSnapshotMismatch {
        expected: Box<VerifiedCopiedSnapshotV1>,
        actual: Option<Box<VerifiedCopiedSnapshotV1>>,
    },
    #[error("rusqlite parity helper response operation did not match its request")]
    ResponseOperationMismatch,
    #[error("rusqlite parity helper rejected the typed request: {error:?}")]
    HelperRejected { error: ErrorPayloadV1 },
    #[error("could not clean rusqlite parity staging: {message}")]
    Cleanup { message: String },
}

/// Runs one parity operation against a daemon-created, single-file store copy.
///
/// No helper lookup or authority-store fallback occurs: all paths and the
/// expected logical identity are supplied by the caller. `known_live_roots`
/// must contain every live store or profile root known to that caller; staging
/// under one of those roots is rejected before a copy can be created.
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(unix), allow(unreachable_code))] // early UnsupportedPlatform return
pub(crate) async fn run_rusqlite_parity_v1(
    helper_executable: &Path,
    authority_store_path: &Path,
    staging_root: &Path,
    known_live_roots: &[PathBuf],
    request: RusqliteParityRequestV1,
    deadline: MonotonicDeadline,
    cancellation: &CancellationToken,
    expected_store_identity: &StoreRuntimeBindingV1,
) -> Result<RusqliteParityResultV1, RusqliteParityInfrastructureErrorV1> {
    #[cfg(not(unix))]
    {
        let _ = (
            helper_executable,
            authority_store_path,
            staging_root,
            known_live_roots,
            &request,
            &deadline,
            cancellation,
            expected_store_identity,
        );
        return Err(RusqliteParityInfrastructureErrorV1::UnsupportedPlatform);
    }

    check_interruption(deadline, cancellation)?;
    validate_regular_file(helper_executable, "helper executable")?;
    validate_regular_file(authority_store_path, "authority store")?;
    let staging_root = validate_staging_root(staging_root, known_live_roots)?;
    if request.store_identity() != expected_store_identity {
        return Err(RusqliteParityInfrastructureErrorV1::StoreIdentityMismatch {
            expected: Box::new(expected_store_identity.clone()),
            request: Box::new(request.store_identity().clone()),
        });
    }
    check_interruption(deadline, cancellation)?;

    let snapshot = run_interruptible(
        crate::sqlite_read_snapshot::open_in(authority_store_path, &staging_root),
        deadline,
        cancellation,
    )
    .await?
    .map_err(|error| RusqliteParityInfrastructureErrorV1::Snapshot {
        message: error.to_string(),
    })?;
    check_interruption(deadline, cancellation)?;
    let staging_root = validate_staging_root(&staging_root, known_live_roots)?;

    let invocation = InvocationDirectory::create(&staging_root)?;
    let result = async {
        let copied_snapshot = invocation.path.join("snapshot.db");
        materialize_single_file(&snapshot, &copied_snapshot, deadline, cancellation).await?;
        check_interruption(deadline, cancellation)?;
        snapshot.validate_source().map_err(|error| {
            RusqliteParityInfrastructureErrorV1::Snapshot {
                message: error.to_string(),
            }
        })?;
        drop(snapshot);
        check_interruption(deadline, cancellation)?;

        let request_id = format!(
            "parity-{}-{}",
            std::process::id(),
            NEXT_INVOCATION.fetch_add(1, Ordering::Relaxed)
        );
        let wire_request = build_wire_request(
            request_id,
            request.command().clone(),
            &copied_snapshot,
            &invocation,
            expected_store_identity,
        )?;
        let response = invoke_helper(
            helper_executable,
            &invocation,
            &wire_request,
            deadline,
            cancellation,
        )
        .await?;
        check_interruption(deadline, cancellation)?;
        let output = validate_response(response, &wire_request)?;
        Ok(RusqliteParityResultV1 {
            store_identity: expected_store_identity.clone(),
            output,
        })
    }
    .await;

    let cleanup = invocation.cleanup();
    match result {
        Err(error) => Err(error),
        Ok(result) => {
            cleanup.map_err(|error| RusqliteParityInfrastructureErrorV1::Cleanup {
                message: error.to_string(),
            })?;
            check_interruption(deadline, cancellation)?;
            Ok(result)
        }
    }
}

fn validate_regular_file(
    path: &Path,
    field: &'static str,
) -> Result<(), RusqliteParityInfrastructureErrorV1> {
    if !path.is_absolute() {
        return Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
            field,
            message: "path must be absolute".to_string(),
        });
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RusqliteParityInfrastructureErrorV1::InvalidPath {
            field,
            message: format!("path must name an existing regular file: {error}"),
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
            field,
            message: "path must name a regular file and not a symlink".to_string(),
        });
    }
    Ok(())
}

fn validate_staging_root(
    path: &Path,
    known_live_roots: &[PathBuf],
) -> Result<PathBuf, RusqliteParityInfrastructureErrorV1> {
    let staging_root = canonical_staging_directory(path, "staging root")?;
    for root in known_live_roots {
        let live_root = canonical_known_live_root(root)?;
        if staging_root.starts_with(&live_root) || live_root.starts_with(&staging_root) {
            return Err(
                RusqliteParityInfrastructureErrorV1::StagingOverlapsKnownLiveRoot {
                    staging_root,
                    live_root,
                },
            );
        }
    }
    Ok(staging_root)
}

fn canonical_known_live_root(path: &Path) -> Result<PathBuf, RusqliteParityInfrastructureErrorV1> {
    const FIELD: &str = "known live store/profile root";

    validate_absolute_nontraversing_path(path, FIELD)?;
    let metadata =
        fs::metadata(path).map_err(|error| RusqliteParityInfrastructureErrorV1::InvalidPath {
            field: FIELD,
            message: format!("path must name an existing directory: {error}"),
        })?;
    if !metadata.is_dir() {
        return Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
            field: FIELD,
            message: "path must name a directory".to_string(),
        });
    }
    fs::canonicalize(path).map_err(|error| RusqliteParityInfrastructureErrorV1::InvalidPath {
        field: FIELD,
        message: format!("could not canonicalize path: {error}"),
    })
}

fn canonical_staging_directory(
    path: &Path,
    field: &'static str,
) -> Result<PathBuf, RusqliteParityInfrastructureErrorV1> {
    validate_absolute_nontraversing_path(path, field)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            fs::canonicalize(path).map_err(|error| {
                RusqliteParityInfrastructureErrorV1::InvalidPath {
                    field,
                    message: format!("could not canonicalize path: {error}"),
                }
            })
        }
        Ok(_) => Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
            field,
            message: "path must name a directory and not a symlink".to_string(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            canonical_missing_directory(path, field)
        }
        Err(error) => Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
            field,
            message: format!("could not inspect path: {error}"),
        }),
    }
}

fn canonical_missing_directory(
    path: &Path,
    field: &'static str,
) -> Result<PathBuf, RusqliteParityInfrastructureErrorV1> {
    let mut cursor = path;
    let mut missing_components = Vec::<OsString>::new();
    loop {
        match fs::symlink_metadata(cursor) {
            Ok(metadata) => {
                if !metadata.file_type().is_dir() {
                    return Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
                        field,
                        message: "an ancestor of the staging root is not a directory".to_string(),
                    });
                }
                let mut canonical = fs::canonicalize(cursor).map_err(|error| {
                    RusqliteParityInfrastructureErrorV1::InvalidPath {
                        field,
                        message: format!("could not canonicalize staging ancestor: {error}"),
                    }
                })?;
                for component in missing_components.into_iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let component = cursor.file_name().ok_or_else(|| {
                    RusqliteParityInfrastructureErrorV1::InvalidPath {
                        field,
                        message: "path has no existing ancestor".to_string(),
                    }
                })?;
                missing_components.push(component.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    RusqliteParityInfrastructureErrorV1::InvalidPath {
                        field,
                        message: "path has no existing ancestor".to_string(),
                    }
                })?;
            }
            Err(error) => {
                return Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
                    field,
                    message: format!("could not inspect staging ancestor: {error}"),
                });
            }
        }
    }
}

fn canonical_existing_directory(
    path: &Path,
    field: &'static str,
) -> Result<PathBuf, RusqliteParityInfrastructureErrorV1> {
    validate_absolute_nontraversing_path(path, field)?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        RusqliteParityInfrastructureErrorV1::InvalidPath {
            field,
            message: format!("path must name an existing directory: {error}"),
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
            field,
            message: "path must name a directory and not a symlink".to_string(),
        });
    }
    fs::canonicalize(path).map_err(|error| RusqliteParityInfrastructureErrorV1::InvalidPath {
        field,
        message: format!("could not canonicalize path: {error}"),
    })
}

fn validate_absolute_nontraversing_path(
    path: &Path,
    field: &'static str,
) -> Result<(), RusqliteParityInfrastructureErrorV1> {
    if !path.is_absolute() {
        return Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
            field,
            message: "path must be absolute".to_string(),
        });
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(RusqliteParityInfrastructureErrorV1::InvalidPath {
            field,
            message: "path must not contain '.' or '..' components".to_string(),
        });
    }
    Ok(())
}

async fn materialize_single_file(
    snapshot: &crate::sqlite_read_snapshot::SnapshotDatabase,
    destination: &Path,
    deadline: MonotonicDeadline,
    cancellation: &CancellationToken,
) -> Result<(), RusqliteParityInfrastructureErrorV1> {
    let destination =
        destination
            .to_str()
            .ok_or_else(|| RusqliteParityInfrastructureErrorV1::InvalidPath {
                field: "copied snapshot",
                message: "path is not valid UTF-8".to_string(),
            })?;
    let connection = snapshot.connection();
    connection
        .execute_batch(QUERY_ONLY_OFF_SQL)
        .await
        .map_err(|error| RusqliteParityInfrastructureErrorV1::Snapshot {
            message: error.to_string(),
        })?;
    let copy = connection.execute(SNAPSHOT_SQL, crate::db::engine::params![destination]);
    tokio::pin!(copy);
    let interrupted = cancellation.cancelled();
    tokio::pin!(interrupted);
    let deadline_wait =
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.instant()));
    tokio::pin!(deadline_wait);
    let copy_result = tokio::select! {
        biased;
        () = &mut interrupted => {
            let () = connection.interrupt();
            let _ = (&mut copy).await;
            Err(RusqliteParityInfrastructureErrorV1::Cancelled)
        }
        () = &mut deadline_wait => {
            let () = connection.interrupt();
            let _ = (&mut copy).await;
            Err(RusqliteParityInfrastructureErrorV1::DeadlineExceeded)
        }
        result = &mut copy => result.map(|_| ()).map_err(|error| {
            RusqliteParityInfrastructureErrorV1::Snapshot {
                message: error.to_string(),
            }
        }),
    };
    let reseal_result = connection.execute_batch(QUERY_ONLY_ON_SQL).await;
    copy_result?;
    reseal_result.map_err(|error| RusqliteParityInfrastructureErrorV1::Snapshot {
        message: format!("could not restore query-only snapshot state: {error}"),
    })?;
    check_interruption(deadline, cancellation)?;
    validate_regular_file(Path::new(destination), "copied snapshot")
}

fn build_wire_request(
    request_id: String,
    command: CommandV1,
    copied_snapshot: &Path,
    invocation: &InvocationDirectory,
    store_identity: &StoreRuntimeBindingV1,
) -> Result<RequestV1, RusqliteParityInfrastructureErrorV1> {
    let request = RequestV1 {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        database: seal_copied_snapshot(copied_snapshot, invocation, store_identity)?,
        command,
    };
    validate_request(&request)
        .map_err(|error| RusqliteParityInfrastructureErrorV1::RequestRejected { error })?;
    Ok(request)
}

fn seal_copied_snapshot(
    copied_snapshot: &Path,
    invocation: &InvocationDirectory,
    store_identity: &StoreRuntimeBindingV1,
) -> Result<CopiedDatabaseV1, RusqliteParityInfrastructureErrorV1> {
    validate_regular_file(copied_snapshot, "copied snapshot")?;
    let staging_root =
        canonical_existing_directory(&invocation.path, "private parity staging root")?;
    let canonical_path = fs::canonicalize(copied_snapshot).map_err(|error| {
        RusqliteParityInfrastructureErrorV1::Snapshot {
            message: format!("could not canonicalize copied snapshot: {error}"),
        }
    })?;
    if !canonical_path.starts_with(&staging_root) {
        return Err(RusqliteParityInfrastructureErrorV1::Snapshot {
            message: "copied snapshot escaped its private staging directory".to_string(),
        });
    }
    let (byte_len, content_digest, file_identity) = snapshot_fingerprint(&canonical_path)?;
    let provenance = CopiedSnapshotProvenanceV1 {
        authority_identity: authority_identity(store_identity)?,
        staging_root,
        canonical_path: canonical_path.clone(),
        byte_len,
        content_digest,
        file_identity,
    };
    Ok(CopiedDatabaseV1 {
        path: canonical_path,
        kind: DatabaseKindV1::CopiedSnapshot,
        provenance,
    })
}

fn snapshot_fingerprint(
    path: &Path,
) -> Result<(u64, String, SnapshotFileIdentityV1), RusqliteParityInfrastructureErrorV1> {
    let mut file =
        fs::File::open(path).map_err(|error| RusqliteParityInfrastructureErrorV1::Snapshot {
            message: format!("could not open copied snapshot for sealing: {error}"),
        })?;
    let before =
        file.metadata()
            .map_err(|error| RusqliteParityInfrastructureErrorV1::Snapshot {
                message: format!("could not inspect copied snapshot for sealing: {error}"),
            })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            RusqliteParityInfrastructureErrorV1::Snapshot {
                message: format!("could not hash copied snapshot for sealing: {error}"),
            }
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let after =
        fs::metadata(path).map_err(|error| RusqliteParityInfrastructureErrorV1::Snapshot {
            message: format!("could not revalidate copied snapshot after sealing: {error}"),
        })?;
    let file_identity = SnapshotFileIdentityV1::from_metadata(&before);
    if before.len() != after.len() || file_identity != SnapshotFileIdentityV1::from_metadata(&after)
    {
        return Err(RusqliteParityInfrastructureErrorV1::Snapshot {
            message: "copied snapshot changed while it was sealed".to_string(),
        });
    }
    Ok((
        before.len(),
        format!("sha256:{}", hex::encode(hasher.finalize())),
        file_identity,
    ))
}

fn authority_identity(
    store_identity: &StoreRuntimeBindingV1,
) -> Result<String, RusqliteParityInfrastructureErrorV1> {
    let binding = serde_json::to_string(store_identity).map_err(|error| {
        RusqliteParityInfrastructureErrorV1::RequestEncoding {
            message: format!("could not encode StoreRuntimeBinding provenance: {error}"),
        }
    })?;
    Ok(format!("store-runtime-binding-v1:{binding}"))
}

async fn run_interruptible<T>(
    future: impl std::future::Future<Output = T>,
    deadline: MonotonicDeadline,
    cancellation: &CancellationToken,
) -> Result<T, RusqliteParityInfrastructureErrorV1> {
    tokio::pin!(future);
    let cancelled = cancellation.cancelled();
    tokio::pin!(cancelled);
    let deadline_wait =
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.instant()));
    tokio::pin!(deadline_wait);
    tokio::select! {
        biased;
        () = &mut cancelled => Err(RusqliteParityInfrastructureErrorV1::Cancelled),
        () = &mut deadline_wait => Err(RusqliteParityInfrastructureErrorV1::DeadlineExceeded),
        output = &mut future => Ok(output),
    }
}

fn check_interruption(
    deadline: MonotonicDeadline,
    cancellation: &CancellationToken,
) -> Result<(), RusqliteParityInfrastructureErrorV1> {
    if cancellation.is_cancelled() {
        Err(RusqliteParityInfrastructureErrorV1::Cancelled)
    } else if deadline.is_elapsed_at(Instant::now()) {
        Err(RusqliteParityInfrastructureErrorV1::DeadlineExceeded)
    } else {
        Ok(())
    }
}

struct InvocationDirectory {
    path: PathBuf,
}

impl InvocationDirectory {
    fn create(root: &Path) -> Result<Self, RusqliteParityInfrastructureErrorV1> {
        let root = canonical_existing_directory(root, "staging root")?;
        for _ in 0..100 {
            let id = NEXT_INVOCATION.fetch_add(1, Ordering::Relaxed);
            let candidate = root.join(format!("rusqlite-parity-{}-{id}", std::process::id()));
            #[cfg_attr(not(unix), allow(unused_mut))] // mode() is unix-only
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&candidate) {
                Ok(()) => {
                    let path = fs::canonicalize(&candidate).map_err(|error| {
                        RusqliteParityInfrastructureErrorV1::Snapshot {
                            message: format!("could not canonicalize parity staging: {error}"),
                        }
                    })?;
                    if !path.starts_with(&root) {
                        return Err(RusqliteParityInfrastructureErrorV1::Snapshot {
                            message: "private parity staging escaped its configured root"
                                .to_string(),
                        });
                    }
                    let invocation = Self { path };
                    for child in ["cwd", "home", "data", "tmp"] {
                        create_private_directory(&invocation.path.join(child)).map_err(
                            |error| RusqliteParityInfrastructureErrorV1::Snapshot {
                                message: format!("could not create parity staging: {error}"),
                            },
                        )?;
                    }
                    return Ok(invocation);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(RusqliteParityInfrastructureErrorV1::Snapshot {
                        message: format!("could not create parity staging: {error}"),
                    });
                }
            }
        }
        Err(RusqliteParityInfrastructureErrorV1::Snapshot {
            message: "could not allocate unique parity staging".to_string(),
        })
    }

    fn cleanup(self) -> io::Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(io::Error::other(format!(
                "refusing to remove replaced staging path '{}'",
                self.path.display()
            )));
        }
        fs::remove_dir_all(&self.path)
    }
}

impl Drop for InvocationDirectory {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink()
        }) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))] // mode() is unix-only
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[cfg_attr(not(unix), allow(unreachable_code))] // early UnsupportedPlatform return
async fn invoke_helper(
    helper_executable: &Path,
    invocation: &InvocationDirectory,
    request: &RequestV1,
    deadline: MonotonicDeadline,
    cancellation: &CancellationToken,
) -> Result<ResponseV1, RusqliteParityInfrastructureErrorV1> {
    #[cfg(not(unix))]
    {
        let _ = (
            helper_executable,
            invocation,
            request,
            deadline,
            cancellation,
        );
        return Err(RusqliteParityInfrastructureErrorV1::UnsupportedPlatform);
    }

    check_interruption(deadline, cancellation)?;
    validate_request(request)
        .map_err(|error| RusqliteParityInfrastructureErrorV1::RequestRejected { error })?;
    let mut request_bytes = serde_json::to_vec(request).map_err(|error| {
        RusqliteParityInfrastructureErrorV1::RequestEncoding {
            message: error.to_string(),
        }
    })?;
    request_bytes.push(b'\n');
    if u64::try_from(request_bytes.len()).unwrap_or(u64::MAX) > MAX_REQUEST_BYTES {
        return Err(RusqliteParityInfrastructureErrorV1::RequestRejected {
            error: ErrorPayloadV1::new(
                ErrorCodeV1::RequestTooLarge,
                format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
            ),
        });
    }

    let home = invocation.path.join("home");
    let data = invocation.path.join("data");
    let temp = invocation.path.join("tmp");
    let mut command = Command::new(helper_executable);
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .current_dir(invocation.path.join("cwd"))
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("TRACEDECAY_DATA_DIR", &data)
        .env("TRACEDECAY_GLOBAL_DB", data.join("global.db"))
        .env("TMPDIR", &temp)
        .env("TEMP", &temp)
        .env("TMP", &temp)
        .env("SQLITE_TMPDIR", &temp);
    configure_process_group(&mut command);

    let mut child =
        command
            .spawn()
            .map_err(|error| RusqliteParityInfrastructureErrorV1::Spawn {
                message: error.to_string(),
            })?;
    let process_group = match child_process_group(&child) {
        Ok(process_group) => process_group,
        Err(error) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(error);
        }
    };
    let stdin = if let Some(stdin) = child.stdin.take() {
        stdin
    } else {
        terminate_and_reap(&mut child, process_group).await;
        return Err(RusqliteParityInfrastructureErrorV1::Transport {
            stage: "stdin setup",
            message: "piped stdin was unavailable".to_string(),
        });
    };
    let stdout = if let Some(stdout) = child.stdout.take() {
        stdout
    } else {
        terminate_and_reap(&mut child, process_group).await;
        return Err(RusqliteParityInfrastructureErrorV1::Transport {
            stage: "stdout setup",
            message: "piped stdout was unavailable".to_string(),
        });
    };
    let stderr = if let Some(stderr) = child.stderr.take() {
        stderr
    } else {
        terminate_and_reap(&mut child, process_group).await;
        return Err(RusqliteParityInfrastructureErrorV1::Transport {
            stage: "stderr setup",
            message: "piped stderr was unavailable".to_string(),
        });
    };

    let mut stdin_task = Some(tokio::spawn(async move {
        let mut stdin = stdin;
        stdin.write_all(&request_bytes).await?;
        stdin.shutdown().await
    }));
    let mut stdout_task = Some(tokio::spawn(read_bounded(stdout, MAX_STDOUT_BYTES)));
    let mut stderr_task = Some(tokio::spawn(read_bounded(stderr, MAX_STDERR_BYTES)));
    let mut stdout_bytes = None;
    let mut stderr_bytes = None;
    let mut status = None;

    let monitored = loop {
        if cancellation.is_cancelled() {
            break Err(RusqliteParityInfrastructureErrorV1::Cancelled);
        }
        if deadline.is_elapsed_at(Instant::now()) {
            break Err(RusqliteParityInfrastructureErrorV1::DeadlineExceeded);
        }
        if stdin_task.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(task) = stdin_task.take()
            && let Err(error) = join_io_task(task, "stdin write").await
        {
            break Err(error);
        }
        if stdout_task.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(task) = stdout_task.take()
        {
            match join_output_task(task, "stdout", MAX_STDOUT_BYTES).await {
                Ok(bytes) => stdout_bytes = Some(bytes),
                Err(error) => break Err(error),
            }
        }
        if stderr_task.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(task) = stderr_task.take()
        {
            match join_output_task(task, "stderr", MAX_STDERR_BYTES).await {
                Ok(bytes) => stderr_bytes = Some(bytes),
                Err(error) => break Err(error),
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(observed) => status = observed,
                Err(error) => {
                    break Err(RusqliteParityInfrastructureErrorV1::Transport {
                        stage: "process wait",
                        message: error.to_string(),
                    });
                }
            }
        }
        if status.is_some()
            && stdin_task.is_none()
            && stdout_task.is_none()
            && stderr_task.is_none()
        {
            break Ok(());
        }
        tokio::time::sleep(PROCESS_POLL_INTERVAL).await;
    };

    if let Err(error) = monitored {
        terminate_and_reap(&mut child, process_group).await;
        abort_task(stdin_task);
        abort_task(stdout_task);
        abort_task(stderr_task);
        return Err(error);
    }

    let status = status.ok_or_else(|| RusqliteParityInfrastructureErrorV1::Transport {
        stage: "process wait",
        message: "completed process had no exit status".to_string(),
    })?;
    let stdout = stdout_bytes.ok_or_else(|| RusqliteParityInfrastructureErrorV1::Transport {
        stage: "stdout",
        message: "completed stdout reader had no result".to_string(),
    })?;
    let stderr = stderr_bytes.ok_or_else(|| RusqliteParityInfrastructureErrorV1::Transport {
        stage: "stderr",
        message: "completed stderr reader had no result".to_string(),
    })?;
    // The helper leader has exited, but descendants may still occupy its
    // daemon-owned process group after closing inherited stdio. Reap them on
    // successful and failed invocations alike.
    terminate_process_group(process_group);
    if !status.success() {
        return Err(RusqliteParityInfrastructureErrorV1::HelperExit {
            status: display_status(status),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        });
    }
    serde_json::from_slice(&stdout).map_err(|error| {
        RusqliteParityInfrastructureErrorV1::MalformedResponse {
            message: error.to_string(),
        }
    })
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // A zero process group creates a new group whose ID is the helper PID.
    // The daemon is therefore never a member of a group it later signals.
    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

fn child_process_group(child: &Child) -> Result<u32, RusqliteParityInfrastructureErrorV1> {
    child
        .id()
        .ok_or_else(|| RusqliteParityInfrastructureErrorV1::Transport {
            stage: "process group",
            message: "spawned helper did not expose a process identifier".to_string(),
        })
}

#[cfg(unix)]
fn terminate_process_group(process_group: u32) {
    const SIGKILL: i32 = 9;
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    let Ok(process_group) = i32::try_from(process_group) else {
        return;
    };
    // SAFETY: `process_group` came from a child started with `setpgid(0, 0)`.
    // A negative PID targets only that child process group, never this daemon.
    let _ = unsafe { kill(-process_group, SIGKILL) };
}

#[cfg(not(unix))]
fn terminate_process_group(_process_group: u32) {}

async fn terminate_and_reap(child: &mut Child, process_group: u32) {
    // Signal descendants before waiting for the group leader. This closes any
    // inherited stdio handles that otherwise keep bounded readers alive.
    terminate_process_group(process_group);
    let _ = child.start_kill();
    let _ = child.wait().await;
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> io::Result<BoundedOutput> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(BoundedOutput {
                bytes,
                exceeded: false,
            });
        }
        let remaining = limit.saturating_add(1).saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        if bytes.len() > limit {
            return Ok(BoundedOutput {
                bytes,
                exceeded: true,
            });
        }
    }
}

struct BoundedOutput {
    bytes: Vec<u8>,
    exceeded: bool,
}

async fn join_io_task(
    task: JoinHandle<io::Result<()>>,
    stage: &'static str,
) -> Result<(), RusqliteParityInfrastructureErrorV1> {
    task.await
        .map_err(|error| RusqliteParityInfrastructureErrorV1::Transport {
            stage,
            message: error.to_string(),
        })?
        .map_err(|error| RusqliteParityInfrastructureErrorV1::Transport {
            stage,
            message: error.to_string(),
        })
}

async fn join_output_task(
    task: JoinHandle<io::Result<BoundedOutput>>,
    stream: &'static str,
    limit: usize,
) -> Result<Vec<u8>, RusqliteParityInfrastructureErrorV1> {
    let output = task
        .await
        .map_err(|error| RusqliteParityInfrastructureErrorV1::Transport {
            stage: stream,
            message: error.to_string(),
        })?
        .map_err(|error| RusqliteParityInfrastructureErrorV1::Transport {
            stage: stream,
            message: error.to_string(),
        })?;
    if output.exceeded {
        Err(RusqliteParityInfrastructureErrorV1::OutputTooLarge { stream, limit })
    } else {
        Ok(output.bytes)
    }
}

fn abort_task<T>(task: Option<JoinHandle<T>>) {
    if let Some(task) = task {
        task.abort();
    }
}

fn display_status(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by signal".to_string(),
        |code| code.to_string(),
    )
}

fn validate_response(
    response: ResponseV1,
    request: &RequestV1,
) -> Result<OutputV1, RusqliteParityInfrastructureErrorV1> {
    let ResponseV1 {
        protocol_version,
        request_id,
        verified_snapshot,
        outcome,
    } = response;
    if protocol_version != PROTOCOL_VERSION {
        return Err(
            RusqliteParityInfrastructureErrorV1::ProtocolVersionMismatch {
                expected: PROTOCOL_VERSION,
                actual: protocol_version,
            },
        );
    }
    if request_id.as_deref() != Some(request.request_id.as_str()) {
        return Err(RusqliteParityInfrastructureErrorV1::ResponseIdentityMismatch);
    }
    let expected_snapshot = VerifiedCopiedSnapshotV1 {
        authority_identity: request.database.provenance.authority_identity.clone(),
        canonical_path: request.database.provenance.canonical_path.clone(),
        byte_len: request.database.provenance.byte_len,
        content_digest: request.database.provenance.content_digest.clone(),
        file_identity: request.database.provenance.file_identity.clone(),
    };
    if verified_snapshot.as_ref() != Some(&expected_snapshot) {
        return Err(
            RusqliteParityInfrastructureErrorV1::ResponseSnapshotMismatch {
                expected: Box::new(expected_snapshot),
                actual: verified_snapshot.map(Box::new),
            },
        );
    }
    match outcome {
        ResponseOutcomeV1::Ok { output } => {
            if !response_matches_command(
                &output,
                &request.command,
                &request.database.provenance.canonical_path,
            ) {
                return Err(RusqliteParityInfrastructureErrorV1::ResponseOperationMismatch);
            }
            Ok(output)
        }
        ResponseOutcomeV1::Error { error } => {
            Err(RusqliteParityInfrastructureErrorV1::HelperRejected { error })
        }
    }
}

fn response_matches_command(
    output: &OutputV1,
    command: &CommandV1,
    expected_snapshot_path: &Path,
) -> bool {
    match (output, command) {
        (OutputV1::Metadata(metadata), CommandV1::Metadata) => {
            metadata.canonical_path.as_path() == expected_snapshot_path
                && metadata.query_only
                && metadata.immutable
        }
        (OutputV1::Schema(_), CommandV1::Schema)
        | (OutputV1::ForeignKeys { .. }, CommandV1::ForeignKeys)
        | (OutputV1::PageSize { .. }, CommandV1::PageSize)
        | (OutputV1::JournalMode(_), CommandV1::JournalMode) => true,
        (OutputV1::Integrity(report), CommandV1::Integrity { check }) => report.check == *check,
        (OutputV1::SessionStoreCount(result), CommandV1::SessionStoreCount { family, table }) => {
            result.family == *family && result.table == *table
        }
        (OutputV1::SessionStoreSchema(result), CommandV1::SessionStoreSchema { family, table }) => {
            result.family == *family
                && result.table == *table
                && (result.exists || (result.columns.is_empty() && result.foreign_keys.is_empty()))
        }
        (
            OutputV1::SessionStorePage(result),
            CommandV1::SessionStorePage {
                family,
                table,
                limit,
                ..
            },
        ) => session_store_page_matches(result, *family, *table, *limit),
        _ => false,
    }
}

fn session_store_page_matches(
    page: &SessionStorePageV1,
    family: SessionStoreFamilyV1,
    table: SessionStoreTableV1,
    limit: u16,
) -> bool {
    page.family == family
        && page.table == table
        && page.digest_algorithm == ROW_DIGEST_ALGORITHM
        && page
            .order_columns
            .iter()
            .map(String::as_str)
            .eq(table.order_columns().iter().copied())
        && page.rows.len() <= usize::from(limit)
        && page
            .rows
            .iter()
            .all(|row| session_store_row_matches(table, row))
        && page
            .next_cursor
            .as_ref()
            .is_none_or(|cursor| session_store_cursor_matches(table, cursor))
}

fn session_store_row_matches(table: SessionStoreTableV1, row: &SessionStoreRowV1) -> bool {
    // Exhaustive by construction: deriving the expected table from the row
    // variant (no wildcard arm) forces every new protocol row to be listed
    // here or the crate fails to compile, closing the silent-drift gap that
    // a `matches!` shape list leaves open.
    let expected = match row {
        SessionStoreRowV1::Observations { .. } => SessionStoreTableV1::Observations,
        SessionStoreRowV1::SourceCursors { .. } => SessionStoreTableV1::SourceCursors,
        SessionStoreRowV1::Sessions { .. } => SessionStoreTableV1::Sessions,
        SessionStoreRowV1::SessionMessages { .. } => SessionStoreTableV1::SessionMessages,
        SessionStoreRowV1::SessionSchemaMigrations { .. } => {
            SessionStoreTableV1::SessionSchemaMigrations
        }
        SessionStoreRowV1::LcmRawMessages { .. } => SessionStoreTableV1::LcmRawMessages,
        SessionStoreRowV1::SessionTemporalSchemaMigrations { .. } => {
            SessionStoreTableV1::SessionTemporalSchemaMigrations
        }
        SessionStoreRowV1::SessionTemporalGenerations { .. } => {
            SessionStoreTableV1::SessionTemporalGenerations
        }
        SessionStoreRowV1::SessionTemporalObservationEffects { .. } => {
            SessionStoreTableV1::SessionTemporalObservationEffects
        }
        SessionStoreRowV1::SessionTemporalProjectionReceipts { .. } => {
            SessionStoreTableV1::SessionTemporalProjectionReceipts
        }
        SessionStoreRowV1::SessionOccurrences { .. } => SessionStoreTableV1::SessionOccurrences,
        SessionStoreRowV1::SessionLogicalCopyEdges { .. } => {
            SessionStoreTableV1::SessionLogicalCopyEdges
        }
        SessionStoreRowV1::SessionAssertions { .. } => SessionStoreTableV1::SessionAssertions,
        SessionStoreRowV1::SessionSummaryNodes { .. } => SessionStoreTableV1::SessionSummaryNodes,
        SessionStoreRowV1::SessionSummarySources { .. } => {
            SessionStoreTableV1::SessionSummarySources
        }
        SessionStoreRowV1::SessionSummarySuccessors { .. } => {
            SessionStoreTableV1::SessionSummarySuccessors
        }
        SessionStoreRowV1::MemoryV2Facts { .. } => SessionStoreTableV1::MemoryV2Facts,
        SessionStoreRowV1::MemoryV2CurrentFacts { .. } => SessionStoreTableV1::MemoryV2CurrentFacts,
        SessionStoreRowV1::MemoryV2Assertions { .. } => SessionStoreTableV1::MemoryV2Assertions,
        SessionStoreRowV1::MemoryV2LineageEvents { .. } => {
            SessionStoreTableV1::MemoryV2LineageEvents
        }
        SessionStoreRowV1::RetrievalAnchors { .. } => SessionStoreTableV1::RetrievalAnchors,
        SessionStoreRowV1::GenerationDiagnostics { .. } => {
            SessionStoreTableV1::GenerationDiagnostics
        }
        SessionStoreRowV1::DiagnosticGenerationPublications { .. } => {
            SessionStoreTableV1::DiagnosticGenerationPublications
        }
        SessionStoreRowV1::ConfigurationRevisions { .. } => {
            SessionStoreTableV1::ConfigurationRevisions
        }
        SessionStoreRowV1::ConfigurationEntries { .. } => SessionStoreTableV1::ConfigurationEntries,
        SessionStoreRowV1::ConfigurationMutationReceipts { .. } => {
            SessionStoreTableV1::ConfigurationMutationReceipts
        }
        SessionStoreRowV1::ConfigurationAuditEvents { .. } => {
            SessionStoreTableV1::ConfigurationAuditEvents
        }
    };
    table == expected && is_canonical_sha256_digest(session_store_row_digest(row))
}

fn session_store_row_digest(row: &SessionStoreRowV1) -> &str {
    match row {
        SessionStoreRowV1::Observations { row_digest, .. }
        | SessionStoreRowV1::SourceCursors { row_digest, .. }
        | SessionStoreRowV1::Sessions { row_digest, .. }
        | SessionStoreRowV1::SessionMessages { row_digest, .. }
        | SessionStoreRowV1::SessionSchemaMigrations { row_digest, .. }
        | SessionStoreRowV1::LcmRawMessages { row_digest, .. }
        | SessionStoreRowV1::SessionTemporalSchemaMigrations { row_digest, .. }
        | SessionStoreRowV1::SessionTemporalGenerations { row_digest, .. }
        | SessionStoreRowV1::SessionTemporalObservationEffects { row_digest, .. }
        | SessionStoreRowV1::SessionTemporalProjectionReceipts { row_digest, .. }
        | SessionStoreRowV1::SessionOccurrences { row_digest, .. }
        | SessionStoreRowV1::SessionLogicalCopyEdges { row_digest, .. }
        | SessionStoreRowV1::SessionAssertions { row_digest, .. }
        | SessionStoreRowV1::SessionSummaryNodes { row_digest, .. }
        | SessionStoreRowV1::SessionSummarySources { row_digest, .. }
        | SessionStoreRowV1::SessionSummarySuccessors { row_digest, .. }
        | SessionStoreRowV1::MemoryV2Facts { row_digest, .. }
        | SessionStoreRowV1::MemoryV2CurrentFacts { row_digest, .. }
        | SessionStoreRowV1::MemoryV2Assertions { row_digest, .. }
        | SessionStoreRowV1::MemoryV2LineageEvents { row_digest, .. }
        | SessionStoreRowV1::RetrievalAnchors { row_digest, .. }
        | SessionStoreRowV1::GenerationDiagnostics { row_digest, .. }
        | SessionStoreRowV1::DiagnosticGenerationPublications { row_digest, .. }
        | SessionStoreRowV1::ConfigurationRevisions { row_digest, .. }
        | SessionStoreRowV1::ConfigurationEntries { row_digest, .. }
        | SessionStoreRowV1::ConfigurationMutationReceipts { row_digest, .. }
        | SessionStoreRowV1::ConfigurationAuditEvents { row_digest, .. } => row_digest,
    }
}

fn session_store_cursor_matches(table: SessionStoreTableV1, cursor: &SessionStoreCursorV1) -> bool {
    // Exhaustive by construction (see `session_store_row_matches`): a new
    // cursor variant must be listed here or the crate fails to compile.
    let expected = match cursor {
        SessionStoreCursorV1::Observations { .. } => SessionStoreTableV1::Observations,
        SessionStoreCursorV1::SourceCursors { .. } => SessionStoreTableV1::SourceCursors,
        SessionStoreCursorV1::Sessions { .. } => SessionStoreTableV1::Sessions,
        SessionStoreCursorV1::SessionMessages { .. } => SessionStoreTableV1::SessionMessages,
        SessionStoreCursorV1::SessionSchemaMigrations { .. } => {
            SessionStoreTableV1::SessionSchemaMigrations
        }
        SessionStoreCursorV1::LcmRawMessages { .. } => SessionStoreTableV1::LcmRawMessages,
        SessionStoreCursorV1::SessionTemporalSchemaMigrations { .. } => {
            SessionStoreTableV1::SessionTemporalSchemaMigrations
        }
        SessionStoreCursorV1::SessionTemporalGenerations { .. } => {
            SessionStoreTableV1::SessionTemporalGenerations
        }
        SessionStoreCursorV1::SessionTemporalObservationEffects { .. } => {
            SessionStoreTableV1::SessionTemporalObservationEffects
        }
        SessionStoreCursorV1::SessionTemporalProjectionReceipts { .. } => {
            SessionStoreTableV1::SessionTemporalProjectionReceipts
        }
        SessionStoreCursorV1::SessionOccurrences { .. } => SessionStoreTableV1::SessionOccurrences,
        SessionStoreCursorV1::SessionLogicalCopyEdges { .. } => {
            SessionStoreTableV1::SessionLogicalCopyEdges
        }
        SessionStoreCursorV1::SessionAssertions { .. } => SessionStoreTableV1::SessionAssertions,
        SessionStoreCursorV1::SessionSummaryNodes { .. } => {
            SessionStoreTableV1::SessionSummaryNodes
        }
        SessionStoreCursorV1::SessionSummarySources { .. } => {
            SessionStoreTableV1::SessionSummarySources
        }
        SessionStoreCursorV1::SessionSummarySuccessors { .. } => {
            SessionStoreTableV1::SessionSummarySuccessors
        }
        SessionStoreCursorV1::MemoryV2Facts { .. } => SessionStoreTableV1::MemoryV2Facts,
        SessionStoreCursorV1::MemoryV2CurrentFacts { .. } => {
            SessionStoreTableV1::MemoryV2CurrentFacts
        }
        SessionStoreCursorV1::MemoryV2Assertions { .. } => SessionStoreTableV1::MemoryV2Assertions,
        SessionStoreCursorV1::MemoryV2LineageEvents { .. } => {
            SessionStoreTableV1::MemoryV2LineageEvents
        }
        SessionStoreCursorV1::RetrievalAnchors { .. } => SessionStoreTableV1::RetrievalAnchors,
        SessionStoreCursorV1::GenerationDiagnostics { .. } => {
            SessionStoreTableV1::GenerationDiagnostics
        }
        SessionStoreCursorV1::DiagnosticGenerationPublications { .. } => {
            SessionStoreTableV1::DiagnosticGenerationPublications
        }
        SessionStoreCursorV1::ConfigurationRevisions { .. } => {
            SessionStoreTableV1::ConfigurationRevisions
        }
        SessionStoreCursorV1::ConfigurationEntries { .. } => {
            SessionStoreTableV1::ConfigurationEntries
        }
        SessionStoreCursorV1::ConfigurationMutationReceipts { .. } => {
            SessionStoreTableV1::ConfigurationMutationReceipts
        }
        SessionStoreCursorV1::ConfigurationAuditEvents { .. } => {
            SessionStoreTableV1::ConfigurationAuditEvents
        }
    };
    table == expected
}

#[cfg(all(test, unix))]
mod tests {
    use std::fmt::Debug;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;
    use tracedecay_sqlite_parity_protocol::{
        EffectiveJournalModeV1, IntegrityCheckV1, IntegrityReportV1, JournalModeMetadataV1,
        JournalModeNormalizationV1, MetadataV1, SchemaMetadataV1, SchemaObjectKindV1,
        SchemaObjectV1, SessionStoreCountV1, SessionStoreSchemaV1, SourceHeaderJournalModeV1,
        SourceJournalModeV1,
    };
    use tracedecay_store::{
        BrainId, ProjectId, StoreAuthorityEpochV1, StoreIncarnationV1, StoreShardIdV1,
        UserProfileId,
    };

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(value.to_owned()).expect("fixture id")
    }

    fn store_identity() -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                id::<BrainId>("brain.parity"),
                id::<UserProfileId>("profile.parity"),
                id::<ProjectId>("project.parity"),
            ),
            StoreIncarnationV1::new(1).unwrap(),
            StoreAuthorityEpochV1::new(1).unwrap(),
        )
    }

    struct FixtureStore {
        path: PathBuf,
        _connection: crate::db::engine::TestConnection,
    }

    async fn fixture_store(root: &Path) -> FixtureStore {
        std::fs::create_dir_all(root).unwrap();
        let path = root.join("authority.db");
        let connection = crate::db::engine::TestConnection::open(&path);
        connection
            .execute_batch(
                "CREATE TABLE parity_fixture(value TEXT NOT NULL);
                 INSERT INTO parity_fixture VALUES ('wal-resident');",
            )
            .await
            .unwrap();
        assert!(
            std::fs::metadata(format!("{}-wal", path.display()))
                .unwrap()
                .len()
                > 0
        );
        FixtureStore {
            path,
            _connection: connection,
        }
    }

    fn roots(temp: &TempDir) -> (PathBuf, PathBuf) {
        let live = temp.path().join("live");
        let staging = temp.path().join("staging");
        std::fs::create_dir(&live).unwrap();
        std::fs::create_dir(&staging).unwrap();
        (live, staging)
    }

    fn helper(root: &Path, body: &str) -> PathBuf {
        let path = root.join(format!(
            "helper-{}-{}",
            std::process::id(),
            NEXT_INVOCATION.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }

    fn fake_response_helper(
        root: &Path,
        output: &OutputV1,
        identity: &StoreRuntimeBindingV1,
    ) -> PathBuf {
        fake_response_helper_with_setup(root, output, identity, "")
    }

    fn fake_response_helper_with_setup(
        root: &Path,
        output: &OutputV1,
        identity: &StoreRuntimeBindingV1,
        before_response: &str,
    ) -> PathBuf {
        let output = serde_json::to_string(output).unwrap();
        let authority = serde_json::to_string(&authority_identity(identity).unwrap()).unwrap();
        let body = r#"IFS= read -r request
id=${request#*\"request_id\":\"}
id=${id%%\"*}
path=${request#*\"path\":\"}
path=${path%%\"*}
digest=${request#*\"content_digest\":\"}
digest=${digest%%\"*}
[ -f "$path" ]
[ ! -e "${path}-wal" ]
[ ! -e "${path}-shm" ]
/usr/bin/grep -aq 'wal-resident' "$path"
set -- $(/usr/bin/stat -c '%s %d %i %h' "$path")
byte_len=$1
device=$2
inode=$3
links=$4
__BEFORE_RESPONSE__
printf '{"protocol_version":1,"request_id":"%s","verified_snapshot":{"authority_identity":%s,"canonical_path":"%s","byte_len":%s,"content_digest":"%s","file_identity":{"platform":"unix","device":%s,"inode":%s,"links":%s}},"status":"ok","output":%s}\n' "$id" __AUTHORITY__ "$path" "$byte_len" "$digest" "$device" "$inode" "$links" __OUTPUT__"#
            .replace("__AUTHORITY__", &shell_quote(&authority))
            .replace("__BEFORE_RESPONSE__", before_response)
            .replace("__OUTPUT__", &shell_quote(&output));
        helper(root, &body)
    }

    fn deadline() -> MonotonicDeadline {
        MonotonicDeadline::at(Instant::now() + Duration::from_secs(5))
    }

    #[allow(clippy::too_many_arguments)]
    async fn invoke(
        helper: &Path,
        authority: &Path,
        staging: &Path,
        known_live_roots: &[PathBuf],
        command: CommandV1,
        deadline: MonotonicDeadline,
        cancellation: &CancellationToken,
    ) -> Result<RusqliteParityResultV1, RusqliteParityInfrastructureErrorV1> {
        let identity = store_identity();
        run_rusqlite_parity_v1(
            helper,
            authority,
            staging,
            known_live_roots,
            RusqliteParityRequestV1::new(identity.clone(), command),
            deadline,
            cancellation,
            &identity,
        )
        .await
    }

    fn assert_invocations_cleaned(staging: &Path) {
        let leftovers = std::fs::read_dir(staging)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("rusqlite-parity-"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
    }

    fn journal_output() -> OutputV1 {
        OutputV1::JournalMode(JournalModeMetadataV1 {
            source_header: SourceHeaderJournalModeV1 {
                read_version: 2,
                write_version: 2,
                mode: SourceJournalModeV1::Wal,
            },
            mode: EffectiveJournalModeV1::Delete,
            immutable_effective_mode: EffectiveJournalModeV1::Delete,
            normalization: JournalModeNormalizationV1::WalSourceImmutableDelete,
        })
    }

    fn session_page_output() -> OutputV1 {
        OutputV1::SessionStorePage(SessionStorePageV1 {
            family: SessionStoreFamilyV1::Observation,
            table: SessionStoreTableV1::Observations,
            order_columns: vec!["sequence".to_owned()],
            digest_algorithm: ROW_DIGEST_ALGORITHM.to_owned(),
            rows: vec![SessionStoreRowV1::Observations {
                sequence: 1,
                observation_id: "observation-1".to_owned(),
                payload_digest: "payload-1".to_owned(),
                row_digest: format!("sha256:{}", "1".repeat(64)),
            }],
            next_cursor: Some(SessionStoreCursorV1::Observations { sequence: 1 }),
        })
    }

    #[test]
    fn session_store_shape_helpers_accept_new_temporal_and_summary_families() {
        let digest = format!("sha256:{}", "1".repeat(64));
        let cases: [(SessionStoreTableV1, SessionStoreRowV1, SessionStoreCursorV1); 17] = [
            (
                SessionStoreTableV1::SessionTemporalProjectionReceipts,
                SessionStoreRowV1::SessionTemporalProjectionReceipts {
                    session_id: "session-1".to_owned(),
                    generation: 1,
                    batch_ordinal: 0,
                    batch_digest: "batch-1".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::SessionTemporalProjectionReceipts {
                    session_id: "session-1".to_owned(),
                    generation: 1,
                    batch_ordinal: 0,
                },
            ),
            (
                SessionStoreTableV1::SessionOccurrences,
                SessionStoreRowV1::SessionOccurrences {
                    session_id: "session-1".to_owned(),
                    generation: 1,
                    occurrence_id: "occurrence-1".to_owned(),
                    role: "user".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::SessionOccurrences {
                    session_id: "session-1".to_owned(),
                    generation: 1,
                    occurrence_id: "occurrence-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::SessionLogicalCopyEdges,
                SessionStoreRowV1::SessionLogicalCopyEdges {
                    session_id: "session-1".to_owned(),
                    generation: 1,
                    occurrence_id: "occurrence-1".to_owned(),
                    copied_from_occurrence_id: "occurrence-0".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::SessionLogicalCopyEdges {
                    session_id: "session-1".to_owned(),
                    generation: 1,
                    occurrence_id: "occurrence-1".to_owned(),
                    copied_from_occurrence_id: "occurrence-0".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::SessionAssertions,
                SessionStoreRowV1::SessionAssertions {
                    session_id: "session-1".to_owned(),
                    generation: 1,
                    assertion_id: "assertion-1".to_owned(),
                    assertion_kind: "fact".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::SessionAssertions {
                    session_id: "session-1".to_owned(),
                    generation: 1,
                    assertion_id: "assertion-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::SessionSummaryNodes,
                SessionStoreRowV1::SessionSummaryNodes {
                    summary_id: "summary-1".to_owned(),
                    session_id: "session-1".to_owned(),
                    summary_anchor_id: "anchor-1".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::SessionSummaryNodes {
                    summary_id: "summary-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::SourceCursors,
                SessionStoreRowV1::SourceCursors {
                    source_json: "{\"source\":\"s\"}".to_owned(),
                    scope_json: "{\"scope\":\"p\"}".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::SourceCursors {
                    source_json: "{\"source\":\"s\"}".to_owned(),
                    scope_json: "{\"scope\":\"p\"}".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::MemoryV2Facts,
                SessionStoreRowV1::MemoryV2Facts {
                    fact_id: "fact-1".to_owned(),
                    owner_kind: "user".to_owned(),
                    project_id: "project-1".to_owned(),
                    identity_json: "{\"k\":\"v\"}".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::MemoryV2Facts {
                    fact_id: "fact-1".to_owned(),
                    owner_kind: "user".to_owned(),
                    project_id: "project-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::MemoryV2CurrentFacts,
                SessionStoreRowV1::MemoryV2CurrentFacts {
                    fact_id: "fact-1".to_owned(),
                    owner_kind: "user".to_owned(),
                    project_id: "project-1".to_owned(),
                    payload_access: "public".to_owned(),
                    projection_state: "active".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::MemoryV2CurrentFacts {
                    fact_id: "fact-1".to_owned(),
                    owner_kind: "user".to_owned(),
                    project_id: "project-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::MemoryV2Assertions,
                SessionStoreRowV1::MemoryV2Assertions {
                    assertion_id: "assertion-1".to_owned(),
                    fact_id: "fact-1".to_owned(),
                    owner_kind: "user".to_owned(),
                    project_id: "project-1".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::MemoryV2Assertions {
                    assertion_id: "assertion-1".to_owned(),
                    fact_id: "fact-1".to_owned(),
                    owner_kind: "user".to_owned(),
                    project_id: "project-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::MemoryV2LineageEvents,
                SessionStoreRowV1::MemoryV2LineageEvents {
                    event_sequence: 1,
                    event_id: "event-1".to_owned(),
                    fact_id: "fact-1".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::MemoryV2LineageEvents { event_sequence: 1 },
            ),
            (
                SessionStoreTableV1::RetrievalAnchors,
                SessionStoreRowV1::RetrievalAnchors {
                    anchor_id: "anchor-1".to_owned(),
                    projection_generation: "1".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::RetrievalAnchors {
                    anchor_id: "anchor-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::GenerationDiagnostics,
                SessionStoreRowV1::GenerationDiagnostics {
                    diagnostic_anchor: "anchor-1".to_owned(),
                    generation_id: "gen-1".to_owned(),
                    severity: "error".to_owned(),
                    record_state: "active".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::GenerationDiagnostics {
                    diagnostic_anchor: "anchor-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::DiagnosticGenerationPublications,
                SessionStoreRowV1::DiagnosticGenerationPublications {
                    generation_id: "gen-1".to_owned(),
                    record_state: "active".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::DiagnosticGenerationPublications {
                    generation_id: "gen-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::ConfigurationRevisions,
                SessionStoreRowV1::ConfigurationRevisions {
                    revision_id: "rev-1".to_owned(),
                    snapshot_id: "snap-1".to_owned(),
                    operation_kind: "set".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::ConfigurationRevisions {
                    revision_id: "rev-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::ConfigurationEntries,
                SessionStoreRowV1::ConfigurationEntries {
                    revision_id: "rev-1".to_owned(),
                    key: "key-1".to_owned(),
                    layer_kind: "user".to_owned(),
                    layer_id: "layer-1".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::ConfigurationEntries {
                    revision_id: "rev-1".to_owned(),
                    key: "key-1".to_owned(),
                    layer_kind: "user".to_owned(),
                    layer_id: "layer-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::ConfigurationMutationReceipts,
                SessionStoreRowV1::ConfigurationMutationReceipts {
                    receipt_id: "receipt-1".to_owned(),
                    result_revision_id: "rev-1".to_owned(),
                    activation_status: "activated".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::ConfigurationMutationReceipts {
                    receipt_id: "receipt-1".to_owned(),
                },
            ),
            (
                SessionStoreTableV1::ConfigurationAuditEvents,
                SessionStoreRowV1::ConfigurationAuditEvents {
                    event_id: "event-1".to_owned(),
                    operation_kind: "set".to_owned(),
                    base_revision_id: "rev-0".to_owned(),
                    row_digest: digest.clone(),
                },
                SessionStoreCursorV1::ConfigurationAuditEvents {
                    event_id: "event-1".to_owned(),
                },
            ),
        ];
        for (table, row, cursor) in &cases {
            assert!(
                session_store_row_matches(*table, row),
                "row shape must match for {table:?}"
            );
            assert!(
                session_store_cursor_matches(*table, cursor),
                "cursor shape must match for {table:?}"
            );
        }

        // A row/cursor whose variant disagrees with the declared table must be
        // rejected: the summary row is not a projection-receipts table row.
        let (_, summary_row, summary_cursor) = &cases[4];
        assert!(
            !session_store_row_matches(
                SessionStoreTableV1::SessionTemporalProjectionReceipts,
                summary_row
            ),
            "mismatched table+row must not match"
        );
        assert!(
            !session_store_cursor_matches(
                SessionStoreTableV1::SessionTemporalProjectionReceipts,
                summary_cursor
            ),
            "mismatched table+cursor must not match"
        );
    }

    #[tokio::test]
    async fn fake_helper_binds_shared_journal_and_session_outputs_to_the_snapshot() {
        let temp = TempDir::new().unwrap();
        let (live, staging) = roots(&temp);
        let authority = fixture_store(&live).await;
        let identity = store_identity();
        let known_live_roots = vec![live.clone()];
        let journal = journal_output();
        let journal_helper = fake_response_helper(temp.path(), &journal, &identity);

        let journal_result = invoke(
            &journal_helper,
            &authority.path,
            &staging,
            &known_live_roots,
            CommandV1::JournalMode,
            deadline(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(journal_result.store_identity(), &identity);
        assert_eq!(journal_result.output(), &journal);

        let session = session_page_output();
        let session_helper = fake_response_helper(temp.path(), &session, &identity);
        let session_result = invoke(
            &session_helper,
            &authority.path,
            &staging,
            &known_live_roots,
            CommandV1::SessionStorePage {
                family: SessionStoreFamilyV1::Observation,
                table: SessionStoreTableV1::Observations,
                cursor: None,
                limit: 1,
            },
            deadline(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(session_result.output(), &session);
        assert_invocations_cleaned(&staging);
    }

    #[tokio::test]
    async fn malformed_helper_output_fails_closed_and_cleans_staging() {
        let temp = TempDir::new().unwrap();
        let (live, staging) = roots(&temp);
        let authority = fixture_store(&live).await;
        let helper = helper(temp.path(), "IFS= read -r request\nprintf 'not-json\\n'");
        let known_live_roots = vec![live];

        let error = invoke(
            &helper,
            &authority.path,
            &staging,
            &known_live_roots,
            CommandV1::PageSize,
            deadline(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            RusqliteParityInfrastructureErrorV1::MalformedResponse { .. }
        ));
        assert_invocations_cleaned(&staging);
    }

    #[tokio::test]
    async fn oversized_helper_output_kills_the_group_and_cleans_staging() {
        let temp = TempDir::new().unwrap();
        let (live, staging) = roots(&temp);
        let authority = fixture_store(&live).await;
        let helper = helper(
            temp.path(),
            &format!(
                "IFS= read -r request\n/usr/bin/head -c {} /dev/zero",
                MAX_STDOUT_BYTES + 1
            ),
        );
        let known_live_roots = vec![live];

        let error = invoke(
            &helper,
            &authority.path,
            &staging,
            &known_live_roots,
            CommandV1::PageSize,
            deadline(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            RusqliteParityInfrastructureErrorV1::OutputTooLarge {
                stream: "stdout",
                limit: MAX_STDOUT_BYTES
            }
        ));
        assert_invocations_cleaned(&staging);
    }

    #[tokio::test]
    async fn oversized_helper_stderr_is_bounded_and_cleans_staging() {
        let temp = TempDir::new().unwrap();
        let (live, staging) = roots(&temp);
        let authority = fixture_store(&live).await;
        let helper = helper(
            temp.path(),
            &format!(
                "IFS= read -r request\n/usr/bin/head -c {} /dev/zero >&2",
                MAX_STDERR_BYTES + 1
            ),
        );

        let error = invoke(
            &helper,
            &authority.path,
            &staging,
            &[live],
            CommandV1::PageSize,
            deadline(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            RusqliteParityInfrastructureErrorV1::OutputTooLarge {
                stream: "stderr",
                limit: MAX_STDERR_BYTES
            }
        ));
        assert_invocations_cleaned(&staging);
    }

    #[tokio::test]
    async fn deadline_kills_and_reaps_helper_and_cleans_staging() {
        let temp = TempDir::new().unwrap();
        let (live, staging) = roots(&temp);
        let authority = fixture_store(&live).await;
        let helper = helper(temp.path(), "IFS= read -r request\nwhile :; do :; done");
        let known_live_roots = vec![live];

        let error = invoke(
            &helper,
            &authority.path,
            &staging,
            &known_live_roots,
            CommandV1::PageSize,
            MonotonicDeadline::at(Instant::now() + Duration::from_millis(50)),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            RusqliteParityInfrastructureErrorV1::DeadlineExceeded
        ));
        assert_invocations_cleaned(&staging);
    }

    fn descendant_helper(root: &Path, pid_file: &Path) -> PathBuf {
        helper(
            root,
            &format!(
                "IFS= read -r request\n(while :; do :; done) &\nprintf '%s\\n' \"$!\" > {}\nwhile :; do :; done",
                shell_quote(&pid_file.display().to_string())
            ),
        )
    }

    async fn cancel_after_pid_file(pid_file: &Path, cancellation: Arc<CancellationToken>) {
        for _ in 0..100 {
            if pid_file.exists() {
                cancellation.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("fake helper did not publish its descendant PID");
    }

    fn process_exists(pid: u32) -> bool {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        // SAFETY: signal zero only checks the process captured from our fake helper.
        unsafe { kill(pid, 0) == 0 }
    }

    async fn assert_process_exited(pid: u32) {
        for _ in 0..100 {
            if !process_exists(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("helper descendant {pid} survived process-group termination");
    }

    #[tokio::test]
    async fn cancellation_kills_the_entire_helper_process_group_and_cleans_staging() {
        let temp = TempDir::new().unwrap();
        let (live, staging) = roots(&temp);
        let authority = fixture_store(&live).await;
        let pid_file = temp.path().join("descendant.pid");
        let helper = descendant_helper(temp.path(), &pid_file);
        let known_live_roots = vec![live];
        let cancellation = Arc::new(CancellationToken::new());
        let wait_for_descendant = cancel_after_pid_file(&pid_file, Arc::clone(&cancellation));
        let operation = invoke(
            &helper,
            &authority.path,
            &staging,
            &known_live_roots,
            CommandV1::PageSize,
            deadline(),
            cancellation.as_ref(),
        );
        let (result, ()) = tokio::join!(operation, wait_for_descendant);
        let error = result.unwrap_err();

        assert!(matches!(
            error,
            RusqliteParityInfrastructureErrorV1::Cancelled
        ));
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_process_exited(pid).await;
        assert_invocations_cleaned(&staging);
    }

    #[tokio::test]
    async fn successful_helper_also_reaps_process_group_descendants() {
        let temp = TempDir::new().unwrap();
        let (live, staging) = roots(&temp);
        let authority = fixture_store(&live).await;
        let pid_file = temp.path().join("successful-descendant.pid");
        let identity = store_identity();
        let setup = format!(
            "(exec >/dev/null 2>&1; while :; do :; done) &\nprintf '%s\\n' \"$!\" > {}",
            shell_quote(&pid_file.display().to_string())
        );
        let helper = fake_response_helper_with_setup(
            temp.path(),
            &OutputV1::PageSize { bytes: 4096 },
            &identity,
            &setup,
        );

        let result = invoke(
            &helper,
            &authority.path,
            &staging,
            &[live],
            CommandV1::PageSize,
            deadline(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.output(), &OutputV1::PageSize { bytes: 4096 });
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_process_exited(pid).await;
        assert_invocations_cleaned(&staging);
    }

    #[tokio::test]
    async fn staging_overlap_with_a_caller_supplied_live_root_is_refused() {
        let temp = TempDir::new().unwrap();
        let (live, _staging) = roots(&temp);
        let authority = fixture_store(&live).await;
        let illegal_staging = live.join("parity-staging");
        std::fs::create_dir(&illegal_staging).unwrap();
        let identity = store_identity();
        let helper =
            fake_response_helper(temp.path(), &OutputV1::PageSize { bytes: 4096 }, &identity);
        let known_live_roots = vec![live];

        let error = run_rusqlite_parity_v1(
            &helper,
            &authority.path,
            &illegal_staging,
            &known_live_roots,
            RusqliteParityRequestV1::new(identity.clone(), CommandV1::PageSize),
            deadline(),
            &CancellationToken::new(),
            &identity,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            RusqliteParityInfrastructureErrorV1::StagingOverlapsKnownLiveRoot { .. }
        ));
        assert!(matches!(
            validate_staging_root(temp.path(), &known_live_roots),
            Err(RusqliteParityInfrastructureErrorV1::StagingOverlapsKnownLiveRoot { .. })
        ));
    }

    fn protocol_request(command: CommandV1) -> RequestV1 {
        let canonical_path = PathBuf::from("/private/staging/snapshot.db");
        RequestV1 {
            protocol_version: PROTOCOL_VERSION,
            request_id: "request-1".to_owned(),
            database: CopiedDatabaseV1 {
                path: canonical_path.clone(),
                kind: DatabaseKindV1::CopiedSnapshot,
                provenance: CopiedSnapshotProvenanceV1 {
                    authority_identity: "store-runtime-binding-v1:fixture".to_owned(),
                    staging_root: PathBuf::from("/private/staging"),
                    canonical_path,
                    byte_len: 17,
                    content_digest: format!("sha256:{}", "1".repeat(64)),
                    file_identity: SnapshotFileIdentityV1::Unsupported,
                },
            },
            command,
        }
    }

    fn verified_snapshot(request: &RequestV1) -> VerifiedCopiedSnapshotV1 {
        VerifiedCopiedSnapshotV1 {
            authority_identity: request.database.provenance.authority_identity.clone(),
            canonical_path: request.database.provenance.canonical_path.clone(),
            byte_len: request.database.provenance.byte_len,
            content_digest: request.database.provenance.content_digest.clone(),
            file_identity: request.database.provenance.file_identity.clone(),
        }
    }

    #[test]
    fn shared_protocol_commands_outputs_and_errors_are_handled_transparently() {
        let journal = journal_output();
        let cases = vec![
            (
                CommandV1::Metadata,
                OutputV1::Metadata(MetadataV1 {
                    canonical_path: PathBuf::from("/private/staging/snapshot.db"),
                    query_only: true,
                    immutable: true,
                    sqlite_version: "3.0.0".to_owned(),
                    compile_options: vec!["ENABLE_FTS5".to_owned()],
                }),
            ),
            (
                CommandV1::Schema,
                OutputV1::Schema(SchemaMetadataV1 {
                    schema_version: 1,
                    user_version: 2,
                    objects: vec![SchemaObjectV1 {
                        kind: SchemaObjectKindV1::Table,
                        name: "nodes".to_owned(),
                        table_name: "nodes".to_owned(),
                        sql: Some("CREATE TABLE nodes".to_owned()),
                    }],
                }),
            ),
            (
                CommandV1::ForeignKeys,
                OutputV1::ForeignKeys { enabled: true },
            ),
            (CommandV1::PageSize, OutputV1::PageSize { bytes: 4096 }),
            (CommandV1::JournalMode, journal),
            (
                CommandV1::Integrity {
                    check: IntegrityCheckV1::Quick,
                },
                OutputV1::Integrity(IntegrityReportV1 {
                    check: IntegrityCheckV1::Quick,
                    findings: vec!["ok".to_owned()],
                }),
            ),
            (
                CommandV1::SessionStoreCount {
                    family: SessionStoreFamilyV1::Observation,
                    table: SessionStoreTableV1::Observations,
                },
                OutputV1::SessionStoreCount(SessionStoreCountV1 {
                    family: SessionStoreFamilyV1::Observation,
                    table: SessionStoreTableV1::Observations,
                    row_count: Some(1),
                }),
            ),
            (
                CommandV1::SessionStoreSchema {
                    family: SessionStoreFamilyV1::Transcript,
                    table: SessionStoreTableV1::Sessions,
                },
                OutputV1::SessionStoreSchema(SessionStoreSchemaV1 {
                    family: SessionStoreFamilyV1::Transcript,
                    table: SessionStoreTableV1::Sessions,
                    exists: false,
                    columns: Vec::new(),
                    foreign_keys: Vec::new(),
                }),
            ),
            (
                CommandV1::SessionStorePage {
                    family: SessionStoreFamilyV1::Observation,
                    table: SessionStoreTableV1::Observations,
                    cursor: None,
                    limit: 1,
                },
                session_page_output(),
            ),
        ];
        for (command, output) in cases {
            let request = protocol_request(command);
            assert!(validate_request(&request).is_ok());
            let response = ResponseV1 {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(request.request_id.clone()),
                verified_snapshot: Some(verified_snapshot(&request)),
                outcome: ResponseOutcomeV1::Ok {
                    output: output.clone(),
                },
            };
            let response =
                serde_json::from_slice::<ResponseV1>(&serde_json::to_vec(&response).unwrap())
                    .unwrap();
            assert_eq!(validate_response(response, &request).unwrap(), output);
        }

        for code in [
            ErrorCodeV1::RequestTooLarge,
            ErrorCodeV1::InvalidRequest,
            ErrorCodeV1::UnsupportedProtocolVersion,
            ErrorCodeV1::InvalidPath,
            ErrorCodeV1::InvalidSnapshotProvenance,
            ErrorCodeV1::RefusedLiveProfile,
            ErrorCodeV1::OpenFailed,
            ErrorCodeV1::ReadOnlyInvariant,
            ErrorCodeV1::InvalidStoreFamily,
            ErrorCodeV1::InvalidPageCursor,
            ErrorCodeV1::InvalidPageLimit,
            ErrorCodeV1::ResultLimitExceeded,
            ErrorCodeV1::InvalidSqliteValue,
            ErrorCodeV1::InvalidSqliteHeader,
            ErrorCodeV1::SqliteFailure,
        ] {
            let request = protocol_request(CommandV1::PageSize);
            let response = ResponseV1 {
                protocol_version: PROTOCOL_VERSION,
                request_id: Some(request.request_id.clone()),
                verified_snapshot: Some(verified_snapshot(&request)),
                outcome: ResponseOutcomeV1::Error {
                    error: ErrorPayloadV1::new(code, "shared protocol error"),
                },
            };
            assert!(matches!(
                validate_response(response, &request),
                Err(RusqliteParityInfrastructureErrorV1::HelperRejected { error })
                    if error.code == code
            ));
        }
    }

    #[test]
    fn response_snapshot_identity_mismatch_fails_closed() {
        let request = protocol_request(CommandV1::PageSize);
        let mut actual = verified_snapshot(&request);
        actual.byte_len += 1;
        let response = ResponseV1 {
            protocol_version: PROTOCOL_VERSION,
            request_id: Some(request.request_id.clone()),
            verified_snapshot: Some(actual),
            outcome: ResponseOutcomeV1::Ok {
                output: OutputV1::PageSize { bytes: 4096 },
            },
        };
        assert!(matches!(
            validate_response(response, &request),
            Err(RusqliteParityInfrastructureErrorV1::ResponseSnapshotMismatch { .. })
        ));
    }
}
