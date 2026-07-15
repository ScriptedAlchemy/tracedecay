use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::migrate::inventory::{MigrationInventory, StoreStatus};
use crate::migrate::registry::{
    RegistryReconstructionReport, reconstruct_registry_from_store_manifest,
};
use crate::storage::{
    EnrollmentMarker, PrivateStoreIo, STORE_MANIFEST_FILENAME, StorageMode, StoreKind,
    has_sqlite_database_header, profile_sharded_data_root, profile_sharded_layout,
    read_enrollment_marker, read_store_manifest, validate_project_id, write_store_manifest,
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
struct SqliteFileFingerprint {
    size_bytes: u64,
    sha256: String,
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
        if result.is_ok()
            && let Err(err) = cleanup_result
            && err.kind() != io::ErrorKind::NotFound
        {
            return Err(err);
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
        if is_sqlite_sidecar_artifact_entry(&artifact.kind, &artifact.path) {
            continue;
        }
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
        if let Some(target) = artifact.target_path.as_ref()
            && let Err(err) = verify_manifest_artifact_contents(manifest, artifact)
        {
            issues.push(format!(
                "artifact '{}' target '{}' does not match source '{}': {err}",
                artifact.kind,
                target.display(),
                artifact.source_path.display()
            ));
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
        if let Some(target) = artifact.target_path.as_ref()
            && let Err(err) = if is_sqlite_database_artifact(&artifact.kind)
                && has_sqlite_database_header(target).unwrap_or(false)
            {
                verify_sqlite_snapshot_file(target)
            } else {
                verify_artifact_contents(&artifact.source_path, target)
            }
        {
            issues.push(format!(
                "backup artifact '{}' target '{}' does not match source '{}': {err}",
                artifact.kind,
                target.display(),
                artifact.source_path.display()
            ));
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

pub async fn apply_migration_manifest(
    manifest: &mut MigrationManifest,
) -> io::Result<MigrationApplyReport> {
    let (project_root, source_data_dir, profile_root, project_id) = manifest_destination(manifest)?;
    let source_metadata = source_data_dir.symlink_metadata().map_err(|error| {
        invalid_manifest(&format!(
            "migration source data_dir '{}' is unavailable: {error}",
            source_data_dir.display()
        ))
    })?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(invalid_manifest(&format!(
            "migration source data_dir '{}' must be a real directory",
            source_data_dir.display()
        )));
    }
    reject_sqlite_sidecar_artifacts(&manifest.artifacts)?;
    reject_sqlite_sidecar_artifacts(&manifest.backup_artifacts)?;
    for artifact in &manifest.artifacts {
        validate_manifest_artifact_paths(manifest, artifact, false)?;
    }
    for artifact in &manifest.backup_artifacts {
        validate_manifest_artifact_paths(manifest, artifact, true)?;
    }
    let backup_root = profile_root
        .join("migration-backups")
        .join(&manifest.migration_id);
    let mut profile_roots =
        migration_profile_roots(manifest, &source_data_dir, &profile_root, &backup_root);
    for root in &profile_roots {
        PrivateStoreIo::create_dir_all(root)?;
    }
    profile_roots = profile_roots
        .into_iter()
        .map(|root| root.canonicalize().unwrap_or(root))
        .collect();
    profile_roots.sort();
    profile_roots.dedup();
    let operation = format!("migration apply {}", manifest.migration_id);
    let mut lifecycle_leases = Vec::with_capacity(profile_roots.len());
    for root in &profile_roots {
        lifecycle_leases.push(
            crate::lifecycle_lease::acquire_exclusive_for_profile(root, &operation)
                .map_err(|error| invalid_manifest(&error.to_string()))?,
        );
    }
    let mut database_scopes = Vec::with_capacity(profile_roots.len());
    for (lease, root) in lifecycle_leases.iter().zip(&profile_roots) {
        database_scopes.push(
            crate::db::enter_maintenance_database_scope(lease, root, &operation)
                .map_err(|error| invalid_manifest(&error.to_string()))?,
        );
    }
    let mut source_databases = manifest
        .artifacts
        .iter()
        .filter(|artifact| is_sqlite_database_artifact(&artifact.kind))
        .map(|artifact| artifact.source_path.clone())
        .collect::<Vec<_>>();
    source_databases.sort();
    source_databases.dedup();
    let mut source_authorities = Vec::with_capacity(source_databases.len());
    for database in source_databases {
        let authority = crate::db::DatabaseAuthority::for_runtime(&database, &operation)
            .map_err(|error| invalid_manifest(&error.to_string()))?;
        source_authorities.push((database, authority));
    }
    apply_migration_manifest_in_scope(
        manifest,
        project_root,
        source_data_dir,
        profile_root,
        project_id,
        &source_authorities,
        &operation,
    )
    .await
}

async fn apply_migration_manifest_in_scope(
    manifest: &mut MigrationManifest,
    project_root: PathBuf,
    source_data_dir: PathBuf,
    profile_root: PathBuf,
    project_id: String,
    source_authorities: &[(PathBuf, crate::db::DatabaseAuthority)],
    operation: &str,
) -> io::Result<MigrationApplyReport> {
    let data_root = profile_sharded_data_root(&profile_root, &project_id);
    let backup_root = profile_root
        .join("migration-backups")
        .join(&manifest.migration_id);
    let original_backup_count = manifest.backup_artifacts.len();
    for index in 0..original_backup_count {
        apply_backup_artifact(
            manifest,
            index,
            &source_data_dir,
            &backup_root,
            source_authorities,
            operation,
        )
        .await?;
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
        MigrationRollbackState::DivergentTargets | MigrationRollbackState::AppliedReady => {
            Err(invalid_manifest(
                "rollback requires an applied manifest with no divergent target writes; registry rollback state is not available yet",
            ))
        }
    }
}

pub fn export_profile_store(
    profile_root: &Path,
    project_id: &str,
    target_dir: &Path,
) -> io::Result<MigrationExportReport> {
    let lifecycle =
        crate::lifecycle_lease::acquire_exclusive_for_profile(profile_root, "profile store export")
            .map_err(|error| {
                invalid_manifest(&format!("could not isolate profile store export: {error}"))
            })?;
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "profile store export",
    )
    .map_err(|error| {
        invalid_manifest(&format!(
            "could not authorize profile store export: {error}"
        ))
    })?;
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
    let profile_root = manifest
        .destination
        .profile_root
        .as_deref()
        .ok_or_else(|| invalid_manifest("migration manifest has no destination profile_root"))?;
    let lifecycle = crate::lifecycle_lease::acquire_exclusive_for_profile(
        profile_root,
        "migration source cleanup",
    )
    .map_err(|error| {
        invalid_manifest(&format!(
            "could not isolate migration source cleanup: {error}"
        ))
    })?;
    let _database_scope = crate::db::enter_maintenance_database_scope(
        &lifecycle,
        profile_root,
        "migration source cleanup",
    )
    .map_err(|error| {
        invalid_manifest(&format!(
            "could not authorize migration source cleanup: {error}"
        ))
    })?;
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

fn migration_profile_roots(
    manifest: &MigrationManifest,
    source_data_dir: &Path,
    destination: &Path,
    backup_root: &Path,
) -> Vec<PathBuf> {
    let mut roots = [source_data_dir, destination, backup_root]
        .into_iter()
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    roots.extend(
        manifest
            .artifacts
            .iter()
            .filter(|artifact| is_sqlite_database_artifact(&artifact.kind))
            .filter(|artifact| artifact.source_path.exists())
            .filter_map(|artifact| artifact.source_path.parent().map(Path::to_path_buf)),
    );
    roots.extend(
        manifest
            .backup_artifacts
            .iter()
            .filter(|artifact| is_sqlite_database_artifact(&artifact.kind))
            .filter_map(|artifact| {
                artifact
                    .target_path
                    .as_deref()?
                    .parent()
                    .map(Path::to_path_buf)
            }),
    );
    roots
}

fn is_sqlite_database_artifact(kind: &str) -> bool {
    matches!(kind, "graph_db" | "sessions_db" | "branch_graph_db")
}

fn is_sqlite_sidecar_artifact(kind: &str) -> bool {
    matches!(
        kind,
        "graph_db_wal"
            | "graph_db_shm"
            | "sessions_db_wal"
            | "sessions_db_shm"
            | "branch_graph_db_wal"
            | "branch_graph_db_shm"
    )
}

fn is_sqlite_sidecar_artifact_entry(kind: &str, path: &Path) -> bool {
    is_sqlite_sidecar_artifact(kind)
        || path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name.ends_with(".db-wal") || name.ends_with(".db-shm")
        })
}

fn reject_sqlite_sidecar_artifacts(artifacts: &[MigrationArtifact]) -> io::Result<()> {
    if let Some(artifact) = artifacts
        .iter()
        .find(|artifact| is_sqlite_sidecar_artifact_entry(&artifact.kind, &artifact.source_path))
    {
        return Err(invalid_manifest(&format!(
            "migration manifest contains transient SQLite sidecar artifact '{}'; rebuild the plan so WAL contents are absorbed into a database snapshot",
            artifact.kind
        )));
    }
    Ok(())
}

async fn copy_sqlite_snapshot(
    source: &Path,
    target: &Path,
    source_authority: &crate::db::DatabaseAuthority,
    operation: &str,
) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        PrivateStoreIo::create_dir_all(parent)?;
    }
    let temporary = migration_snapshot_temp_path(target);
    remove_stale_snapshot_temp(&temporary)?;
    let (source_db, _) = crate::db::Database::open_read_only(source, source_authority)
        .await
        .map_err(io::Error::other)?;
    let snapshot_result = source_db
        .snapshot_to(&temporary)
        .await
        .map_err(io::Error::other);
    source_db.close();
    if let Err(error) = snapshot_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    set_snapshot_permissions(&temporary)?;
    let temporary_authority = crate::db::DatabaseAuthority::for_runtime(
        &temporary,
        &format!("{operation} verify snapshot staging"),
    )
    .map_err(io::Error::other)?;
    verify_sqlite_integrity(&temporary, &temporary_authority).await?;
    drop(temporary_authority);
    let temporary_fingerprint = fingerprint_file(&temporary)?;
    sync_file(&temporary)?;
    crate::db::DatabaseAuthority::replace_file_atomically(
        &temporary,
        target,
        "migration SQLite snapshot",
    )
    .map_err(io::Error::other)?;
    sync_parent_directory(target)?;
    let target_fingerprint = fingerprint_file(target)?;
    if target_fingerprint != temporary_fingerprint {
        return Err(invalid_manifest(&format!(
            "published SQLite snapshot '{}' differs from staged snapshot of '{}'",
            target.display(),
            source.display()
        )));
    }
    Ok(())
}

fn migration_snapshot_temp_path(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(format!(".migration-{}.tmp", std::process::id()));
    PathBuf::from(name)
}

fn remove_stale_snapshot_temp(path: &Path) -> io::Result<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path)
        }
        Ok(_) => Err(invalid_manifest(&format!(
            "migration snapshot temp '{}' is not a regular file",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn set_snapshot_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_snapshot_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn sync_file(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_manifest("migration target has no parent directory"))?;
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn copy_file_atomically(source: &Path, target: &Path, label: &str) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        PrivateStoreIo::create_dir_all(parent)?;
    }
    let temporary = migration_snapshot_temp_path(target);
    remove_stale_snapshot_temp(&temporary)?;
    PrivateStoreIo::copy_artifact(source, &temporary)?;
    sync_file(&temporary)?;
    crate::db::DatabaseAuthority::replace_file_atomically(&temporary, target, label)
        .map_err(io::Error::other)?;
    sync_parent_directory(target)
}

async fn verify_sqlite_snapshot(path: &Path, operation: &str) -> io::Result<()> {
    let authority = crate::db::DatabaseAuthority::for_runtime(
        path,
        &format!("{operation} verify SQLite snapshot"),
    )
    .map_err(io::Error::other)?;
    verify_sqlite_integrity(path, &authority).await
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
    let sqlite = is_sqlite_database_artifact(&manifest.artifacts[index].kind)
        && has_sqlite_database_header(&source_path)?;
    let snapshot = sqlite
        .then(|| {
            manifest
                .backup_artifacts
                .iter()
                .find(|artifact| {
                    artifact.kind == manifest.artifacts[index].kind
                        && artifact.source_path == source_path
                        && artifact.state == ArtifactState::Verified
                })
                .and_then(|artifact| artifact.target_path.clone())
                .ok_or_else(|| {
                    invalid_manifest(
                        "verified SQLite migration backup is missing before target copy",
                    )
                })
        })
        .transpose()?;
    if matches!(
        manifest.artifacts[index].state,
        ArtifactState::Applied | ArtifactState::Verified
    ) {
        verify_migration_target(&source_path, snapshot.as_deref(), &target_path)?;
        return Ok(());
    }
    if target_path.exists() {
        if matches!(
            manifest.artifacts[index].state,
            ArtifactState::Locked | ArtifactState::Copied
        ) {
            verify_migration_target(&source_path, snapshot.as_deref(), &target_path)?;
            if manifest.artifacts[index].state == ArtifactState::Locked {
                transition_and_save(manifest, index, ArtifactState::Copied)?;
            }
            return transition_and_save(manifest, index, ArtifactState::Verified);
        }
        return Err(invalid_manifest(&format!(
            "migration target '{}' already exists",
            target_path.display()
        )));
    }
    if manifest.artifacts[index].state == ArtifactState::Planned {
        transition_and_save(manifest, index, ArtifactState::Locked)?;
    } else if manifest.artifacts[index].state != ArtifactState::Locked {
        return Err(invalid_manifest("migration artifact is not resumable"));
    }
    let copy_result = if let Some(snapshot) = snapshot.as_deref() {
        copy_file_atomically(snapshot, &target_path, "migration SQLite target")
    } else {
        PrivateStoreIo::copy_artifact(&source_path, &target_path).map(|_| ())
    };
    if let Err(err) = copy_result {
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
    verify_migration_target(&source_path, snapshot.as_deref(), &target_path)?;
    transition_and_save(manifest, index, ArtifactState::Verified)
}

fn verify_migration_target(
    source: &Path,
    snapshot: Option<&Path>,
    target: &Path,
) -> io::Result<()> {
    if let Some(snapshot) = snapshot {
        verify_sqlite_artifact_contents(snapshot, target)
    } else {
        verify_artifact_contents(source, target)
    }
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

async fn apply_backup_artifact(
    manifest: &mut MigrationManifest,
    index: usize,
    source_data_dir: &Path,
    backup_root: &Path,
    source_authorities: &[(PathBuf, crate::db::DatabaseAuthority)],
    operation: &str,
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
    let sqlite = is_sqlite_database_artifact(&manifest.backup_artifacts[index].kind)
        && has_sqlite_database_header(&source_path)?;
    let source_authority = sqlite
        .then(|| {
            source_authorities
                .iter()
                .find(|(path, _)| path == &source_path)
                .map(|(_, authority)| authority)
                .ok_or_else(|| invalid_manifest("SQLite migration source authority is missing"))
        })
        .transpose()?;
    if manifest.backup_artifacts[index].state == ArtifactState::Verified {
        if source_authority.is_some() {
            verify_sqlite_snapshot(&target_path, operation).await?;
        } else {
            verify_artifact_contents(&source_path, &target_path)?;
        }
        return Ok(());
    }
    if target_path.exists() {
        if matches!(
            manifest.backup_artifacts[index].state,
            ArtifactState::Locked | ArtifactState::Copied
        ) {
            if source_authority.is_some() {
                verify_sqlite_snapshot(&target_path, operation).await?;
            } else {
                verify_artifact_contents(&source_path, &target_path)?;
            }
            if manifest.backup_artifacts[index].state == ArtifactState::Locked {
                transition_backup_and_save(manifest, index, ArtifactState::Copied)?;
            }
            return transition_backup_and_save(manifest, index, ArtifactState::Verified);
        }
        return Err(invalid_manifest(&format!(
            "migration backup target '{}' already exists",
            target_path.display()
        )));
    }
    if manifest.backup_artifacts[index].state == ArtifactState::Planned {
        transition_backup_and_save(manifest, index, ArtifactState::Locked)?;
    } else if manifest.backup_artifacts[index].state != ArtifactState::Locked {
        return Err(invalid_manifest(
            "migration backup artifact is not resumable",
        ));
    }
    let copy_result = if let Some(authority) = source_authority {
        copy_sqlite_snapshot(&source_path, &target_path, authority, operation).await
    } else {
        PrivateStoreIo::copy_artifact(&source_path, &target_path).map(|_| ())
    };
    if let Err(err) = copy_result {
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
    if source_authority.is_none() {
        verify_artifact_contents(&source_path, &target_path)?;
    }
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
        if verify_manifest_artifact_contents(manifest, artifact).is_err() {
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

fn verify_manifest_artifact_contents(
    manifest: &MigrationManifest,
    artifact: &MigrationArtifact,
) -> io::Result<()> {
    let target = artifact
        .target_path
        .as_deref()
        .ok_or_else(|| invalid_manifest("migration artifact has no target_path"))?;
    if is_sqlite_database_artifact(&artifact.kind)
        && has_sqlite_database_header(&artifact.source_path)?
    {
        let snapshot = manifest
            .backup_artifacts
            .iter()
            .find(|backup| {
                backup.kind == artifact.kind
                    && backup.source_path == artifact.source_path
                    && backup.state == ArtifactState::Verified
            })
            .and_then(|backup| backup.target_path.as_deref())
            .ok_or_else(|| invalid_manifest("verified SQLite migration backup is missing"))?;
        return verify_sqlite_artifact_contents(snapshot, target);
    }
    verify_artifact_contents(&artifact.source_path, target)
}

fn verify_sqlite_snapshot_file(path: &Path) -> io::Result<()> {
    require_regular_file(path, "SQLite snapshot")?;
    if !has_sqlite_database_header(path)? {
        return Err(invalid_manifest(&format!(
            "SQLite snapshot '{}' has no database header",
            path.display()
        )));
    }
    Ok(())
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
    if has_sqlite_database_header(source)? && has_sqlite_database_header(target)? {
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

fn verify_sqlite_artifact_contents(source: &Path, target: &Path) -> io::Result<()> {
    if fingerprint_file(source)? != fingerprint_file(target)? {
        return Err(invalid_manifest(&format!(
            "SQLite target '{}' differs from snapshot '{}'",
            target.display(),
            source.display()
        )));
    }
    Ok(())
}

async fn verify_sqlite_integrity(
    path: &Path,
    authority: &crate::db::DatabaseAuthority,
) -> io::Result<()> {
    let (db, _) = crate::db::Database::open_read_only(path, authority)
        .await
        .map_err(io::Error::other)?;
    db.close();
    Ok(())
}

fn fingerprint_file(path: &Path) -> io::Result<SqliteFileFingerprint> {
    require_regular_file(path, "SQLite fingerprint source")?;
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    let mut size_bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size_bytes = size_bytes.saturating_add(read as u64);
        digest.update(&buffer[..read]);
    }
    Ok(SqliteFileFingerprint {
        size_bytes,
        sha256: hex::encode(digest.finalize()),
    })
}

fn require_regular_file(path: &Path, subject: &str) -> io::Result<()> {
    let metadata = path.symlink_metadata()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid_manifest(&format!(
            "{subject} '{}' must be a regular non-symlink file",
            path.display()
        )));
    }
    Ok(())
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
