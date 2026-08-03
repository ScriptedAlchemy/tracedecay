use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{self, TRACEDECAY_DIR};
use crate::errors::{Result, TraceDecayError};

pub const ENROLLMENT_FILENAME: &str = "enrollment.json";
pub const STORE_MANIFEST_FILENAME: &str = "store_manifest.json";
pub const IDENTITY_CUTOVER_BACKUP_MANIFEST_FILENAME: &str =
    "store_manifest.identity-cutover-backup.json";
pub const SESSIONS_DB_FILENAME: &str = "sessions.db";
pub const BRANCH_META_FILENAME: &str = "branch-meta.json";
pub const REPOSITORY_IDENTITY_FILENAME: &str = "tracedecay-project.json";
/// Filename prefix for corrupt `branch-meta.json` files renamed out of the
/// way by the post-update health pass (`branch-meta.json.corrupt-<timestamp>`).
pub const BRANCH_META_QUARANTINE_PREFIX: &str = "branch-meta.json.corrupt-";
pub const STORE_MANIFEST_SCHEMA_VERSION: u32 = 1;
pub const REPOSITORY_IDENTITY_SCHEMA_VERSION: u32 = 1;

/// Checks the fixed 16-byte `SQLite` header without opening the database.
///
/// This is deliberately file-only: libsql may create or rewrite WAL/SHM
/// sidecars before reporting that the main file is not a database. Recovery
/// paths use this preflight to preserve the complete on-disk recovery set.
pub(crate) fn has_sqlite_database_header(path: &Path) -> io::Result<bool> {
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
    PrivateStoreIo::write_file(&path, &text).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to write enrollment marker '{}': {e}",
            path.display()
        ),
    })
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

pub fn repository_identity_path(project_root: &Path) -> Option<PathBuf> {
    if crate::worktree::is_detached_linked_worktree(project_root) {
        return None;
    }
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
    if stored_key != current_key && stored_common_dir.exists() {
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

pub fn default_profile_project_id(project_root: &Path) -> String {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("proj_{}", &digest[..16])
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

pub(crate) fn resolve_persisted_layout(
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

/// Finds pre-repository-identity profile stores that were keyed by an older
/// path-derived project id but still name this exact local checkout, or one of
/// its linked worktrees, in their manifest. Remote URLs are deliberately not
/// considered: two clones of one remote are different local identities.
pub(crate) fn matching_legacy_profile_layouts(
    project_root: &Path,
    profile_root: &Path,
    excluded_project_id: Option<&str>,
) -> Result<(Vec<StoreLayout>, bool)> {
    matching_legacy_profile_layouts_with_git_resolver(
        project_root,
        profile_root,
        excluded_project_id,
        crate::worktree::is_detached_linked_worktree,
        crate::worktree::git_common_dir,
    )
}

fn matching_legacy_profile_layouts_with_git_resolver<D, G>(
    project_root: &Path,
    profile_root: &Path,
    excluded_project_id: Option<&str>,
    mut is_detached_linked_worktree: D,
    mut git_common_dir: G,
) -> Result<(Vec<StoreLayout>, bool)>
where
    D: FnMut(&Path) -> bool,
    G: FnMut(&Path) -> Option<PathBuf>,
{
    let projects_root = profile_root.join("projects");
    let Ok(entries) = fs::read_dir(&projects_root) else {
        return Ok((Vec::new(), false));
    };
    let mut manifest_paths = entries
        .flatten()
        .map(|entry| entry.path().join(STORE_MANIFEST_FILENAME))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    manifest_paths.sort();

    let mut exact_manifests = Vec::new();
    let mut non_exact_manifests = Vec::new();
    let mut selected_manifest_matches_exact_root = false;
    for manifest_path in manifest_paths {
        let Ok(manifest) = read_store_manifest(&manifest_path) else {
            continue;
        };
        let exact_root = same_local_path(&manifest.project_root, project_root);
        if manifest.project_id.is_some() && manifest.project_id.as_deref() == excluded_project_id {
            selected_manifest_matches_exact_root |= exact_root;
            continue;
        }
        if exact_root {
            exact_manifests.push((manifest_path, manifest));
            continue;
        }
        non_exact_manifests.push((manifest_path, manifest));
    }

    // A linked worktree may have its own profile shard while sharing a Git
    // common directory with every sibling checkout. A non-excluded exact
    // manifest overrides the selected identity. Otherwise the shared-Git
    // recovery path still runs, and the caller decides whether a selected
    // identity naming this exact checkout outranks what it finds.
    let selected_is_sole_exact_root =
        selected_manifest_matches_exact_root && exact_manifests.is_empty();
    let matching_manifests = if exact_manifests.is_empty() {
        let project_git_common_dir = (!is_detached_linked_worktree(project_root))
            .then(|| git_common_dir(project_root))
            .flatten();
        let mut legacy_git_common_dirs = HashMap::<PathBuf, Option<PathBuf>>::new();
        non_exact_manifests
            .into_iter()
            .filter(|(_, manifest)| {
                project_git_common_dir.as_deref().is_some_and(|current| {
                    legacy_git_common_dirs
                        .entry(manifest.project_root.clone())
                        .or_insert_with(|| {
                            manifest
                                .project_root
                                .is_dir()
                                .then(|| git_common_dir(&manifest.project_root))
                                .flatten()
                        })
                        .as_deref()
                        .is_some_and(|legacy| same_local_path(legacy, current))
                })
            })
            .collect()
    } else {
        exact_manifests
    };
    let mut layouts = Vec::new();
    for (manifest_path, manifest) in matching_manifests {
        let project_id = manifest
            .project_id
            .as_deref()
            .ok_or_else(|| invalid_legacy_manifest(&manifest_path, "project_id is missing"))?;
        validate_project_id(project_id)
            .map_err(|message| invalid_legacy_manifest(&manifest_path, message))?;
        if manifest.schema_version != STORE_MANIFEST_SCHEMA_VERSION
            || manifest.store_kind != StoreKind::CodeProject
            || manifest.storage_mode != StorageMode::ProfileSharded
        {
            return Err(invalid_legacy_manifest(
                &manifest_path,
                "unsupported schema, store kind, or storage mode",
            ));
        }

        let layout = profile_sharded_layout(
            project_root,
            profile_root,
            &EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: StorageMode::ProfileSharded,
            },
        )?;
        let manifest_data_root = manifest
            .data_root
            .canonicalize()
            .unwrap_or_else(|_| manifest.data_root.clone());
        let layout_data_root = layout
            .data_root
            .canonicalize()
            .unwrap_or_else(|_| layout.data_root.clone());
        if manifest_path.parent() != Some(manifest.data_root.as_path())
            || manifest_data_root != layout_data_root
            || manifest.data_root.join(&manifest.graph_db_relpath) != layout.graph_db_path
            || manifest.data_root.join(&manifest.sessions_db_relpath) != layout.sessions_db_path
            || manifest.data_root.join(&manifest.branch_meta_relpath) != layout.branch_meta_path
        {
            return Err(invalid_legacy_manifest(
                &manifest_path,
                "manifest paths do not match the profile shard layout",
            ));
        }
        layouts.push(layout);
    }
    Ok((layouts, selected_is_sole_exact_root))
}

pub(crate) fn retire_identity_cutover_manifest(layout: &StoreLayout) -> Result<PathBuf> {
    let source = layout
        .manifest_path
        .as_ref()
        .ok_or_else(|| TraceDecayError::Config {
            message: "profile store has no manifest path".to_string(),
        })?;
    let backup = layout
        .data_root
        .join(IDENTITY_CUTOVER_BACKUP_MANIFEST_FILENAME);
    if !source.exists() && backup.is_file() {
        return Ok(backup);
    }
    if backup.exists() {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace existing identity-cutover backup '{}'",
                backup.display()
            ),
        });
    }
    fs::rename(source, &backup).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to retire empty identity-cutover manifest '{}' to '{}': {error}",
            source.display(),
            backup.display()
        ),
    })?;
    Ok(backup)
}

fn same_local_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn invalid_legacy_manifest(path: &Path, detail: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "legacy profile store manifest '{}' cannot be adopted safely: {detail}",
            path.display()
        ),
    }
}

pub fn default_profile_root() -> Result<PathBuf> {
    config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not resolve user profile data directory".to_string(),
    })
}

pub fn resolve_layout_for_current_profile(project_root: &Path) -> Result<StoreLayout> {
    match read_enrollment_marker(project_root)? {
        Some(marker) if marker.storage_mode == StorageMode::ProfileSharded => {
            let profile_root = default_profile_root()?;
            profile_sharded_layout(project_root, &profile_root, &marker)
        }
        Some(marker) => Err(TraceDecayError::Config {
            message: format!(
                "unsupported storage_mode={:?} in enrollment marker for '{}'; \
                 run TraceDecay migration to move this project into the user profile store",
                marker.storage_mode,
                project_root.display()
            ),
        }),
        None => {
            let profile_root = default_profile_root()?;
            default_profile_sharded_layout(project_root, &profile_root)
        }
    }
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

impl StoreManifest {
    pub fn from_layout(layout: &StoreLayout) -> Self {
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

fn reject_symlink_components(path: &Path, subject: &str) -> io::Result<()> {
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
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| OsString::from("append"));
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
pub(crate) fn try_acquire_sidecar_lock(lock_path: &Path) -> io::Result<Option<fs::File>> {
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
        .map(|d| u64::from(d.subsec_nanos()) % u64::from(attempt + 1))
        .unwrap_or(0);
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

pub(crate) fn validate_project_id(project_id: &str) -> std::result::Result<(), &'static str> {
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
fn set_private_file_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::cell::RefCell;
    use std::sync::{Arc, Barrier};

    #[test]
    fn exact_root_manifest_overrides_shared_git_discovery() {
        fn write_manifest(profile_root: &Path, project_id: &str, project_root: &Path) {
            let data_root = profile_root.join("projects").join(project_id);
            fs::create_dir_all(&data_root).unwrap();
            write_store_manifest_to_path(
                &data_root.join(STORE_MANIFEST_FILENAME),
                &StoreManifest {
                    schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                    project_id: Some(project_id.to_string()),
                    store_kind: StoreKind::CodeProject,
                    storage_mode: StorageMode::ProfileSharded,
                    project_root: project_root.to_path_buf(),
                    data_root,
                    graph_db_relpath: "tracedecay.db".into(),
                    sessions_db_relpath: "sessions.db".into(),
                    branch_meta_relpath: "branch-meta.json".into(),
                },
            )
            .unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("repo");
        let unrelated_root = dir.path().join("unrelated");
        let profile_root = dir.path().join("profile");
        fs::create_dir_all(&project_root).unwrap();
        fs::create_dir_all(&unrelated_root).unwrap();
        write_manifest(&profile_root, "proj_exact", &project_root);
        write_manifest(&profile_root, "proj_unrelated", &unrelated_root);

        let resolver_calls = RefCell::new(Vec::new());
        let (layouts, selected_is_sole_exact_root) =
            matching_legacy_profile_layouts_with_git_resolver(
                &project_root,
                &profile_root,
                None,
                |_| false,
                |root| {
                    resolver_calls.borrow_mut().push(root.to_path_buf());
                    Some(dir.path().join("shared.git"))
                },
            )
            .unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(
            layouts[0].identity.project_id.as_deref(),
            Some("proj_exact")
        );
        assert!(!selected_is_sole_exact_root);
        assert!(
            resolver_calls.borrow().is_empty(),
            "exact-root selection must not invoke shared-Git discovery"
        );

        resolver_calls.borrow_mut().clear();
        let (layouts, selected_is_sole_exact_root) =
            matching_legacy_profile_layouts_with_git_resolver(
                &project_root,
                &profile_root,
                Some("proj_exact"),
                |_| false,
                |root| {
                    resolver_calls.borrow_mut().push(root.to_path_buf());
                    Some(dir.path().join("shared.git"))
                },
            )
            .unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(
            layouts[0].identity.project_id.as_deref(),
            Some("proj_unrelated")
        );
        assert!(
            selected_is_sole_exact_root,
            "the caller decides whether the selected exact root outranks recovery"
        );
        assert_eq!(
            resolver_calls.borrow().as_slice(),
            [project_root, unrelated_root],
            "an excluded selected exact root must retain shared-Git recovery"
        );
    }

    #[test]
    fn exact_root_manifest_without_project_id_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("repo");
        let profile_root = dir.path().join("profile");
        let data_root = profile_root.join("projects").join("legacy-missing-id");
        fs::create_dir_all(&project_root).unwrap();
        fs::create_dir_all(&data_root).unwrap();
        write_store_manifest_to_path(
            &data_root.join(STORE_MANIFEST_FILENAME),
            &StoreManifest {
                schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                project_id: None,
                store_kind: StoreKind::CodeProject,
                storage_mode: StorageMode::ProfileSharded,
                project_root: project_root.clone(),
                data_root,
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();

        let error = matching_legacy_profile_layouts_with_git_resolver(
            &project_root,
            &profile_root,
            None,
            |_| false,
            |_| None,
        )
        .expect_err("missing project_id must fail closed");
        assert!(error.to_string().contains("project_id is missing"));
    }

    #[test]
    fn non_exact_identity_retains_historical_git_discovery() {
        fn write_manifest(profile_root: &Path, project_id: &str, project_root: &Path) {
            let data_root = profile_root.join("projects").join(project_id);
            fs::create_dir_all(&data_root).unwrap();
            write_store_manifest_to_path(
                &data_root.join(STORE_MANIFEST_FILENAME),
                &StoreManifest {
                    schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                    project_id: Some(project_id.to_string()),
                    store_kind: StoreKind::CodeProject,
                    storage_mode: StorageMode::ProfileSharded,
                    project_root: project_root.to_path_buf(),
                    data_root,
                    graph_db_relpath: "tracedecay.db".into(),
                    sessions_db_relpath: "sessions.db".into(),
                    branch_meta_relpath: "branch-meta.json".into(),
                },
            )
            .unwrap();
        }

        let dir = tempfile::tempdir().unwrap();
        let main_root = dir.path().join("repo");
        let worktree_root = dir.path().join("repo-worktree");
        let historical_root = dir.path().join("historical-worktree");
        let profile_root = dir.path().join("profile");
        for root in [&main_root, &worktree_root, &historical_root] {
            fs::create_dir_all(root).unwrap();
        }
        write_manifest(&profile_root, "proj_selected", &main_root);
        write_manifest(&profile_root, "proj_historical", &historical_root);

        let resolver_calls = RefCell::new(Vec::new());
        let (layouts, selected_is_sole_exact_root) =
            matching_legacy_profile_layouts_with_git_resolver(
                &worktree_root,
                &profile_root,
                Some("proj_selected"),
                |_| false,
                |root| {
                    resolver_calls.borrow_mut().push(root.to_path_buf());
                    Some(dir.path().join("shared.git"))
                },
            )
            .unwrap();

        assert_eq!(layouts.len(), 1);
        assert!(!selected_is_sole_exact_root);
        assert_eq!(
            resolver_calls.borrow().as_slice(),
            [worktree_root, historical_root],
            "a selected identity from a sibling root must retain shared-Git recovery"
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
