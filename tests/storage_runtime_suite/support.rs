//! Shared, hermetic helpers for storage-runtime integration tests.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};

const CARGO_RUSQLITE_PARITY_BIN: &str = env!("CARGO_BIN_EXE_tracedecay-rusqlite-parity");

/// Owns a canonical temporary root for one test fixture.
pub(crate) struct IsolatedTempRoot {
    _directory: TempDir,
    canonical_path: PathBuf,
}

impl IsolatedTempRoot {
    pub(crate) fn new(label: &str) -> Self {
        assert!(
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
            "temporary-root label must contain only ASCII letters, digits, '-' or '_': {label:?}"
        );
        let directory = Builder::new()
            .prefix(&format!("tracedecay-storage-runtime-{label}-"))
            .tempdir()
            .expect("create isolated storage-runtime temporary root");
        let canonical_path = directory
            .path()
            .canonicalize()
            .expect("canonicalize isolated storage-runtime temporary root");
        Self {
            _directory: directory,
            canonical_path,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.canonical_path
    }
}

impl AsRef<Path> for IsolatedTempRoot {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}

impl std::fmt::Debug for IsolatedTempRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IsolatedTempRoot")
            .field("path", &self.canonical_path)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum DatabaseArtifactKind {
    Database,
    Wal,
    Shm,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ArtifactHash {
    pub(crate) byte_len: u64,
    pub(crate) sha256: String,
}

/// Includes absent sidecars so creation or deletion is treated as mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DatabaseArtifactInventory {
    pub(crate) database_path: PathBuf,
    pub(crate) artifacts: BTreeMap<DatabaseArtifactKind, Option<ArtifactHash>>,
}

pub(crate) fn database_artifact_paths(
    database_path: &Path,
) -> [(DatabaseArtifactKind, PathBuf); 3] {
    [
        (DatabaseArtifactKind::Database, database_path.to_path_buf()),
        (
            DatabaseArtifactKind::Wal,
            path_with_suffix(database_path, "-wal"),
        ),
        (
            DatabaseArtifactKind::Shm,
            path_with_suffix(database_path, "-shm"),
        ),
    ]
}

pub(crate) fn inventory_database_artifacts(database_path: &Path) -> DatabaseArtifactInventory {
    let database_path = absolute_path(database_path);
    let artifacts = database_artifact_paths(&database_path)
        .into_iter()
        .map(|(kind, path)| (kind, hash_optional_regular_file(&path)))
        .collect();
    DatabaseArtifactInventory {
        database_path,
        artifacts,
    }
}

pub(crate) fn assert_artifacts_unchanged(
    before: &DatabaseArtifactInventory,
    after: &DatabaseArtifactInventory,
    context: &str,
) {
    assert_eq!(
        before, after,
        "{context} mutated the database, WAL, or SHM artifacts\nbefore: {before:#?}\nafter: {after:#?}"
    );
}

pub(crate) fn rusqlite_parity_binary() -> PathBuf {
    validate_binary_path(
        Path::new(CARGO_RUSQLITE_PARITY_BIN),
        "CARGO_BIN_EXE_tracedecay-rusqlite-parity",
    )
}

/// Owns a spawned helper and kills it if the parent unwinds before reaping.
struct KillOnDrop(Option<Child>);

impl KillOnDrop {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn wait_with_output(mut self) -> io::Result<std::process::Output> {
        self.0
            .take()
            .expect("JSON subprocess child must still be owned")
            .wait_with_output()
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Sends one JSON value to an explicitly named executable and parses one JSON response.
pub(crate) fn invoke_json<T: Serialize>(binary: &Path, request: &T) -> Value {
    let binary = validate_binary_path(binary, "JSON subprocess binary");
    let request = serde_json::to_vec(request).expect("serialize JSON subprocess request");
    let mut child = KillOnDrop::new(
        Command::new(&binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| {
                panic!("spawn JSON subprocess '{}': {error}", binary.display())
            }),
    );
    {
        let stdin = child
            .0
            .as_mut()
            .expect("JSON subprocess child must still be owned")
            .stdin
            .take()
            .expect("JSON subprocess stdin must be piped");
        // Drop closes stdin so the helper sees EOF and can exit.
        let mut stdin = stdin;
        stdin.write_all(&request).unwrap_or_else(|error| {
            panic!(
                "write JSON request to subprocess '{}': {error}",
                binary.display()
            )
        });
    }
    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("wait for JSON subprocess '{}': {error}", binary.display()));
    assert!(
        output.status.success(),
        "JSON subprocess '{}' failed with {}: {}",
        binary.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "JSON subprocess '{}' returned invalid JSON: {error}; stdout={:?}; stderr={:?}",
            binary.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

pub(crate) fn invoke_rusqlite_parity<T: Serialize>(request: &T) -> Value {
    invoke_json(&rusqlite_parity_binary(), request)
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .expect("resolve current directory for artifact inventory")
            .join(path)
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn hash_optional_regular_file(path: &Path) -> Option<ArtifactHash> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => panic!("inspect storage artifact '{}': {error}", path.display()),
    };
    assert!(
        metadata.file_type().is_file(),
        "storage artifact must be a regular file: {}",
        path.display()
    );
    let mut file = File::open(path)
        .unwrap_or_else(|error| panic!("open storage artifact '{}': {error}", path.display()));
    let mut hasher = Sha256::new();
    let mut byte_len = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("hash storage artifact '{}': {error}", path.display()));
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        byte_len = byte_len
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .expect("storage artifact length fits u64");
    }
    Some(ArtifactHash {
        byte_len,
        sha256: hex::encode(hasher.finalize()),
    })
}

pub(crate) fn snapshot_content_digest(path: &Path) -> String {
    let hash = hash_optional_regular_file(path)
        .unwrap_or_else(|| panic!("copied snapshot does not exist: {}", path.display()));
    format!("sha256:{}", hash.sha256)
}

fn validate_binary_path(path: &Path, source: &str) -> PathBuf {
    assert!(
        path.is_absolute(),
        "{source} must be an absolute path, not {:?}",
        path.as_os_str()
    );
    let metadata = fs::symlink_metadata(path)
        .unwrap_or_else(|error| panic!("inspect {source} '{}': {error}", path.display()));
    assert!(
        !metadata.file_type().is_symlink() && metadata.file_type().is_file(),
        "{source} must name an existing regular file and not a symlink: {}",
        path.display()
    );
    ensure_executable(&metadata, path, source);
    let canonical = path
        .canonicalize()
        .unwrap_or_else(|error| panic!("canonicalize {source} '{}': {error}", path.display()));
    assert!(
        canonical.is_absolute(),
        "canonical {source} path is not absolute"
    );
    canonical
}

#[cfg(unix)]
fn ensure_executable(metadata: &fs::Metadata, path: &Path, source: &str) {
    use std::os::unix::fs::PermissionsExt;

    assert_ne!(
        metadata.permissions().mode() & 0o111,
        0,
        "{source} is not executable: {}",
        path.display()
    );
}

#[cfg(not(unix))]
fn ensure_executable(_metadata: &fs::Metadata, _path: &Path, _source: &str) {}
