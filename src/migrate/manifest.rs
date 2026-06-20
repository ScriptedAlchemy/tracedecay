use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use libsql::{Builder, Connection, OpenFlags, Value};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::migrate::inventory::{MigrationInventory, StoreStatus};
use crate::migrate::registry::{
    reconstruct_registry_from_store_manifest, RegistryReconstructionReport,
};
use crate::storage::{
    profile_sharded_data_root, profile_sharded_layout, read_enrollment_marker, read_store_manifest,
    validate_project_id, write_store_manifest, EnrollmentMarker, PrivateStoreIo, StorageMode,
    StoreKind, STORE_MANIFEST_FILENAME,
};

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
    #[serde(default)]
    pub backup_artifacts: Vec<MigrationArtifact>,
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
    pub cutover_ready: bool,
    pub apply_supported: bool,
    pub registry_reconstruction: RegistryReconstructionReport,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationApplyReport {
    pub migration_id: String,
    pub project_root: PathBuf,
    pub profile_root: PathBuf,
    pub project_id: String,
    pub artifact_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationRollbackReport {
    pub migration_id: String,
    pub artifact_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationExportReport {
    pub project_id: String,
    pub source_profile_root: PathBuf,
    pub source_data_root: PathBuf,
    pub target_dir: PathBuf,
    pub artifact_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationCleanupSourcesReport {
    pub migration_id: String,
    pub removed_artifacts: usize,
}

#[derive(Debug, PartialEq, Eq)]
struct SqliteLogicalSummary {
    user_version: i64,
    schema: Vec<String>,
    tables: Vec<SqliteTableSummary>,
}

#[derive(Debug, PartialEq, Eq)]
struct SqliteTableSummary {
    name: String,
    columns: Vec<String>,
    row_count: u64,
    checksum: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationRollbackState {
    NotApplied,
    PartialApply,
    CutoverIncomplete,
    DivergentTargets,
    AppliedReady,
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
            backup_artifacts: Vec::new(),
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
    validate_migration_id(&manifest.migration_id).map_err(|message| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "invalid migration_id '{}': {message}",
                manifest.migration_id
            ),
        )
    })?;
    let protocol = &manifest.protocol;
    validate_protocol_paths(protocol, &manifest.migration_id)?;
    let bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    let mut lock_written = false;
    let result = (|| {
        PrivateStoreIo::write_file(&protocol.lock_path, manifest.migration_id.as_bytes())?;
        lock_written = true;
        PrivateStoreIo::write_file_atomically(
            &protocol.manifest_path,
            &protocol.temp_manifest_path,
            &bytes,
        )
    })();
    if lock_written {
        let cleanup_result = fs::remove_file(&protocol.lock_path);
        if result.is_ok() {
            if let Err(err) = cleanup_result {
                if err.kind() != io::ErrorKind::NotFound {
                    return Err(err);
                }
            }
        }
    }
    result
}

pub fn load_manifest(path: impl AsRef<Path>) -> io::Result<MigrationManifest> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub fn build_plan_manifest(
    inventory: MigrationInventory,
    options: MigrationPlanOptions,
) -> std::result::Result<MigrationManifest, String> {
    validate_migration_id(&options.migration_id)
        .map_err(|message| format!("invalid migration_id '{}': {message}", options.migration_id))?;
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
    let backup_root = options
        .target_profile_root
        .join("migration-backups")
        .join(&manifest.migration_id);
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
            Some(target_root.join(&relpath)),
        ));
        manifest.backup_artifacts.push(MigrationArtifact::new(
            artifact.kind.clone(),
            artifact.path.clone(),
            Some(backup_root.join(relpath)),
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

    let mut issues = registry_reconstruction.issues.clone();
    for artifact in manifest.artifacts.iter().filter(|artifact| {
        matches!(
            artifact.state,
            ArtifactState::Verified | ArtifactState::Applied
        )
    }) {
        if let Err(err) = validate_manifest_artifact_paths(manifest, artifact, false) {
            issues.push(format!(
                "artifact '{}' path validation failed: {err}",
                artifact.kind
            ));
            continue;
        }
        if let Some(target) = artifact.target_path.as_ref() {
            if let Err(err) = verify_artifact_contents(&artifact.source_path, target) {
                issues.push(format!(
                    "artifact '{}' target '{}' does not match source '{}': {err}",
                    artifact.kind,
                    target.display(),
                    artifact.source_path.display()
                ));
            }
        }
    }
    for artifact in manifest
        .backup_artifacts
        .iter()
        .filter(|artifact| artifact.state == ArtifactState::Verified)
    {
        if let Err(err) = validate_manifest_artifact_paths(manifest, artifact, true) {
            issues.push(format!(
                "backup artifact '{}' path validation failed: {err}",
                artifact.kind
            ));
            continue;
        }
        if let Some(target) = artifact.target_path.as_ref() {
            if let Err(err) = verify_artifact_contents(&artifact.source_path, target) {
                issues.push(format!(
                    "backup artifact '{}' target '{}' does not match source '{}': {err}",
                    artifact.kind,
                    target.display(),
                    artifact.source_path.display()
                ));
            }
        }
    }
    let marker_matches = match (
        manifest.source.project_root.as_ref(),
        manifest.destination.project_id.as_ref(),
    ) {
        (Some(project_root), Some(project_id)) => read_enrollment_marker(project_root)
            .ok()
            .flatten()
            .is_some_and(|marker| {
                marker.storage_mode == StorageMode::ProfileSharded
                    && marker.project_id == *project_id
            }),
        _ => false,
    };
    if manifest
        .artifacts
        .iter()
        .all(|artifact| artifact.state == ArtifactState::Applied)
        && !marker_matches
    {
        issues.push("enrollment marker does not match migration destination".to_string());
    }
    let cutover_ready = missing_targets == 0
        && !manifest.artifacts.is_empty()
        && manifest.artifacts.iter().all(|artifact| {
            matches!(
                artifact.state,
                ArtifactState::Verified | ArtifactState::Applied
            )
        })
        && store_manifest_count > 0
        && registry_reconstruction.plans.len() == 1
        && issues.is_empty();
    let apply_supported = cutover_ready
        && manifest
            .artifacts
            .iter()
            .all(|artifact| artifact.state == ArtifactState::Applied)
        && marker_matches;
    MigrationVerifyReport {
        migration_id: manifest.migration_id.clone(),
        artifact_count: manifest.artifacts.len(),
        planned_targets,
        missing_targets,
        store_manifest_count,
        registry_plan_count: registry_reconstruction.plans.len(),
        cutover_ready,
        apply_supported,
        registry_reconstruction,
        issues,
    }
}

pub fn apply_migration_manifest(
    manifest: &mut MigrationManifest,
) -> io::Result<MigrationApplyReport> {
    let (project_root, source_data_dir, profile_root, project_id) = manifest_destination(manifest)?;
    let data_root = profile_sharded_data_root(&profile_root, &project_id);
    let backup_root = profile_root
        .join("migration-backups")
        .join(&manifest.migration_id);
    let original_backup_count = manifest.backup_artifacts.len();
    for index in 0..original_backup_count {
        apply_backup_artifact(manifest, index, &source_data_dir, &backup_root)?;
    }
    let original_artifact_count = manifest.artifacts.len();
    for index in 0..original_artifact_count {
        if manifest.artifacts[index].kind == "store_manifest" {
            continue;
        }
        apply_copy_artifact(manifest, index, &source_data_dir, &data_root)?;
    }
    apply_store_manifest_artifact(manifest, &project_root, &profile_root, &project_id)?;
    let report = verify_migration_manifest(manifest);
    if !report.cutover_ready {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "migration manifest is not ready for cutover after staging: {} missing target(s), {} issue(s)",
                report.missing_targets,
                report.issues.len()
            ),
        ));
    }
    Ok(MigrationApplyReport {
        migration_id: manifest.migration_id.clone(),
        project_root,
        profile_root,
        project_id,
        artifact_count: manifest.artifacts.len(),
    })
}

pub fn finalize_migration_apply(manifest: &mut MigrationManifest) -> io::Result<()> {
    let (project_root, _, _, project_id) = manifest_destination(manifest)?;
    let marker =
        read_enrollment_marker(&project_root).map_err(|err| invalid_manifest(&err.to_string()))?;
    if !matches!(
        marker,
        Some(marker)
            if marker.storage_mode == StorageMode::ProfileSharded
                && marker.project_id == project_id
    ) {
        return Err(invalid_manifest(
            "migration cutover requires an enrollment marker before finalizing apply",
        ));
    }
    let report = verify_migration_manifest(manifest);
    if !report.cutover_ready {
        return Err(invalid_manifest(
            "migration manifest is not ready for cutover finalization",
        ));
    }
    for index in 0..manifest.artifacts.len() {
        if manifest.artifacts[index].state == ArtifactState::Verified {
            transition_and_save(manifest, index, ArtifactState::Applied)?;
        }
    }
    let report = verify_migration_manifest(manifest);
    if !report.apply_supported {
        return Err(invalid_manifest(
            "migration manifest did not verify after cutover finalization",
        ));
    }
    Ok(())
}

pub fn assess_migration_rollback_state(manifest: &MigrationManifest) -> MigrationRollbackState {
    if manifest.artifacts.is_empty()
        || manifest
            .artifacts
            .iter()
            .all(|artifact| artifact.state == ArtifactState::Planned)
    {
        return MigrationRollbackState::NotApplied;
    }
    if manifest.artifacts.iter().any(|artifact| {
        matches!(
            artifact.state,
            ArtifactState::Failed | ArtifactState::Locked | ArtifactState::Copied
        )
    }) || manifest.artifacts.iter().any(|artifact| {
        artifact.state == ArtifactState::Planned
            && manifest.artifacts.iter().any(|other| {
                matches!(
                    other.state,
                    ArtifactState::Verified | ArtifactState::Applied
                )
            })
    }) {
        return MigrationRollbackState::PartialApply;
    }
    if manifest
        .artifacts
        .iter()
        .any(|artifact| artifact.state == ArtifactState::Verified)
    {
        return MigrationRollbackState::CutoverIncomplete;
    }
    if manifest
        .artifacts
        .iter()
        .all(|artifact| artifact.state == ArtifactState::Applied)
    {
        if detect_divergent_applied_targets(manifest).is_some() {
            return MigrationRollbackState::DivergentTargets;
        }
        return MigrationRollbackState::AppliedReady;
    }
    MigrationRollbackState::PartialApply
}

pub fn rollback_migration_manifest(
    manifest: &mut MigrationManifest,
) -> io::Result<MigrationRollbackReport> {
    match assess_migration_rollback_state(manifest) {
        MigrationRollbackState::NotApplied => Err(invalid_manifest(
            "rollback requires an applied manifest; migration has not been applied",
        )),
        MigrationRollbackState::PartialApply => Err(invalid_manifest(
            "rollback rejected: migration is in a partial apply state and must be resumed or repaired manually",
        )),
        MigrationRollbackState::CutoverIncomplete => Err(invalid_manifest(
            "rollback rejected: migration cutover is incomplete; finish apply or remove staged profile-shard artifacts manually",
        )),
        MigrationRollbackState::DivergentTargets | MigrationRollbackState::AppliedReady => Err(invalid_manifest(
            "rollback requires an applied manifest with no divergent target writes; registry rollback state is not available yet",
        )),
    }
}

pub fn export_profile_store(
    profile_root: &Path,
    project_id: &str,
    target_dir: &Path,
) -> io::Result<MigrationExportReport> {
    validate_project_id(project_id).map_err(|message| {
        invalid_manifest(&format!("invalid project_id '{project_id}': {message}"))
    })?;
    let source_data_root = profile_sharded_data_root(profile_root, project_id);
    if target_dir.starts_with(&source_data_root) {
        return Err(invalid_manifest(
            "export target must not be inside the source profile shard",
        ));
    }
    if target_dir.exists() && fs::read_dir(target_dir)?.next().is_some() {
        return Err(invalid_manifest(
            "export target directory already exists and is not empty",
        ));
    }
    let manifest_path = source_data_root.join(STORE_MANIFEST_FILENAME);
    let mut store_manifest =
        read_store_manifest(&manifest_path).map_err(|err| invalid_manifest(&err.to_string()))?;
    if store_manifest.project_id.as_deref() != Some(project_id) {
        return Err(invalid_manifest(
            "profile store manifest project_id does not match requested export",
        ));
    }
    if store_manifest.store_kind != StoreKind::CodeProject
        || store_manifest.storage_mode != StorageMode::ProfileSharded
    {
        return Err(invalid_manifest(
            "only profile-sharded code project stores can be exported",
        ));
    }

    PrivateStoreIo::copy_artifact(&source_data_root, target_dir)?;
    store_manifest.data_root = target_dir.to_path_buf();
    let manifest_bytes = serde_json::to_vec_pretty(&store_manifest).map_err(io::Error::other)?;
    PrivateStoreIo::write_file(&target_dir.join(STORE_MANIFEST_FILENAME), &manifest_bytes)?;

    Ok(MigrationExportReport {
        project_id: project_id.to_string(),
        source_profile_root: profile_root.to_path_buf(),
        source_data_root,
        target_dir: target_dir.to_path_buf(),
        artifact_count: count_store_artifacts(target_dir),
    })
}

pub fn cleanup_migration_sources(
    manifest: &MigrationManifest,
) -> io::Result<MigrationCleanupSourcesReport> {
    if manifest.inventory.stores.len() > 1 {
        return Err(invalid_manifest(
            "cleanup-sources currently supports at most one manifest inventory store",
        ));
    }
    let source_data_dir = manifest
        .source
        .data_dir
        .clone()
        .ok_or_else(|| invalid_manifest("migration manifest has no source data_dir"))?;
    for artifact in &manifest.artifacts {
        if artifact.kind == "store_manifest" || artifact.state != ArtifactState::Applied {
            continue;
        }
        validate_manifest_path_under(
            &artifact.source_path,
            &source_data_dir,
            "cleanup source",
            "source store",
        )?;
    }
    let report = verify_migration_manifest(manifest);
    if !report.apply_supported {
        return Err(invalid_manifest(
            "cleanup-sources requires a verified applied manifest with profile-sharded cutover complete",
        ));
    }
    let mut removed_artifacts = 0;
    for artifact in &manifest.artifacts {
        if artifact.kind == "store_manifest" || artifact.state != ArtifactState::Applied {
            continue;
        }
        validate_manifest_path_under(
            &artifact.source_path,
            &source_data_dir,
            "cleanup source",
            "source store",
        )?;
        if !artifact.source_path.exists() {
            continue;
        }
        let meta = artifact.source_path.symlink_metadata()?;
        if meta.file_type().is_symlink() {
            return Err(invalid_manifest(
                "cleanup-sources refuses to remove symlinked artifacts",
            ));
        }
        if meta.is_dir() {
            fs::remove_dir_all(&artifact.source_path)?;
        } else {
            fs::remove_file(&artifact.source_path)?;
        }
        removed_artifacts += 1;
    }

    Ok(MigrationCleanupSourcesReport {
        migration_id: manifest.migration_id.clone(),
        removed_artifacts,
    })
}

fn manifest_destination(
    manifest: &MigrationManifest,
) -> io::Result<(PathBuf, PathBuf, PathBuf, String)> {
    let project_root = manifest
        .source
        .project_root
        .clone()
        .ok_or_else(|| invalid_manifest("migration manifest has no source project_root"))?;
    let source_data_dir = manifest
        .source
        .data_dir
        .clone()
        .ok_or_else(|| invalid_manifest("migration manifest has no source data_dir"))?;
    let profile_root =
        manifest.destination.profile_root.clone().ok_or_else(|| {
            invalid_manifest("migration manifest has no destination profile_root")
        })?;
    let project_id = manifest
        .destination
        .project_id
        .clone()
        .ok_or_else(|| invalid_manifest("migration manifest has no destination project_id"))?;
    validate_project_id(&project_id).map_err(|message| {
        invalid_manifest(&format!(
            "invalid destination project_id '{project_id}': {message}"
        ))
    })?;
    if manifest.inventory.stores.len() != 1 {
        return Err(invalid_manifest(
            "migrate apply currently supports exactly one manifest inventory store",
        ));
    }
    Ok((project_root, source_data_dir, profile_root, project_id))
}

fn apply_copy_artifact(
    manifest: &mut MigrationManifest,
    index: usize,
    source_data_dir: &Path,
    data_root: &Path,
) -> io::Result<()> {
    let source_path = manifest.artifacts[index].source_path.clone();
    let target_path = manifest.artifacts[index]
        .target_path
        .clone()
        .ok_or_else(|| invalid_manifest("migration artifact has no target_path"))?;
    validate_manifest_path_under(
        &source_path,
        source_data_dir,
        "migration source",
        "source store",
    )?;
    validate_manifest_path_under(&target_path, data_root, "migration target", "profile shard")?;
    if manifest.artifacts[index].state == ArtifactState::Applied
        || manifest.artifacts[index].state == ArtifactState::Verified
    {
        verify_artifact_contents(&source_path, &target_path)?;
        return Ok(());
    }
    if target_path.exists() {
        return Err(invalid_manifest(&format!(
            "migration target '{}' already exists",
            target_path.display()
        )));
    }
    transition_and_save(manifest, index, ArtifactState::Locked)?;
    if let Err(err) = PrivateStoreIo::copy_artifact(&source_path, &target_path) {
        mark_failed(manifest, index)?;
        return Err(io::Error::new(
            err.kind(),
            format!(
                "failed to copy migration artifact '{}' to '{}': {err}",
                source_path.display(),
                target_path.display()
            ),
        ));
    }
    transition_and_save(manifest, index, ArtifactState::Copied)?;
    verify_artifact_contents(&source_path, &target_path)?;
    transition_and_save(manifest, index, ArtifactState::Verified)
}

fn apply_store_manifest_artifact(
    manifest: &mut MigrationManifest,
    project_root: &Path,
    profile_root: &Path,
    project_id: &str,
) -> io::Result<()> {
    let marker = EnrollmentMarker {
        project_id: project_id.to_string(),
        storage_mode: StorageMode::ProfileSharded,
    };
    let layout = profile_sharded_layout(project_root, profile_root, &marker)
        .map_err(|err| invalid_manifest(&err.to_string()))?;
    write_store_manifest(&layout).map_err(|err| invalid_manifest(&err.to_string()))?;
    let manifest_path = layout
        .manifest_path
        .clone()
        .unwrap_or_else(|| layout.data_root.join(STORE_MANIFEST_FILENAME));
    let index = if let Some(index) = manifest
        .artifacts
        .iter()
        .position(|artifact| artifact.kind == "store_manifest")
    {
        manifest.artifacts[index]
            .source_path
            .clone_from(&manifest_path);
        manifest.artifacts[index].target_path = Some(manifest_path.clone());
        manifest.artifacts[index].state = ArtifactState::Planned;
        save_manifest(manifest)?;
        index
    } else {
        manifest.artifacts.push(MigrationArtifact::new(
            "store_manifest",
            manifest_path.clone(),
            Some(manifest_path),
        ));
        save_manifest(manifest)?;
        manifest.artifacts.len() - 1
    };
    transition_and_save(manifest, index, ArtifactState::Locked)?;
    transition_and_save(manifest, index, ArtifactState::Copied)?;
    transition_and_save(manifest, index, ArtifactState::Verified)?;
    Ok(())
}

fn apply_backup_artifact(
    manifest: &mut MigrationManifest,
    index: usize,
    source_data_dir: &Path,
    backup_root: &Path,
) -> io::Result<()> {
    let source_path = manifest.backup_artifacts[index].source_path.clone();
    let target_path = manifest.backup_artifacts[index]
        .target_path
        .clone()
        .ok_or_else(|| invalid_manifest("migration backup artifact has no target_path"))?;
    validate_manifest_path_under(
        &source_path,
        source_data_dir,
        "migration backup source",
        "source store",
    )?;
    validate_manifest_path_under(
        &target_path,
        backup_root,
        "migration backup target",
        "backup root",
    )?;
    if manifest.backup_artifacts[index].state == ArtifactState::Verified {
        verify_artifact_contents(&source_path, &target_path)?;
        return Ok(());
    }
    if target_path.exists() {
        return Err(invalid_manifest(&format!(
            "migration backup target '{}' already exists",
            target_path.display()
        )));
    }
    transition_backup_and_save(manifest, index, ArtifactState::Locked)?;
    if let Err(err) = PrivateStoreIo::copy_artifact(&source_path, &target_path) {
        mark_backup_failed(manifest, index)?;
        return Err(io::Error::new(
            err.kind(),
            format!(
                "failed to back up migration artifact '{}' to '{}': {err}",
                source_path.display(),
                target_path.display()
            ),
        ));
    }
    transition_backup_and_save(manifest, index, ArtifactState::Copied)?;
    verify_artifact_contents(&source_path, &target_path)?;
    transition_backup_and_save(manifest, index, ArtifactState::Verified)
}

fn detect_divergent_applied_targets(manifest: &MigrationManifest) -> Option<String> {
    for artifact in &manifest.artifacts {
        if artifact.kind == "store_manifest" {
            continue;
        }
        let Some(target_path) = artifact.target_path.as_ref() else {
            continue;
        };
        if validate_manifest_artifact_paths(manifest, artifact, false).is_err() {
            return Some(format!(
                "migration target '{}' diverged from source '{}'",
                target_path.display(),
                artifact.source_path.display()
            ));
        }
        if verify_artifact_contents(&artifact.source_path, target_path).is_err() {
            return Some(format!(
                "migration target '{}' diverged from source '{}'",
                target_path.display(),
                artifact.source_path.display()
            ));
        }
    }
    None
}

fn transition_and_save(
    manifest: &mut MigrationManifest,
    index: usize,
    next: ArtifactState,
) -> io::Result<()> {
    manifest.artifacts[index]
        .transition_to(next)
        .map_err(io::Error::other)?;
    save_manifest(manifest)
}

fn transition_backup_and_save(
    manifest: &mut MigrationManifest,
    index: usize,
    next: ArtifactState,
) -> io::Result<()> {
    manifest.backup_artifacts[index]
        .transition_to(next)
        .map_err(io::Error::other)?;
    save_manifest(manifest)
}

fn mark_failed(manifest: &mut MigrationManifest, index: usize) -> io::Result<()> {
    let _ = manifest.artifacts[index].transition_to(ArtifactState::Failed);
    save_manifest(manifest)
}

fn mark_backup_failed(manifest: &mut MigrationManifest, index: usize) -> io::Result<()> {
    let _ = manifest.backup_artifacts[index].transition_to(ArtifactState::Failed);
    save_manifest(manifest)
}

fn verify_artifact_contents(source: &Path, target: &Path) -> io::Result<()> {
    let source_meta = source.symlink_metadata()?;
    let target_meta = target.symlink_metadata()?;
    if source_meta.file_type().is_symlink() || target_meta.file_type().is_symlink() {
        return Err(invalid_manifest("migration artifacts must not be symlinks"));
    }
    if source_meta.is_dir() {
        if !target_meta.is_dir() {
            return Err(invalid_manifest(
                "migration target type differs from source",
            ));
        }
        return verify_directory_contents(source, target);
    }
    if !source_meta.is_file() || !target_meta.is_file() {
        return Err(invalid_manifest("migration artifact is not a regular file"));
    }
    if is_sqlite_database_file(source)? && is_sqlite_database_file(target)? {
        return verify_sqlite_artifact_contents(source, target);
    }
    if fs::read(source)? != fs::read(target)? {
        return Err(invalid_manifest(&format!(
            "migration target '{}' differs from source '{}'",
            target.display(),
            source.display()
        )));
    }
    Ok(())
}

fn is_sqlite_database_file(path: &Path) -> io::Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 16];
    match file.read_exact(&mut header) {
        Ok(()) => Ok(header == *b"SQLite format 3\0"),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(err) => Err(err),
    }
}

fn verify_sqlite_artifact_contents(source: &Path, target: &Path) -> io::Result<()> {
    let source = source.to_path_buf();
    let target = target.to_path_buf();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(io::Error::other)?;
        runtime.block_on(async {
            let source_summary = summarize_sqlite_database(&source).await?;
            let target_summary = summarize_sqlite_database(&target).await?;
            Ok::<_, io::Error>((source, target, source_summary, target_summary))
        })
    });
    let (source, target, source_summary, target_summary) = worker
        .join()
        .map_err(|_| invalid_manifest("SQLite logical verification thread panicked"))??;
    if source_summary != target_summary {
        return Err(invalid_manifest(&format!(
            "SQLite logical verification failed for target '{}' against source '{}'",
            target.display(),
            source.display()
        )));
    }
    Ok(())
}

async fn summarize_sqlite_database(path: &Path) -> io::Result<SqliteLogicalSummary> {
    let db = Builder::new_local(path)
        .flags(OpenFlags::SQLITE_OPEN_READ_ONLY)
        .build()
        .await
        .map_err(|e| {
            invalid_manifest(&format!(
                "failed to open SQLite DB '{}': {e}",
                path.display()
            ))
        })?;
    let conn = db.connect().map_err(|e| {
        invalid_manifest(&format!(
            "failed to connect to SQLite DB '{}': {e}",
            path.display()
        ))
    })?;
    if !sqlite_quick_check(&conn, path).await? {
        return Err(invalid_manifest(&format!(
            "SQLite quick_check failed for '{}'",
            path.display()
        )));
    }
    let user_version = sqlite_i64(&conn, "PRAGMA user_version", path).await?;
    let schema = sqlite_schema_summary(&conn, path).await?;
    let tables = sqlite_table_summaries(&conn, path).await?;
    Ok(SqliteLogicalSummary {
        user_version,
        schema,
        tables,
    })
}

async fn sqlite_quick_check(conn: &Connection, path: &Path) -> io::Result<bool> {
    let mut rows = conn.query("PRAGMA quick_check", ()).await.map_err(|e| {
        invalid_manifest(&format!(
            "failed to run quick_check on '{}': {e}",
            path.display()
        ))
    })?;
    let Some(row) = rows.next().await.map_err(|e| {
        invalid_manifest(&format!(
            "failed to read quick_check result for '{}': {e}",
            path.display()
        ))
    })?
    else {
        return Ok(false);
    };
    let result = row.get::<String>(0).map_err(|e| {
        invalid_manifest(&format!(
            "failed to decode quick_check result for '{}': {e}",
            path.display()
        ))
    })?;
    Ok(result == "ok")
}

async fn sqlite_i64(conn: &Connection, sql: &str, path: &Path) -> io::Result<i64> {
    let mut rows = conn.query(sql, ()).await.map_err(|e| {
        invalid_manifest(&format!(
            "failed to query SQLite metadata for '{}': {e}",
            path.display()
        ))
    })?;
    let Some(row) = rows.next().await.map_err(|e| {
        invalid_manifest(&format!(
            "failed to read SQLite metadata for '{}': {e}",
            path.display()
        ))
    })?
    else {
        return Err(invalid_manifest(&format!(
            "SQLite metadata query returned no rows for '{}'",
            path.display()
        )));
    };
    row.get::<i64>(0).map_err(|e| {
        invalid_manifest(&format!(
            "failed to decode SQLite metadata for '{}': {e}",
            path.display()
        ))
    })
}

async fn sqlite_schema_summary(conn: &Connection, path: &Path) -> io::Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT type, name, tbl_name, COALESCE(sql, '')
             FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name, tbl_name, sql",
            (),
        )
        .await
        .map_err(|e| {
            invalid_manifest(&format!(
                "failed to read SQLite schema for '{}': {e}",
                path.display()
            ))
        })?;
    let mut schema = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| {
        invalid_manifest(&format!(
            "failed to read SQLite schema row for '{}': {e}",
            path.display()
        ))
    })? {
        let name = row.get::<String>(1).map_err(|e| {
            invalid_manifest(&format!(
                "failed to decode SQLite schema name for '{}': {e}",
                path.display()
            ))
        })?;
        if is_fts_shadow_table(&name) {
            continue;
        }
        let entry = format!(
            "{}\x1f{}\x1f{}\x1f{}",
            row.get::<String>(0).map_err(|e| invalid_manifest(&format!(
                "failed to decode SQLite schema type for '{}': {e}",
                path.display()
            )))?,
            name,
            row.get::<String>(2).map_err(|e| invalid_manifest(&format!(
                "failed to decode SQLite schema table for '{}': {e}",
                path.display()
            )))?,
            row.get::<String>(3).map_err(|e| invalid_manifest(&format!(
                "failed to decode SQLite schema SQL for '{}': {e}",
                path.display()
            )))?
        );
        schema.push(entry);
    }
    Ok(schema)
}

async fn sqlite_table_summaries(
    conn: &Connection,
    path: &Path,
) -> io::Result<Vec<SqliteTableSummary>> {
    let mut rows = conn
        .query(
            "SELECT name, COALESCE(sql, '')
             FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
            (),
        )
        .await
        .map_err(|e| {
            invalid_manifest(&format!(
                "failed to list SQLite tables for '{}': {e}",
                path.display()
            ))
        })?;
    let mut tables = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| {
        invalid_manifest(&format!(
            "failed to read SQLite table row for '{}': {e}",
            path.display()
        ))
    })? {
        let name = row.get::<String>(0).map_err(|e| {
            invalid_manifest(&format!(
                "failed to decode SQLite table name for '{}': {e}",
                path.display()
            ))
        })?;
        let sql = row.get::<String>(1).map_err(|e| {
            invalid_manifest(&format!(
                "failed to decode SQLite table SQL for '{}': {e}",
                path.display()
            ))
        })?;
        if is_fts_shadow_table(&name) || is_virtual_table_sql(&sql) {
            continue;
        }
        tables.push(sqlite_table_summary(conn, path, &name).await?);
    }
    Ok(tables)
}

async fn sqlite_table_summary(
    conn: &Connection,
    path: &Path,
    table: &str,
) -> io::Result<SqliteTableSummary> {
    let columns = sqlite_table_columns(conn, path, table).await?;
    let mut checksum = Sha256::new();
    checksum.update(table.as_bytes());
    for column in &columns {
        checksum.update(b"\x1f");
        checksum.update(column.as_bytes());
    }
    let mut row_count = 0_u64;
    if !columns.is_empty() {
        let column_list = columns
            .iter()
            .map(|column| quote_sqlite_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {column_list} FROM {} ORDER BY {column_list}",
            quote_sqlite_identifier(table)
        );
        let mut rows = conn.query(&sql, ()).await.map_err(|e| {
            invalid_manifest(&format!(
                "failed to read SQLite table '{}' from '{}': {e}",
                table,
                path.display()
            ))
        })?;
        while let Some(row) = rows.next().await.map_err(|e| {
            invalid_manifest(&format!(
                "failed to read SQLite row from table '{}' in '{}': {e}",
                table,
                path.display()
            ))
        })? {
            row_count = row_count.saturating_add(1);
            for index in 0..columns.len() {
                let index = i32::try_from(index).map_err(|e| {
                    invalid_manifest(&format!(
                        "too many SQLite columns in table '{}' in '{}': {e}",
                        table,
                        path.display()
                    ))
                })?;
                let value = row.get::<Value>(index).map_err(|e| {
                    invalid_manifest(&format!(
                        "failed to decode SQLite row from table '{}' in '{}': {e}",
                        table,
                        path.display()
                    ))
                })?;
                checksum.update(sqlite_value_fingerprint(value).as_bytes());
                checksum.update(b"\x1e");
            }
        }
    }
    Ok(SqliteTableSummary {
        name: table.to_string(),
        columns,
        row_count,
        checksum: hex::encode(checksum.finalize()),
    })
}

async fn sqlite_table_columns(
    conn: &Connection,
    path: &Path,
    table: &str,
) -> io::Result<Vec<String>> {
    let sql = format!("PRAGMA table_info({})", quote_sqlite_identifier(table));
    let mut rows = conn.query(&sql, ()).await.map_err(|e| {
        invalid_manifest(&format!(
            "failed to inspect SQLite table '{}' in '{}': {e}",
            table,
            path.display()
        ))
    })?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| {
        invalid_manifest(&format!(
            "failed to read SQLite table info for '{}' in '{}': {e}",
            table,
            path.display()
        ))
    })? {
        columns.push(row.get::<String>(1).map_err(|e| {
            invalid_manifest(&format!(
                "failed to decode SQLite column name for '{}' in '{}': {e}",
                table,
                path.display()
            ))
        })?);
    }
    Ok(columns)
}

fn is_fts_shadow_table(name: &str) -> bool {
    name.contains("_fts_")
}

fn is_virtual_table_sql(sql: &str) -> bool {
    sql.to_ascii_uppercase().contains("VIRTUAL TABLE")
}

fn quote_sqlite_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn sqlite_value_fingerprint(value: Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Integer(value) => format!("integer:{value}"),
        Value::Real(value) => format!("real:{:016x}", value.to_bits()),
        Value::Text(value) => format!("text:{}:{value}", value.len()),
        Value::Blob(value) => format!("blob:{}:{}", value.len(), hex::encode(value)),
    }
}

fn verify_directory_contents(source: &Path, target: &Path) -> io::Result<()> {
    let mut source_entries = fs::read_dir(source)?.collect::<io::Result<Vec<_>>>()?;
    let mut target_entries = fs::read_dir(target)?.collect::<io::Result<Vec<_>>>()?;
    source_entries.sort_by_key(std::fs::DirEntry::file_name);
    target_entries.sort_by_key(std::fs::DirEntry::file_name);
    let source_names = source_entries
        .iter()
        .map(std::fs::DirEntry::file_name)
        .collect::<Vec<_>>();
    let target_names = target_entries
        .iter()
        .map(std::fs::DirEntry::file_name)
        .collect::<Vec<_>>();
    if source_names != target_names {
        return Err(invalid_manifest(&format!(
            "migration target directory '{}' differs from source '{}'",
            target.display(),
            source.display()
        )));
    }
    for entry in source_entries {
        verify_artifact_contents(&entry.path(), &target.join(entry.file_name()))?;
    }
    Ok(())
}

fn validate_manifest_artifact_paths(
    manifest: &MigrationManifest,
    artifact: &MigrationArtifact,
    backup: bool,
) -> io::Result<()> {
    let source_data_dir = manifest
        .source
        .data_dir
        .as_deref()
        .ok_or_else(|| invalid_manifest("migration manifest has no source data_dir"))?;
    if artifact.kind != "store_manifest" {
        let source_label = if backup {
            "migration backup source"
        } else {
            "migration source"
        };
        validate_manifest_path_under(
            &artifact.source_path,
            source_data_dir,
            source_label,
            "source store",
        )?;
    }
    let Some(target_path) = artifact.target_path.as_deref() else {
        return Ok(());
    };
    let profile_root = manifest
        .destination
        .profile_root
        .as_deref()
        .ok_or_else(|| invalid_manifest("migration manifest has no destination profile_root"))?;
    let target_root = if backup {
        profile_root
            .join("migration-backups")
            .join(&manifest.migration_id)
    } else {
        let project_id =
            manifest.destination.project_id.as_deref().ok_or_else(|| {
                invalid_manifest("migration manifest has no destination project_id")
            })?;
        profile_sharded_data_root(profile_root, project_id)
    };
    let target_label = if backup {
        "migration backup target"
    } else {
        "migration target"
    };
    let root_label = if backup {
        "backup root"
    } else {
        "profile shard"
    };
    validate_manifest_path_under(target_path, &target_root, target_label, root_label)
}

fn validate_manifest_path_under(
    path: &Path,
    root: &Path,
    path_label: &str,
    root_label: &str,
) -> io::Result<()> {
    let normalized_path = normalize_manifest_path(path, path_label)?;
    let normalized_root = normalize_manifest_path(root, root_label)?;
    if !normalized_path.starts_with(&normalized_root) {
        return Err(invalid_manifest(&format!(
            "{path_label} '{}' is outside {root_label} '{}'",
            path.display(),
            root.display()
        )));
    }
    Ok(())
}

fn normalize_manifest_path(path: &Path, label: &str) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid_manifest(&format!(
            "{label} '{}' must be absolute",
            path.display()
        )));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(invalid_manifest(&format!(
                    "{label} '{}' contains path traversal",
                    path.display()
                )));
            }
        }
    }
    Ok(normalized)
}

fn invalid_manifest(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn infer_profile_root_from_store_manifest(path: &Path) -> Option<PathBuf> {
    let data_root = path.parent()?;
    let projects_root = data_root.parent()?;
    if projects_root.file_name()? != "projects" {
        return None;
    }
    projects_root.parent().map(PathBuf::from)
}

fn count_store_artifacts(path: &Path) -> usize {
    let Ok(meta) = path.symlink_metadata() else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if meta.is_file() {
        return 1;
    }
    if !meta.is_dir() {
        return 0;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| count_store_artifacts(&entry.path()))
        .sum()
}

fn artifact_relative_path(path: &Path, data_dir: &Path) -> std::result::Result<PathBuf, String> {
    path.strip_prefix(data_dir)
        .map(Path::to_path_buf)
        .map_err(|_| {
            format!(
                "artifact '{}' is outside store data_dir '{}'",
                path.display(),
                data_dir.display()
            )
        })
}

fn validate_protocol_paths(protocol: &MigrationProtocol, migration_id: &str) -> io::Result<()> {
    let expected = MigrationProtocol::for_manifest(&protocol.manifest_path, migration_id);
    if protocol.temp_manifest_path != expected.temp_manifest_path
        || protocol.lock_path != expected.lock_path
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "migration manifest protocol paths must be derived from manifest_path and migration_id",
        ));
    }
    Ok(())
}

fn validate_migration_id(migration_id: &str) -> std::result::Result<(), &'static str> {
    if migration_id.is_empty() {
        return Err("migration_id must not be empty");
    }
    if migration_id.contains('/') || migration_id.contains('\\') || migration_id.contains("..") {
        return Err("migration_id must be a single safe path segment");
    }
    if !migration_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err("migration_id contains unsupported characters");
    }
    Ok(())
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
