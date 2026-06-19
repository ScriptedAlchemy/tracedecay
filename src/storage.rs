use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{self, TRACEDECAY_DIR};
use crate::errors::{Result, TraceDecayError};

pub const ENROLLMENT_FILENAME: &str = "enrollment.json";
pub const STORE_MANIFEST_FILENAME: &str = "store_manifest.json";
pub const SESSIONS_DB_FILENAME: &str = "sessions.db";
pub const BRANCH_META_FILENAME: &str = "branch-meta.json";
pub const STORE_MANIFEST_SCHEMA_VERSION: u32 = 1;

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
    HermesProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentMarker {
    pub project_id: String,
    pub storage_mode: StorageMode,
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

pub fn project_local_layout(project_root: &Path) -> StoreLayout {
    let data_root = config::get_tracedecay_dir(project_root);
    StoreLayout::new(
        ProjectIdentity {
            project_id: None,
            display_root: project_root.to_path_buf(),
            primary_alias: project_root.to_path_buf(),
        },
        StoreKind::CodeProject,
        StorageMode::ProjectLocal,
        project_root.to_path_buf(),
        data_root,
        None,
    )
}

pub fn profile_sharded_data_root(profile_root: &Path, project_id: &str) -> PathBuf {
    profile_root.join("projects").join(project_id)
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
    match read_enrollment_marker(project_root)? {
        Some(marker) if marker.storage_mode == StorageMode::ProfileSharded => {
            profile_sharded_layout(project_root, profile_root, &marker)
        }
        Some(_) | None => Ok(project_local_layout(project_root)),
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
        Some(_) | None => Ok(project_local_layout(project_root)),
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
    if let Some(parent) = path.parent() {
        PrivateStoreIo::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to create store manifest directory '{}': {e}",
                parent.display()
            ),
        })?;
    }
    let manifest = StoreManifest::from_layout(layout);
    let text = serde_json::to_string_pretty(&manifest).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to serialize store manifest '{}': {e}",
            path.display()
        ),
    })?;
    PrivateStoreIo::write_file(path, text.as_bytes()).map_err(|e| TraceDecayError::Config {
        message: format!("failed to write store manifest '{}': {e}", path.display()),
    })?;
    Ok(manifest)
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
        fs::write(path, contents)?;
        set_private_file_permissions(path)
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
        fs::rename(temp_path, path)?;
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
    let mut current = PathBuf::new();
    let mut has_normal_component = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => {
                current.push(component.as_os_str());
                has_normal_component = true;
            }
            Component::RootDir | Component::Prefix(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir | Component::ParentDir => {
                return Err(invalid_input(format!("{subject} path must be normalized")));
            }
        }
        if !has_normal_component {
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
fn set_private_dir_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
