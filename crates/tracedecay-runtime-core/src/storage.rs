#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
use std::cell::Cell;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;

pub const ENROLLMENT_FILENAME: &str = "enrollment.json";
pub const STORE_MANIFEST_FILENAME: &str = "store_manifest.json";
pub const PROFILE_IDENTITY_FILENAME: &str = "profile-identity.json";
pub const SESSIONS_DB_FILENAME: &str = "sessions.db";
pub const BRANCH_META_FILENAME: &str = "branch-meta.json";
pub(crate) const REPOSITORY_IDENTITY_FILENAME: &str = "tracedecay-project.json";
/// Legacy filename prefix used to recognize already-quarantined branch
/// metadata as non-authoritative debris.
pub const BRANCH_META_QUARANTINE_PREFIX: &str = "branch-meta.json.corrupt-";
pub const DURABLE_REMOVAL_TOMBSTONE_PREFIX: &str = ".tracedecay-deleted-";
pub const STORE_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
thread_local! {
    static DURABLE_ATOMIC_WRITE_FAULT: Cell<u8> = const { Cell::new(0) };
    static DURABLE_NAMESPACE_SYNC_FAULT: Cell<u8> = const { Cell::new(0) };
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
#[derive(Clone, Copy)]
pub enum DurableAtomicWriteFaultForTest {
    AfterTempSync = 1,
    AfterRename = 2,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
struct DurableAtomicWriteFaultScope {
    previous: u8,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
impl Drop for DurableAtomicWriteFaultScope {
    fn drop(&mut self) {
        DURABLE_ATOMIC_WRITE_FAULT.with(|state| state.set(self.previous));
    }
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
struct DurableNamespaceSyncFaultScope {
    previous: u8,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
impl Drop for DurableNamespaceSyncFaultScope {
    fn drop(&mut self) {
        DURABLE_NAMESPACE_SYNC_FAULT.with(|state| state.set(self.previous));
    }
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub fn with_durable_atomic_write_fault_for_test<T>(
    fault: DurableAtomicWriteFaultForTest,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = DURABLE_ATOMIC_WRITE_FAULT.with(|state| state.replace(fault as u8));
    let _scope = DurableAtomicWriteFaultScope { previous };
    operation()
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub fn with_durable_namespace_sync_fault_for_test<T>(
    sync_ordinal: u8,
    operation: impl FnOnce() -> T,
) -> T {
    assert!(sync_ordinal > 0, "sync fault ordinal must be one-based");
    let previous = DURABLE_NAMESPACE_SYNC_FAULT.with(|state| state.replace(sync_ordinal));
    let _scope = DurableNamespaceSyncFaultScope { previous };
    operation()
}

#[derive(Clone, Copy)]
enum DurableAtomicWritePhase {
    AfterTempSync = 1,
    AfterRename = 2,
}

fn inject_durable_atomic_write_fault(_phase: DurableAtomicWritePhase) -> io::Result<()> {
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    if DURABLE_ATOMIC_WRITE_FAULT.with(|fault| {
        if fault.get() != _phase as u8 {
            return false;
        }
        fault.set(0);
        true
    }) {
        return Err(io::Error::other(match _phase {
            DurableAtomicWritePhase::AfterTempSync => {
                "injected durable atomic write failure after temp sync"
            }
            DurableAtomicWritePhase::AfterRename => {
                "injected durable atomic write failure after rename"
            }
        }));
    }
    Ok(())
}

fn inject_durable_namespace_sync_fault() -> io::Result<()> {
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    if DURABLE_NAMESPACE_SYNC_FAULT.with(|fault| match fault.get() {
        0 => false,
        1 => {
            fault.set(0);
            true
        }
        remaining => {
            fault.set(remaining - 1);
            false
        }
    }) {
        return Err(io::Error::other(
            "injected durable namespace synchronization failure",
        ));
    }
    Ok(())
}
pub const REPOSITORY_IDENTITY_SCHEMA_VERSION: u32 = 1;

/// Checks the fixed 16-byte `SQLite` header without opening the database.
///
/// This is deliberately file-only: opening `SQLite` may create or rewrite WAL/SHM
/// sidecars before reporting that the main file is not a database. Recovery
/// paths use this preflight to preserve the complete on-disk recovery set.
pub fn has_sqlite_database_header(path: &Path) -> io::Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 16];
    match file.read_exact(&mut header) {
        Ok(()) => Ok(header == *b"SQLite format 3\0"),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(err) => Err(err),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    ProjectLocal,
    ProfileSharded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreKind {
    CodeProject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentMarker {
    pub project_id: String,
    pub storage_mode: StorageMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryIdentityMarker {
    pub schema_version: u32,
    pub project_id: String,
    pub git_common_dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectIdentity {
    pub project_id: Option<String>,
    pub display_root: PathBuf,
    pub primary_alias: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreLayout {
    pub identity: ProjectIdentity,
    pub store_kind: StoreKind,
    pub storage_mode: StorageMode,
    pub project_root: PathBuf,
    pub data_root: PathBuf,
    pub graph_db_path: PathBuf,
    pub config_path: PathBuf,
    pub branch_meta_path: PathBuf,
    pub sessions_db_path: PathBuf,
    pub response_handle_root: PathBuf,
    pub lcm_payload_root: PathBuf,
    pub dashboard_root: PathBuf,
    pub manifest_path: Option<PathBuf>,
    pub dirty_path: PathBuf,
    pub sync_lock_path: PathBuf,
    pub branch_add_lock_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectStorageStatus {
    RepoLocal,
    ProfileSharded,
    ManifestReconstructable,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectStorageLocation {
    pub project_root: PathBuf,
    pub data_root: PathBuf,
    pub marker_root: Option<PathBuf>,
    pub status: ProjectStorageStatus,
}

impl ProjectStorageStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::RepoLocal => "repo-local",
            Self::ProfileSharded => "profile-sharded",
            Self::ManifestReconstructable => "manifest-reconstructable",
            Self::Stale => "stale",
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, Self::RepoLocal | Self::ProfileSharded)
    }
}

pub fn classify_project_storage(project_root: &Path) -> ProjectStorageLocation {
    match resolve_layout_for_current_profile(project_root) {
        Ok(layout) => classify_layout_storage(project_root, layout),
        Err(_) => ProjectStorageLocation {
            project_root: project_root.to_path_buf(),
            data_root: config::get_tracedecay_dir(project_root),
            marker_root: None,
            status: ProjectStorageStatus::Stale,
        },
    }
}

fn classify_layout_storage(project_root: &Path, layout: StoreLayout) -> ProjectStorageLocation {
    let graph_exists = layout.graph_db_path.exists();
    let manifest_exists = layout
        .manifest_path
        .as_ref()
        .is_some_and(|path| path.is_file());
    let status = match layout.storage_mode {
        StorageMode::ProjectLocal if graph_exists => ProjectStorageStatus::RepoLocal,
        StorageMode::ProfileSharded if graph_exists => ProjectStorageStatus::ProfileSharded,
        StorageMode::ProfileSharded if manifest_exists => {
            ProjectStorageStatus::ManifestReconstructable
        }
        _ => ProjectStorageStatus::Stale,
    };
    let marker_root = (layout.storage_mode == StorageMode::ProfileSharded)
        .then(|| project_root.join(config::TRACEDECAY_DIR));
    ProjectStorageLocation {
        project_root: project_root.to_path_buf(),
        data_root: layout.data_root,
        marker_root,
        status,
    }
}

pub fn classify_registry_storage_value(
    project_root: &Path,
    profile_root: &Path,
    store: &serde_json::Value,
) -> Option<ProjectStorageLocation> {
    classify_registry_storage_fields(
        project_root,
        profile_root,
        store.get("storage_mode")?.as_str()?,
        store.get("store_relpath")?.as_str()?,
        store
            .get("manifest_relpath")
            .and_then(serde_json::Value::as_str),
    )
}

/// Field-shaped registry classifier. The `StoreInstanceRecord` wrapper
/// lives in the root crate because `global_db` stays above this kernel.
pub fn classify_registry_storage_fields(
    project_root: &Path,
    profile_root: &Path,
    storage_mode: &str,
    store_relpath: &str,
    manifest_relpath: Option<&str>,
) -> Option<ProjectStorageLocation> {
    if storage_mode != "profile_sharded" {
        return None;
    }
    let store_relpath = registry_relpath(store_relpath);
    let manifest_relpath = manifest_relpath.map(registry_relpath);
    let mut stale_location = None;
    let mut manifest_location = None;
    for profile_root in registry_profile_roots(profile_root) {
        let Ok(data_root) = StoreArtifactPath::resolve(&profile_root, &store_relpath) else {
            continue;
        };
        let data_root = data_root.absolute_path();
        let manifest_exists = manifest_relpath.as_ref().map_or_else(
            || data_root.join(STORE_MANIFEST_FILENAME).is_file(),
            |relpath| {
                [&profile_root, &data_root].iter().any(|root| {
                    StoreArtifactPath::resolve(root, relpath)
                        .ok()
                        .is_some_and(|path| path.absolute_path().is_file())
                })
            },
        );
        let status = if data_root.join(config::db_filename(&data_root)).exists() {
            ProjectStorageStatus::ProfileSharded
        } else if manifest_exists {
            ProjectStorageStatus::ManifestReconstructable
        } else {
            ProjectStorageStatus::Stale
        };
        let location = ProjectStorageLocation {
            project_root: project_root.to_path_buf(),
            data_root,
            marker_root: Some(project_root.join(config::TRACEDECAY_DIR)),
            status,
        };
        match location.status {
            ProjectStorageStatus::ProfileSharded => return Some(location),
            ProjectStorageStatus::ManifestReconstructable if manifest_location.is_none() => {
                manifest_location = Some(location);
            }
            ProjectStorageStatus::Stale if stale_location.is_none() => {
                stale_location = Some(location);
            }
            _ => {}
        }
    }
    manifest_location.or(stale_location)
}

fn registry_relpath(value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return path.to_path_buf();
    }
    value
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect()
}

fn registry_profile_roots(profile_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![profile_root.to_path_buf()];
    if let Ok(canonical) = profile_root.canonicalize()
        && !roots.iter().any(|root| root == &canonical)
    {
        roots.push(canonical);
    }
    roots
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreManifest {
    pub schema_version: u32,
    pub project_id: Option<String>,
    pub store_kind: StoreKind,
    pub storage_mode: StorageMode,
    pub project_root: PathBuf,
    pub data_root: PathBuf,
    pub graph_db_relpath: PathBuf,
    pub sessions_db_relpath: PathBuf,
    pub branch_meta_relpath: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphScopeId {
    Project,
    Branch(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryTarget {
    pub graph_db_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProjectContext {
    pub layout: StoreLayout,
    pub scope_id: GraphScopeId,
    pub query_target: QueryTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPath {
    absolute_path: PathBuf,
    relative_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreArtifactPath {
    absolute_path: PathBuf,
    relative_path: PathBuf,
}

/// An existing profile-sharded store whose root, manifest, and session database
/// have been validated without creating or opening the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedProfileShard {
    store_root: PathBuf,
    sessions_db_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileShardValidationError {
    Unavailable { path: PathBuf },
    NonCanonical { reason: String },
}

pub struct PrivateStoreIo;

mod identity;
mod layout;
mod manifest;
mod paths_and_io;

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub use identity::pin_fixture_repository_identity;
pub use identity::{
    has_repository_identity_marker, legacy_enrollment_marker_path, read_legacy_enrollment_marker,
    read_repository_identity_marker, repository_identity_path, write_repository_identity_marker,
};
pub(crate) use layout::has_path_local_profile_store;
pub use layout::{
    default_profile_project_id, default_profile_root, default_profile_sharded_layout,
    path_local_profile_project_id, profile_sharded_data_root, profile_sharded_layout,
    resolve_enrolled_layout_for_current_profile, resolve_layout,
    resolve_layout_for_current_profile, resolve_lcm_payload_root, resolve_persisted_layout,
    resolve_project_session_db_path, resolve_response_handle_root,
};
pub use manifest::{read_store_manifest, write_store_manifest, write_store_manifest_to_path};
pub use paths_and_io::{
    acquire_sidecar_lock_blocking, append_lock_path, reject_symlink_components,
    retry_transient_file_op, set_private_dir_permissions, try_acquire_sidecar_lock,
    validate_project_id,
};

#[cfg(test)]
use paths_and_io::open_lock_file;
use paths_and_io::validate_enrollment_marker;

include!("storage/tests.rs");
include!("storage/identity_tests.rs");
