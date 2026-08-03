use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
use std::sync::atomic::{AtomicU8, Ordering};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::framed_log::DirectorySyncPolicy;

use crate::config::{self, TRACEDECAY_DIR};
use crate::errors::{Result, TraceDecayError};

pub const ENROLLMENT_FILENAME: &str = "enrollment.json";
pub const STORE_MANIFEST_FILENAME: &str = "store_manifest.json";
pub const PROFILE_IDENTITY_FILENAME: &str = "profile-identity.json";
pub const SESSIONS_DB_FILENAME: &str = "sessions.db";
pub const BRANCH_META_FILENAME: &str = "branch-meta.json";
pub(crate) const REPOSITORY_IDENTITY_FILENAME: &str = "tracedecay-project.json";
/// Filename prefix for corrupt `branch-meta.json` files renamed out of the
/// way by the post-update health pass (`branch-meta.json.corrupt-<timestamp>`).
pub const BRANCH_META_QUARANTINE_PREFIX: &str = "branch-meta.json.corrupt-";
pub const STORE_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
static DURABLE_ATOMIC_WRITE_FAULT: AtomicU8 = AtomicU8::new(0);

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
#[derive(Clone, Copy)]
pub enum DurableAtomicWriteFaultForTest {
    AfterTempSync = 1,
    AfterRename = 2,
}

#[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
pub fn set_durable_atomic_write_fault_for_test(fault: DurableAtomicWriteFaultForTest) {
    DURABLE_ATOMIC_WRITE_FAULT.store(fault as u8, Ordering::SeqCst);
}

#[derive(Clone, Copy)]
enum DurableAtomicWritePhase {
    AfterTempSync = 1,
    AfterRename = 2,
}

fn inject_durable_atomic_write_fault(_phase: DurableAtomicWritePhase) -> io::Result<()> {
    #[cfg(any(test, feature = "test-helpers", feature = "test-transport"))]
    if DURABLE_ATOMIC_WRITE_FAULT
        .compare_exchange(_phase as u8, 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
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

pub fn enrollment_marker_path(project_root: &Path) -> PathBuf {
    project_root.join(TRACEDECAY_DIR).join(ENROLLMENT_FILENAME)
}

pub fn has_enrollment_marker(project_root: &Path) -> bool {
    matches!(
        read_enrollment_marker(project_root),
        Ok(Some(marker)) if marker.storage_mode == StorageMode::ProfileSharded
    )
}

pub fn read_enrollment_marker(project_root: &Path) -> Result<Option<EnrollmentMarker>> {
    let path = enrollment_marker_path(project_root);
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| TraceDecayError::Config {
        message: format!("failed to read enrollment marker '{}': {e}", path.display()),
    })?;
    let marker = serde_json::from_str(&text).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to parse enrollment marker '{}': {e}",
            path.display()
        ),
    })?;
    validate_enrollment_marker(&marker, &path)?;
    Ok(Some(marker))
}

pub fn write_enrollment_marker(project_root: &Path, marker: &EnrollmentMarker) -> Result<()> {
    validate_enrollment_marker(marker, &enrollment_marker_path(project_root))?;
    let path = enrollment_marker_path(project_root);
    let text = serde_json::to_vec_pretty(marker).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to serialize enrollment marker '{}': {e}",
            path.display()
        ),
    })?;
    // Several independent paths enroll the same project (CLI init, the
    // daemon's first-touch open, enrollment-root repair) while the store
    // resolver may read the marker concurrently. A truncate-then-write here
    // briefly exposes an empty file, which the resolver reports as an
    // invalid/missing enrollment and callers surface as a denial. Replace
    // atomically so a reader only ever sees a complete marker or none.
    let temp_path = path.with_extension(format!(
        "json.tmp-{}-{}",
        std::process::id(),
        enrollment_marker_temp_nonce()
    ));
    PrivateStoreIo::write_file_atomically(&path, &temp_path, &text).map_err(|e| {
        TraceDecayError::Config {
            message: format!(
                "failed to write enrollment marker '{}': {e}",
                path.display()
            ),
        }
    })
}

/// Distinguishes concurrent in-process enrollment writers, which would
/// otherwise race each other on one shared pid-derived temp path.
fn enrollment_marker_temp_nonce() -> u64 {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub fn remove_enrollment_marker(project_root: &Path, project_id: &str) -> Result<bool> {
    let path = enrollment_marker_path(project_root);
    let Some(marker) = read_enrollment_marker(project_root)? else {
        return Ok(false);
    };
    if marker.project_id != project_id || marker.storage_mode != StorageMode::ProfileSharded {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to remove enrollment marker '{}': it does not match project_id '{}'",
                path.display(),
                project_id
            ),
        });
    }
    fs::remove_file(&path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to remove enrollment marker '{}': {e}",
            path.display()
        ),
    })?;
    Ok(true)
}

/// The repository-wide identity marker shared by every checkout of a
/// repository, including detached linked worktrees.
///
/// Detached worktrees were once excluded here so they could not be served
/// another checkout's index. That protection belongs to the graph-scope axis,
/// not the identity axis: a detached HEAD in the *primary* checkout already
/// shares the repository store and is served the default-branch index with an
/// explicit fallback warning (see `TraceDecay::resolve_db_for_branch`).
/// Excluding only the worktree case bought no extra safety and cost a
/// duplicate project store per detached worktree.
pub fn repository_identity_path(project_root: &Path) -> Option<PathBuf> {
    crate::worktree::git_common_dir(project_root)
        .map(|common_dir| common_dir.join(REPOSITORY_IDENTITY_FILENAME))
}

pub fn read_repository_identity_marker(
    project_root: &Path,
) -> Result<Option<RepositoryIdentityMarker>> {
    let Some(path) = repository_identity_path(project_root) else {
        return Ok(None);
    };
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to read repository identity marker '{}': {e}",
            path.display()
        ),
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to parse repository identity marker '{}': {e}",
                path.display()
            ),
        })?;
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "repository identity marker '{}' has no valid schema_version",
                path.display()
            ),
        })?;
    if schema_version != REPOSITORY_IDENTITY_SCHEMA_VERSION {
        return Err(TraceDecayError::Config {
            message: format!(
                "unsupported repository identity schema_version={} in '{}'; expected {}",
                schema_version,
                path.display(),
                REPOSITORY_IDENTITY_SCHEMA_VERSION
            ),
        });
    }
    let marker: RepositoryIdentityMarker =
        serde_json::from_value(value).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to parse repository identity marker '{}': {e}",
                path.display()
            ),
        })?;
    validate_project_id(&marker.project_id).map_err(|message| TraceDecayError::Config {
        message: format!(
            "invalid repository identity marker '{}': {message}",
            path.display()
        ),
    })?;
    let stored_common_dir = Path::new(&marker.git_common_dir);
    if !stored_common_dir.is_absolute() {
        return Err(TraceDecayError::Config {
            message: format!(
                "invalid repository identity marker '{}': git_common_dir must be absolute",
                path.display()
            ),
        });
    }
    let current_common_dir = path.parent().ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "repository identity marker '{}' has no parent directory",
            path.display()
        ),
    })?;
    let stored_key = stored_common_dir
        .canonicalize()
        .unwrap_or_else(|_| stored_common_dir.to_path_buf());
    let current_key = current_common_dir
        .canonicalize()
        .unwrap_or_else(|_| current_common_dir.to_path_buf());
    if stored_key != current_key
        && stored_common_dir.exists()
        && stored_dir_marker_names_project(stored_common_dir, &marker.project_id)
    {
        // The stored git common dir still exists, canonicalizes to a different
        // live directory, and hosts a marker naming the SAME project: this is a
        // genuine true copy (e.g. `cp -a`/rsync duplicated the marker) with two
        // live checkouts claiming one project id. Fail closed. A move where the
        // old path was reused by an UNRELATED repo (absent/unreadable/different
        // marker there) is accepted below and self-heals on the next writable
        // open, which rewrites git_common_dir to this checkout.
        return Err(TraceDecayError::Config {
            message: format!(
                "repository identity conflict: marker '{}' names project '{}' but its original \
                 git common directory '{}' is still live; this checkout uses '{}'",
                path.display(),
                marker.project_id,
                stored_common_dir.display(),
                current_common_dir.display()
            ),
        });
    }
    Ok(Some(marker))
}

/// Probe the repository identity marker stored inside `stored_common_dir` and
/// report whether it names `expected_project_id`.
///
/// This is a raw JSON read that deliberately does NOT recurse through
/// [`read_repository_identity_marker`] (which would re-run conflict detection
/// against the probed directory). An absent, unreadable, malformed, or
/// differently-named marker returns `false`.
fn stored_dir_marker_names_project(stored_common_dir: &Path, expected_project_id: &str) -> bool {
    let marker_path = stored_common_dir.join(REPOSITORY_IDENTITY_FILENAME);
    let Ok(text) = fs::read_to_string(&marker_path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    value.get("project_id").and_then(serde_json::Value::as_str) == Some(expected_project_id)
}

pub fn write_repository_identity_marker(project_root: &Path, project_id: &str) -> Result<bool> {
    validate_project_id(project_id).map_err(|message| TraceDecayError::Config {
        message: message.to_string(),
    })?;
    let Some(path) = repository_identity_path(project_root) else {
        return Ok(false);
    };
    let git_common_dir = path.parent().ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "repository identity marker '{}' has no parent directory",
            path.display()
        ),
    })?;
    let marker = RepositoryIdentityMarker {
        schema_version: REPOSITORY_IDENTITY_SCHEMA_VERSION,
        project_id: project_id.to_string(),
        git_common_dir: git_common_dir.to_string_lossy().to_string(),
    };
    let contents = serde_json::to_vec_pretty(&marker).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to serialize repository identity marker '{}': {e}",
            path.display()
        ),
    })?;
    let temp_path = path.with_extension(format!("json.tmp-{}", std::process::id()));
    PrivateStoreIo::write_file_atomically(&path, &temp_path, &contents).map_err(|e| {
        TraceDecayError::Config {
            message: format!(
                "failed to write repository identity marker '{}': {e}",
                path.display()
            ),
        }
    })?;
    Ok(true)
}

pub fn profile_sharded_data_root(profile_root: &Path, project_id: &str) -> PathBuf {
    profile_root.join("projects").join(project_id)
}

fn project_id_for_identity_root(identity_root: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(identity_root.to_string_lossy().as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("proj_{}", &digest[..16])
}

/// The id a store keyed to this exact directory would use, ignoring any
/// repository it belongs to.
///
/// Only discovery wants this. Discovery asks a narrower question than identity
/// resolution — not "which repository owns this checkout" but "was a store
/// ever minted for this exact directory" — and answering it with the
/// repository id would report every linked worktree of an initialized
/// repository as independently initialized.
pub fn path_local_profile_project_id(project_root: &Path) -> String {
    project_id_for_identity_root(
        &project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf()),
    )
}

/// The default identity for a project root.
///
/// This is the only place a project id is minted, so a linked worktree cannot
/// acquire a store of its own even when every marker and registry lookup has
/// missed: the fallback itself resolves to the repository. A primary checkout
/// resolves to itself, so every id minted before repository collapse existed
/// is byte-identical and no live store is orphaned.
pub fn default_profile_project_id(project_root: &Path) -> String {
    match crate::worktree::repository_identity_root(project_root) {
        Some(repository_root) => project_id_for_identity_root(&repository_root),
        None => path_local_profile_project_id(project_root),
    }
}

/// Whether a profile shard keyed to this exact path already holds a graph.
///
/// See [`path_local_profile_project_id`] for why discovery must not consult
/// the repository-collapsed identity here.
pub(crate) fn has_path_local_profile_store(project_root: &Path) -> bool {
    let Ok(profile_root) = default_profile_root() else {
        return false;
    };
    let data_root =
        profile_sharded_data_root(&profile_root, &path_local_profile_project_id(project_root));
    data_root.join(config::db_filename(&data_root)).exists()
}

pub fn default_profile_sharded_layout(
    project_root: &Path,
    profile_root: &Path,
) -> Result<StoreLayout> {
    let marker = EnrollmentMarker {
        project_id: default_profile_project_id(project_root),
        storage_mode: StorageMode::ProfileSharded,
    };
    profile_sharded_layout(project_root, profile_root, &marker)
}

pub fn profile_sharded_layout(
    project_root: &Path,
    profile_root: &Path,
    marker: &EnrollmentMarker,
) -> Result<StoreLayout> {
    if marker.storage_mode != StorageMode::ProfileSharded {
        return Err(TraceDecayError::Config {
            message: format!(
                "enrollment marker for '{}' uses storage_mode={:?}, not profile_sharded",
                project_root.display(),
                marker.storage_mode
            ),
        });
    }
    validate_project_id(&marker.project_id).map_err(|message| TraceDecayError::Config {
        message: format!(
            "invalid enrollment marker for '{}': {message}",
            project_root.display()
        ),
    })?;
    let data_root = profile_sharded_data_root(profile_root, &marker.project_id);
    Ok(StoreLayout::new(
        ProjectIdentity {
            project_id: Some(marker.project_id.clone()),
            display_root: project_root.to_path_buf(),
            primary_alias: project_root.to_path_buf(),
        },
        StoreKind::CodeProject,
        StorageMode::ProfileSharded,
        project_root.to_path_buf(),
        data_root,
        Some(STORE_MANIFEST_FILENAME),
    ))
}

pub fn resolve_layout(project_root: &Path, profile_root: &Path) -> Result<StoreLayout> {
    if let Some(layout) = resolve_persisted_layout(project_root, profile_root)? {
        return Ok(layout);
    }
    default_profile_sharded_layout(project_root, profile_root)
}

pub fn resolve_persisted_layout(
    project_root: &Path,
    profile_root: &Path,
) -> Result<Option<StoreLayout>> {
    if let Some(marker) = read_enrollment_marker(project_root)? {
        if marker.storage_mode != StorageMode::ProfileSharded {
            return Err(TraceDecayError::Config {
                message: format!(
                    "unsupported storage_mode={:?} in enrollment marker for '{}'; \
                     run TraceDecay migration to move this project into the user profile store",
                    marker.storage_mode,
                    project_root.display()
                ),
            });
        }
        return profile_sharded_layout(project_root, profile_root, &marker).map(Some);
    }
    let Some(marker) = read_repository_identity_marker(project_root)? else {
        return Ok(None);
    };
    profile_sharded_layout(
        project_root,
        profile_root,
        &EnrollmentMarker {
            project_id: marker.project_id,
            storage_mode: StorageMode::ProfileSharded,
        },
    )
    .map(Some)
}

pub fn default_profile_root() -> Result<PathBuf> {
    config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not resolve user profile data directory".to_string(),
    })
}

/// Synchronous store resolution for callers that cannot await the registry:
/// hooks, MCP response handles, config resolution, the agent command, Doctor,
/// and diagnostics.
///
/// This used to read only the enrollment marker and otherwise derive a project
/// id from the checkout path, so it disagreed with the async registry resolver
/// about the same directory and split one repository across shards. It now
/// consults every authority available without awaiting — the same enrollment
/// marker and repository identity marker via [`resolve_persisted_layout`].
pub fn resolve_layout_for_current_profile(project_root: &Path) -> Result<StoreLayout> {
    let profile_root = default_profile_root()?;
    match resolve_enrolled_layout(project_root, &profile_root)? {
        Some(layout) => Ok(layout),
        None => default_profile_sharded_layout(project_root, &profile_root),
    }
}

/// Resolves this checkout's store only when an authority already names it, and
/// reports `Ok(None)` when the answer would be a path-derived guess.
///
/// Callers that merely want somewhere to put a file — hook analytics is the
/// motivating one — must not enroll a directory as a side effect. Every
/// directory this resolver declines is a store shard that never gets minted for
/// a path that was never a project.
pub fn resolve_enrolled_layout_for_current_profile(
    project_root: &Path,
) -> Result<Option<StoreLayout>> {
    let profile_root = default_profile_root()?;
    resolve_enrolled_layout(project_root, &profile_root)
}

fn resolve_enrolled_layout(
    project_root: &Path,
    profile_root: &Path,
) -> Result<Option<StoreLayout>> {
    resolve_persisted_layout(project_root, profile_root)
}

pub fn resolve_project_session_db_path(project_root: &Path) -> Result<PathBuf> {
    Ok(resolve_layout_for_current_profile(project_root)?.sessions_db_path)
}

pub fn resolve_response_handle_root(project_root: &Path) -> Result<PathBuf> {
    Ok(resolve_layout_for_current_profile(project_root)?.response_handle_root)
}

pub fn resolve_lcm_payload_root(project_root: &Path) -> Result<PathBuf> {
    Ok(resolve_layout_for_current_profile(project_root)?.lcm_payload_root)
}

pub fn write_store_manifest(layout: &StoreLayout) -> Result<StoreManifest> {
    let path = layout
        .manifest_path
        .as_ref()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "store manifest path is not defined for {:?} storage",
                layout.storage_mode
            ),
        })?;
    let manifest = StoreManifest::from_layout(layout);
    write_store_manifest_payload(path, &manifest)?;
    Ok(manifest)
}

/// Writes `manifest` to `path` without rebuilding it from a [`StoreLayout`].
pub fn write_store_manifest_to_path(path: &Path, manifest: &StoreManifest) -> Result<()> {
    write_store_manifest_payload(path, manifest)
}

fn write_store_manifest_payload(path: &Path, manifest: &StoreManifest) -> Result<()> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to serialize store manifest '{}': {e}",
            path.display()
        ),
    })?;
    let temp_path = path.with_extension("json.tmp");
    PrivateStoreIo::write_file_atomically(path, &temp_path, text.as_bytes()).map_err(|e| {
        TraceDecayError::Config {
            message: format!("failed to write store manifest '{}': {e}", path.display()),
        }
    })
}

pub fn read_store_manifest(path: &Path) -> Result<StoreManifest> {
    let text = fs::read_to_string(path).map_err(|e| TraceDecayError::Config {
        message: format!("failed to read store manifest '{}': {e}", path.display()),
    })?;
    serde_json::from_str(&text).map_err(|e| TraceDecayError::Config {
        message: format!("failed to parse store manifest '{}': {e}", path.display()),
    })
}

impl ValidatedProfileShard {
    /// Resolves an already-registered profile shard without creating artifacts
    /// or letting the database library touch an invalid database family.
    pub fn resolve_existing(
        profile_root: &Path,
        project_id: &str,
    ) -> std::result::Result<Self, ProfileShardValidationError> {
        let expected_relpath_text = format!("projects/{project_id}");
        let expected_relpath = PathBuf::from(&expected_relpath_text);
        let canonical_profile_root =
            profile_root
                .canonicalize()
                .map_err(|_| ProfileShardValidationError::Unavailable {
                    path: profile_root.to_path_buf(),
                })?;

        let store_root = profile_sharded_data_root(profile_root, project_id);
        require_regular_artifact(&store_root, RequiredArtifactKind::Directory)?;
        let canonical_store_root =
            store_root
                .canonicalize()
                .map_err(|_| ProfileShardValidationError::Unavailable {
                    path: store_root.clone(),
                })?;
        if canonical_store_root != canonical_profile_root.join(&expected_relpath) {
            return Err(ProfileShardValidationError::NonCanonical {
                reason: format!(
                    "store root resolves outside '{}'",
                    expected_relpath.display()
                ),
            });
        }

        let manifest_path = store_root.join(STORE_MANIFEST_FILENAME);
        require_regular_artifact(&manifest_path, RequiredArtifactKind::File)?;
        validate_profile_shard_manifest(project_id, &canonical_store_root, &manifest_path)?;

        let sessions_db_path = store_root.join(SESSIONS_DB_FILENAME);
        require_regular_artifact(&sessions_db_path, RequiredArtifactKind::File)?;
        match has_sqlite_database_header(&sessions_db_path) {
            Ok(true) => {}
            Ok(false) => {
                return Err(ProfileShardValidationError::NonCanonical {
                    reason: format!("'{}' is not a SQLite database", sessions_db_path.display()),
                });
            }
            Err(_) => {
                return Err(ProfileShardValidationError::Unavailable {
                    path: sessions_db_path,
                });
            }
        }
        let sessions_db_path = sessions_db_path.canonicalize().map_err(|_| {
            ProfileShardValidationError::Unavailable {
                path: sessions_db_path.clone(),
            }
        })?;

        Ok(Self {
            store_root: canonical_store_root,
            sessions_db_path,
        })
    }

    pub fn store_root(&self) -> &Path {
        &self.store_root
    }

    pub fn sessions_db_path(&self) -> &Path {
        &self.sessions_db_path
    }
}

#[derive(Clone, Copy)]
enum RequiredArtifactKind {
    Directory,
    File,
}

impl RequiredArtifactKind {
    fn description(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
        }
    }

    fn matches(self, file_type: fs::FileType) -> bool {
        match self {
            Self::Directory => file_type.is_dir(),
            Self::File => file_type.is_file(),
        }
    }
}

fn require_regular_artifact(
    path: &Path,
    kind: RequiredArtifactKind,
) -> std::result::Result<(), ProfileShardValidationError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ProfileShardValidationError::Unavailable {
            path: path.to_path_buf(),
        })?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() || !kind.matches(file_type) {
        return Err(ProfileShardValidationError::NonCanonical {
            reason: format!(
                "'{}' is not a regular {}",
                path.display(),
                kind.description()
            ),
        });
    }
    Ok(())
}

fn validate_profile_shard_manifest(
    project_id: &str,
    store_root: &Path,
    manifest_path: &Path,
) -> std::result::Result<(), ProfileShardValidationError> {
    let manifest = read_store_manifest(manifest_path).map_err(|error| {
        ProfileShardValidationError::NonCanonical {
            reason: format!("store manifest is invalid: {error}"),
        }
    })?;
    let invalid = |reason: String| ProfileShardValidationError::NonCanonical { reason };
    if manifest.schema_version != STORE_MANIFEST_SCHEMA_VERSION {
        return Err(invalid(format!(
            "manifest schema must be {STORE_MANIFEST_SCHEMA_VERSION}, found {}",
            manifest.schema_version
        )));
    }
    if manifest.project_id.as_deref() != Some(project_id) {
        return Err(invalid(
            "manifest project id does not match the registry".to_string(),
        ));
    }
    if manifest.store_kind != StoreKind::CodeProject {
        return Err(invalid(
            "manifest store kind must be 'code_project'".to_string(),
        ));
    }
    if manifest.storage_mode != StorageMode::ProfileSharded {
        return Err(invalid(
            "manifest storage mode must be 'profile_sharded'".to_string(),
        ));
    }
    if manifest.sessions_db_relpath != Path::new(SESSIONS_DB_FILENAME) {
        return Err(invalid(format!(
            "manifest sessions database path must be '{SESSIONS_DB_FILENAME}'"
        )));
    }
    let manifest_data_root = manifest
        .data_root
        .canonicalize()
        .map_err(|_| invalid("manifest data root is unavailable".to_string()))?;
    if manifest_data_root != store_root {
        return Err(invalid(
            "manifest data root does not match the registered store".to_string(),
        ));
    }
    Ok(())
}

impl StoreManifest {
    pub(crate) fn from_layout(layout: &StoreLayout) -> Self {
        Self {
            schema_version: STORE_MANIFEST_SCHEMA_VERSION,
            project_id: layout.identity.project_id.clone(),
            store_kind: layout.store_kind.clone(),
            storage_mode: layout.storage_mode.clone(),
            project_root: layout.project_root.clone(),
            data_root: layout.data_root.clone(),
            graph_db_relpath: relative_to_data_root(&layout.graph_db_path, &layout.data_root),
            sessions_db_relpath: relative_to_data_root(&layout.sessions_db_path, &layout.data_root),
            branch_meta_relpath: relative_to_data_root(&layout.branch_meta_path, &layout.data_root),
        }
    }
}

impl ActiveProjectContext {
    pub fn new(layout: StoreLayout, scope_id: GraphScopeId) -> Self {
        let query_target = QueryTarget {
            graph_db_path: layout.graph_db_path.clone(),
        };
        Self {
            layout,
            scope_id,
            query_target,
        }
    }
}

impl ProjectPath {
    pub fn resolve(project_root: &Path, path: &Path) -> Result<Self> {
        validate_no_nul(path)?;
        validate_normal_components(path, true)?;
        let root = project_root
            .canonicalize()
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to canonicalize project root '{}': {e}",
                    project_root.display()
                ),
            })?;
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        };
        let absolute_path = candidate
            .canonicalize()
            .map_err(|e| TraceDecayError::Config {
                message: format!(
                    "failed to canonicalize project path '{}': {e}",
                    candidate.display()
                ),
            })?;
        let relative_path = absolute_path
            .strip_prefix(&root)
            .map_err(|_| TraceDecayError::Config {
                message: format!(
                    "path '{}' escapes project root '{}'",
                    path.display(),
                    project_root.display()
                ),
            })?
            .to_path_buf();
        Ok(Self {
            absolute_path,
            relative_path,
        })
    }

    pub fn absolute_path(&self) -> PathBuf {
        self.absolute_path.clone()
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    pub fn relative_path_string(&self) -> String {
        self.relative_path.to_string_lossy().replace('\\', "/")
    }
}

impl StoreArtifactPath {
    pub fn resolve(store_root: &Path, relpath: &Path) -> Result<Self> {
        validate_no_nul(relpath)?;
        validate_normal_components(relpath, false)?;
        if relpath.is_absolute() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "store artifact path '{}' must be relative",
                    relpath.display()
                ),
            });
        }
        let absolute_path = store_root.join(relpath);
        reject_symlink_components(&absolute_path, "store artifact path").map_err(|e| {
            TraceDecayError::Config {
                message: format!("store artifact path '{}' is unsafe: {e}", relpath.display()),
            }
        })?;
        Ok(Self {
            absolute_path,
            relative_path: relpath.to_path_buf(),
        })
    }

    pub fn absolute_path(&self) -> PathBuf {
        self.absolute_path.clone()
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}

impl PrivateStoreIo {
    pub fn create_dir_all(path: &Path) -> io::Result<()> {
        reject_symlink_components(path, "private store directory")?;
        fs::create_dir_all(path)?;
        set_private_dir_permissions(path)
    }

    pub fn write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            Self::create_dir_all(parent)?;
        }
        reject_symlink_components(path, "private store file")?;
        Self::open_private(path, fs::OpenOptions::new().write(true).truncate(true))?
            .write_all(contents)?;
        set_private_file_permissions(path)
    }

    /// Appends one line to the private store `path` while holding the shared
    /// sidecar append lock, so concurrent threads and processes never interleave
    /// partial lines. See [`append_line_locked`] and the sidecar-lock module
    /// note for the read+write-handle rationale.
    pub fn append_line(path: &Path, line: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            Self::create_dir_all(parent)?;
        }
        retry_transient_file_op(|| append_line_locked(path, line, true))
    }

    /// Writes one newline-terminated line to the private store data file with
    /// owner-only permissions. Callers must already hold the sidecar append lock
    /// (see [`append_line_locked`]).
    fn append_line_data(path: &Path, line: &str) -> io::Result<()> {
        reject_symlink_components(path, "private store file")?;
        let mut options = fs::OpenOptions::new();
        options.append(true);
        let mut file = Self::open_private(path, &mut options)?;
        file.write_all(format!("{line}\n").as_bytes())?;
        file.flush()?;
        drop(file);
        set_private_file_permissions(path)
    }

    /// Opens `path` for writing, creating it if missing with owner-only
    /// permissions applied at create time (Unix), so a fresh file never
    /// exists with umask-default permissions before the trailing
    /// `set_private_file_permissions` call. Pre-existing files keep their
    /// mode here and are tightened by that trailing call.
    fn open_private(path: &Path, options: &mut fs::OpenOptions) -> io::Result<fs::File> {
        options.create(true);
        apply_private_create_mode(options);
        options.open(path)
    }

    pub fn write_file_atomically(path: &Path, temp_path: &Path, contents: &[u8]) -> io::Result<()> {
        if path_parent(path) != path_parent(temp_path) {
            return Err(invalid_input(
                "private store atomic write temp path must share the target directory",
            ));
        }
        if path == temp_path {
            return Err(invalid_input(
                "private store atomic write temp path must differ from the target",
            ));
        }
        if let Some(parent) = path.parent() {
            Self::create_dir_all(parent)?;
        }
        reject_symlink_components(path, "private store file")?;
        reject_symlink_components(temp_path, "private store temp file")?;
        fs::write(temp_path, contents)?;
        set_private_file_permissions(temp_path)?;
        crate::db::DatabaseAuthority::replace_file_atomically(
            temp_path,
            path,
            "private store file",
        )
        .map_err(io::Error::other)?;
        set_private_file_permissions(path)
    }

    /// Atomically replaces a private-store file and establishes the durability
    /// barrier required before a destructive operation may trust it.
    pub fn write_file_atomically_durable(
        path: &Path,
        temp_path: &Path,
        contents: &[u8],
    ) -> io::Result<()> {
        if path_parent(path) != path_parent(temp_path) || path == temp_path {
            return Err(invalid_input(
                "durable private-store write requires a distinct sibling temp path",
            ));
        }
        if let Some(parent) = path.parent() {
            Self::create_dir_all(parent)?;
        }
        reject_symlink_components(path, "private store file")?;
        reject_symlink_components(temp_path, "private store temp file")?;
        {
            let mut options = fs::OpenOptions::new();
            options.write(true).truncate(true);
            let mut temp = Self::open_private(temp_path, &mut options)?;
            temp.write_all(contents)?;
            temp.sync_all()?;
        }
        inject_durable_atomic_write_fault(DurableAtomicWritePhase::AfterTempSync)?;
        crate::db::DatabaseAuthority::replace_file_atomically(
            temp_path,
            path,
            "private store durable file",
        )
        .map_err(io::Error::other)?;
        set_private_file_permissions(path)?;
        if let Err(error) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .and_then(|()| inject_durable_atomic_write_fault(DurableAtomicWritePhase::AfterRename))
            .and_then(|()| sync_parent_directory(path))
        {
            let _ = fs::remove_file(path);
            let _ = sync_parent_directory(path);
            return Err(error);
        }
        Ok(())
    }

    /// Synchronizes the durable members of one `SQLite` WAL family. The SHM
    /// coordination file is intentionally excluded because `SQLite` rebuilds it.
    pub fn sync_sqlite_family(path: &Path) -> io::Result<()> {
        reject_symlink_components(path, "private SQLite store")?;
        for member in [
            path.to_path_buf(),
            PathBuf::from(format!("{}-wal", path.display())),
        ] {
            match fs::OpenOptions::new().read(true).write(true).open(&member) {
                Ok(file) => file.sync_all()?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        sync_parent_directory(path)
    }

    pub fn copy_artifact(source: &Path, target: &Path) -> io::Result<u64> {
        let meta = source.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            return Err(invalid_input(
                "private store artifact source must not be a symlink",
            ));
        }
        reject_symlink_components(target, "private store artifact target")?;
        if meta.is_dir() {
            return Self::copy_dir(source, target);
        }
        if let Some(parent) = target.parent() {
            Self::create_dir_all(parent)?;
        }
        let bytes = fs::copy(source, target)?;
        set_private_file_permissions(target)?;
        Ok(bytes)
    }

    fn copy_dir(source: &Path, target: &Path) -> io::Result<u64> {
        Self::create_dir_all(target)?;
        let mut bytes = 0;
        let mut entries = fs::read_dir(source)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let source_path = entry.path();
            let target_path = target.join(entry.file_name());
            let meta = source_path.symlink_metadata()?;
            if meta.file_type().is_symlink() {
                return Err(invalid_input(
                    "private store artifact source must not contain symlinks",
                ));
            }
            if meta.is_dir() {
                bytes += Self::copy_dir(&source_path, &target_path)?;
            } else if meta.is_file() {
                bytes += Self::copy_artifact(&source_path, &target_path)?;
            }
        }
        Ok(bytes)
    }
}

fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("private store durable file has no parent directory"))?;
    tracedecay_domain::framed_log::sync_directory(parent, DirectorySyncPolicy::Strict)
}

pub fn reject_symlink_components(path: &Path, subject: &str) -> io::Result<()> {
    let is_absolute = path.is_absolute();
    let mut current = PathBuf::new();
    let mut normal_components = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => {
                current.push(component.as_os_str());
                normal_components += 1;
            }
            Component::RootDir | Component::Prefix(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir | Component::ParentDir => {
                return Err(invalid_input(format!("{subject} path must be normalized")));
            }
        }
        if normal_components == 0 || (is_absolute && normal_components == 1) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(invalid_input(format!(
                    "{subject} path must not contain symlinks"
                )));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => break,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn path_parent(path: &Path) -> &Path {
    path.parent().unwrap_or_else(|| Path::new(""))
}

/// Sibling `<file>.lock` path used to serialize appends without locking the
/// data file's own handle. Shared with the automation run ledger writer.
pub fn append_lock_path(path: &Path) -> PathBuf {
    let mut lock_name = path
        .file_name()
        .map_or_else(|| OsString::from("append"), std::ffi::OsStr::to_os_string);
    lock_name.push(".lock");
    path.with_file_name(lock_name)
}

// ── Cross-process sidecar lock utility ──────────────────────────────
//
// TraceDecay sanctions two cross-process file-coordination strategies; new code
// should reuse one rather than hand-rolling a third:
//
//   1. Sidecar advisory lock (this utility). Open a dedicated `<file>.lock`
//      handle for read+write and hold an `fs2` `flock` on it while mutating the
//      real file. Use it to serialize writers to an append-only log or an
//      mmap/config file where readers must never see a torn write and a crashed
//      holder must not leave a stale marker (the OS drops the lock on process
//      death). Callers: private-store appends, the automation run ledger, the
//      monitor ring buffer and single-instance guard, the structured-backfill
//      sweep, and the user-config save.
//   2. Atomic rename + hash ownership (see `write_file_atomically` and the
//      dashboard curation writers). Write a sibling temp file and `rename` it
//      over the target so readers always observe a whole file, using a content
//      hash to decide the final owner. Use it for whole-file replaces where
//      last-writer-wins is acceptable.
//
// The lock is always taken on a *separate* r/w `<file>.lock` handle, never on
// the data handle. Rust opens append-only handles with
// `FILE_GENERIC_WRITE & !FILE_WRITE_DATA` (no read-data, no write-data), and
// Windows `LockFileEx` requires the handle to carry `FILE_READ_DATA` or
// `FILE_WRITE_DATA`, so locking such a handle fails with `ERROR_ACCESS_DENIED`
// (os error 5). Locking the r/w sidecar sidesteps that and avoids locking the
// data region being written. This rationale lives here once; call sites point
// back to it rather than restating it.

fn open_lock_file(lock_path: &Path, private: bool) -> io::Result<fs::File> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).truncate(false);
    let file = if private {
        PrivateStoreIo::open_private(lock_path, &mut options)?
    } else {
        options.create(true).open(lock_path)?
    };
    if private {
        set_private_file_permissions(lock_path)?;
    }
    Ok(file)
}

/// Non-blocking sidecar lock acquisition. Returns the held lock file on
/// success, or `None` when another process/thread already holds it (the caller
/// then skips its critical section). See the sidecar-lock module note above for
/// the read+write-handle rationale.
pub fn try_acquire_sidecar_lock(lock_path: &Path) -> io::Result<Option<fs::File>> {
    let file = open_lock_file(lock_path, false)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(err) => Err(err),
    }
}

/// Blocking sidecar lock acquisition. Returns the held lock file once the
/// exclusive lock is granted. See the sidecar-lock module note above for the
/// read+write-handle rationale.
pub fn acquire_sidecar_lock_blocking(lock_path: &Path) -> io::Result<fs::File> {
    acquire_lock_file_blocking(lock_path, false)
}

fn acquire_lock_file_blocking(lock_path: &Path, private: bool) -> io::Result<fs::File> {
    let file = open_lock_file(lock_path, private)?;
    file.lock_exclusive()?;
    Ok(file)
}

/// Appends `line` (newline-terminated) to `path` under the shared sidecar
/// append lock. When `private`, the data file is created owner-only and both
/// the data and lock paths are symlink-checked (the private-store contract);
/// otherwise a plain create+append handle is used (the automation run ledger).
pub(crate) fn append_line_locked(path: &Path, line: &str, private: bool) -> io::Result<()> {
    let lock_path = append_lock_path(path);
    if private {
        reject_symlink_components(&lock_path, "private store lock file")?;
    }
    let lock_file = acquire_lock_file_blocking(&lock_path, private)?;
    let write_result = if private {
        PrivateStoreIo::append_line_data(path, line)
    } else {
        append_line_plain(path, line)
    };
    let unlock_result = lock_file.unlock();
    write_result?;
    unlock_result?;
    if private {
        set_private_file_permissions(&lock_path)?;
    }
    Ok(())
}

fn append_line_plain(path: &Path, line: &str) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(format!("{line}\n").as_bytes())?;
    file.flush()
}

/// Runs `op`, retrying a bounded number of times on Windows for the transient
/// file-access error codes that antivirus scanners and delete-pending handle
/// states briefly produce: `ERROR_ACCESS_DENIED` (5), `ERROR_SHARING_VIOLATION`
/// (32), and `ERROR_LOCK_VIOLATION` (33). The retries total well under ~250ms
/// and the final error is always propagated. On non-Windows platforms `op`
/// runs exactly once.
pub fn retry_transient_file_op<F>(mut op: F) -> io::Result<()>
where
    F: FnMut() -> io::Result<()>,
{
    #[cfg(windows)]
    {
        const MAX_ATTEMPTS: u32 = 5;
        let mut attempt: u32 = 1;
        loop {
            match op() {
                Ok(()) => return Ok(()),
                Err(err) if attempt < MAX_ATTEMPTS && is_transient_windows_file_error(&err) => {
                    std::thread::sleep(transient_file_backoff(attempt));
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        }
    }
    #[cfg(not(windows))]
    {
        op()
    }
}

#[cfg(windows)]
fn is_transient_windows_file_error(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(5 | 32 | 33))
}

#[cfg(windows)]
fn transient_file_backoff(attempt: u32) -> std::time::Duration {
    // Base 10, 20, 40, 80 ms (sum 150 ms across the 4 retries) plus a small
    // jitter derived from the wall clock to de-correlate contending writers.
    let base = 10u64 << (attempt - 1);
    let jitter = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()) % u64::from(attempt + 1));
    std::time::Duration::from_millis(base + jitter)
}

fn relative_to_data_root(path: &Path, data_root: &Path) -> PathBuf {
    path.strip_prefix(data_root).unwrap_or(path).to_path_buf()
}

impl StoreLayout {
    fn new(
        identity: ProjectIdentity,
        store_kind: StoreKind,
        storage_mode: StorageMode,
        project_root: PathBuf,
        data_root: PathBuf,
        manifest_filename: Option<&str>,
    ) -> Self {
        let graph_db_path = data_root.join(config::db_filename(&data_root));
        let config_path = data_root.join("config.json");
        let branch_meta_path = data_root.join(BRANCH_META_FILENAME);
        let sessions_db_path = data_root.join(SESSIONS_DB_FILENAME);
        let response_handle_root = data_root.join("response-handles");
        let lcm_payload_root = data_root.join("lcm-payloads");
        let dashboard_root = data_root.join("dashboard");
        let manifest_path = manifest_filename.map(|filename| data_root.join(filename));
        let dirty_path = data_root.join("dirty");
        let sync_lock_path = data_root.join("sync.lock");
        let branch_add_lock_path = data_root.join(".branch-add.lock");
        Self {
            identity,
            store_kind,
            storage_mode,
            project_root,
            data_root,
            graph_db_path,
            config_path,
            branch_meta_path,
            sessions_db_path,
            response_handle_root,
            lcm_payload_root,
            dashboard_root,
            manifest_path,
            dirty_path,
            sync_lock_path,
            branch_add_lock_path,
        }
    }
}

fn validate_enrollment_marker(marker: &EnrollmentMarker, path: &Path) -> Result<()> {
    validate_project_id(&marker.project_id).map_err(|message| TraceDecayError::Config {
        message: format!("invalid enrollment marker '{}': {message}", path.display()),
    })
}

pub fn validate_project_id(project_id: &str) -> std::result::Result<(), &'static str> {
    if project_id.is_empty() {
        return Err("project_id must not be empty");
    }
    if project_id.starts_with('.')
        || project_id.contains('/')
        || project_id.contains('\\')
        || project_id.contains("..")
    {
        return Err("project_id must be a single safe path segment");
    }
    if !project_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err("project_id contains unsupported characters");
    }
    Ok(())
}

fn validate_no_nul(path: &Path) -> Result<()> {
    if path.to_string_lossy().contains('\0') {
        return Err(TraceDecayError::Config {
            message: format!("path '{}' contains a NUL byte", path.display()),
        });
    }
    Ok(())
}

fn validate_normal_components(path: &Path, allow_absolute: bool) -> Result<()> {
    if path.as_os_str().is_empty() || has_current_dir_segment(path) {
        return Err(TraceDecayError::Config {
            message: format!("path '{}' is not normalized", path.display()),
        });
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::RootDir | Component::Prefix(_) if allow_absolute => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(TraceDecayError::Config {
                    message: format!("path '{}' is not normalized", path.display()),
                });
            }
        }
    }
    Ok(())
}

fn has_current_dir_segment(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text == "."
        || text.starts_with("./")
        || text.starts_with(".\\")
        || text.ends_with("/.")
        || text.ends_with("\\.")
        || text.contains("/./")
        || text.contains("\\.\\")
}

#[cfg(unix)]
pub fn set_private_dir_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Keep platform implementations signature-compatible.
pub fn set_private_dir_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(unix)]
fn apply_private_create_mode(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600);
}

#[cfg(not(unix))]
fn apply_private_create_mode(_options: &mut fs::OpenOptions) {}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Keep platform implementations signature-compatible.
fn set_private_file_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::{Arc, Barrier};

    #[test]
    fn repository_marker_keeps_the_existing_store_when_fallback_identity_differs() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("repo");
        let profile_root = dir.path().join("profile");
        fs::create_dir_all(&project_root).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&project_root)
            .status()
            .unwrap();
        assert!(init.success(), "the fixture repository must initialize");

        let fallback_id = default_profile_project_id(&project_root);
        let existing_id = "proj_existing_marker_store";
        assert_ne!(
            fallback_id, existing_id,
            "the fixture must model a changed fallback derivation"
        );
        let existing_store = profile_root.join("projects").join(existing_id);
        fs::create_dir_all(&existing_store).unwrap();
        let sentinel = existing_store.join("existing-store-sentinel");
        fs::write(&sentinel, "do not orphan").unwrap();
        assert!(write_repository_identity_marker(&project_root, existing_id).unwrap());

        let resolved = resolve_layout(&project_root, &profile_root).unwrap();

        assert_eq!(
            resolved.identity.project_id.as_deref(),
            Some(existing_id),
            "persisted repository identity must outrank a newly-derived fallback id"
        );
        assert_eq!(resolved.data_root, existing_store);
        assert!(
            sentinel.is_file(),
            "the selected existing store must stay intact"
        );
    }

    #[test]
    fn append_line_keeps_concurrent_jsonl_writes_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(
            dir.path()
                .canonicalize()
                .unwrap()
                .join("hook_analytics.jsonl"),
        );
        let writers = 8;
        let lines_per_writer = 100;
        let barrier = Arc::new(Barrier::new(writers));
        let mut handles = Vec::new();

        for writer in 0..writers {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for line in 0..lines_per_writer {
                    let payload = serde_json::json!({
                        "event": "hook_invoked",
                        "writer": writer,
                        "line": line,
                        "padding": "x".repeat(4096),
                    });
                    PrivateStoreIo::append_line(&path, &payload.to_string()).unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let contents = std::fs::read_to_string(&*path).unwrap();
        let rows = contents.lines().collect::<Vec<_>>();
        assert_eq!(rows.len(), writers * lines_per_writer);
        for row in rows {
            serde_json::from_str::<Value>(row).unwrap();
        }
        assert!(append_lock_path(&path).is_file());
    }

    #[test]
    #[cfg(unix)]
    fn symlink_guard_skips_leading_system_alias_but_rejects_managed_tail() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        // A normal store path below a possibly symlinked system temp root
        // (macOS /var -> /private/var) must be tolerated.
        let real = root.join("real");
        std::fs::create_dir_all(real.join("store")).unwrap();
        PrivateStoreIo::append_line(&real.join("store").join("f.jsonl"), "{\"n\":1}")
            .expect("normal store path must not be rejected");

        // A symlinked directory is caught when the write path ensures it:
        // the directory is then the checked final component.
        let parent_link = root.join("plink");
        symlink(real.join("store"), &parent_link).unwrap();
        let err = PrivateStoreIo::create_dir_all(&parent_link).unwrap_err();
        assert!(
            err.to_string().contains("must not contain symlinks"),
            "{err}"
        );

        // A symlinked final component is rejected.
        let target = real.join("store").join("h.jsonl");
        std::fs::write(&target, "").unwrap();
        let file_link = real.join("store").join("h-link.jsonl");
        symlink(&target, &file_link).unwrap();
        let err = PrivateStoreIo::append_line(&file_link, "{}").unwrap_err();
        assert!(
            err.to_string().contains("must not contain symlinks"),
            "{err}"
        );
    }

    #[test]
    fn append_line_uses_a_reusable_sidecar_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        // Canonicalize: on macOS the tempdir lives under /var -> /private/var,
        // which the symlink guard would otherwise reject.
        let path = dir.path().canonicalize().unwrap().join("ledger.jsonl");
        let lock_path = append_lock_path(&path);
        assert_eq!(lock_path.file_name().unwrap(), "ledger.jsonl.lock");

        PrivateStoreIo::append_line(&path, "{\"n\":1}").unwrap();
        assert!(lock_path.is_file(), "sidecar lock file should be created");

        // A second append reuses the same sidecar and never locks the data
        // handle, so it must succeed and leave both entries intact.
        PrivateStoreIo::append_line(&path, "{\"n\":2}").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 2);
        assert!(lock_path.is_file());
        // The lock file is metadata only; it must not accumulate ledger bytes.
        assert_eq!(std::fs::metadata(&lock_path).unwrap().len(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn private_lock_file_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().canonicalize().unwrap().join("private.lock");
        let file = open_lock_file(&lock_path, true).unwrap();
        drop(file);

        assert_eq!(
            std::fs::metadata(lock_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn append_line_leaves_data_file_writable() {
        let dir = tempfile::tempdir().unwrap();
        // Canonicalize: on macOS the tempdir lives under /var -> /private/var,
        // which the symlink guard would otherwise reject.
        let path = dir.path().canonicalize().unwrap().join("perms.jsonl");

        PrivateStoreIo::append_line(&path, "{\"a\":1}").unwrap();
        PrivateStoreIo::append_line(&path, "{\"a\":2}").unwrap();

        let meta = std::fs::metadata(&path).unwrap();
        // Guards against any Windows FILE_ATTRIBUTE_READONLY regression and any
        // Unix mode regression that would strip the owner write bit.
        assert!(
            !meta.permissions().readonly(),
            "appended data file must stay writable"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o600,
                "private data file must retain owner-only 0o600 permissions"
            );
        }

        // The file must still be openable for a further append after the cycle.
        PrivateStoreIo::append_line(&path, "{\"a\":3}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
    }
}

/// The Bugbot review asked whether the two id paths can disagree about one
/// repository: `default_profile_project_id` hashes what
/// `repository_identity_root` returns, while the primary-checkout fallback
/// hashes an explicitly canonicalized path. These exercise the ways a caller
/// can hand in a path that is spelled differently from its canonical form.
#[cfg(test)]
mod identity_root_canonicalization_tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    /// A repository with one linked worktree, returned as (primary, linked).
    fn repository(temp: &Path) -> (PathBuf, PathBuf) {
        let primary = temp.join("primary");
        fs::create_dir_all(&primary).expect("create primary");
        git(&primary, &["init", "--initial-branch=main"]);
        git(&primary, &["config", "user.email", "test@example.com"]);
        git(&primary, &["config", "user.name", "test"]);
        fs::write(primary.join("file.txt"), "x").expect("seed file");
        git(&primary, &["add", "file.txt"]);
        git(&primary, &["commit", "-m", "seed"]);

        let linked = temp.join("linked");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "linked",
                linked.to_str().expect("utf-8 path"),
            ],
        );
        (primary, linked)
    }

    #[test]
    fn a_linked_worktree_and_its_primary_checkout_agree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, linked) = repository(temp.path());
        assert_eq!(
            default_profile_project_id(&primary),
            default_profile_project_id(&linked),
        );
    }

    #[test]
    fn a_trailing_separator_does_not_change_the_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, linked) = repository(temp.path());
        let expected = default_profile_project_id(&primary);

        for root in [&primary, &linked] {
            let mut spelled = root.as_os_str().to_os_string();
            spelled.push("/");
            assert_eq!(
                default_profile_project_id(Path::new(&spelled)),
                expected,
                "trailing separator changed the id for {}",
                root.display()
            );
        }
    }

    #[test]
    fn a_dot_dot_segment_does_not_change_the_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, linked) = repository(temp.path());
        let expected = default_profile_project_id(&primary);

        for root in [&primary, &linked] {
            let name = root.file_name().expect("checkout name");
            let indirect = root.join("..").join(name);
            assert_eq!(
                default_profile_project_id(&indirect),
                expected,
                "a .. segment changed the id for {}",
                root.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_checkout_does_not_change_the_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, linked) = repository(temp.path());
        let expected = default_profile_project_id(&primary);

        for (root, link_name) in [(&primary, "primary-link"), (&linked, "linked-link")] {
            let link = temp.path().join(link_name);
            std::os::unix::fs::symlink(root, &link).expect("create symlink");
            assert_eq!(
                default_profile_project_id(&link),
                expected,
                "a symlinked spelling changed the id for {}",
                root.display()
            );
        }
    }

    #[test]
    fn a_subdirectory_is_not_absorbed_into_the_repository() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, _linked) = repository(temp.path());
        let nested = primary.join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        assert_ne!(
            default_profile_project_id(&nested),
            default_profile_project_id(&primary),
        );
    }
}
