//! Complete profile backup, verification, and restore rehearsal.
//!
//! A complete profile backup is a directory of database-aware snapshot
//! artifacts (SQLite backup-API snapshots, verified Grafeo graph snapshots,
//! and plain copies for everything else) plus a manifest that binds the
//! backup to its exact brain/profile identity, the durable identity of every
//! contained project store, and a SHA-256 inventory of every artifact.
//! Restore rehearsal re-verifies all of that before publishing anything,
//! proves each restored database artifact opens through its engine, rebinds
//! restored store manifests to the rehearsal location, and publishes
//! atomically under a crash-recoverable ownership marker. Recovery finishes
//! or clears only material owned by the exact same rehearsal attempt;
//! foreign markers are a typed conflict, never collateral cleanup.

use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::DirectorySyncPolicy;

#[path = "profile_backup/error.rs"]
mod error;
#[path = "profile_backup/identity.rs"]
mod identity;
#[path = "profile_backup/snapshot.rs"]
mod snapshot;
#[cfg(test)]
#[path = "profile_backup/tests.rs"]
mod tests;

pub use error::ProfileBackupError;
pub use identity::ProfileBackupProjectIdentity;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RehearsalPublicationFault {
    None,
    BeforeRename,
    AfterRenameBeforeParentSync,
    AfterParentSyncBeforeMarkerRemoval,
}

thread_local! {
    static REHEARSAL_PUBLICATION_FAULT: Cell<RehearsalPublicationFault> =
        const { Cell::new(RehearsalPublicationFault::None) };
}

/// Test-only fault injection for rehearsal publication boundaries.
#[doc(hidden)]
pub fn set_rehearsal_publication_fault_for_test(fault: &str) {
    let fault = match fault {
        "before_rename" => RehearsalPublicationFault::BeforeRename,
        "after_rename_before_parent_sync" => RehearsalPublicationFault::AfterRenameBeforeParentSync,
        "after_parent_sync_before_marker_removal" => {
            RehearsalPublicationFault::AfterParentSyncBeforeMarkerRemoval
        }
        _ => RehearsalPublicationFault::None,
    };
    REHEARSAL_PUBLICATION_FAULT.with(|cell| cell.set(fault));
}

fn inject_rehearsal_publication_fault(
    phase: RehearsalPublicationFault,
) -> Result<(), ProfileBackupError> {
    let injected = REHEARSAL_PUBLICATION_FAULT.with(|cell| {
        if cell.get() == phase {
            cell.set(RehearsalPublicationFault::None);
            true
        } else {
            false
        }
    });
    if injected {
        Err(ProfileBackupError::unavailable(format!(
            "injected rehearsal publication fault at {phase:?}"
        )))
    } else {
        Ok(())
    }
}

const BACKUP_MANIFEST_SCHEMA_VERSION: u32 = 2;
const REHEARSAL_MARKER_SCHEMA_VERSION: u32 = 3;
const REHEARSAL_MARKER_FILENAME: &str = ".tracedecay-profile-rehearsal.json";
const REQUIRED_PROFILE_PATHS: &[&str] = &[
    "global.db",
    "user-sessions.db",
    "user-memory.db",
    "projects",
    "enrollment.json",
    "config.toml",
    "migration-inventory",
    "profile-identity.json",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBackupEntry {
    pub logical_path: String,
    pub present: bool,
    pub byte_len: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteProfileBackupManifest {
    pub schema_version: u32,
    pub backup_id: String,
    pub created_at: i64,
    pub source_profile_identity_sha256: String,
    pub source_brain_id: String,
    pub source_profile_id: String,
    pub projects: Vec<ProfileBackupProjectIdentity>,
    pub entries: Vec<ProfileBackupEntry>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileBackupRehearsalMarker {
    schema_version: u32,
    backup_id: String,
    backup_root: PathBuf,
    manifest_sha256: String,
    source_profile_identity_sha256: String,
    restore_root: PathBuf,
}

struct VerifiedCompleteProfileBackup {
    root: PathBuf,
    manifest: CompleteProfileBackupManifest,
    manifest_sha256: String,
}

pub fn create_complete_profile_backup(
    profile_root: &Path,
    backup_parent: &Path,
    backup_id: &str,
    created_at: i64,
    lifecycle: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
) -> Result<PathBuf, ProfileBackupError> {
    if backup_id.is_empty() || created_at <= 0 {
        return Err(ProfileBackupError::invalid(
            "backup identity and timestamp must be non-empty",
        ));
    }
    let source = fs::canonicalize(profile_root).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "canonicalize profile '{}': {error}",
            profile_root.display()
        ))
    })?;
    if !lifecycle.is_exclusive() || !lifecycle.guards_profile(&source) {
        return Err(ProfileBackupError::denied(
            "complete profile backup requires the exact exclusive profile lease",
        ));
    }
    fs::create_dir_all(backup_parent).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "create backup parent '{}': {error}",
            backup_parent.display()
        ))
    })?;
    let parent = fs::canonicalize(backup_parent).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "canonicalize backup parent '{}': {error}",
            backup_parent.display()
        ))
    })?;
    if parent.starts_with(&source) {
        return Err(ProfileBackupError::invalid(
            "backup destination must be outside the source profile",
        ));
    }

    let final_root = parent.join(backup_id);
    let staging = parent.join(format!(".{backup_id}.tmp"));
    if final_root.exists() || staging.exists() {
        return Err(ProfileBackupError::conflict(
            "backup destination already exists",
        ));
    }
    fs::create_dir(&staging).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "create backup staging '{}': {error}",
            staging.display()
        ))
    })?;

    let result = create_backup_contents(&source, &staging, backup_id, created_at);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    fs::rename(&staging, &final_root).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "publish backup '{}' to '{}': {error}",
            staging.display(),
            final_root.display()
        ))
    })?;
    sync_directory(&parent)?;
    Ok(final_root)
}

pub fn rehearse_complete_profile_backup(
    backup_root: &Path,
    restore_root: &Path,
) -> Result<CompleteProfileBackupManifest, ProfileBackupError> {
    let backup = load_verified_backup(backup_root)?;
    let restore_root = absolute_destination(restore_root)?;
    let marker = ProfileBackupRehearsalMarker {
        schema_version: REHEARSAL_MARKER_SCHEMA_VERSION,
        backup_id: backup.manifest.backup_id.clone(),
        backup_root: backup.root.clone(),
        manifest_sha256: backup.manifest_sha256.clone(),
        source_profile_identity_sha256: backup.manifest.source_profile_identity_sha256.clone(),
        restore_root: restore_root.clone(),
    };
    if recover_interrupted_publication(&restore_root, &marker, &backup)? {
        return Ok(backup.manifest);
    }
    let staging = rehearsal_staging_path(&restore_root)?;
    recover_interrupted_staging(&staging, &marker)?;
    if restore_root.exists() {
        return Err(ProfileBackupError::conflict(
            "restore destination must not already exist",
        ));
    }
    fs::create_dir(&staging).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "create restore staging directory '{}': {error}",
            staging.display()
        ))
    })?;
    restrict_private_directory(&staging)?;
    let marker_path = staging.join(REHEARSAL_MARKER_FILENAME);
    write_new_synced(
        &marker_path,
        &serde_json::to_vec_pretty(&marker).map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "encode profile rehearsal marker: {error}"
            ))
        })?,
    )?;
    let result = (|| {
        for entry in backup.manifest.entries.iter().filter(|entry| entry.present) {
            let source = checked_join(&backup.root, &entry.logical_path)?;
            let destination = checked_join(&staging, &entry.logical_path)?;
            copy_verified_file(&source, &destination, entry)?;
        }
        let restored = verify_restored_copy(&staging, &backup.manifest)?;
        if restored != backup.manifest {
            return Err(ProfileBackupError::corrupt(
                "restored profile inventory differs from backup manifest",
            ));
        }
        rebind_restored_store_manifests(&staging, &restore_root)?;
        verify_restored_rehearsal(&staging, &restore_root, &backup)?;
        // Keep the ownership marker through rename and parent sync so a crash
        // never leaves an unowned staging directory or an unmarked published
        // root that recovery cannot finish.
        sync_directory(&staging)?;
        inject_rehearsal_publication_fault(RehearsalPublicationFault::BeforeRename)?;
        fs::rename(&staging, &restore_root).map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "publish rehearsed profile '{}' to '{}': {error}",
                staging.display(),
                restore_root.display()
            ))
        })?;
        inject_rehearsal_publication_fault(RehearsalPublicationFault::AfterRenameBeforeParentSync)?;
        finish_published_rehearsal(&restore_root, &marker)
    })();
    if let Err(error) = result {
        // Leave marker-owned staging or published roots for crash recovery.
        // Only scrub unmarked partial staging created before ownership settled.
        let staging_marked = staging.join(REHEARSAL_MARKER_FILENAME).is_file();
        let published_marked = restore_root.join(REHEARSAL_MARKER_FILENAME).is_file();
        if !staging_marked && !published_marked {
            let _ = fs::remove_dir_all(&staging);
        }
        return Err(error);
    }
    Ok(backup.manifest)
}

fn absolute_destination(path: &Path) -> Result<PathBuf, ProfileBackupError> {
    let name = path
        .file_name()
        .ok_or_else(|| ProfileBackupError::invalid("restore destination must name a directory"))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize().map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "canonicalize restore destination parent '{}': {error}",
            parent.display()
        ))
    })?;
    Ok(parent.join(name))
}

fn rehearsal_staging_path(restore_root: &Path) -> Result<PathBuf, ProfileBackupError> {
    let parent = restore_root
        .parent()
        .ok_or_else(|| ProfileBackupError::invalid("restore destination has no parent"))?;
    let name = restore_root
        .file_name()
        .ok_or_else(|| ProfileBackupError::invalid("restore destination must name a directory"))?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.tracedecay-rehearsal")))
}

fn recover_interrupted_publication(
    restore_root: &Path,
    expected_marker: &ProfileBackupRehearsalMarker,
    backup: &VerifiedCompleteProfileBackup,
) -> Result<bool, ProfileBackupError> {
    let metadata = match fs::symlink_metadata(restore_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(ProfileBackupError::unavailable(format!(
                "inspect restore destination '{}': {error}",
                restore_root.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(false);
    }
    let marker_path = restore_root.join(REHEARSAL_MARKER_FILENAME);
    match fs::symlink_metadata(&marker_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(ProfileBackupError::unavailable(format!(
                "inspect published rehearsal marker '{}': {error}",
                marker_path.display()
            )));
        }
        Ok(marker_metadata) => {
            if marker_metadata.file_type().is_symlink() || !marker_metadata.is_file() {
                return Err(ProfileBackupError::partial_restore_conflict(format!(
                    "published rehearsal marker '{}' is not a regular file",
                    marker_path.display()
                )));
            }
        }
    }
    let marker = read_rehearsal_marker(&marker_path)?;
    if marker != *expected_marker {
        return Err(ProfileBackupError::partial_restore_conflict(format!(
            "published rehearsal root '{}' belongs to another restore attempt",
            restore_root.display()
        )));
    }
    verify_restored_rehearsal(restore_root, restore_root, backup)?;
    finish_published_rehearsal(restore_root, expected_marker)?;
    Ok(true)
}

fn recover_interrupted_staging(
    staging: &Path,
    expected_marker: &ProfileBackupRehearsalMarker,
) -> Result<(), ProfileBackupError> {
    let metadata = match fs::symlink_metadata(staging) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(ProfileBackupError::unavailable(format!(
                "inspect profile rehearsal staging '{}': {error}",
                staging.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProfileBackupError::partial_restore_conflict(format!(
            "profile rehearsal staging '{}' is not an owned directory",
            staging.display()
        )));
    }
    let marker_path = staging.join(REHEARSAL_MARKER_FILENAME);
    let marker_metadata = fs::symlink_metadata(&marker_path).map_err(|error| {
        ProfileBackupError::partial_restore_conflict(format!(
            "interrupted profile rehearsal staging '{}' has no readable ownership marker: {error}",
            staging.display()
        ))
    })?;
    if marker_metadata.file_type().is_symlink() || !marker_metadata.is_file() {
        return Err(ProfileBackupError::partial_restore_conflict(format!(
            "profile rehearsal marker '{}' is not a regular file",
            marker_path.display()
        )));
    }
    let marker = read_rehearsal_marker(&marker_path)?;
    if marker != *expected_marker {
        return Err(ProfileBackupError::partial_restore_conflict(format!(
            "profile rehearsal staging '{}' belongs to another restore attempt",
            staging.display()
        )));
    }
    fs::remove_dir_all(staging).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "clear interrupted profile rehearsal staging '{}': {error}",
            staging.display()
        ))
    })?;
    sync_directory(staging.parent().ok_or_else(|| {
        ProfileBackupError::invalid("profile rehearsal staging has no parent")
    })?)
}

fn finish_published_rehearsal(
    restore_root: &Path,
    expected_marker: &ProfileBackupRehearsalMarker,
) -> Result<(), ProfileBackupError> {
    let marker_path = restore_root.join(REHEARSAL_MARKER_FILENAME);
    let marker = read_rehearsal_marker(&marker_path)?;
    if marker != *expected_marker {
        return Err(ProfileBackupError::partial_restore_conflict(format!(
            "published rehearsal root '{}' belongs to another restore attempt",
            restore_root.display()
        )));
    }
    let parent = restore_root
        .parent()
        .ok_or_else(|| ProfileBackupError::invalid("restore destination has no parent"))?;
    sync_directory(parent)?;
    inject_rehearsal_publication_fault(
        RehearsalPublicationFault::AfterParentSyncBeforeMarkerRemoval,
    )?;
    fs::remove_file(&marker_path).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "remove settled profile rehearsal marker '{}': {error}",
            marker_path.display()
        ))
    })?;
    sync_directory(restore_root)?;
    sync_directory(parent)
}

fn read_rehearsal_marker(
    marker_path: &Path,
) -> Result<ProfileBackupRehearsalMarker, ProfileBackupError> {
    serde_json::from_slice(&fs::read(marker_path).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "read profile rehearsal marker '{}': {error}",
            marker_path.display()
        ))
    })?)
    .map_err(|error| {
        ProfileBackupError::partial_restore_conflict(format!(
            "decode profile rehearsal marker '{}': {error}",
            marker_path.display()
        ))
    })
}

fn rebind_restored_store_manifests(
    staged_profile_root: &Path,
    published_profile_root: &Path,
) -> Result<(), ProfileBackupError> {
    let projects = staged_profile_root.join("projects");
    let metadata = match fs::symlink_metadata(&projects) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(ProfileBackupError::unavailable(format!(
                "inspect restored project stores '{}': {error}",
                projects.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProfileBackupError::corrupt(format!(
            "restored project stores '{}' must be a regular directory",
            projects.display()
        )));
    }
    let mut stores = fs::read_dir(&projects)
        .map_err(|error| {
            ProfileBackupError::unavailable(format!("read restored project stores: {error}"))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            ProfileBackupError::unavailable(format!("read restored project store entry: {error}"))
        })?;
    stores.sort_by_key(fs::DirEntry::file_name);
    for store in stores {
        let store_root = store.path();
        let metadata = fs::symlink_metadata(&store_root).map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "inspect restored store '{}': {error}",
                store_root.display()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ProfileBackupError::corrupt(format!(
                "restored store '{}' must not be a symlink",
                store_root.display()
            )));
        }
        if !metadata.is_dir() {
            continue;
        }
        let manifest_path =
            store_root.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME);
        let manifest_metadata = match fs::symlink_metadata(&manifest_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ProfileBackupError::corrupt(format!(
                    "restored store '{}' is missing required {}",
                    store_root.display(),
                    tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME
                )));
            }
            Err(error) => {
                return Err(ProfileBackupError::unavailable(format!(
                    "inspect restored store manifest '{}': {error}",
                    manifest_path.display()
                )));
            }
        };
        if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
            return Err(ProfileBackupError::corrupt(format!(
                "restored store manifest '{}' must be a regular file",
                manifest_path.display()
            )));
        }
        let manifest = tracedecay_runtime_core::storage::read_store_manifest(&manifest_path)
            .map_err(|error| {
                ProfileBackupError::corrupt(format!(
                    "read restored store manifest '{}': {error}",
                    manifest_path.display()
                ))
            })?;
        let project_id = store.file_name().into_string().map_err(|_| {
            ProfileBackupError::corrupt("restored project store id is not Unicode")
        })?;
        let manifest = rebound_store_manifest(
            manifest,
            &project_id,
            published_profile_root,
            &manifest_path,
        )?;
        tracedecay_runtime_core::storage::write_store_manifest_to_path(&manifest_path, &manifest)
            .map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "write rebound store manifest '{}': {error}",
                manifest_path.display()
            ))
        })?;
        let reread = tracedecay_runtime_core::storage::read_store_manifest(&manifest_path)
            .map_err(|error| {
                ProfileBackupError::unavailable(format!(
                    "reread rebound store manifest '{}': {error}",
                    manifest_path.display()
                ))
            })?;
        if reread != manifest {
            return Err(ProfileBackupError::unavailable(format!(
                "restored store manifest '{}' changed during rebinding",
                manifest_path.display()
            )));
        }
    }
    Ok(())
}

fn rebound_store_manifest(
    mut manifest: tracedecay_runtime_core::storage::StoreManifest,
    project_id: &str,
    published_profile_root: &Path,
    manifest_path: &Path,
) -> Result<tracedecay_runtime_core::storage::StoreManifest, ProfileBackupError> {
    if manifest.schema_version != tracedecay_runtime_core::storage::STORE_MANIFEST_SCHEMA_VERSION
        || manifest.project_id.as_deref() != Some(project_id)
        || manifest.store_kind != tracedecay_runtime_core::storage::StoreKind::CodeProject
        || manifest.storage_mode != tracedecay_runtime_core::storage::StorageMode::ProfileSharded
    {
        return Err(ProfileBackupError::corrupt(format!(
            "restored store manifest '{}' does not match its enrollment",
            manifest_path.display()
        )));
    }
    for relative in [
        &manifest.graph_db_relpath,
        &manifest.sessions_db_relpath,
        &manifest.branch_meta_relpath,
    ] {
        validate_restored_store_relative_path(relative)?;
    }
    let source_data_root = manifest.data_root.clone();
    if let Some(source_profile_root) = source_data_root
        .parent()
        .and_then(Path::parent)
        .filter(|root| source_data_root == root.join("projects").join(project_id))
        && let Ok(relative_project_root) = manifest.project_root.strip_prefix(source_profile_root)
    {
        manifest.project_root = published_profile_root.join(relative_project_root);
    }
    manifest.data_root = published_profile_root.join("projects").join(project_id);
    Ok(manifest)
}

fn verify_restored_rehearsal(
    restored_profile_root: &Path,
    published_profile_root: &Path,
    backup: &VerifiedCompleteProfileBackup,
) -> Result<(), ProfileBackupError> {
    for entry in backup.manifest.entries.iter().filter(|entry| entry.present) {
        if identity::restored_store_manifest_project_id(&entry.logical_path).is_none() {
            let path = checked_join(restored_profile_root, &entry.logical_path)?;
            verify_file(&path, entry)?;
            snapshot::verify_restored_artifact(&path)?;
        }
    }
    for entry in backup.manifest.entries.iter().filter(|entry| entry.present) {
        let Some(project_id) = identity::restored_store_manifest_project_id(&entry.logical_path)
        else {
            continue;
        };
        let source_path = checked_join(&backup.root, &entry.logical_path)?;
        let source_manifest = tracedecay_runtime_core::storage::read_store_manifest(&source_path)
            .map_err(|error| {
            ProfileBackupError::corrupt(format!(
                "read backup store manifest '{}': {error}",
                source_path.display()
            ))
        })?;
        let expected = rebound_store_manifest(
            source_manifest,
            project_id,
            published_profile_root,
            &source_path,
        )?;
        let restored_path = checked_join(restored_profile_root, &entry.logical_path)?;
        let restored = tracedecay_runtime_core::storage::read_store_manifest(&restored_path)
            .map_err(|error| {
                ProfileBackupError::corrupt(format!(
                    "read restored store manifest '{}': {error}",
                    restored_path.display()
                ))
            })?;
        if restored != expected {
            return Err(ProfileBackupError::corrupt(format!(
                "restored store manifest '{}' does not match its rebound backup manifest",
                restored_path.display()
            )));
        }
    }
    Ok(())
}

fn validate_restored_store_relative_path(path: &Path) -> Result<(), ProfileBackupError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(ProfileBackupError::corrupt(format!(
            "restored store manifest contains unsafe relative path '{}'",
            path.display()
        )));
    }
    Ok(())
}

pub fn load_and_verify_backup(
    backup_root: &Path,
) -> Result<CompleteProfileBackupManifest, ProfileBackupError> {
    Ok(load_verified_backup(backup_root)?.manifest)
}

fn load_verified_backup(
    backup_root: &Path,
) -> Result<VerifiedCompleteProfileBackup, ProfileBackupError> {
    let root = fs::canonicalize(backup_root).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "canonicalize backup '{}': {error}",
            backup_root.display()
        ))
    })?;
    let metadata = fs::symlink_metadata(&root).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "inspect backup root '{}': {error}",
            root.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(ProfileBackupError::corrupt(format!(
            "complete-profile backup root '{}' is not a directory",
            root.display()
        )));
    }
    let manifest_path = root.join("backup-manifest.json");
    let bytes = fs::read(&manifest_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ProfileBackupError::corrupt(format!(
                "backup manifest '{}' is missing",
                manifest_path.display()
            ))
        } else {
            ProfileBackupError::unavailable(format!(
                "read backup manifest '{}': {error}",
                manifest_path.display()
            ))
        }
    })?;
    let manifest: CompleteProfileBackupManifest = serde_json::from_slice(&bytes)
        .map_err(|error| {
            ProfileBackupError::corrupt(format!("decode backup manifest: {error}"))
        })?;
    validate_manifest(&manifest)?;
    for entry in manifest.entries.iter().filter(|entry| entry.present) {
        let path = checked_join(&root, &entry.logical_path)?;
        verify_file(&path, entry)?;
    }
    let (brain_id, profile_id) = identity::read_required_profile_identity(&root, true)?;
    if brain_id != manifest.source_brain_id || profile_id != manifest.source_profile_id {
        return Err(ProfileBackupError::corrupt(
            "backup profile identity does not match its manifest",
        ));
    }
    if identity::collect_project_identities(&root, &manifest.entries)? != manifest.projects {
        return Err(ProfileBackupError::corrupt(
            "backup project identities do not match their manifests",
        ));
    }
    Ok(VerifiedCompleteProfileBackup {
        root,
        manifest,
        manifest_sha256: hex::encode(Sha256::digest(&bytes)),
    })
}

fn create_backup_contents(
    source: &Path,
    staging: &Path,
    backup_id: &str,
    created_at: i64,
) -> Result<(), ProfileBackupError> {
    let mut entries = Vec::new();
    for logical in REQUIRED_PROFILE_PATHS {
        let path = source.join(logical);
        if !path.exists() {
            entries.push(ProfileBackupEntry {
                logical_path: (*logical).to_owned(),
                present: false,
                byte_len: None,
                sha256: None,
            });
            continue;
        }
        let before = entries.len();
        collect_files(source, &path, &mut entries)?;
        if entries.len() == before {
            entries.push(ProfileBackupEntry {
                logical_path: (*logical).to_owned(),
                present: false,
                byte_len: None,
                sha256: None,
            });
        }
    }
    entries.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    entries.dedup_by(|left, right| left.logical_path == right.logical_path);
    for entry in entries.iter_mut().filter(|entry| entry.present) {
        let source_path = checked_join(source, &entry.logical_path)?;
        let destination = checked_join(staging, &entry.logical_path)?;
        snapshot::snapshot_artifact(&source_path, &destination)?;
        let metadata = fs::metadata(&destination).map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "inspect completed backup artifact '{}': {error}",
                destination.display()
            ))
        })?;
        entry.byte_len = Some(metadata.len());
        entry.sha256 = Some(sha256_file(&destination)?);
    }

    let (source_brain_id, source_profile_id) =
        identity::read_required_profile_identity(source, false)?;
    let staged_identity = staging.join("profile-identity.json");
    let manifest = CompleteProfileBackupManifest {
        schema_version: BACKUP_MANIFEST_SCHEMA_VERSION,
        backup_id: backup_id.to_owned(),
        created_at,
        source_profile_identity_sha256: sha256_file(&staged_identity)?,
        source_brain_id,
        source_profile_id,
        projects: identity::collect_project_identities(source, &entries)?,
        entries,
    };
    validate_manifest(&manifest)?;
    let manifest_path = staging.join("backup-manifest.json");
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        ProfileBackupError::unavailable(format!("encode backup manifest: {error}"))
    })?;
    write_new_synced(&manifest_path, &bytes)?;
    sync_directory(staging)
}

fn collect_files(
    root: &Path,
    path: &Path,
    entries: &mut Vec<ProfileBackupEntry>,
) -> Result<(), ProfileBackupError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "inspect backup source '{}': {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(ProfileBackupError::invalid(format!(
            "backup source is a symlink: '{}'",
            path.display()
        )));
    }
    // Write-ahead and shared-memory sidecars are folded into their owning
    // database's snapshot artifact instead of being copied byte-for-byte.
    if snapshot::is_database_sidecar(path) {
        return Ok(());
    }
    if metadata.is_dir() {
        let mut children = fs::read_dir(path)
            .map_err(|error| {
                ProfileBackupError::unavailable(format!(
                    "read backup directory '{}': {error}",
                    path.display()
                ))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                ProfileBackupError::unavailable(format!(
                    "read backup directory '{}': {error}",
                    path.display()
                ))
            })?;
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            collect_files(root, &child.path(), entries)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(ProfileBackupError::invalid(format!(
            "backup source is not a regular file: '{}'",
            path.display()
        )));
    }
    let logical = path
        .strip_prefix(root)
        .map_err(|_| ProfileBackupError::invalid("backup source escaped profile root"))?
        .to_string_lossy()
        .replace('\\', "/");
    entries.push(ProfileBackupEntry {
        logical_path: logical,
        present: true,
        byte_len: None,
        sha256: None,
    });
    Ok(())
}

fn verify_restored_copy(
    restore_root: &Path,
    expected: &CompleteProfileBackupManifest,
) -> Result<CompleteProfileBackupManifest, ProfileBackupError> {
    for entry in expected.entries.iter().filter(|entry| entry.present) {
        verify_file(&checked_join(restore_root, &entry.logical_path)?, entry)?;
    }
    Ok(expected.clone())
}

fn copy_verified_file(
    source: &Path,
    destination: &Path,
    expected: &ProfileBackupEntry,
) -> Result<(), ProfileBackupError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            ProfileBackupError::unavailable(format!(
                "create backup destination directory '{}': {error}",
                parent.display()
            ))
        })?;
    }
    fs::copy(source, destination).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "copy backup file '{}' to '{}': {error}",
            source.display(),
            destination.display()
        ))
    })?;
    sync_file(destination)?;
    verify_file(destination, expected)
}

fn verify_file(path: &Path, expected: &ProfileBackupEntry) -> Result<(), ProfileBackupError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ProfileBackupError::corrupt(format!(
            "inspect backup file '{}': {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ProfileBackupError::corrupt(format!(
            "backup artifact is not a regular file: '{}'",
            path.display()
        )));
    }
    if Some(metadata.len()) != expected.byte_len || Some(sha256_file(path)?) != expected.sha256 {
        return Err(ProfileBackupError::corrupt(format!(
            "backup artifact checksum mismatch: '{}'",
            path.display()
        )));
    }
    Ok(())
}

fn validate_manifest(manifest: &CompleteProfileBackupManifest) -> Result<(), ProfileBackupError> {
    if manifest.schema_version != BACKUP_MANIFEST_SCHEMA_VERSION {
        return Err(ProfileBackupError::reset_required(format!(
            "unsupported complete-profile backup manifest schema {}; \
             this backup is not a restore input for the current release",
            manifest.schema_version
        )));
    }
    if manifest.backup_id.is_empty()
        || manifest.created_at <= 0
        || manifest.source_brain_id.is_empty()
        || manifest.source_profile_id.is_empty()
    {
        return Err(ProfileBackupError::corrupt(
            "invalid complete-profile backup manifest identity",
        ));
    }
    let mut previous = None;
    for entry in &manifest.entries {
        if entry.logical_path.is_empty()
            || entry.logical_path.starts_with('/')
            || entry.logical_path.split('/').any(|part| part == "..")
            || entry.present != (entry.byte_len.is_some() && entry.sha256.is_some())
            || entry
                .sha256
                .as_ref()
                .is_some_and(|digest| digest.len() != 64)
            || previous.is_some_and(|value: &str| value >= entry.logical_path.as_str())
        {
            return Err(ProfileBackupError::corrupt(
                "invalid complete-profile backup manifest entry",
            ));
        }
        previous = Some(entry.logical_path.as_str());
    }
    for required in REQUIRED_PROFILE_PATHS {
        if !manifest.entries.iter().any(|entry| {
            entry.logical_path == *required
                || entry
                    .logical_path
                    .strip_prefix(required)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        }) {
            return Err(ProfileBackupError::corrupt(format!(
                "backup manifest omits required path '{required}'"
            )));
        }
    }
    let profile_identity_sha256 = manifest
        .entries
        .iter()
        .find(|entry| entry.logical_path == "profile-identity.json" && entry.present)
        .and_then(|entry| entry.sha256.clone())
        .ok_or_else(|| {
            ProfileBackupError::corrupt("backup manifest omits the required profile identity")
        })?;
    if manifest.source_profile_identity_sha256 != profile_identity_sha256 {
        return Err(ProfileBackupError::corrupt(
            "backup manifest source profile identity digest does not match content",
        ));
    }
    identity::validate_project_identities(&manifest.projects, &manifest.entries)
}

fn checked_join(root: &Path, logical: &str) -> Result<PathBuf, ProfileBackupError> {
    let path = Path::new(logical);
    if path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(ProfileBackupError::corrupt(
            "backup manifest path is not relative",
        ));
    }
    Ok(root.join(path))
}

fn sha256_file(path: &Path) -> Result<String, ProfileBackupError> {
    let mut file = File::open(path).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "open '{}' for hashing: {error}",
            path.display()
        ))
    })?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            ProfileBackupError::unavailable(format!("hash '{}': {error}", path.display()))
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), ProfileBackupError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        ProfileBackupError::unavailable(format!("create '{}': {error}", path.display()))
    })?;
    file.write_all(bytes).map_err(|error| {
        ProfileBackupError::unavailable(format!("write '{}': {error}", path.display()))
    })?;
    file.sync_all().map_err(|error| {
        ProfileBackupError::unavailable(format!("sync '{}': {error}", path.display()))
    })
}

fn sync_file(path: &Path) -> Result<(), ProfileBackupError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            ProfileBackupError::unavailable(format!("sync '{}': {error}", path.display()))
        })
}

fn sync_directory(path: &Path) -> Result<(), ProfileBackupError> {
    tracedecay_application::sync_directory(path, DirectorySyncPolicy::Strict).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "sync directory '{}': {error}",
            path.display()
        ))
    })
}

#[cfg(unix)]
fn restrict_private_directory(path: &Path) -> Result<(), ProfileBackupError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        ProfileBackupError::unavailable(format!(
            "restrict directory '{}': {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn restrict_private_directory(_path: &Path) -> Result<(), ProfileBackupError> {
    Ok(())
}
