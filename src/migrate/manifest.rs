use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::migrate::inventory::{MigrationInventory, StoreStatus};
use crate::migrate::registry::{
    reconstruct_registry_from_store_manifest, RegistryReconstructionReport,
};
use crate::storage::{profile_sharded_data_root, validate_project_id};

pub const MIGRATION_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationManifest {
    pub migration_id: String,
    pub schema_version: u32,
    pub tracedecay_version: String,
    pub created_at_unix: i64,
    pub confirmation_token: String,
    pub command_args: Vec<String>,
    pub env_overrides: Vec<String>,
    pub source: MigrationEndpoint,
    pub destination: MigrationDestination,
    pub validation_summaries: Vec<String>,
    pub protocol: MigrationProtocol,
    pub inventory: MigrationInventory,
    pub artifacts: Vec<MigrationArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationProtocol {
    pub manifest_path: PathBuf,
    pub temp_manifest_path: PathBuf,
    pub lock_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactState {
    Planned,
    Locked,
    Copied,
    Verified,
    Applied,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationArtifact {
    pub kind: String,
    pub source_path: PathBuf,
    pub target_path: Option<PathBuf>,
    pub state: ArtifactState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationEndpoint {
    pub project_root: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationDestination {
    pub profile_root: Option<PathBuf>,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreArtifactPath {
    pub root: PathBuf,
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreArtifactPathValidationError {
    PathTraversal,
    NonNormalComponent,
    NulByte,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlanOptions {
    pub manifest_path: PathBuf,
    pub migration_id: String,
    pub tracedecay_version: String,
    pub created_at_unix: i64,
    pub confirmation_token: String,
    pub target_profile_root: PathBuf,
    pub project_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationVerifyReport {
    pub migration_id: String,
    pub artifact_count: usize,
    pub planned_targets: usize,
    pub missing_targets: usize,
    pub store_manifest_count: usize,
    pub registry_plan_count: usize,
    pub apply_supported: bool,
    pub registry_reconstruction: RegistryReconstructionReport,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactStateTransitionError {
    from: ArtifactState,
    to: ArtifactState,
}

impl MigrationManifest {
    pub fn new(
        migration_id: impl Into<String>,
        tracedecay_version: impl Into<String>,
        created_at_unix: i64,
        confirmation_token: impl Into<String>,
        protocol: MigrationProtocol,
        inventory: MigrationInventory,
    ) -> Self {
        let migration_id = migration_id.into();
        let confirmation_token = confirmation_token.into();
        Self {
            migration_id,
            schema_version: MIGRATION_MANIFEST_SCHEMA_VERSION,
            tracedecay_version: tracedecay_version.into(),
            created_at_unix,
            confirmation_token,
            command_args: Vec::new(),
            env_overrides: Vec::new(),
            source: MigrationEndpoint::default(),
            destination: MigrationDestination::default(),
            validation_summaries: Vec::new(),
            protocol,
            inventory,
            artifacts: Vec::new(),
        }
    }
}

pub fn save_manifest(manifest: &MigrationManifest) -> io::Result<()> {
    if manifest.confirmation_token.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "confirmation_token is required before saving a migration manifest",
        ));
    }
    let protocol = &manifest.protocol;
    if let Some(parent) = protocol.manifest_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&protocol.lock_path, manifest.migration_id.as_bytes())?;
    let bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    fs::write(&protocol.temp_manifest_path, bytes)?;
    fs::rename(&protocol.temp_manifest_path, &protocol.manifest_path)?;
    let _ = fs::remove_file(&protocol.lock_path);
    Ok(())
}

pub fn load_manifest(path: impl AsRef<Path>) -> io::Result<MigrationManifest> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub fn build_plan_manifest(
    inventory: MigrationInventory,
    options: MigrationPlanOptions,
) -> std::result::Result<MigrationManifest, String> {
    validate_project_id(&options.project_id)
        .map_err(|message| format!("invalid project_id '{}': {message}", options.project_id))?;
    if inventory.stores.len() != 1 {
        return Err("migration planning currently supports exactly one store".to_string());
    }
    let store = inventory
        .stores
        .first()
        .ok_or_else(|| "migration inventory did not include a store".to_string())?;
    if store
        .statuses
        .iter()
        .any(|status| !matches!(status, StoreStatus::Ok))
    {
        return Err(format!(
            "store '{}' is not safe to plan: {:?}",
            store.data_dir.display(),
            store.statuses
        ));
    }
    let protocol = MigrationProtocol::for_manifest(&options.manifest_path, &options.migration_id);
    let confirmation_token = if options.confirmation_token.is_empty() {
        format!("confirm-{}", options.migration_id)
    } else {
        options.confirmation_token
    };
    let mut manifest = MigrationManifest::new(
        options.migration_id,
        options.tracedecay_version,
        options.created_at_unix,
        confirmation_token,
        protocol,
        inventory,
    );
    let store = manifest
        .inventory
        .stores
        .first()
        .ok_or_else(|| "migration inventory did not include a store".to_string())?;
    let target_root = profile_sharded_data_root(&options.target_profile_root, &options.project_id);
    manifest.source = MigrationEndpoint {
        project_root: Some(store.project_root.clone()),
        data_dir: Some(store.data_dir.clone()),
    };
    manifest.destination = MigrationDestination {
        profile_root: Some(options.target_profile_root),
        project_id: Some(options.project_id),
    };
    for artifact in &store.artifacts {
        let relpath = artifact_relative_path(&artifact.path, &store.data_dir)?;
        manifest.artifacts.push(MigrationArtifact::new(
            artifact.kind.clone(),
            artifact.path.clone(),
            Some(target_root.join(relpath)),
        ));
    }
    Ok(manifest)
}

pub fn verify_migration_manifest(manifest: &MigrationManifest) -> MigrationVerifyReport {
    let planned_targets = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.target_path.is_some())
        .count();
    let missing_targets = manifest
        .artifacts
        .iter()
        .filter(|artifact| {
            artifact
                .target_path
                .as_ref()
                .is_some_and(|target| !target.exists())
        })
        .count();
    let mut registry_reconstruction = RegistryReconstructionReport::default();
    let mut store_manifest_count = 0;

    for artifact in manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.kind == "store_manifest")
    {
        let Some(path) = artifact
            .target_path
            .as_ref()
            .filter(|target| target.exists())
            .or_else(|| {
                artifact
                    .target_path
                    .is_none()
                    .then_some(&artifact.source_path)
                    .filter(|source| source.exists())
            })
        else {
            continue;
        };
        let Some(profile_root) = infer_profile_root_from_store_manifest(path) else {
            registry_reconstruction.issues.push(format!(
                "could not infer profile root for store manifest '{}'",
                path.display()
            ));
            continue;
        };
        store_manifest_count += 1;
        let report = reconstruct_registry_from_store_manifest(
            path,
            &profile_root,
            crate::tracedecay::current_timestamp(),
        );
        registry_reconstruction.plans.extend(report.plans);
        registry_reconstruction.issues.extend(report.issues);
    }

    let issues = registry_reconstruction.issues.clone();
    MigrationVerifyReport {
        migration_id: manifest.migration_id.clone(),
        artifact_count: manifest.artifacts.len(),
        planned_targets,
        missing_targets,
        store_manifest_count,
        registry_plan_count: registry_reconstruction.plans.len(),
        apply_supported: false,
        registry_reconstruction,
        issues,
    }
}

fn infer_profile_root_from_store_manifest(path: &Path) -> Option<PathBuf> {
    let data_root = path.parent()?;
    let projects_root = data_root.parent()?;
    if projects_root.file_name()? != "projects" {
        return None;
    }
    projects_root.parent().map(PathBuf::from)
}

fn artifact_relative_path(path: &Path, data_dir: &Path) -> std::result::Result<PathBuf, String> {
    if let Ok(relpath) = path.strip_prefix(data_dir) {
        return Ok(relpath.to_path_buf());
    }
    path.file_name()
        .map(PathBuf::from)
        .ok_or_else(|| format!("artifact '{}' has no file name", path.display()))
}

impl MigrationProtocol {
    pub fn for_manifest(manifest_path: impl AsRef<Path>, migration_id: &str) -> Self {
        let manifest_path = manifest_path.as_ref().to_path_buf();
        let file_name = manifest_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("migration-manifest.json");
        let parent = manifest_path.parent().unwrap_or_else(|| Path::new(""));
        Self {
            temp_manifest_path: parent.join(format!(".{file_name}.{migration_id}.tmp")),
            lock_path: parent.join(format!("{file_name}.lock")),
            manifest_path,
        }
    }
}

impl MigrationArtifact {
    pub fn new(
        kind: impl Into<String>,
        source_path: PathBuf,
        target_path: Option<PathBuf>,
    ) -> Self {
        Self {
            kind: kind.into(),
            source_path,
            target_path,
            state: ArtifactState::Planned,
        }
    }

    pub fn transition_to(
        &mut self,
        next: ArtifactState,
    ) -> std::result::Result<(), ArtifactStateTransitionError> {
        if self.state.can_transition_to(&next) {
            self.state = next;
            Ok(())
        } else {
            Err(ArtifactStateTransitionError {
                from: self.state.clone(),
                to: next,
            })
        }
    }
}

impl StoreArtifactPath {
    pub fn from_relative(
        root: &Path,
        relative_path: &Path,
        size_bytes: u64,
    ) -> std::result::Result<Self, StoreArtifactPathValidationError> {
        validate_artifact_relpath(relative_path)?;
        let absolute_path = root.join(relative_path);
        reject_symlink_components(root, relative_path)?;
        Ok(Self {
            root: root.to_path_buf(),
            relative_path: relative_path.to_path_buf(),
            absolute_path,
            size_bytes,
        })
    }
}

impl ArtifactState {
    fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::Planned, Self::Locked)
                | (Self::Locked, Self::Copied)
                | (Self::Copied, Self::Verified)
                | (Self::Verified, Self::Applied)
                | (
                    Self::Planned | Self::Locked | Self::Copied | Self::Verified,
                    Self::Failed
                )
        )
    }
}

impl fmt::Display for ArtifactStateTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid migration artifact state transition from {:?} to {:?}",
            self.from, self.to
        )
    }
}

impl std::error::Error for ArtifactStateTransitionError {}

fn validate_artifact_relpath(
    relative_path: &Path,
) -> std::result::Result<(), StoreArtifactPathValidationError> {
    if relative_path.to_string_lossy().contains('\0') {
        return Err(StoreArtifactPathValidationError::NulByte);
    }
    if relative_path.is_absolute() {
        return Err(StoreArtifactPathValidationError::PathTraversal);
    }
    for component in relative_path.components() {
        match component {
            Component::Normal(_) => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(StoreArtifactPathValidationError::PathTraversal);
            }
            Component::CurDir => return Err(StoreArtifactPathValidationError::NonNormalComponent),
        }
    }
    Ok(())
}

fn reject_symlink_components(
    root: &Path,
    relative_path: &Path,
) -> std::result::Result<(), StoreArtifactPathValidationError> {
    let mut current = root.to_path_buf();
    for component in relative_path.components() {
        current.push(component.as_os_str());
        if current
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_symlink())
        {
            return Err(StoreArtifactPathValidationError::Symlink);
        }
    }
    Ok(())
}
